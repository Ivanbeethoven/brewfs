//! Hold the actual Committer's secondary RPC after a successful primary RPC.
//! No timing race or fabricated transaction-completion flag is used.

use super::*;
use crate::mock::{MockKvClient, MockPdClient};
use crate::proto::{keyspacepb, metapb};
use crate::region::{RegionId, RegionVerId, RegionWithLeader, StoreId};
use crate::store::{KvClient, RegionStore, Request, Store};
use async_trait::async_trait;
use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Delivery {
    prewrites: AtomicUsize,
    primaries: AtomicUsize,
    secondaries: AtomicUsize,
    completed: AtomicUsize,
    entered: Notify,
    resume: Notify,
    terminal: Notify,
    fail_secondary: bool,
}

#[derive(Clone)]
struct HeldKv(Arc<Delivery>);

#[async_trait]
impl KvClient for HeldKv {
    async fn dispatch(&self, request: &dyn Request) -> Result<Box<dyn Any>> {
        let request = request.as_any();
        if request.downcast_ref::<kvrpcpb::PrewriteRequest>().is_some() {
            self.0.prewrites.fetch_add(1, Ordering::SeqCst);
            return Ok(Box::<kvrpcpb::PrewriteResponse>::default());
        }
        if request
            .downcast_ref::<kvrpcpb::BatchRollbackRequest>()
            .is_some()
        {
            return Ok(Box::<kvrpcpb::BatchRollbackResponse>::default());
        }
        let commit = request
            .downcast_ref::<kvrpcpb::CommitRequest>()
            .expect("unexpected request in secondary completion contract");
        assert_eq!(commit.keys.len(), 1);
        if commit.keys[0] == b"a" {
            self.0.primaries.fetch_add(1, Ordering::SeqCst);
            return Ok(Box::<kvrpcpb::CommitResponse>::default());
        }
        assert_eq!(commit.keys[0], b"b");
        self.0.secondaries.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        self.0.resume.notified().await;
        self.0.completed.fetch_add(1, Ordering::SeqCst);
        self.0.terminal.notify_one();
        if self.0.fail_secondary {
            return Err(Error::StringError("held secondary delivery failed".into()));
        }
        Ok(Box::<kvrpcpb::CommitResponse>::default())
    }
}

struct HeldPd {
    routing: MockPdClient,
    client: HeldKv,
    owner: crate::ClientResourceOwner,
}

#[async_trait]
impl PdClient for HeldPd {
    type KvClient = HeldKv;
    fn resource_owner(&self) -> Option<crate::ClientResourceOwner> {
        Some(self.owner.clone())
    }
    async fn map_region_to_store(self: Arc<Self>, region: RegionWithLeader) -> Result<RegionStore> {
        Ok(RegionStore::new(region, Arc::new(self.client.clone())))
    }
    async fn region_for_key(&self, key: &Key) -> Result<RegionWithLeader> {
        self.routing.region_for_key(key).await
    }
    async fn region_for_id(&self, id: RegionId) -> Result<RegionWithLeader> {
        self.routing.region_for_id(id).await
    }
    async fn get_timestamp(self: Arc<Self>) -> Result<Timestamp> {
        Ok(Timestamp::from_version(2))
    }
    async fn update_safepoint(self: Arc<Self>, _: u64) -> Result<bool> {
        unreachable!()
    }
    async fn load_keyspace(&self, _: &str) -> Result<keyspacepb::KeyspaceMeta> {
        unreachable!()
    }
    async fn all_stores(&self) -> Result<Vec<Store>> {
        Ok(vec![Store::new(Arc::new(self.client.clone()))])
    }
    async fn update_leader(&self, _: RegionVerId, _: metapb::Peer) -> Result<()> {
        unreachable!()
    }
    async fn invalidate_region_cache(&self, _: RegionVerId) {
        unreachable!()
    }
    async fn invalidate_store_cache(&self, _: StoreId) {
        unreachable!()
    }
}

#[derive(Debug)]
struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn fixture(
    wait: Option<Duration>,
    fail_secondary: bool,
) -> (
    Committer<HeldPd>,
    Arc<Delivery>,
    crate::ClientResourceOwner,
    Arc<AtomicUsize>,
) {
    let drops = Arc::new(AtomicUsize::new(0));
    let owner = crate::ClientResourceOwner::new(Arc::new(Lease(drops.clone())));
    let delivery = Arc::new(Delivery {
        fail_secondary,
        ..Default::default()
    });
    let pd = Arc::new(HeldPd {
        routing: MockPdClient::new(MockKvClient::default()),
        client: HeldKv(delivery.clone()),
        owner: owner.clone(),
    });
    let mut options = TransactionOptions::new_optimistic();
    if let Some(wait) = wait {
        options = options.wait_for_secondary_commit();
        options.secondary_commit_wait = Some(wait);
    }
    let mutations = [b"a", b"b"]
        .into_iter()
        .map(|key| kvrpcpb::Mutation {
            op: kvrpcpb::Op::Put.into(),
            key: key.to_vec(),
            value: b"value".to_vec(),
            ..Default::default()
        })
        .collect();
    (
        Committer::new(
            Some(Key::from(b"a".to_vec())),
            mutations,
            Timestamp::from_version(1),
            pd,
            options,
            Keyspace::Disable,
            12,
            Instant::now(),
        ),
        delivery,
        owner,
        drops,
    )
}

async fn entered(delivery: &Delivery) {
    tokio::time::timeout(Duration::from_secs(2), delivery.entered.notified())
        .await
        .expect("actual secondary RPC did not reach the held boundary");
    assert_eq!(delivery.prewrites.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.primaries.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn heartbeat_terminal_fast_commits_reuse_owned_task_slots() {
    let (committer, delivery, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let rpc = committer.rpc.clone();
    drop(committer);
    let mut committed = Vec::new();
    for ordinal in 0..128 {
        let mut transaction = Transaction::new(
            Timestamp::from_version(1),
            rpc.clone(),
            TransactionOptions::new_optimistic()
                .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(3600)))
                .wait_for_secondary_commit(),
            Keyspace::Disable,
        );
        transaction.put("a".to_owned(), "value").await.unwrap();
        transaction.commit().await.unwrap_or_else(|error| {
            panic!("completed transactions exhausted owned task slots at {ordinal}: {error}")
        });
        // Keeping the Transaction alive distinguishes Committed notification
        // from a fix that only wakes heartbeats when Transaction is dropped.
        committed.push(transaction);
        tokio::task::yield_now().await;
    }
    assert_eq!(delivery.prewrites.load(Ordering::SeqCst), 128);
    assert_eq!(delivery.primaries.load(Ordering::SeqCst), 128);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 0);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn heartbeat_terminal_rollback_releases_rpc_before_interval() {
    let (committer, _, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let rpc = committer.rpc.clone();
    drop(committer);
    let mut transaction = Transaction::new(
        Timestamp::from_version(1),
        rpc.clone(),
        TransactionOptions::new_optimistic()
            .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(3600))),
        Keyspace::Disable,
    );
    transaction.put("a".to_owned(), "value").await.unwrap();
    transaction.start_auto_heartbeat().await;
    assert_eq!(Arc::strong_count(&rpc), 3);
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_millis(200), async {
        while Arc::strong_count(&rpc) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("rolled-back heartbeat retained its RPC until the heartbeat interval");
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn heartbeat_terminal_drop_releases_rpc_before_interval() {
    let (committer, _, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let rpc = committer.rpc.clone();
    drop(committer);
    let mut transaction = Transaction::new(
        Timestamp::from_version(1),
        rpc.clone(),
        TransactionOptions::new_optimistic()
            .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(3600)))
            .drop_check(CheckLevel::None),
        Keyspace::Disable,
    );
    transaction.put("a".to_owned(), "value").await.unwrap();
    transaction.start_auto_heartbeat().await;
    assert_eq!(Arc::strong_count(&rpc), 3);
    drop(transaction);
    tokio::time::timeout(Duration::from_millis(200), async {
        while Arc::strong_count(&rpc) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropped transaction's heartbeat retained its RPC until the heartbeat interval");
    owner.shutdown().await.unwrap();
}

async fn assert_heartbeat_panic_releases_rpc(unwind: bool) {
    let (committer, _, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let rpc = committer.rpc.clone();
    drop(committer);
    let task_rpc = rpc.clone();
    let failure = tokio::spawn(async move {
        let mut transaction = Transaction::new(
            Timestamp::from_version(1),
            task_rpc,
            TransactionOptions::new_optimistic()
                .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(3600)))
                .drop_check(CheckLevel::Panic),
            Keyspace::Disable,
        );
        transaction.put("a".to_owned(), "value").await.unwrap();
        transaction.start_auto_heartbeat().await;
        if unwind {
            panic!("actual transaction caller unwinds");
        }
        drop(transaction);
    })
    .await
    .unwrap_err();
    assert!(failure.is_panic());
    tokio::time::timeout(Duration::from_millis(200), async {
        while Arc::strong_count(&rpc) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panic left an active heartbeat retaining the RPC");
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn heartbeat_terminal_unwind_releases_rpc_before_interval() {
    assert_heartbeat_panic_releases_rpc(true).await;
}

#[tokio::test]
async fn heartbeat_terminal_active_drop_panic_releases_rpc_before_interval() {
    assert_heartbeat_panic_releases_rpc(false).await;
}

#[tokio::test]
async fn heartbeat_terminal_unknown_commit_keeps_heartbeat_and_single_secondary() {
    let (committer, delivery, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let rpc = committer.rpc.clone();
    drop(committer);
    let mut options = TransactionOptions::new_optimistic()
        .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(3600)))
        .wait_for_secondary_commit();
    options.secondary_commit_wait = Some(Duration::from_millis(50));
    let mut transaction =
        Transaction::new(Timestamp::from_version(1), rpc, options, Keyspace::Disable);
    transaction.put("a".to_owned(), "value").await.unwrap();
    transaction.put("b".to_owned(), "value").await.unwrap();
    assert!(matches!(
        transaction.commit().await,
        Err(Error::UndeterminedError(_))
    ));
    assert!(transaction.get_status() == TransactionStatus::StartedCommit);
    tokio::task::yield_now().await;
    assert_eq!(Arc::strong_count(&transaction.status), 2);
    assert_eq!(delivery.primaries.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    delivery.resume.notify_one();
    delivery.terminal.notified().await;
    drop(transaction);
    owner.shutdown().await.unwrap();
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn secondary_join_success_waits_for_actual_secondary_rpc() {
    let (committer, delivery, owner, drops) = fixture(Some(Duration::from_secs(2)), false);
    let commit = tokio::spawn(committer.commit());
    entered(&delivery).await;
    assert!(
        !commit.is_finished(),
        "commit returned before secondary RPC acknowledgement"
    );
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    delivery.resume.notify_one();
    assert!(commit.await.unwrap().unwrap().is_some());
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn secondary_join_default_options_keep_background_return() {
    let (committer, delivery, owner, _) = fixture(None, false);
    assert!(committer.commit().await.unwrap().is_some());
    entered(&delivery).await;
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    delivery.resume.notify_one();
    delivery.terminal.notified().await;
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn secondary_join_cancelled_waiter_keeps_one_owned_delivery() {
    let (committer, delivery, owner, drops) = fixture(Some(Duration::from_secs(2)), false);
    let commit = tokio::spawn(committer.commit());
    entered(&delivery).await;
    commit.abort();
    assert!(commit.await.unwrap_err().is_cancelled());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    delivery.resume.notify_one();
    delivery.terminal.notified().await;
    owner.shutdown().await.unwrap();
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn secondary_join_error_after_primary_is_undetermined_without_reissue() {
    let (committer, delivery, owner, _) = fixture(Some(Duration::from_secs(2)), true);
    let commit = tokio::spawn(committer.commit());
    entered(&delivery).await;
    delivery.resume.notify_one();
    assert!(matches!(
        commit.await.unwrap(),
        Err(Error::UndeterminedError(_))
    ));
    assert_eq!(delivery.primaries.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn secondary_join_timeout_keeps_owned_delivery_without_reissue() {
    let (committer, delivery, owner, drops) = fixture(Some(Duration::from_millis(100)), false);
    let commit = tokio::spawn(committer.commit());
    entered(&delivery).await;
    assert!(matches!(
        commit.await.unwrap(),
        Err(Error::UndeterminedError(_))
    ));
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    delivery.resume.notify_one();
    delivery.terminal.notified().await;
    owner.shutdown().await.unwrap();
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn secondary_join_owner_shutdown_is_undetermined_without_reissue() {
    let (committer, delivery, owner, _) = fixture(Some(Duration::from_secs(2)), false);
    let commit = tokio::spawn(committer.commit());
    entered(&delivery).await;
    owner.shutdown().await.unwrap();
    assert!(matches!(
        commit.await.unwrap(),
        Err(Error::UndeterminedError(_))
    ));
    assert_eq!(delivery.completed.load(Ordering::SeqCst), 0);
    assert_eq!(delivery.secondaries.load(Ordering::SeqCst), 1);
}

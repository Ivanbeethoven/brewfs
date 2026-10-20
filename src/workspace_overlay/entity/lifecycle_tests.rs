use super::*;
use crate::workspace_overlay::entity::backend::MemoryEntityBackend;
use crate::workspace_overlay::entity::backend::{EntityBackend, RowSnapshot};
use crate::workspace_overlay::entity::ownership::{Ownership, SliceOwnership, SliceState};
use std::sync::atomic::{AtomicBool, Ordering};

fn lower() -> LowerBinding {
    LowerBinding {
        manifest_key: b"verified-reference-manifest".to_vec(),
        manifest_digest: [7; 32],
        root_ino: 1,
        max_ino: 100,
    }
}

async fn fixture() -> (
    Arc<MemoryEntityBackend>,
    Arc<Lifecycle<MemoryEntityBackend>>,
    WriterSession,
) {
    let backend = Arc::new(MemoryEntityBackend::new(10));
    let lifecycle = Arc::new(Lifecycle::new(
        backend.clone(),
        EntityKeys::new(Uuid::new_v4(), Uuid::new_v4()),
    ));
    lifecycle.provision(&lower()).await.unwrap();
    let session = lifecycle.acquire(Uuid::new_v4(), 100).await.unwrap();
    (backend, lifecycle, session)
}

#[tokio::test]
async fn live_or_expired_writer_cannot_be_silently_replaced() {
    let (backend, lifecycle, session) = fixture().await;
    assert!(matches!(
        lifecycle.acquire(Uuid::new_v4(), 100).await,
        Err(EntityError::Busy)
    ));
    backend.set_time(110).unwrap();
    assert!(matches!(
        lifecycle.authority(&session).await,
        Err(EntityError::Fenced)
    ));
    assert!(matches!(
        lifecycle.renew(&session, 100).await,
        Err(EntityError::Fenced)
    ));
    assert!(matches!(
        lifecycle.acquire(Uuid::new_v4(), 100).await,
        Err(EntityError::Busy)
    ));
    assert!(matches!(
        lifecycle.enter_maintenance(&session).await,
        Err(EntityError::Fenced)
    ));
}

#[tokio::test]
async fn backend_time_expiry_fences_prepared_writes_without_partial_mutation() {
    let (backend, lifecycle, session) = fixture().await;
    let authority = lifecycle.authority(&session).await.unwrap();
    let key = lifecycle.keys().xattr(1, b"user.test");
    let plan = EntityPlan {
        checks: vec![RowCheck {
            key: key.clone(),
            expected: None,
        }],
        writes: vec![RowWrite::Put {
            key: key.clone(),
            value: b"must-not-appear".to_vec(),
        }],
    };
    backend.set_time(110).unwrap();
    assert_eq!(
        backend.commit(&authority, &plan).await.unwrap_err(),
        EntityError::Fenced
    );
    assert_eq!(backend.read(&[key]).await.unwrap().rows, vec![None]);
}

#[tokio::test]
async fn renewal_keeps_identity_and_invalidates_old_exact_authority() {
    let (backend, lifecycle, session) = fixture().await;
    let old = lifecycle.authority(&session).await.unwrap();
    let identity = session.identity();
    backend.set_time(50).unwrap();
    lifecycle.renew(&session, 200).await.unwrap();
    assert_eq!(session.identity(), identity);
    assert_eq!(
        backend.commit(&old, &EntityPlan::default()).await.unwrap(),
        CommitOutcome::Conflict
    );
    backend.set_time(200).unwrap();
    lifecycle.authority(&session).await.unwrap();
    let foreign = Lifecycle::new(
        backend.clone(),
        EntityKeys::new(Uuid::new_v4(), Uuid::new_v4()),
    );
    foreign.provision(&lower()).await.unwrap();
    assert!(matches!(
        foreign.renew(&session, 100).await,
        Err(EntityError::Fenced)
    ));
}

#[tokio::test]
async fn missing_lower_pin_prevents_runtime_admission() {
    let (backend, lifecycle, session) = fixture().await;
    let authority = lifecycle.authority(&session).await.unwrap();
    let plan = EntityPlan {
        checks: Vec::new(),
        writes: vec![RowWrite::Delete {
            key: lifecycle.keys().lower_pin(),
        }],
    };
    assert_eq!(
        backend.commit(&authority, &plan).await.unwrap(),
        CommitOutcome::Committed
    );
    assert!(matches!(
        lifecycle.authority(&session).await,
        Err(EntityError::Fenced)
    ));
}

#[tokio::test]
async fn maintenance_waits_for_retained_read_or_handle_guards() {
    let (backend, lifecycle, session) = fixture().await;
    let read_plan = session.admit().unwrap();
    let writer = session.clone();
    let maintenance_lifecycle = lifecycle.clone();
    let task = tokio::spawn(async move { maintenance_lifecycle.enter_maintenance(&writer).await });
    for _ in 0..100 {
        if session.is_closed() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(session.is_closed());
    assert!(!task.is_finished());
    assert!(matches!(session.admit(), Err(EntityError::Fenced)));
    drop(read_plan);
    let token = task.await.unwrap().unwrap();
    assert_eq!(
        lifecycle.descriptor().await.unwrap().state,
        NamespaceState::Quiescent
    );
    backend.set_time(10_000).unwrap();
    lifecycle.maintenance_authority(&token).await.unwrap();
    // Same locally drained session can recover an uncertain successful reply.
    let recovered = lifecycle.enter_maintenance(&session).await.unwrap();
    lifecycle.maintenance_authority(&recovered).await.unwrap();
    assert!(matches!(
        lifecycle.authority(&session).await,
        Err(EntityError::Fenced)
    ));
}

#[tokio::test]
async fn one_hundred_lower_owners_minus_ninety_nine_preserves_the_last_owner() {
    let backend = Arc::new(MemoryEntityBackend::new(10));
    let volume = Uuid::new_v4();
    let mut namespaces = Vec::new();
    for _ in 0..100 {
        let lifecycle = Lifecycle::new(backend.clone(), EntityKeys::new(volume, Uuid::new_v4()));
        lifecycle.provision(&lower()).await.unwrap();
        namespaces.push(lifecycle);
    }
    let registry_key = namespaces[0]
        .keys()
        .lower_registry(&lower().manifest_digest);
    let read_count = |rows: Vec<Option<Vec<u8>>>| {
        decode::<LowerRegistry>(rows[0].as_deref().unwrap())
            .unwrap()
            .owners
    };
    assert_eq!(
        read_count(
            backend
                .read(std::slice::from_ref(&registry_key))
                .await
                .unwrap()
                .rows
        ),
        100
    );
    for lifecycle in namespaces.iter().take(99) {
        let writer = lifecycle.acquire(Uuid::new_v4(), 100).await.unwrap();
        let token = lifecycle.enter_maintenance(&writer).await.unwrap();
        lifecycle.discard_empty(&token).await.unwrap();
        lifecycle.discard_empty(&token).await.unwrap();
    }
    assert_eq!(
        read_count(backend.read(&[registry_key]).await.unwrap().rows),
        1
    );
    let last = &namespaces[99];
    let writer = last.acquire(Uuid::new_v4(), 100).await.unwrap();
    last.authority(&writer).await.unwrap();
    let owner_key = last
        .keys()
        .lower_owner(&lower().manifest_digest, last.keys().namespace_id());
    assert!(backend.read(&[owner_key]).await.unwrap().rows[0].is_some());
}

#[tokio::test]
async fn allocators_preserve_lower_inode_boundary_and_volume_slice_identity() {
    let (backend, lifecycle, session) = fixture().await;
    let authority = lifecycle.authority(&session).await.unwrap();
    assert_eq!(
        backend
            .allocate(&authority, &lifecycle.keys().counter("inode"))
            .await
            .unwrap(),
        101
    );
    assert_eq!(
        backend
            .allocate(&authority, &lifecycle.keys().counter("slice"))
            .await
            .unwrap(),
        1
    );
    // A second namespace in this volume cannot reset the shared slice counter.
    let volume_prefix = lifecycle.keys().volume_prefix();
    let volume_string = std::str::from_utf8(volume_prefix).unwrap();
    let volume_id = Uuid::parse_str(
        volume_string
            .strip_prefix("{brewfs-entity:")
            .unwrap()
            .strip_suffix("}/")
            .unwrap(),
    )
    .unwrap();
    let second = Lifecycle::new(backend.clone(), EntityKeys::new(volume_id, Uuid::new_v4()));
    second.provision(&lower()).await.unwrap();
    let second_session = second.acquire(Uuid::new_v4(), 100).await.unwrap();
    let second_authority = second.authority(&second_session).await.unwrap();
    assert_eq!(
        backend
            .allocate(&second_authority, &second.keys().counter("slice"))
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn equal_namespace_and_lease_uuids_in_different_volumes_cannot_share_authority() {
    let backend = Arc::new(MemoryEntityBackend::new(10));
    let namespace = Uuid::new_v4();
    let lease_id = Uuid::new_v4();
    let first = Lifecycle::new(backend.clone(), EntityKeys::new(Uuid::new_v4(), namespace));
    let second = Lifecycle::new(backend, EntityKeys::new(Uuid::new_v4(), namespace));
    first.provision(&lower()).await.unwrap();
    second.provision(&lower()).await.unwrap();
    let writer_a = first.acquire(lease_id, 100).await.unwrap();
    let writer_b = second.acquire(lease_id, 100).await.unwrap();
    assert_eq!(writer_a.identity(), writer_b.identity());
    let retained_b = writer_b.admit().unwrap();
    assert!(matches!(
        second.authority(&writer_a).await,
        Err(EntityError::Fenced)
    ));
    assert!(matches!(
        second.renew(&writer_a, 100).await,
        Err(EntityError::Fenced)
    ));
    assert!(matches!(
        second.enter_maintenance(&writer_a).await,
        Err(EntityError::Fenced)
    ));
    assert!(
        !writer_a.is_closed(),
        "wrong lifecycle must not close a foreign session"
    );
    let token_a = first.enter_maintenance(&writer_a).await.unwrap();
    assert!(matches!(
        second.maintenance_authority(&token_a).await,
        Err(EntityError::Fenced)
    ));
    assert!(!writer_b.is_closed());
    second.authority(&writer_b).await.unwrap();
    drop(retained_b);
}

/// Models the important uncertain outcome: server committed, caller lost reply.
struct LostReplyBackend {
    inner: MemoryEntityBackend,
    lose_next_commit: AtomicBool,
}

#[async_trait::async_trait]
impl EntityBackend for LostReplyBackend {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot> {
        self.inner.read(keys).await
    }
    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome> {
        let outcome = self.inner.commit(authority, plan).await?;
        if outcome == CommitOutcome::Committed
            && self.lose_next_commit.swap(false, Ordering::SeqCst)
        {
            return Err(EntityError::Backend(
                "injected lost successful reply".into(),
            ));
        }
        Ok(outcome)
    }
    async fn allocate(&self, authority: &TransactionAuthority, key: &[u8]) -> Result<u64> {
        self.inner.allocate(authority, key).await
    }
    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix, limit).await
    }
}

#[tokio::test]
async fn lost_maintenance_and_object_replies_remain_recoverable_without_owner_gaps() {
    let backend = Arc::new(LostReplyBackend {
        inner: MemoryEntityBackend::new(10),
        lose_next_commit: AtomicBool::new(false),
    });
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
    let ownership = Ownership::new(backend.clone(), keys.clone());
    lifecycle.provision(&lower()).await.unwrap();
    let session = lifecycle.acquire(Uuid::new_v4(), 100).await.unwrap();

    // No upload can start without a registration reply. Its uncertain Pending
    // row nevertheless remains durable and cannot be mistaken for garbage.
    backend.lose_next_commit.store(true, Ordering::SeqCst);
    assert!(matches!(
        ownership.register_upload(&session, 1, 16).await,
        Err(EntityError::Backend(_))
    ));
    let pending: SliceOwnership = decode(
        backend.read(&[keys.slice(1)]).await.unwrap().rows[0]
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(pending.state, SliceState::Pending { .. }));
    assert!(pending.staging_hold);

    let upload = ownership.register_upload(&session, 2, 16).await.unwrap();
    backend.lose_next_commit.store(true, Ordering::SeqCst);
    assert!(matches!(
        ownership.uploaded(&session, &upload).await,
        Err(EntityError::Backend(_))
    ));
    ownership.uploaded(&session, &upload).await.unwrap();
    drop(upload);

    backend.lose_next_commit.store(true, Ordering::SeqCst);
    assert!(matches!(
        lifecycle.enter_maintenance(&session).await,
        Err(EntityError::Backend(_))
    ));
    assert!(session.is_closed());
    assert_eq!(
        lifecycle.descriptor().await.unwrap().state,
        NamespaceState::Quiescent
    );
    let maintenance = lifecycle.enter_maintenance(&session).await.unwrap();

    for slice_id in [1, 2] {
        assert!(matches!(
            ownership.reserve_delete(&maintenance, slice_id).await,
            Err(EntityError::Busy)
        ));
        ownership
            .abandon_upload(&maintenance, slice_id)
            .await
            .unwrap();
        backend.lose_next_commit.store(true, Ordering::SeqCst);
        assert!(matches!(
            ownership.reserve_delete(&maintenance, slice_id).await,
            Err(EntityError::Backend(_))
        ));
        let reserved_bytes = backend.read(&[keys.slice(slice_id)]).await.unwrap().rows[0].clone();
        let deletion = ownership
            .reserve_delete(&maintenance, slice_id)
            .await
            .unwrap();
        assert_eq!(
            backend.read(&[keys.slice(slice_id)]).await.unwrap().rows[0],
            reserved_bytes,
            "reservation retry keeps the same durable exclusion identity"
        );
        // The transport's idempotent DELETE would occur before this completion.
        backend.lose_next_commit.store(true, Ordering::SeqCst);
        assert!(matches!(
            ownership.complete_delete(&maintenance, &deletion).await,
            Err(EntityError::Backend(_))
        ));
        ownership
            .complete_delete(&maintenance, &deletion)
            .await
            .unwrap();
        let row: SliceOwnership = decode(
            backend.read(&[keys.slice(slice_id)]).await.unwrap().rows[0]
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(row.state, SliceState::Deleted { .. }));
        assert_eq!(row.current_refs, 0);
    }
    lifecycle.discard_empty(&maintenance).await.unwrap();
}

#[tokio::test]
async fn provision_refuses_stray_upper_rows_without_a_descriptor() {
    let backend = Arc::new(MemoryEntityBackend::new(10));
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let bootstrap = TransactionAuthority::from_validated(
        vec![RowCheck {
            key: keys.descriptor(),
            expected: None,
        }],
        None,
        None,
    )
    .unwrap();
    backend
        .commit(
            &bootstrap,
            &EntityPlan {
                checks: Vec::new(),
                writes: vec![RowWrite::Put {
                    key: keys.inode(101),
                    value: b"stray entity after partial restore".to_vec(),
                }],
            },
        )
        .await
        .unwrap();
    let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
    assert!(matches!(
        lifecycle.provision(&lower()).await,
        Err(EntityError::Corrupt(_))
    ));
    assert!(backend.read(&[keys.descriptor()]).await.unwrap().rows[0].is_none());
}

#[tokio::test]
async fn lost_resume_reply_never_reconstructs_a_second_admission_session() {
    let backend = Arc::new(LostReplyBackend {
        inner: MemoryEntityBackend::new(10),
        lose_next_commit: AtomicBool::new(false),
    });
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let lifecycle = Lifecycle::new(backend.clone(), keys);
    lifecycle.provision(&lower()).await.unwrap();
    let old = lifecycle.acquire(Uuid::new_v4(), 100).await.unwrap();
    let maintenance = lifecycle.enter_maintenance(&old).await.unwrap();
    let new_id = Uuid::new_v4();
    backend.lose_next_commit.store(true, Ordering::SeqCst);
    assert!(matches!(
        lifecycle
            .resume_after_maintenance(&maintenance, new_id, 100)
            .await,
        Err(EntityError::Backend(_))
    ));
    assert_eq!(
        lifecycle.descriptor().await.unwrap().active_lease,
        Some(LeaseIdentity {
            lease_id: new_id,
            holder_generation: old.identity().holder_generation + 1,
        })
    );
    assert!(old.is_closed());
    assert!(matches!(
        lifecycle.authority(&old).await,
        Err(EntityError::Fenced)
    ));
    assert!(matches!(
        lifecycle
            .resume_after_maintenance(&maintenance, new_id, 100)
            .await,
        Err(EntityError::Fenced)
    ));
}

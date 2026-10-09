// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

use std::iter;
use std::sync::atomic;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::Instant;

use derive_new::new;
use fail::fail_point;
use futures::prelude::*;
use log::{debug, error, info, trace, warn};
use tokio::time::Duration;

use crate::backoff::Backoff;
use crate::backoff::DEFAULT_REGION_BACKOFF;
use crate::kv::HexRepr;
use crate::pd::PdClient;
use crate::pd::PdRpcClient;
use crate::proto::kvrpcpb;
use crate::proto::pdpb::Timestamp;
use crate::request::Collect;
use crate::request::CollectError;
use crate::request::CollectSingle;
use crate::request::CollectWithShard;
use crate::request::EncodeKeyspace;
use crate::request::KeyMode;
use crate::request::Keyspace;
use crate::request::Plan;
use crate::request::PlanBuilder;
use crate::request::RetryOptions;
use crate::request::TruncateKeyspace;
use crate::timestamp::TimestampExt;
use crate::transaction::buffer::Buffer;
use crate::transaction::lowering::*;
use crate::BoundRange;
use crate::Error;
use crate::Key;
use crate::KvPair;
use crate::Result;
use crate::Value;

fn bounded_page_end(requested_end: Option<&Key>, region_end: &Key) -> Option<Key> {
    match (requested_end, region_end.is_empty()) {
        (None, true) => None,
        (None, false) => Some(region_end.clone()),
        (Some(end), true) => Some(end.clone()),
        (Some(end), false) => Some(end.min(region_end).clone()),
    }
}

#[cfg(test)]
mod bounded_page_tests {
    use super::*;

    #[test]
    fn single_region_scan_clamps_each_requested_range() {
        let b = Key::from(b"b".to_vec());
        let c = Key::from(b"c".to_vec());
        assert_eq!(bounded_page_end(None, &Key::EMPTY), None);
        assert_eq!(bounded_page_end(None, &b), Some(b.clone()));
        assert_eq!(bounded_page_end(Some(&b), &Key::EMPTY), Some(b.clone()));
        assert_eq!(bounded_page_end(Some(&b), &c), Some(b.clone()));
        assert_eq!(bounded_page_end(Some(&c), &b), Some(b.clone()));
        assert_eq!(bounded_page_end(Some(&b), &b), Some(b));
    }
}

/// An undo-able set of actions on the dataset.
///
/// Create a transaction using a [`TransactionClient`](crate::TransactionClient), then run actions
/// (such as `get`, or `put`) on the transaction. Reads are executed immediately, writes are
/// buffered locally. Once complete, `commit` the transaction. Behind the scenes, the client will
/// perform a two phase commit and return success as soon as the writes are guaranteed to be
/// committed (some finalisation may continue in the background after the return, but no data can be
/// lost).
///
/// TiKV transactions use multi-version concurrency control. All reads logically happen at the start
/// of the transaction (at the start timestamp, `start_ts`). Once a transaction is commited, a
/// its writes atomically become visible to other transactions at (logically) the commit timestamp.
///
/// In other words, a transaction can read data that was committed at `commit_ts` < its `start_ts`,
/// and its writes are readable by transactions with `start_ts` >= its `commit_ts`.
///
/// Mutations are buffered locally and sent to the TiKV cluster at the time of commit.
/// In a pessimistic transaction, all write operations and `xxx_for_update` operations will immediately
/// acquire locks from TiKV. Such a lock blocks other transactions from writing to that key.
/// A lock exists until the transaction is committed or rolled back, or the lock reaches its time to
/// live (TTL).
///
/// For details, the [SIG-Transaction](https://github.com/tikv/sig-transaction)
/// provides materials explaining designs and implementations of TiKV transactions.
///
/// # Examples
///
/// ```rust,no_run
/// # use tikv_client::{Config, TransactionClient};
/// # use futures::prelude::*;
/// # futures::executor::block_on(async {
/// let client = TransactionClient::new(vec!["192.168.0.100"]).await.unwrap();
/// let mut txn = client.begin_optimistic().await.unwrap();
/// let foo = txn.get("foo".to_owned()).await.unwrap().unwrap();
/// txn.put("bar".to_owned(), foo).await.unwrap();
/// txn.commit().await.unwrap();
/// # });
/// ```
pub struct Transaction<PdC: PdClient = PdRpcClient> {
    status: Arc<AtomicU8>,
    terminal_status: Arc<tokio::sync::Notify>,
    timestamp: Timestamp,
    buffer: Buffer,
    rpc: Arc<PdC>,
    options: TransactionOptions,
    keyspace: Keyspace,
    is_heartbeat_started: bool,
    start_instant: Instant,
}

impl<PdC: PdClient> Transaction<PdC> {
    pub(crate) fn new(
        timestamp: Timestamp,
        rpc: Arc<PdC>,
        options: TransactionOptions,
        keyspace: Keyspace,
    ) -> Transaction<PdC> {
        let status = if options.read_only {
            TransactionStatus::ReadOnly
        } else {
            TransactionStatus::Active
        };
        Transaction {
            status: Arc::new(AtomicU8::new(status as u8)),
            terminal_status: Arc::new(tokio::sync::Notify::new()),
            timestamp,
            buffer: Buffer::new(options.is_pessimistic()),
            rpc,
            options,
            keyspace,
            is_heartbeat_started: false,
            start_instant: std::time::Instant::now(),
        }
    }

    /// Create a new 'get' request
    ///
    /// Once resolved this request will result in the fetching of the value associated with the
    /// given key.
    ///
    /// Retuning `Ok(None)` indicates the key does not exist in TiKV.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use tikv_client::{Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key = "TiKV".to_owned();
    /// let result: Option<Value> = txn.get(key).await.unwrap();
    /// # });
    /// ```
    pub async fn get(&mut self, key: impl Into<Key>) -> Result<Option<Value>> {
        trace!("invoking transactional get request");
        self.check_allow_operation().await?;
        let timestamp = self.timestamp.clone();
        let rpc = self.rpc.clone();
        let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
        let retry_options = self.options.retry_options.clone();
        let keyspace = self.keyspace;

        self.buffer
            .get_or_else(key, |key| async move {
                let request = new_get_request(key, timestamp.clone());
                let plan = PlanBuilder::new(rpc, keyspace, request)
                    .resolve_lock(timestamp, retry_options.lock_backoff, keyspace)
                    .retry_multi_region(DEFAULT_REGION_BACKOFF)
                    .merge(CollectSingle)
                    .post_process_default()
                    .plan();
                plan.execute().await
            })
            .await
    }

    /// Read one key at this transaction's timestamp without a read cache,
    /// region fanout, lock resolution, or automatic retry. Region/lock errors
    /// are returned to the caller. Use a configured transport decoding cap.
    pub async fn get_uncached_single_region(
        &mut self,
        key: impl Into<Key>,
    ) -> Result<Option<Value>> {
        self.check_allow_operation().await?;
        if !self.options.read_only {
            return Err(Error::StringError(
                "single-region bounded read requires read-only transaction".into(),
            ));
        }
        let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
        let store = self.rpc.clone().store_for_key(&key).await?;
        let region_version = store.region_with_leader.ver_id();
        let request = new_get_request(key, self.timestamp.clone());
        let result = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
            .single_region_with_store(store)
            .await?
            .extract_error()
            .post_process_default()
            .plan()
            .execute()
            .await;
        if result.is_err() {
            self.rpc.invalidate_region_cache(region_version).await;
        }
        result
    }

    /// One uncached Get at the original read-only timestamp with a fixed
    /// data-batch deadline. Topology inside this data attempt consumes time;
    /// client construction and initial TSO remain outside this deadline.
    /// There is no lock resolver, transaction restart, fanout or retry.
    pub async fn get_uncached_single_region_until(
        &mut self,
        key: impl Into<Key>,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Value>> {
        if tokio::time::Instant::now() >= deadline {
            return Err(crate::request::bounded_read_deadline_error());
        }
        let result = tokio::time::timeout_at(deadline, async {
            self.check_allow_operation().await?;
            if !self.options.read_only {
                return Err(Error::StringError(
                    "single-region bounded read requires read-only transaction".into(),
                ));
            }
            let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
            let store = self.rpc.clone().store_for_key(&key).await?;
            let region_version = store.region_with_leader.ver_id();
            let request = new_get_request(key, self.timestamp.clone());
            let result = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
                .single_region_with_store_until(store, deadline)
                .await?
                .extract_error()
                .post_process_default()
                .plan()
                .execute()
                .await;
            if result.is_err() {
                self.rpc.invalidate_region_cache(region_version).await;
            }
            result
        })
        .await
        .map_err(|_| crate::request::bounded_read_deadline_error())?;
        if tokio::time::Instant::now() >= deadline {
            return Err(crate::request::bounded_read_deadline_error());
        }
        result
    }

    /// One page from the first region in `range`, at this read-only
    /// transaction's timestamp. Never collects all regions or caches rows.
    /// The continuation is inclusive and stays within the requested range.
    /// Region/lock errors fail closed; callers may restart their whole read.
    pub async fn scan_single_region_page(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
    ) -> Result<(Vec<KvPair>, Option<Key>)> {
        self.check_allow_operation().await?;
        if !self.options.read_only || limit == 0 {
            return Err(Error::StringError(
                "single-region bounded scan requires read-only transaction and positive limit"
                    .into(),
            ));
        }
        let (start, requested_end) = range
            .into()
            .encode_keyspace(self.keyspace, KeyMode::Txn)
            .into_keys();
        if requested_end.as_ref().is_some_and(|end| &start >= end) {
            return Ok((Vec::new(), None));
        }
        let store = self.rpc.clone().store_for_key(&start).await?;
        let region_version = store.region_with_leader.ver_id();
        let region_end = store.region_with_leader.end_key();
        let page_end = bounded_page_end(requested_end.as_ref(), &region_end);
        if page_end.as_ref().is_some_and(|end| &start >= end) {
            return Err(Error::StringError(
                "single-region scan has a nonadvancing region fence".into(),
            ));
        }
        let request = crate::transaction::requests::new_scan_request(
            start.clone().into(),
            page_end.clone().unwrap_or_default().into(),
            self.timestamp.version(),
            limit,
            false,
            false,
        );
        let result = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
            .single_region_with_store(store)
            .await?
            .extract_error()
            .plan()
            .execute()
            .await;
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.rpc.invalidate_region_cache(region_version).await;
                return Err(error);
            }
        };
        if response.pairs.len() > limit as usize {
            return Err(Error::StringError(
                "single-region scan exceeded record limit".into(),
            ));
        }
        let mut previous: Option<Key> = None;
        for pair in &response.pairs {
            let key = Key::from(pair.key.clone());
            if key < start
                || page_end.as_ref().is_some_and(|end| &key >= end)
                || previous.as_ref().is_some_and(|last| &key <= last)
            {
                return Err(Error::StringError(
                    "single-region scan returned invalid key order/fence".into(),
                ));
            }
            previous = Some(key);
        }
        let continuation = if response.pairs.len() == limit as usize {
            previous.map(|last| last.next_key())
        } else {
            page_end
        }
        .filter(|next| requested_end.as_ref().is_none_or(|end| next < end));
        let pairs = response
            .pairs
            .into_iter()
            .map(|pair| KvPair::from(pair).truncate_keyspace(self.keyspace))
            .collect();
        Ok((
            pairs,
            continuation.map(|key| key.truncate_keyspace(self.keyspace)),
        ))
    }

    /// Create a `get for update` request.
    ///
    /// The request reads and "locks" a key. It is similar to `SELECT ... FOR
    /// UPDATE` in TiDB, and has different behavior in optimistic and
    /// pessimistic transactions.
    ///
    /// # Optimistic transaction
    ///
    /// It reads at the "start timestamp" and caches the value, just like normal
    /// get requests. The lock is written in prewrite and commit, so it cannot
    /// prevent concurrent transactions from writing the same key, but can only
    /// prevent itself from committing.
    ///
    /// # Pessimistic transaction
    ///
    /// It reads at the "current timestamp" and thus does not cache the value.
    /// So following read requests won't be affected by the `get_for_udpate`.
    /// A lock will be acquired immediately with this request, which prevents
    /// concurrent transactions from mutating the keys.
    ///
    /// The "current timestamp" (also called `for_update_ts` of the request) is fetched from PD.
    ///
    /// Note: The behavior of this command under pessimistic transaction does not follow snapshot.
    /// It reads the latest value (using current timestamp), and the value is not cached in the
    /// local buffer. So normal `get`-like commands after `get_for_update` will not be influenced,
    /// they still read values at the transaction's `start_ts`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_pessimistic().await.unwrap();
    /// let key = "TiKV".to_owned();
    /// let result: Value = txn.get_for_update(key).await.unwrap().unwrap();
    /// // now the key "TiKV" is locked, other transactions cannot modify it
    /// // Finish the transaction...
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn get_for_update(&mut self, key: impl Into<Key>) -> Result<Option<Value>> {
        debug!("invoking transactional get_for_update request");
        self.check_allow_operation().await?;
        if !self.is_pessimistic() {
            let key = key.into();
            self.lock_keys(iter::once(key.clone())).await?;
            self.get(key).await
        } else {
            let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
            let mut pairs = self.pessimistic_lock(iter::once(key), true).await?;
            debug_assert!(pairs.len() <= 1);
            match pairs.pop() {
                Some(pair) => Ok(Some(pair.1)),
                None => Ok(None),
            }
        }
    }

    /// One uncached locking read with a hard decoder and fixed batch deadline.
    /// The caller must roll back after any error, including an unknown reply.
    /// No ordinary lock resolver, region retry, transaction restart or fanout.
    pub async fn get_for_update_uncached_single_region_until(
        &mut self,
        key: impl Into<Key>,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Value>> {
        if tokio::time::Instant::now() >= deadline {
            return Err(crate::request::bounded_read_deadline_error());
        }
        let response = tokio::time::timeout_at(deadline, async {
            self.check_allow_operation().await?;
            if !self.is_pessimistic() || self.options.read_only {
                return Err(Error::StringError(
                    "bounded locking read requires writable pessimistic transaction".into(),
                ));
            }
            let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
            let primary = self.buffer.get_primary_key().unwrap_or_else(|| key.clone());
            let for_update_ts = self.rpc.clone().get_timestamp().await?;
            self.options.push_for_update_ts(for_update_ts.clone());
            let store = self.rpc.clone().store_for_key(&key).await?;
            let mut request = new_pessimistic_lock_request(
                iter::once(key.clone()),
                primary.clone(),
                self.timestamp.clone(),
                MAX_TTL,
                for_update_ts.clone(),
                true,
            );
            request.check_existence = true;
            // An oversized/rejected/lost response can follow a successful server
            // lock. Register its possible lock before dispatch for rollback.
            self.buffer.lock(key.clone());
            let mut result = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
                .single_region_with_store_until(store, deadline)
                .await?
                .extract_error()
                .plan()
                .execute()
                .await;
            if let Err(Error::GrpcAPI(status)) = &mut result {
                crate::bounded_codec::certify_authentication_write_conflict(
                    status,
                    (&key).into(),
                    (&primary).into(),
                    self.timestamp.version(),
                    for_update_ts.version(),
                );
            }
            result
        })
        .await
        .map_err(|_| crate::request::bounded_read_deadline_error())??;
        if tokio::time::Instant::now() >= deadline {
            return Err(crate::request::bounded_read_deadline_error());
        }
        // The codec limits these counts before prost allocates their vectors.
        // Exact presence is mandatory; ambiguous legacy empty values fail closed.
        if response.values.len() != 1
            || response.not_founds.len() != 1
            || !response.results.is_empty()
        {
            return Err(Error::StringError(
                "malformed bounded single-key locking response".into(),
            ));
        }
        self.start_auto_heartbeat().await;
        let mut values = response.values;
        let value = values.pop().unwrap();
        if response.not_founds[0] {
            if !value.is_empty() {
                return Err(Error::StringError(
                    "bounded missing-key locking response contains a value".into(),
                ));
            }
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Check whether a key exists.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_pessimistic().await.unwrap();
    /// let exists = txn.key_exists("k1".to_owned()).await.unwrap();
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn key_exists(&mut self, key: impl Into<Key>) -> Result<bool> {
        debug!("invoking transactional key_exists request");
        Ok(self.get(key).await?.is_some())
    }

    /// Create a new 'batch get' request.
    ///
    /// Once resolved this request will result in the fetching of the values associated with the
    /// given keys.
    ///
    /// Non-existent entries will not appear in the result. The order of the keys is not retained in
    /// the result.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # use std::collections::HashMap;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let keys = vec!["TiKV".to_owned(), "TiDB".to_owned()];
    /// let result: HashMap<Key, Value> = txn
    ///     .batch_get(keys)
    ///     .await
    ///     .unwrap()
    ///     .map(|pair| (pair.0, pair.1))
    ///     .collect();
    /// // Finish the transaction...
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn batch_get(
        &mut self,
        keys: impl IntoIterator<Item = impl Into<Key>>,
    ) -> Result<impl Iterator<Item = KvPair>> {
        debug!("invoking transactional batch_get request");
        self.check_allow_operation().await?;
        let timestamp = self.timestamp.clone();
        let rpc = self.rpc.clone();
        let keyspace = self.keyspace;
        let keys = keys
            .into_iter()
            .map(move |k| k.into().encode_keyspace(keyspace, KeyMode::Txn));
        let retry_options = self.options.retry_options.clone();

        self.buffer
            .batch_get_or_else(keys, move |keys| async move {
                let request = new_batch_get_request(keys, timestamp.clone());
                let plan = PlanBuilder::new(rpc, keyspace, request)
                    .resolve_lock(timestamp, retry_options.lock_backoff, keyspace)
                    .retry_multi_region(retry_options.region_backoff)
                    .merge(Collect)
                    .plan();
                plan.execute()
                    .await
                    .map(|r| r.into_iter().map(Into::into).collect())
            })
            .await
            .map(move |pairs| pairs.map(move |pair| pair.truncate_keyspace(keyspace)))
    }

    /// Create a new 'batch get for update' request.
    ///
    /// Similar to [`get_for_update`](Transaction::get_for_update), but it works
    /// for a batch of keys.
    ///
    /// Non-existent entries will not appear in the result. The order of the
    /// keys is not retained in the result.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Value, Config, TransactionClient, KvPair};
    /// # use futures::prelude::*;
    /// # use std::collections::HashMap;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_pessimistic().await.unwrap();
    /// let keys = vec!["foo".to_owned(), "bar".to_owned()];
    /// let result: Vec<KvPair> = txn
    ///     .batch_get_for_update(keys)
    ///     .await
    ///     .unwrap();
    /// // now "foo" and "bar" are both locked
    /// // Finish the transaction...
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn batch_get_for_update(
        &mut self,
        keys: impl IntoIterator<Item = impl Into<Key>>,
    ) -> Result<Vec<KvPair>> {
        debug!("invoking transactional batch_get_for_update request");
        self.check_allow_operation().await?;
        if !self.is_pessimistic() {
            let keys: Vec<Key> = keys.into_iter().map(|k| k.into()).collect();
            self.lock_keys(keys.clone()).await?;
            Ok(self.batch_get(keys).await?.collect())
        } else {
            let keyspace = self.keyspace;
            let keys = keys
                .into_iter()
                .map(move |k| k.into().encode_keyspace(keyspace, KeyMode::Txn));
            let pairs = self
                .pessimistic_lock(keys, true)
                .await?
                .truncate_keyspace(keyspace);
            Ok(pairs)
        }
    }

    /// Create a new 'scan' request.
    ///
    /// Once resolved this request will result in a `Vec` of all key-value pairs that lie in the
    /// specified range.
    ///
    /// If the number of eligible key-value pairs are greater than `limit`,
    /// only the first `limit` pairs are returned, ordered by key.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, KvPair, Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # use std::collections::HashMap;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key1: Key = b"foo".to_vec().into();
    /// let key2: Key = b"bar".to_vec().into();
    /// let result: Vec<KvPair> = txn
    ///     .scan(key1..key2, 10)
    ///     .await
    ///     .unwrap()
    ///     .collect();
    /// // Finish the transaction...
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn scan(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
    ) -> Result<impl Iterator<Item = KvPair>> {
        debug!("invoking transactional scan request");
        self.scan_inner(range, limit, false, false).await
    }

    /// Create a new 'scan' request that only returns the keys.
    ///
    /// Once resolved this request will result in a `Vec` of keys that lies in the specified range.
    ///
    /// If the number of eligible keys are greater than `limit`,
    /// only the first `limit` keys are returned, ordered by key.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, KvPair, Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # use std::collections::HashMap;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key1: Key = b"foo".to_vec().into();
    /// let key2: Key = b"bar".to_vec().into();
    /// let result: Vec<Key> = txn
    ///     .scan_keys(key1..key2, 10)
    ///     .await
    ///     .unwrap()
    ///     .collect();
    /// // Finish the transaction...
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn scan_keys(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
    ) -> Result<impl Iterator<Item = Key>> {
        debug!("invoking transactional scan_keys request");
        Ok(self
            .scan_inner(range, limit, true, false)
            .await?
            .map(KvPair::into_key))
    }

    /// Create a 'scan_reverse' request.
    ///
    /// Similar to [`scan`](Transaction::scan), but scans in the reverse direction.
    pub async fn scan_reverse(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
    ) -> Result<impl Iterator<Item = KvPair>> {
        debug!("invoking transactional scan_reverse request");
        self.scan_inner(range, limit, false, true).await
    }

    /// Create a 'scan_keys_reverse' request.
    ///
    /// Similar to [`scan`](Transaction::scan_keys), but scans in the reverse direction.
    pub async fn scan_keys_reverse(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
    ) -> Result<impl Iterator<Item = Key>> {
        debug!("invoking transactional scan_keys_reverse request");
        Ok(self
            .scan_inner(range, limit, true, true)
            .await?
            .map(KvPair::into_key))
    }

    /// Sets the value associated with the given key.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key = "foo".to_owned();
    /// let val = "FOO".to_owned();
    /// txn.put(key, val);
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn put(&mut self, key: impl Into<Key>, value: impl Into<Value>) -> Result<()> {
        trace!("invoking transactional put request");
        self.check_allow_operation().await?;
        let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
        if self.is_pessimistic() {
            self.pessimistic_lock(iter::once(key.clone()), false)
                .await?;
        }
        self.buffer.put(key, value.into());
        Ok(())
    }

    /// Inserts the value associated with the given key.
    ///
    /// Similar to [`put'], but it has an additional constraint that the key should not exist
    /// before this operation.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Value, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key = "foo".to_owned();
    /// let val = "FOO".to_owned();
    /// txn.insert(key, val);
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn insert(&mut self, key: impl Into<Key>, value: impl Into<Value>) -> Result<()> {
        debug!("invoking transactional insert request");
        self.check_allow_operation().await?;
        let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
        if self.buffer.get(&key).is_some() {
            return Err(Error::DuplicateKeyInsertion);
        }
        if self.is_pessimistic() {
            self.pessimistic_lock(
                iter::once((key.clone(), kvrpcpb::Assertion::NotExist)),
                false,
            )
            .await?;
        }
        self.buffer.insert(key, value.into());
        Ok(())
    }

    /// Deletes the given key and its value from the database.
    ///
    /// Deleting a non-existent key will not result in an error.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let key = "foo".to_owned();
    /// txn.delete(key);
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn delete(&mut self, key: impl Into<Key>) -> Result<()> {
        debug!("invoking transactional delete request");
        self.check_allow_operation().await?;
        let key = key.into().encode_keyspace(self.keyspace, KeyMode::Txn);
        if self.is_pessimistic() {
            self.pessimistic_lock(iter::once(key.clone()), false)
                .await?;
        }
        self.buffer.delete(key);
        Ok(())
    }

    /// Batch mutate the database.
    ///
    /// Only `Put` and `Delete` are supported.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Key, Config, TransactionClient, transaction::Mutation};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100", "192.168.0.101"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// let mutations = vec![
    ///     Mutation::Delete("k0".to_owned().into()),
    ///     Mutation::Put("k1".to_owned().into(), b"v1".to_vec()),
    /// ];
    /// txn.batch_mutate(mutations).await.unwrap();
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn batch_mutate(
        &mut self,
        mutations: impl IntoIterator<Item = Mutation>,
    ) -> Result<()> {
        debug!("invoking transactional batch mutate request");
        self.check_allow_operation().await?;
        let mutations: Vec<Mutation> = mutations
            .into_iter()
            .map(|mutation| mutation.encode_keyspace(self.keyspace, KeyMode::Txn))
            .collect();
        if self.is_pessimistic() {
            self.pessimistic_lock(mutations.iter().map(|m| m.key().clone()), false)
                .await?;
            for m in mutations {
                self.buffer.mutate(m);
            }
        } else {
            for m in mutations.into_iter() {
                self.buffer.mutate(m);
            }
        }
        Ok(())
    }

    /// Lock the given keys without mutating their values.
    ///
    /// In optimistic mode, write conflicts are not checked until commit.
    /// So use this command to indicate that
    /// "I do not want to commit if the value associated with this key has been modified".
    /// It's useful to avoid the *write skew* anomaly.
    ///
    /// In pessimistic mode, it is similar to [`batch_get_for_update`](Transaction::batch_get_for_update),
    /// except that it does not read values.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Config, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// txn.lock_keys(vec!["TiKV".to_owned(), "Rust".to_owned()]);
    /// // ... Do some actions.
    /// txn.commit().await.unwrap();
    /// # });
    /// ```
    pub async fn lock_keys(
        &mut self,
        keys: impl IntoIterator<Item = impl Into<Key>>,
    ) -> Result<()> {
        debug!("invoking transactional lock_keys request");
        self.check_allow_operation().await?;
        let keyspace = self.keyspace;
        let keys = keys
            .into_iter()
            .map(move |k| k.into().encode_keyspace(keyspace, KeyMode::Txn));
        match self.options.kind {
            TransactionKind::Optimistic => {
                for key in keys {
                    self.buffer.lock(key);
                }
            }
            TransactionKind::Pessimistic(_) => {
                self.pessimistic_lock(keys, false).await?;
            }
        }
        Ok(())
    }

    /// Commits the actions of the transaction. On success, we return the commit timestamp (or
    /// `None` if there was nothing to commit).
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Config, Timestamp, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// // ... Do some actions.
    /// let result: Timestamp = txn.commit().await.unwrap().unwrap();
    /// # });
    /// ```
    pub async fn commit(&mut self) -> Result<Option<Timestamp>> {
        debug!("commiting transaction");
        if !self.transit_status(
            |status| {
                matches!(
                    status,
                    TransactionStatus::StartedCommit | TransactionStatus::Active
                )
            },
            TransactionStatus::StartedCommit,
        ) {
            return Err(Error::OperationAfterCommitError);
        }

        let primary_key = self.buffer.get_primary_key();
        let mutations = self.buffer.to_proto_mutations();
        if mutations.is_empty() {
            assert!(primary_key.is_none());
            return Ok(None);
        }

        self.start_auto_heartbeat().await;

        let res = Committer::new(
            primary_key,
            mutations,
            self.timestamp.clone(),
            self.rpc.clone(),
            self.options.clone(),
            self.keyspace,
            self.buffer.get_write_size() as u64,
            self.start_instant,
        )
        .commit()
        .await;

        if res.is_ok() {
            self.set_status(TransactionStatus::Committed);
        }
        res
    }

    /// Rollback the transaction.
    ///
    /// If it succeeds, all mutations made by this transaction will be discarded.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tikv_client::{Config, Timestamp, TransactionClient};
    /// # use futures::prelude::*;
    /// # futures::executor::block_on(async {
    /// # let client = TransactionClient::new(vec!["192.168.0.100"]).await.unwrap();
    /// let mut txn = client.begin_optimistic().await.unwrap();
    /// // ... Do some actions.
    /// txn.rollback().await.unwrap();
    /// # });
    /// ```
    pub async fn rollback(&mut self) -> Result<()> {
        debug!("rolling back transaction");
        if !self.transit_status(
            |status| {
                matches!(
                    status,
                    TransactionStatus::StartedRollback
                        | TransactionStatus::Active
                        | TransactionStatus::StartedCommit
                )
            },
            TransactionStatus::StartedRollback,
        ) {
            return Err(Error::OperationAfterCommitError);
        }

        let primary_key = self.buffer.get_primary_key();
        let mutations = self.buffer.to_proto_mutations();
        let res = Committer::new(
            primary_key,
            mutations,
            self.timestamp.clone(),
            self.rpc.clone(),
            self.options.clone(),
            self.keyspace,
            self.buffer.get_write_size() as u64,
            self.start_instant,
        )
        .rollback()
        .await;

        if res.is_ok() {
            self.set_status(TransactionStatus::Rolledback);
        }
        res
    }

    /// Get the start timestamp of this transaction.
    pub fn start_timestamp(&self) -> Timestamp {
        self.timestamp.clone()
    }

    /// Send a heart beat message to keep the transaction alive on the server and update its TTL.
    ///
    /// Returns the TTL set on the transaction's locks by TiKV.
    #[doc(hidden)]
    pub async fn send_heart_beat(&mut self) -> Result<u64> {
        debug!("sending heart_beat");
        self.check_allow_operation().await?;
        let primary_key = match self.buffer.get_primary_key() {
            Some(k) => k,
            None => return Err(Error::NoPrimaryKey),
        };
        let request = new_heart_beat_request(
            self.timestamp.clone(),
            primary_key,
            self.start_instant.elapsed().as_millis() as u64 + MAX_TTL,
        );
        let plan = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
            .resolve_lock(
                self.timestamp.clone(),
                self.options.retry_options.lock_backoff.clone(),
                self.keyspace,
            )
            .retry_multi_region(self.options.retry_options.region_backoff.clone())
            .extract_error()
            .merge(CollectSingle)
            .post_process_default()
            .plan();
        plan.execute().await
    }

    async fn scan_inner(
        &mut self,
        range: impl Into<BoundRange>,
        limit: u32,
        key_only: bool,
        reverse: bool,
    ) -> Result<impl Iterator<Item = KvPair>> {
        self.check_allow_operation().await?;
        let timestamp = self.timestamp.clone();
        let rpc = self.rpc.clone();
        let retry_options = self.options.retry_options.clone();
        let keyspace = self.keyspace;
        let range = range.into().encode_keyspace(self.keyspace, KeyMode::Txn);

        self.buffer
            .scan_and_fetch(
                range,
                limit,
                !key_only,
                reverse,
                move |new_range, new_limit| async move {
                    let request = new_scan_request(
                        new_range,
                        timestamp.clone(),
                        new_limit,
                        key_only,
                        reverse,
                    );
                    let plan = PlanBuilder::new(rpc, keyspace, request)
                        .resolve_lock(timestamp, retry_options.lock_backoff, keyspace)
                        .retry_multi_region(retry_options.region_backoff)
                        .merge(Collect)
                        .plan();
                    plan.execute()
                        .await
                        .map(|r| r.into_iter().map(Into::into).collect())
                },
            )
            .await
            .map(move |pairs| pairs.map(move |pair| pair.truncate_keyspace(keyspace)))
    }

    /// Pessimistically lock the keys, and optionally retrieve corresponding values.
    /// If a key does not exist, the corresponding pair will not appear in the result.
    ///
    /// Once resolved it acquires locks on the keys in TiKV.
    /// A lock prevents other transactions from mutating the entry until it is released.
    ///
    /// # Panics
    ///
    /// Only valid for pessimistic transactions, panics if called on an optimistic transaction.
    async fn pessimistic_lock(
        &mut self,
        keys: impl IntoIterator<Item = impl PessimisticLock>,
        need_value: bool,
    ) -> Result<Vec<KvPair>> {
        debug!("acquiring pessimistic lock");
        assert!(
            matches!(self.options.kind, TransactionKind::Pessimistic(_)),
            "`pessimistic_lock` is only valid to use with pessimistic transactions"
        );

        let keys: Vec<_> = keys.into_iter().collect();
        if keys.is_empty() {
            return Ok(vec![]);
        }

        let first_key = keys[0].clone().key();
        // we do not set the primary key here, because pessimistic lock request
        // can fail, in which case the keys may not be part of the transaction.
        let primary_lock = self
            .buffer
            .get_primary_key()
            .unwrap_or_else(|| first_key.clone());
        let for_update_ts = self.rpc.clone().get_timestamp().await?;
        self.options.push_for_update_ts(for_update_ts.clone());
        let request = new_pessimistic_lock_request(
            keys.clone().into_iter(),
            primary_lock,
            self.timestamp.clone(),
            MAX_TTL,
            for_update_ts.clone(),
            need_value,
        );
        let plan = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
            .resolve_lock(
                self.timestamp.clone(),
                self.options.retry_options.lock_backoff.clone(),
                self.keyspace,
            )
            .preserve_shard()
            .retry_multi_region_preserve_results(self.options.retry_options.region_backoff.clone())
            .merge(CollectWithShard)
            .plan();
        let pairs = plan.execute().await;

        if let Err(err) = pairs {
            match err {
                Error::PessimisticLockError {
                    inner,
                    success_keys,
                } if !success_keys.is_empty() => {
                    let keys = success_keys.into_iter().map(Key::from);
                    self.pessimistic_lock_rollback(keys, self.timestamp.clone(), for_update_ts)
                        .await?;
                    Err(*inner)
                }
                _ => Err(err),
            }
        } else {
            // primary key will be set here if needed
            self.buffer.primary_key_or(&first_key);

            self.start_auto_heartbeat().await;

            for key in keys {
                self.buffer.lock(key.key());
            }

            pairs
        }
    }

    /// Rollback pessimistic lock
    async fn pessimistic_lock_rollback(
        &mut self,
        keys: impl Iterator<Item = Key>,
        start_version: Timestamp,
        for_update_ts: Timestamp,
    ) -> Result<()> {
        debug!("rollback pessimistic lock");

        let keys: Vec<_> = keys.into_iter().collect();
        if keys.is_empty() {
            return Ok(());
        }

        let req = new_pessimistic_rollback_request(
            keys.clone().into_iter(),
            start_version.clone(),
            for_update_ts,
        );
        let plan = PlanBuilder::new(self.rpc.clone(), self.keyspace, req)
            .resolve_lock(
                start_version,
                self.options.retry_options.lock_backoff.clone(),
                self.keyspace,
            )
            .retry_multi_region(self.options.retry_options.region_backoff.clone())
            .extract_error()
            .plan();
        plan.execute().await?;

        for key in keys {
            self.buffer.unlock(&key);
        }
        Ok(())
    }

    /// Checks if the transaction can perform arbitrary operations.
    async fn check_allow_operation(&self) -> Result<()> {
        match self.get_status() {
            TransactionStatus::ReadOnly | TransactionStatus::Active => Ok(()),
            TransactionStatus::Committed
            | TransactionStatus::Rolledback
            | TransactionStatus::StartedCommit
            | TransactionStatus::StartedRollback
            | TransactionStatus::Dropped => Err(Error::OperationAfterCommitError),
        }
    }

    fn is_pessimistic(&self) -> bool {
        matches!(self.options.kind, TransactionKind::Pessimistic(_))
    }

    async fn start_auto_heartbeat(&mut self) {
        debug!("starting auto_heartbeat");
        if !self.options.heartbeat_option.is_auto_heartbeat() || self.is_heartbeat_started {
            return;
        }
        self.is_heartbeat_started = true;

        let status = self.status.clone();
        let terminal_status = self.terminal_status.clone();
        let primary_key = self
            .buffer
            .get_primary_key()
            .expect("Primary key should exist");
        let start_ts = self.timestamp.clone();
        let region_backoff = self.options.retry_options.region_backoff.clone();
        let rpc = self.rpc.clone();
        let heartbeat_interval = match self.options.heartbeat_option {
            HeartbeatOption::NoHeartbeat => DEFAULT_HEARTBEAT_INTERVAL,
            HeartbeatOption::FixedTime(heartbeat_interval) => heartbeat_interval,
        };
        let start_instant = self.start_instant;
        let keyspace = self.keyspace;

        let heartbeat_task = async move {
            loop {
                // Register before inspecting status so a concurrent terminal
                // transition cannot be lost between the check and the wait.
                let terminal = terminal_status.notified();
                tokio::pin!(terminal);
                terminal.as_mut().enable();
                {
                    let status: TransactionStatus = status.load(atomic::Ordering::Acquire).into();
                    if matches!(
                        status,
                        TransactionStatus::Rolledback
                            | TransactionStatus::Committed
                            | TransactionStatus::Dropped
                    ) {
                        break;
                    }
                }
                tokio::select! {
                    _ = &mut terminal => continue,
                    _ = tokio::time::sleep(heartbeat_interval) => {}
                }
                let current: TransactionStatus = status.load(atomic::Ordering::Acquire).into();
                if matches!(
                    current,
                    TransactionStatus::Rolledback
                        | TransactionStatus::Committed
                        | TransactionStatus::Dropped
                ) {
                    break;
                }
                let request = new_heart_beat_request(
                    start_ts.clone(),
                    primary_key.clone(),
                    start_instant.elapsed().as_millis() as u64 + MAX_TTL,
                );
                let plan = PlanBuilder::new(rpc.clone(), keyspace, request)
                    .retry_multi_region(region_backoff.clone())
                    .merge(CollectSingle)
                    .plan();
                plan.execute().await?;
            }
            Ok::<(), Error>(())
        };

        let owner = self.rpc.resource_owner();
        if let Err(error) = crate::resources::spawn_owned(owner, async {
            if let Err(err) = heartbeat_task.await {
                log::error!("Error: While sending heartbeat. {}", err);
            }
        }) {
            log::error!("Cannot start heartbeat task: {}", error);
        }
    }

    fn get_status(&self) -> TransactionStatus {
        self.status.load(atomic::Ordering::Acquire).into()
    }

    fn set_status(&self, status: TransactionStatus) {
        self.status.store(status as u8, atomic::Ordering::Release);
        if matches!(
            status,
            TransactionStatus::Rolledback
                | TransactionStatus::Committed
                | TransactionStatus::Dropped
        ) {
            self.terminal_status.notify_waiters();
        }
    }

    fn transit_status<F>(&self, check_status: F, next: TransactionStatus) -> bool
    where
        F: Fn(TransactionStatus) -> bool,
    {
        let mut current = self.get_status();
        while check_status(current) {
            if current == next {
                return true;
            }
            match self.status.compare_exchange_weak(
                current as u8,
                next as u8,
                atomic::Ordering::AcqRel,
                atomic::Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(x) => current = x.into(),
            }
        }
        false
    }
}

impl<PdC: PdClient> Drop for Transaction<PdC> {
    fn drop(&mut self) {
        debug!("dropping transaction");
        let previous_status = self.get_status();
        // Retire the heartbeat even when unwinding or the active-transaction
        // drop check itself panics. Keep that check's original behavior.
        self.set_status(TransactionStatus::Dropped);
        if std::thread::panicking() {
            return;
        }
        if previous_status == TransactionStatus::Active {
            match self.options.check_level {
                CheckLevel::Panic => {
                    panic!("Dropping an active transaction. Consider commit or rollback it.")
                }
                CheckLevel::Warn => {
                    warn!("Dropping an active transaction. Consider commit or rollback it.")
                }

                CheckLevel::None => {}
            }
        }
    }
}

/// The default max TTL of a lock in milliseconds. Also called `ManagedLockTTL` in TiDB.
const MAX_TTL: u64 = 20000;
/// The default TTL of a lock in milliseconds.
const DEFAULT_LOCK_TTL: u64 = 3000;
/// The default heartbeat interval
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(MAX_TTL / 2);
/// TiKV recommends each RPC packet should be less than around 1MB. We keep KV size of
/// each request below 16KB.
pub const TXN_COMMIT_BATCH_SIZE: u64 = 16 * 1024;
const TTL_FACTOR: f64 = 6000.0;

/// Optimistic or pessimistic transaction.
#[derive(Clone, PartialEq, Debug)]
pub enum TransactionKind {
    Optimistic,
    /// Argument is the transaction's for_update_ts
    Pessimistic(Timestamp),
}

/// Options for configuring a transaction.
///
/// `TransactionOptions` has a builder-style API.
#[derive(Clone, PartialEq, Debug)]
pub struct TransactionOptions {
    /// Optimistic or pessimistic (default) transaction.
    kind: TransactionKind,
    /// Try using 1pc rather than 2pc (default is to always use 2pc).
    try_one_pc: bool,
    /// Try to use async commit (default is not to).
    async_commit: bool,
    /// Wait for this transaction's owned secondary task before reporting success.
    secondary_commit_wait: Option<Duration>,
    /// Is the transaction read only? (Default is no).
    read_only: bool,
    /// How to retry in the event of certain errors.
    retry_options: RetryOptions,
    /// What to do if the transaction is dropped without an attempt to commit or rollback
    check_level: CheckLevel,
    #[doc(hidden)]
    heartbeat_option: HeartbeatOption,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HeartbeatOption {
    NoHeartbeat,
    FixedTime(Duration),
}

impl Default for TransactionOptions {
    fn default() -> TransactionOptions {
        Self::new_pessimistic()
    }
}

impl TransactionOptions {
    /// Default options for an optimistic transaction.
    pub fn new_optimistic() -> TransactionOptions {
        TransactionOptions {
            kind: TransactionKind::Optimistic,
            try_one_pc: false,
            async_commit: false,
            secondary_commit_wait: None,
            read_only: false,
            retry_options: RetryOptions::default_optimistic(),
            check_level: CheckLevel::Panic,
            heartbeat_option: HeartbeatOption::FixedTime(DEFAULT_HEARTBEAT_INTERVAL),
        }
    }

    /// Default options for a pessimistic transaction.
    pub fn new_pessimistic() -> TransactionOptions {
        TransactionOptions {
            kind: TransactionKind::Pessimistic(Timestamp::from_version(0)),
            try_one_pc: false,
            async_commit: false,
            secondary_commit_wait: None,
            read_only: false,
            retry_options: RetryOptions::default_pessimistic(),
            check_level: CheckLevel::Panic,
            heartbeat_option: HeartbeatOption::FixedTime(DEFAULT_HEARTBEAT_INTERVAL),
        }
    }

    /// Try to use async commit.
    #[must_use]
    pub fn use_async_commit(mut self) -> TransactionOptions {
        self.async_commit = true;
        self
    }

    /// Wait up to 30 seconds for the actual owned secondary-commit task.
    ///
    /// Strict bounded readers do not resolve locks. Catalogs using those
    /// readers must finish their own secondary writes before returning success.
    /// Failure or timeout after the primary decision is an undetermined result;
    /// it does not authorize another transaction or replacement commit request.
    /// Dropping the waiter leaves the same client-owned task running.
    #[must_use]
    pub fn wait_for_secondary_commit(mut self) -> TransactionOptions {
        self.secondary_commit_wait = Some(Duration::from_secs(30));
        self
    }

    /// Try to use 1pc.
    #[must_use]
    pub fn try_one_pc(mut self) -> TransactionOptions {
        self.try_one_pc = true;
        self
    }

    /// Make the transaction read only.
    #[must_use]
    pub fn read_only(mut self) -> TransactionOptions {
        self.read_only = true;
        self
    }

    /// Don't automatically resolve locks and retry if keys are locked.
    #[must_use]
    pub fn no_resolve_locks(mut self) -> TransactionOptions {
        self.retry_options.lock_backoff = Backoff::no_backoff();
        self
    }

    /// Don't automatically resolve regions with PD if we have outdated region information.
    #[must_use]
    pub fn no_resolve_regions(mut self) -> TransactionOptions {
        self.retry_options.region_backoff = Backoff::no_backoff();
        self
    }

    /// Set RetryOptions.
    #[must_use]
    pub fn retry_options(mut self, options: RetryOptions) -> TransactionOptions {
        self.retry_options = options;
        self
    }

    /// Set the behavior when dropping a transaction without an attempt to commit or rollback it.
    #[must_use]
    pub fn drop_check(mut self, level: CheckLevel) -> TransactionOptions {
        self.check_level = level;
        self
    }

    fn push_for_update_ts(&mut self, for_update_ts: Timestamp) {
        match &mut self.kind {
            TransactionKind::Optimistic => unreachable!(),
            TransactionKind::Pessimistic(old_for_update_ts) => {
                self.kind = TransactionKind::Pessimistic(Timestamp::from_version(std::cmp::max(
                    old_for_update_ts.version(),
                    for_update_ts.version(),
                )));
            }
        }
    }

    #[must_use]
    pub fn heartbeat_option(mut self, heartbeat_option: HeartbeatOption) -> TransactionOptions {
        self.heartbeat_option = heartbeat_option;
        self
    }

    // Returns true if these options describe a pessimistic transaction.
    pub fn is_pessimistic(&self) -> bool {
        match self.kind {
            TransactionKind::Pessimistic(_) => true,
            TransactionKind::Optimistic => false,
        }
    }
}

/// Determines what happens when a transaction is dropped without being rolled back or committed.
///
/// The default is to panic.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum CheckLevel {
    /// The program will panic.
    ///
    /// Note that if the thread is already panicking, then we will not double-panic and abort, but
    /// just ignore the issue.
    Panic,
    /// Log a warning.
    Warn,
    /// Do nothing
    None,
}

impl HeartbeatOption {
    pub fn is_auto_heartbeat(&self) -> bool {
        !matches!(self, HeartbeatOption::NoHeartbeat)
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Mutation {
    Put(Key, Value),
    Delete(Key),
}

impl Mutation {
    pub fn key(&self) -> &Key {
        match self {
            Mutation::Put(key, _) => key,
            Mutation::Delete(key) => key,
        }
    }
}

/// A struct wrapping the details of two-phase commit protocol (2PC).
///
/// The two phases are `prewrite` and `commit`.
/// Generally, the `prewrite` phase is to send data to all regions and write them.
/// The `commit` phase is to mark all written data as successfully committed.
///
/// The committer implements `prewrite`, `commit` and `rollback` functions.
#[allow(clippy::too_many_arguments)]
#[derive(new)]
struct Committer<PdC: PdClient = PdRpcClient> {
    primary_key: Option<Key>,
    mutations: Vec<kvrpcpb::Mutation>,
    start_version: Timestamp,
    rpc: Arc<PdC>,
    options: TransactionOptions,
    keyspace: Keyspace,
    #[new(default)]
    undetermined: bool,
    write_size: u64,
    start_instant: Instant,
}

impl<PdC: PdClient> Committer<PdC> {
    async fn commit(mut self) -> Result<Option<Timestamp>> {
        debug!("committing");

        let min_commit_ts = self.prewrite().await?;

        fail_point!("after-prewrite", |_| {
            Err(Error::StringError(
                "failpoint: after-prewrite return error".to_owned(),
            ))
        });

        // If we didn't use 1pc, prewrite will set `try_one_pc` to false.
        if self.options.try_one_pc {
            return Ok(min_commit_ts);
        }

        let commit_ts = if self.options.async_commit {
            // FIXME: min_commit_ts == 0 => fallback to normal 2PC
            min_commit_ts.unwrap()
        } else {
            match self.commit_primary_with_retry().await {
                Ok(commit_ts) => commit_ts,
                Err(e) => {
                    return if self.undetermined {
                        Err(Error::UndeterminedError(Box::new(e)))
                    } else {
                        Err(e)
                    };
                }
            }
        };
        let wait = self.options.secondary_commit_wait;
        let owner = self.rpc.resource_owner();
        let secondary = self.commit_secondary(commit_ts.clone());
        finish_owned_secondary(owner, secondary, wait).await?;
        Ok(Some(commit_ts))
    }

    async fn prewrite(&mut self) -> Result<Option<Timestamp>> {
        debug!("prewriting");
        let primary_lock = self.primary_key.clone().unwrap();
        let elapsed = self.start_instant.elapsed().as_millis() as u64;
        let lock_ttl = self.calc_txn_lock_ttl();
        let mut request = match &self.options.kind {
            TransactionKind::Optimistic => new_prewrite_request(
                self.mutations.clone(),
                primary_lock,
                self.start_version.clone(),
                lock_ttl + elapsed,
            ),
            TransactionKind::Pessimistic(for_update_ts) => new_pessimistic_prewrite_request(
                self.mutations.clone(),
                primary_lock,
                self.start_version.clone(),
                lock_ttl + elapsed,
                for_update_ts.clone(),
            ),
        };

        request.use_async_commit = self.options.async_commit;
        request.try_one_pc = self.options.try_one_pc;
        request.secondaries = self
            .mutations
            .iter()
            .filter(|m| self.primary_key.as_ref().unwrap() != m.key.as_ref())
            .map(|m| m.key.clone())
            .collect();
        // FIXME set max_commit_ts and min_commit_ts

        let plan = PlanBuilder::new(self.rpc.clone(), self.keyspace, request)
            .resolve_lock(
                self.start_version.clone(),
                self.options.retry_options.lock_backoff.clone(),
                self.keyspace,
            )
            .retry_multi_region(self.options.retry_options.region_backoff.clone())
            .merge(CollectError)
            .extract_error()
            .plan();
        let response = plan.execute().await?;

        if self.options.try_one_pc && response.len() == 1 {
            if response[0].one_pc_commit_ts == 0 {
                return Err(Error::OnePcFailure);
            }

            return Ok(Timestamp::try_from_version(response[0].one_pc_commit_ts));
        }

        self.options.try_one_pc = false;

        let min_commit_ts = response
            .iter()
            .map(|r| {
                assert_eq!(r.one_pc_commit_ts, 0);
                r.min_commit_ts
            })
            .max()
            .map(Timestamp::from_version);

        Ok(min_commit_ts)
    }

    /// Commits the primary key and returns the commit version
    async fn commit_primary(&mut self) -> Result<Timestamp> {
        debug!("committing primary");
        let primary_key = self.primary_key.clone().into_iter();
        let commit_version = self.rpc.clone().get_timestamp().await?;
        let req = new_commit_request(
            primary_key,
            self.start_version.clone(),
            commit_version.clone(),
        );
        let plan = PlanBuilder::new(self.rpc.clone(), self.keyspace, req)
            .resolve_lock(
                self.start_version.clone(),
                self.options.retry_options.lock_backoff.clone(),
                self.keyspace,
            )
            .retry_multi_region(self.options.retry_options.region_backoff.clone())
            .extract_error()
            .plan();
        plan.execute()
            .inspect_err(|e| {
                debug!(
                    "commit primary error: {:?}, start_ts: {}",
                    e,
                    self.start_version.version()
                );
                // We don't know whether the transaction is committed or not if we fail to receive
                // the response. Then, we mark the transaction as undetermined and propagate the
                // error to the user.
                if let Error::Grpc(_) = e {
                    self.undetermined = true;
                }
            })
            .await?;

        Ok(commit_version)
    }

    async fn commit_primary_with_retry(&mut self) -> Result<Timestamp> {
        loop {
            match self.commit_primary().await {
                Ok(commit_version) => return Ok(commit_version),
                Err(Error::ExtractedErrors(mut errors)) => match errors.pop() {
                    Some(Error::KeyError(key_err)) => {
                        if let Some(expired) = key_err.commit_ts_expired {
                            // Ref: https://github.com/tikv/client-go/blob/tidb-8.5/txnkv/transaction/commit.go
                            info!("2PC commit_ts rejected by TiKV, retry with a newer commit_ts, start_ts: {}",
                                self.start_version.version());

                            let primary_key = self.primary_key.as_ref().unwrap();
                            if primary_key != expired.key.as_ref() {
                                error!("2PC commit_ts rejected by TiKV, but the key is not the primary key, start_ts: {}, key: {}, primary: {:?}",
                                    self.start_version.version(), HexRepr(&expired.key), primary_key);
                                return Err(Error::StringError("2PC commitTS rejected by TiKV, but the key is not the primary key".to_string()));
                            }

                            // Do not retry for a txn which has a too large min_commit_ts.
                            // 3600000 << 18 = 943718400000
                            if expired
                                .min_commit_ts
                                .saturating_sub(expired.attempted_commit_ts)
                                > 943718400000
                            {
                                let msg = format!("2PC min_commit_ts is too large, we got min_commit_ts: {}, and attempted_commit_ts: {}",
                                                     expired.min_commit_ts, expired.attempted_commit_ts);
                                return Err(Error::StringError(msg));
                            }
                            continue;
                        } else {
                            return Err(Error::KeyError(key_err));
                        }
                    }
                    Some(err) => return Err(err),
                    None => unreachable!(),
                },
                Err(err) => return Err(err),
            }
        }
    }

    async fn commit_secondary(self, commit_version: Timestamp) -> Result<()> {
        debug!("committing secondary");
        let start_version = self.start_version.clone();
        let mutations_len = self.mutations.len();
        let primary_only = mutations_len == 1;
        #[cfg(not(feature = "integration-tests"))]
        let mutations = self.mutations.into_iter();

        #[cfg(feature = "integration-tests")]
        let mutations = self.mutations.into_iter().take({
            // Truncate mutation to a new length as `percent/100`.
            // Return error when truncate to zero.
            let fp = || -> Result<usize> {
                let mut new_len = mutations_len;
                fail_point!("before-commit-secondary", |percent| {
                    let percent = percent.unwrap().parse::<usize>().unwrap();
                    new_len = mutations_len * percent / 100;
                    if new_len == 0 {
                        Err(Error::StringError(
                            "failpoint: before-commit-secondary return error".to_owned(),
                        ))
                    } else {
                        debug!(
                            "failpoint: before-commit-secondary truncate mutation {} -> {}",
                            mutations_len, new_len
                        );
                        Ok(new_len)
                    }
                });
                Ok(new_len)
            };
            fp()?
        });

        let req = if self.options.async_commit {
            let keys = mutations.map(|m| m.key.into());
            new_commit_request(keys, start_version.clone(), commit_version)
        } else if primary_only {
            return Ok(());
        } else {
            let primary_key = self.primary_key.unwrap();
            let keys = mutations
                .map(|m| m.key.into())
                .filter(|key| &primary_key != key);
            new_commit_request(keys, start_version.clone(), commit_version)
        };
        let plan = PlanBuilder::new(self.rpc, self.keyspace, req)
            .resolve_lock(
                start_version,
                self.options.retry_options.lock_backoff,
                self.keyspace,
            )
            .retry_multi_region(self.options.retry_options.region_backoff)
            .extract_error()
            .plan();
        plan.execute().await?;
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        debug!("rolling back");
        if self.options.kind == TransactionKind::Optimistic && self.mutations.is_empty() {
            return Ok(());
        }
        let keys = self
            .mutations
            .into_iter()
            .map(|mutation| mutation.key.into());
        let start_version = self.start_version.clone();
        match self.options.kind {
            TransactionKind::Optimistic => {
                let req = new_batch_rollback_request(keys, start_version.clone());
                let plan = PlanBuilder::new(self.rpc, self.keyspace, req)
                    .resolve_lock(
                        start_version.clone(),
                        self.options.retry_options.lock_backoff,
                        self.keyspace,
                    )
                    .retry_multi_region(self.options.retry_options.region_backoff)
                    .extract_error()
                    .plan();
                plan.execute().await?;
            }
            TransactionKind::Pessimistic(for_update_ts) => {
                let req =
                    new_pessimistic_rollback_request(keys, start_version.clone(), for_update_ts);
                let plan = PlanBuilder::new(self.rpc, self.keyspace, req)
                    .resolve_lock(
                        start_version.clone(),
                        self.options.retry_options.lock_backoff,
                        self.keyspace,
                    )
                    .retry_multi_region(self.options.retry_options.region_backoff)
                    .extract_error()
                    .plan();
                plan.execute().await?;
            }
        }
        Ok(())
    }

    fn calc_txn_lock_ttl(&mut self) -> u64 {
        let mut lock_ttl = DEFAULT_LOCK_TTL;
        if self.write_size > TXN_COMMIT_BATCH_SIZE {
            let size_mb = self.write_size as f64 / 1024.0 / 1024.0;
            lock_ttl = (TTL_FACTOR * size_mb.sqrt()) as u64;
            lock_ttl = lock_ttl.clamp(DEFAULT_LOCK_TTL, MAX_TTL);
        }
        lock_ttl
    }
}

/// One task and one result receiver. This never reissues a secondary task when
/// the caller times out or drops the receiver; the resource owner keeps its join.
async fn finish_owned_secondary<F>(
    owner: Option<crate::ClientResourceOwner>,
    secondary: F,
    wait: Option<Duration>,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let receiver = crate::resources::spawn_owned(owner, async move {
        let result = secondary.await;
        if let Err(error) = &result {
            log::warn!("Failed to commit secondary keys: {}", error);
        }
        result
    })
    .map_err(|error| Error::UndeterminedError(Box::new(error)))?;
    let Some(wait) = wait else {
        return Ok(());
    };
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(error))) => Err(Error::UndeterminedError(Box::new(error))),
        Ok(Err(_)) => Err(Error::UndeterminedError(Box::new(crate::internal_err!(
            "owned secondary commit result receiver closed"
        )))),
        Err(_) => Err(Error::UndeterminedError(Box::new(crate::internal_err!(
            "owned secondary commit wait exceeded its deadline"
        )))),
    }
}

#[cfg(test)]
#[path = "secondary_wait_tests.rs"]
mod secondary_wait_tests;

#[derive(PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
enum TransactionStatus {
    /// The transaction is read-only [`Snapshot`](super::Snapshot), no need to commit or rollback or panic on drop.
    ReadOnly = 0,
    /// The transaction have not been committed or rolled back.
    Active = 1,
    /// The transaction has committed.
    Committed = 2,
    /// The transaction has tried to commit. Only `commit` is allowed.
    StartedCommit = 3,
    /// The transaction has rolled back.
    Rolledback = 4,
    /// The transaction has tried to rollback. Only `rollback` is allowed.
    StartedRollback = 5,
    /// The transaction has been dropped.
    Dropped = 6,
}

impl From<u8> for TransactionStatus {
    fn from(num: u8) -> Self {
        match num {
            0 => TransactionStatus::ReadOnly,
            1 => TransactionStatus::Active,
            2 => TransactionStatus::Committed,
            3 => TransactionStatus::StartedCommit,
            4 => TransactionStatus::Rolledback,
            5 => TransactionStatus::StartedRollback,
            6 => TransactionStatus::Dropped,
            _ => panic!("Unknown transaction status {}", num),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::io;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    use fail::FailScenario;

    use crate::mock::MockKvClient;
    use crate::mock::MockPdClient;
    use crate::proto::kvrpcpb;
    use crate::proto::pdpb::Timestamp;
    use crate::request::Keyspace;
    use crate::transaction::HeartbeatOption;
    use crate::Transaction;
    use crate::TransactionOptions;

    #[tokio::test]
    #[ignore = "requires isolated BREWFS_TEST_TIKV_PD_ENDPOINTS"]
    async fn real_bounded_reader_recovers_after_held_prewrite_is_rolled_back() {
        use crate::{CheckLevel, ClientResourceOwner, Config, TransactionClient};
        use futures::FutureExt;
        let endpoints = std::env::var("BREWFS_TEST_TIKV_PD_ENDPOINTS")
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let nonce = rand::random::<u128>();
        let key = format!("brewfs-bounded-lock-test/{nonce:032x}").into_bytes();
        let writer = TransactionClient::new_with_config(
            endpoints.clone(),
            Config::default().with_bounded_resources(ClientResourceOwner::new(Arc::new(()))),
        )
        .await
        .unwrap();
        let reader = TransactionClient::new_with_config(
            endpoints,
            Config::default()
                .with_grpc_max_decoding_message_size(16 << 10)
                .with_grpc_accept_gzip(false)
                .with_bounded_get_scan_schema(true)
                .with_bounded_resources(ClientResourceOwner::new(Arc::new(()))),
        )
        .await
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            std::panic::AssertUnwindSafe(async {
                let mut initial = writer.begin_optimistic().await.unwrap();
                initial.put(key.clone(), b"old".to_vec()).await.unwrap();
                initial.commit().await.unwrap();
                let mut held = writer.begin_optimistic().await.unwrap();
                held.put(key.clone(), b"new".to_vec()).await.unwrap();
                let mut committer = super::Committer::new(
                    held.buffer.get_primary_key(),
                    held.buffer.to_proto_mutations(),
                    held.timestamp.clone(),
                    held.rpc.clone(),
                    held.options.clone(),
                    held.keyspace,
                    held.buffer.get_write_size() as u64,
                    held.start_instant,
                );
                assert!(!committer.options.try_one_pc && !committer.options.async_commit);
                committer.prewrite().await.unwrap();
                let options = TransactionOptions::new_optimistic()
                    .read_only()
                    .drop_check(CheckLevel::Warn);
                let mut snapshot = reader.begin_with_options(options).await.unwrap();
                let error = snapshot
                    .get_uncached_single_region(key.clone())
                    .await
                    .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("bounded read returned a key or region error"),
                    "{error}"
                );
                held.rollback().await.unwrap();
                // Retry the same timestamp after actual TiKV rollback, proving this
                // error does not permanently disable the client or change snapshot.
                assert_eq!(
                    snapshot
                        .get_uncached_single_region(key.clone())
                        .await
                        .unwrap(),
                    Some(b"old".to_vec())
                );
                let mut restarted = reader
                    .begin_with_options(
                        TransactionOptions::new_optimistic()
                            .read_only()
                            .drop_check(CheckLevel::Warn),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    restarted
                        .get_uncached_single_region(key.clone())
                        .await
                        .unwrap(),
                    Some(b"old".to_vec())
                );
            })
            .catch_unwind(),
        )
        .await;
        let mut cleanup = writer.begin_optimistic().await.unwrap();
        cleanup.delete(key).await.unwrap();
        cleanup.commit().await.unwrap();
        reader.shutdown().await.unwrap();
        writer.shutdown().await.unwrap();
        assert!(
            matches!(result, Ok(Ok(()))),
            "held-prewrite bounded read case failed: {result:?}"
        );
    }

    #[rstest::rstest]
    #[case(Keyspace::Disable)]
    #[case(Keyspace::Enable { keyspace_id: 0 })]
    #[tokio::test]
    async fn test_optimistic_heartbeat(#[case] keyspace: Keyspace) -> Result<(), io::Error> {
        let scenario = FailScenario::setup();
        fail::cfg("after-prewrite", "sleep(1500)").unwrap();
        let heartbeats = Arc::new(AtomicUsize::new(0));
        let heartbeats_cloned = heartbeats.clone();
        let pd_client = Arc::new(MockPdClient::new(MockKvClient::with_dispatch_hook(
            move |req: &dyn Any| {
                if req.downcast_ref::<kvrpcpb::TxnHeartBeatRequest>().is_some() {
                    heartbeats_cloned.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::<kvrpcpb::TxnHeartBeatResponse>::default() as Box<dyn Any>)
                } else if req.downcast_ref::<kvrpcpb::PrewriteRequest>().is_some() {
                    Ok(Box::<kvrpcpb::PrewriteResponse>::default() as Box<dyn Any>)
                } else {
                    Ok(Box::<kvrpcpb::CommitResponse>::default() as Box<dyn Any>)
                }
            },
        )));
        let key1 = "key1".to_owned();
        let mut heartbeat_txn = Transaction::new(
            Timestamp::default(),
            pd_client,
            TransactionOptions::new_optimistic()
                .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(1))),
            keyspace,
        );
        heartbeat_txn.put(key1.clone(), "foo").await.unwrap();
        let heartbeat_txn_handle = tokio::task::spawn_blocking(move || {
            assert!(futures::executor::block_on(heartbeat_txn.commit()).is_ok())
        });
        assert_eq!(heartbeats.load(Ordering::SeqCst), 0);
        heartbeat_txn_handle.await.unwrap();
        assert_eq!(heartbeats.load(Ordering::SeqCst), 1);
        scenario.teardown();
        Ok(())
    }

    #[rstest::rstest]
    #[case(Keyspace::Disable)]
    #[case(Keyspace::Enable { keyspace_id: 0 })]
    #[tokio::test]
    async fn test_pessimistic_heartbeat(#[case] keyspace: Keyspace) -> Result<(), io::Error> {
        let heartbeats = Arc::new(AtomicUsize::new(0));
        let heartbeats_cloned = heartbeats.clone();
        let pd_client = Arc::new(MockPdClient::new(MockKvClient::with_dispatch_hook(
            move |req: &dyn Any| {
                if req.downcast_ref::<kvrpcpb::TxnHeartBeatRequest>().is_some() {
                    heartbeats_cloned.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::<kvrpcpb::TxnHeartBeatResponse>::default() as Box<dyn Any>)
                } else if req.downcast_ref::<kvrpcpb::PrewriteRequest>().is_some() {
                    Ok(Box::<kvrpcpb::PrewriteResponse>::default() as Box<dyn Any>)
                } else if req
                    .downcast_ref::<kvrpcpb::PessimisticLockRequest>()
                    .is_some()
                {
                    Ok(Box::<kvrpcpb::PessimisticLockResponse>::default() as Box<dyn Any>)
                } else {
                    Ok(Box::<kvrpcpb::CommitResponse>::default() as Box<dyn Any>)
                }
            },
        )));
        let key1 = "key1".to_owned();
        let mut heartbeat_txn = Transaction::new(
            Timestamp::default(),
            pd_client,
            TransactionOptions::new_pessimistic()
                .heartbeat_option(HeartbeatOption::FixedTime(Duration::from_secs(1))),
            keyspace,
        );
        heartbeat_txn.put(key1.clone(), "foo").await.unwrap();
        assert_eq!(heartbeats.load(Ordering::SeqCst), 0);
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
        assert_eq!(heartbeats.load(Ordering::SeqCst), 1);
        let heartbeat_txn_handle = tokio::spawn(async move {
            assert!(heartbeat_txn.commit().await.is_ok());
        });
        heartbeat_txn_handle.await.unwrap();
        Ok(())
    }
}

#[cfg(test)]
#[path = "bounded_get_timestamp_tests.rs"]
mod bounded_get_timestamp_tests;

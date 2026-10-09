// Copyright 2026 TiKV Project Authors. Licensed under Apache-2.0.

//! Absolute deadline for an opted-in single-region read; no owned task or retry.

use std::any::Any;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;
use tonic::transport::Channel;

use crate::proto::kvrpcpb;
use crate::proto::tikvpb::tikv_client::TikvClient;
use crate::region::RegionWithLeader;
use crate::request::{Dispatch, KvRequest, Plan};
use crate::stats::tikv_stats;
use crate::store::Request;
use crate::{Error, Result};

pub(crate) fn deadline_error() -> Error {
    Error::StringError("bounded read data-batch deadline exhausted".into())
}

fn remaining_rpc_timeout(
    configured: Duration,
    deadline: Instant,
    now: Instant,
) -> Result<Duration> {
    if configured.is_zero() || now >= deadline {
        return Err(deadline_error());
    }
    Ok(configured.min(deadline - now))
}

/// Keeps ordinary Dispatch and mutating/region-fanout plans unchanged.
#[derive(Clone)]
pub struct BoundedDeadlineDispatch<R: KvRequest> {
    pub(crate) inner: Dispatch<R>,
    pub(crate) deadline: Instant,
}

#[async_trait]
impl<R: KvRequest> Plan for BoundedDeadlineDispatch<R> {
    type Result = R::Response;

    async fn execute(&self) -> Result<Self::Result> {
        // timeout_at may poll an immediately-ready future even if already past
        // its deadline; reject explicitly before invoking the data client.
        if Instant::now() >= self.deadline {
            return Err(deadline_error());
        }
        let request = DeadlineRequest {
            inner: self.inner.request.clone(),
            deadline: self.deadline,
        };
        let client = self
            .inner
            .kv_client
            .as_ref()
            .expect("bounded deadline plan has no single-region client");
        let stats = tikv_stats(request.label());
        let result = match tokio::time::timeout_at(self.deadline, client.dispatch(&request)).await {
            Ok(result) => result,
            Err(_) => Err(deadline_error()),
        };
        let result = if Instant::now() >= self.deadline {
            Err(deadline_error())
        } else {
            result
        };
        stats.done(result).map(|response| {
            *response
                .downcast()
                .expect("bounded deadline response type mismatch")
        })
    }
}

/// Owns the same bounded request; no borrowed request escapes the caller.
/// Delegated as_any preserves mock/client request inspection.
struct DeadlineRequest<R> {
    inner: R,
    deadline: Instant,
}

#[async_trait]
impl<R: Request> Request for DeadlineRequest<R> {
    async fn dispatch(
        &self,
        client: &TikvClient<Channel>,
        configured_timeout: Duration,
    ) -> Result<Box<dyn Any>> {
        let remaining = remaining_rpc_timeout(configured_timeout, self.deadline, Instant::now())?;
        // Original request code creates tonic::Request and sets grpc_timeout
        // using remaining, not the configured timeout reset at every attempt.
        let result = tokio::time::timeout_at(self.deadline, self.inner.dispatch(client, remaining))
            .await
            .map_err(|_| deadline_error())?;
        if Instant::now() >= self.deadline {
            return Err(deadline_error());
        }
        result
    }

    fn label(&self) -> &'static str {
        self.inner.label()
    }

    fn as_any(&self) -> &dyn Any {
        self.inner.as_any()
    }

    fn set_leader(&mut self, leader: &RegionWithLeader) -> Result<()> {
        self.inner.set_leader(leader)
    }

    fn set_api_version(&mut self, api_version: kvrpcpb::ApiVersion) {
        self.inner.set_api_version(api_version);
    }
}

#[cfg(test)]
mod terminal_deadline_tests {
    use super::*;
    use crate::store::KvClient;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct ReadyRead {
        calls: Arc<AtomicUsize>,
        delay: Duration,
    }

    #[async_trait]
    impl KvClient for ReadyRead {
        async fn dispatch(&self, request: &dyn Request) -> Result<Box<dyn Any>> {
            assert!(request.as_any().is::<kvrpcpb::GetRequest>());
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            Ok(Box::<kvrpcpb::GetResponse>::default())
        }
    }

    fn ready_plan(
        deadline: Instant,
        delay: Duration,
        calls: Arc<AtomicUsize>,
    ) -> BoundedDeadlineDispatch<kvrpcpb::GetRequest> {
        BoundedDeadlineDispatch {
            inner: Dispatch {
                request: kvrpcpb::GetRequest::default(),
                kv_client: Some(Arc::new(ReadyRead { calls, delay })),
            },
            deadline,
        }
    }

    #[tokio::test]
    async fn expired_deadline_never_calls_a_truly_ready_data_client() {
        let calls = Arc::new(AtomicUsize::new(0));
        let result = ready_plan(Instant::now(), Duration::ZERO, calls.clone())
            .execute()
            .await;
        assert!(matches!(result, Err(Error::StringError(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn inline_ready_response_after_deadline_is_rejected_at_terminal_check() {
        let calls = Arc::new(AtomicUsize::new(0));
        let result = ready_plan(
            Instant::now() + Duration::from_millis(5),
            Duration::from_millis(20),
            calls.clone(),
        )
        .execute()
        .await;
        assert!(matches!(result, Err(Error::StringError(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::store::KvClient;

    struct DropReceipt(Arc<AtomicUsize>);
    impl Drop for DropReceipt {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct HeldRead {
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl KvClient for HeldRead {
        async fn dispatch(&self, request: &dyn Request) -> Result<Box<dyn Any>> {
            let get = request
                .as_any()
                .downcast_ref::<kvrpcpb::GetRequest>()
                .unwrap();
            assert_eq!(get.key, b"private-key");
            assert_eq!(get.version, 123);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _receipt = DropReceipt(self.dropped.clone());
            futures::future::pending::<()>().await;
            unreachable!("held read never completes")
        }
    }

    fn plan(
        deadline: Instant,
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    ) -> BoundedDeadlineDispatch<kvrpcpb::GetRequest> {
        BoundedDeadlineDispatch {
            inner: Dispatch {
                request: kvrpcpb::GetRequest {
                    key: b"private-key".to_vec(),
                    version: 123,
                    ..Default::default()
                },
                kv_client: Some(Arc::new(HeldRead { calls, dropped })),
            },
            deadline,
        }
    }

    #[test]
    fn rpc_timeout_decreases_with_absolute_deadline_without_reset() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(4);
        assert_eq!(
            remaining_rpc_timeout(Duration::from_secs(2), deadline, now).unwrap(),
            Duration::from_secs(2)
        );
        assert_eq!(
            remaining_rpc_timeout(
                Duration::from_secs(2),
                deadline,
                deadline - Duration::from_millis(7),
            )
            .unwrap(),
            Duration::from_millis(7)
        );
        assert!(remaining_rpc_timeout(Duration::from_secs(2), deadline, deadline).is_err());
        assert!(remaining_rpc_timeout(Duration::ZERO, deadline, now).is_err());
    }

    #[tokio::test]
    async fn expired_deadline_does_not_dispatch_even_immediately_ready_mock() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let result = plan(Instant::now(), calls.clone(), dropped.clone())
            .execute()
            .await;
        assert!(matches!(result, Err(Error::StringError(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn held_read_deadline_drops_inline_dispatch_without_retry_or_owned_task() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let result = plan(
            Instant::now() + Duration::from_millis(40),
            calls.clone(),
            dropped.clone(),
        )
        .execute()
        .await;
        assert!(matches!(result, Err(Error::StringError(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn caller_cancellation_drops_held_inline_read_future() {
        use futures::FutureExt;
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let request_plan = plan(
            Instant::now() + Duration::from_secs(2),
            calls.clone(),
            dropped.clone(),
        );
        let mut future = Box::pin(request_plan.execute());
        assert!(future.as_mut().now_or_never().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(future);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod bridge_transport_tests;

// Copyright 2026 TiKV Project Authors. Licensed under Apache-2.0.

use std::fmt;
use std::sync::Arc;

use crate::internal_err;

/// A caller-owned resident-memory reservation. The SDK does not depend on the
/// caller's accounting implementation. The last client, dispatched request or
/// SDK background task retains this owner until its terminal state.
pub trait ClientResourceLease: fmt::Debug + Send + Sync + 'static {}
impl<T: fmt::Debug + Send + Sync + 'static> ClientResourceLease for T {}

type TaskSlot = Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>;

#[derive(Debug, Default)]
struct Tasks {
    closed: bool,
    slots: Vec<TaskSlot>,
}

#[derive(Debug)]
struct Owner {
    lease: Arc<dyn ClientResourceLease>,
    tasks: std::sync::Mutex<Tasks>,
}

#[derive(Clone, Debug)]
pub struct ClientResourceOwner(Arc<Owner>);

impl ClientResourceOwner {
    pub fn new(lease: Arc<dyn ClientResourceLease>) -> Self {
        Self(Arc::new(Owner {
            lease,
            tasks: std::sync::Mutex::default(),
        }))
    }

    pub async fn shutdown(&self) -> crate::Result<()> {
        let slots = {
            let mut tasks = self.0.tasks.lock().unwrap();
            tasks.closed = true;
            tasks.slots.clone()
        };
        for slot in slots {
            let mut join = slot.lock().await;
            if let Some(handle) = join.as_mut() {
                handle.abort();
                if let Err(error) = handle.await {
                    if !error.is_cancelled() {
                        return Err(crate::internal_err!("SDK task join failed: {}", error));
                    }
                }
                join.take();
            }
        }
        Ok(())
    }

    /// Begin cancellation without claiming a terminal join. Captured leases
    /// remain with task futures until the runtime acknowledges cancellation.
    pub fn cancel(&self) {
        let mut tasks = self.0.tasks.lock().unwrap();
        tasks.closed = true;
        for slot in &tasks.slots {
            if let Ok(join) = slot.try_lock() {
                if let Some(handle) = join.as_ref() {
                    handle.abort();
                }
            }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        for slot in &self.tasks.get_mut().unwrap().slots {
            if let Ok(join) = slot.try_lock() {
                if let Some(handle) = join.as_ref() {
                    handle.abort();
                }
            }
        }
    }
}

/// Keep cancellation and terminal-join ownership in the client, even when a
/// caller drops its result receiver. At most 64 task slots may be retained.
pub(crate) fn spawn_owned<F, T>(
    owner: Option<ClientResourceOwner>,
    future: F,
) -> crate::Result<tokio::sync::oneshot::Receiver<T>>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Some(owner) = owner {
        let mut tasks = owner.0.tasks.lock().unwrap();
        tasks.slots.retain(|slot| {
            slot.try_lock().map_or(true, |join| {
                join.as_ref().is_some_and(|handle| !handle.is_finished())
            })
        });
        if tasks.closed || tasks.slots.len() >= 64 {
            return Err(crate::internal_err!("SDK task owner closed or full"));
        }
        let lease = owner.0.lease.clone();
        let handle = tokio::spawn(async move {
            let result = future.await;
            // A cancelled caller's discarded response drops before the lease.
            let _ = tx.send(result);
            drop(lease);
        });
        tasks
            .slots
            .push(Arc::new(tokio::sync::Mutex::new(Some(handle))));
    } else {
        tokio::spawn(async move {
            let _ = tx.send(future.await);
        });
    }
    Ok(rx)
}

impl PartialEq for ClientResourceOwner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ClientResourceOwner {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Lease(Arc<AtomicUsize>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelled_receiver_retains_lease_until_terminal_join() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = ClientResourceOwner::new(Arc::new(Lease(drops.clone())));
        let started = Arc::new(tokio::sync::Notify::new());
        let ready = started.clone();
        let receiver = spawn_owned(Some(owner.clone()), async move {
            ready.notify_one();
            std::future::pending::<()>().await;
        })
        .unwrap();
        started.notified().await;
        drop(receiver);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        owner.shutdown().await.unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(spawn_owned(Some(owner.clone()), async {}).is_err());
        drop(owner);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn task_slots_refuse_before_spawning_the_sixty_fifth_task() {
        let owner = ClientResourceOwner::new(Arc::new(()));
        let mut receivers = Vec::new();
        for _ in 0..64 {
            receivers.push(spawn_owned(Some(owner.clone()), std::future::pending::<()>()).unwrap());
        }
        assert!(spawn_owned(Some(owner.clone()), async { panic!("must not spawn") }).is_err());
        owner.shutdown().await.unwrap();
        assert!(receivers
            .into_iter()
            .all(|mut receiver| receiver.try_recv().is_err()));
    }

    #[tokio::test]
    async fn cancelled_shutdown_keeps_every_join_slot_for_retry() {
        let owner = ClientResourceOwner::new(Arc::new(()));
        let slot = Arc::new(tokio::sync::Mutex::new(None));
        owner.0.tasks.lock().unwrap().slots.push(slot.clone());
        let held = slot.lock().await;
        let shutdown_owner = owner.clone();
        let mut shutdown = Box::pin(shutdown_owner.shutdown());
        assert!(futures::poll!(&mut shutdown).is_pending());
        drop(shutdown);
        assert_eq!(owner.0.tasks.lock().unwrap().slots.len(), 1);
        drop(held);
        owner.shutdown().await.unwrap();
    }
}

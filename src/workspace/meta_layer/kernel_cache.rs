//! Bounded metadata freshness without a cross-mount invalidation channel.
//!
//! Replies expire at one shared deadline measured BEFORE the remote check.
//! Hits never extend it. Once it expires, revalidate the workspace, writer
//! lease, head/base generations and independent packed binding, then discard
//! older upper rows before allowing another cache fill. Local commits already
//! invalidate their upper rows. Kernel mutation replies handle local dentries.

use super::*;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct KernelCache {
    layers: Option<[LayerRecord; 2]>,
    deadline: Option<Instant>,
    guard: Option<HeadGuard>,
    ttl: Duration,
    #[cfg(test)]
    pub(super) now: Option<Instant>,
}

impl KernelCache {
    fn now(&self) -> Instant {
        // Keep boundary tests independent of SQLite latency and scheduling.
        // The override belongs to this cache instance, never a global clock.
        #[cfg(test)]
        if let Some(now) = self.now {
            return now;
        }
        Instant::now()
    }
}

impl<W: WorkspaceStore + 'static> WorkspaceMetaLayer<W> {
    pub(super) async fn kernel_cache_deadline(
        &self,
        ttl: Duration,
    ) -> Result<Option<Instant>, MetaError> {
        if ttl.is_zero() || !self.store.supports_versioned_permissions() {
            return Ok(None);
        }
        // Serialize refreshes, not metadata operations. No background worker
        // or notifications can deadlock a FUSE mutation awaiting its reply.
        let mut cache = self.kernel_cache.lock().await;
        let guard = self.guard().await;
        if let Some(deadline) = cache.deadline
            && deadline > cache.now()
            && cache.guard.as_ref() == Some(&guard)
            && cache.ttl == ttl
        {
            return Ok(Some(deadline));
        }
        let started = cache.now();
        // An unrepresentable operator TTL must fail closed, never become an
        // unbounded cache lifetime.
        let Some(deadline) = started.checked_add(ttl) else {
            return Ok(None);
        };
        let layers = retry_busy_metadata_read(|| async {
            let chain = self
                .store
                .load_layer_chain(guard.expected_head_layer_id)
                .await
                .map_err(workspace_to_meta)?;
            validate_fixed_layer_pair(&chain).map_err(workspace_to_meta)?;
            let layers: [LayerRecord; 2] = chain
                .try_into()
                .map_err(|_| workspace_to_meta(WorkspaceError::Fenced))?;
            if let Some(lower) = self.packed_lower() {
                if layers[1].layer_id != lower.binding.base_layer_id {
                    return Err(workspace_to_meta(WorkspaceError::Fenced));
                }
                lower
                    .authority
                    .validate(&guard, &lower.binding)
                    .await
                    .map_err(workspace_to_meta)?;
            }
            self.store
                .validate_read_fence(guard.clone(), layers.clone())
                .await
                .map_err(workspace_to_meta)?;
            Ok(layers)
        })
        .await?;
        if cache.layers.as_ref() != Some(&layers) {
            self.invalidate_upper_rows();
            cache.layers = Some(layers);
        }
        cache.deadline = Some(deadline);
        cache.guard = Some(guard);
        cache.ttl = ttl;
        Ok(Some(deadline))
    }
}

/// Rebuild a read-only metadata snapshot after a transient routing conflict.
/// Every attempt performs all authority checks; successful cache lifetimes
/// retain the caller's original deadline.
pub(super) async fn retry_busy_metadata_read<T, F, Fut>(mut read: F) -> Result<T, MetaError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, MetaError>>,
{
    for attempt in 0..8 {
        match read().await {
            Err(MetaError::Io(ref error))
                if error.raw_os_error() == Some(libc::EBUSY) && attempt < 7 =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            result => return result,
        }
    }
    unreachable!("the final metadata read attempt always returns")
}

//! Compactor: coordinates MetaStore and BlockStore to compact chunk slices.
use crate::chunk::ChunkLayout;
use crate::chunk::slice::{SliceDesc, SliceOffset, block_span_iter_chunk, block_span_iter_slice};
use crate::chunk::store::{BlockKey, BlockStore};
use crate::chunk::writer::upload_permit;
use crate::meta::SLICE_ID_KEY;
use crate::meta::config::CompactConfig;
use crate::meta::store::{MetaError, MetaStore};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactResult {
    Skipped,
    Light { removed: usize },
    Heavy { new_slice_id: u64 },
}

pub struct Compactor<B, M: MetaStore + ?Sized> {
    meta_store: Arc<M>,
    block_store: Arc<B>,
    layout: ChunkLayout,
    config: CompactConfig,
}

impl<B, M: ?Sized> Compactor<B, M>
where
    M: MetaStore + Send + Sync + 'static,
    B: BlockStore + Send + Sync + 'static,
{
    #[allow(dead_code)]
    pub fn new(meta_store: Arc<M>, block_store: Arc<B>) -> Self {
        Self {
            meta_store,
            block_store,
            layout: ChunkLayout::default(),
            config: CompactConfig::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_layout(meta_store: Arc<M>, block_store: Arc<B>, layout: ChunkLayout) -> Self {
        Self {
            meta_store,
            block_store,
            layout,
            config: CompactConfig::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_config(
        meta_store: Arc<M>,
        block_store: Arc<B>,
        layout: ChunkLayout,
        config: CompactConfig,
    ) -> Self {
        Self {
            meta_store,
            block_store,
            layout,
            config,
        }
    }

    pub fn block_store(&self) -> &Arc<B> {
        &self.block_store
    }
    pub async fn analyze_chunk(&self, chunk_id: u64) -> Result<(usize, u64, f64), CompactorError> {
        let slices = self.meta_store.get_slices(chunk_id).await?;
        let count = slices.len();
        if count == 0 {
            return Ok((0, 0, 0.0));
        }
        let total: u64 = slices.iter().map(|s| s.length).sum();
        let frag = SliceDesc::calculate_fragmentation(&slices);
        Ok((count, total, frag))
    }

    /// Check whether a chunk should be compacted and, if so, whether it
    /// should be done synchronously (blocking writes).
    pub async fn should_compact(&self, chunk_id: u64) -> Result<(bool, bool), CompactorError> {
        let (count, _total, frag) = self.analyze_chunk(chunk_id).await?;
        let cfg = &self.config;

        if count < cfg.min_slice_count {
            return Ok((false, false));
        }
        if frag < cfg.min_fragment_ratio {
            return Ok((false, false));
        }
        let is_sync = count >= cfg.sync_threshold;
        Ok((true, is_sync))
    }

    /// Try to compact a chunk with sequential light-then-heavy strategy.
    pub async fn compact_sequential(&self, chunk_id: u64) -> Result<CompactResult, CompactorError> {
        let (count, _, frag) = self.analyze_chunk(chunk_id).await?;
        if count <= 1 {
            return Ok(CompactResult::Skipped);
        }

        // Determine if heavy compaction might be needed
        let mut needs_heavy = self.config.heavy_enabled
            && (frag >= self.config.heavy_fragment_threshold
                || count >= self.config.heavy_slice_threshold);

        let mut light_removed = 0usize;
        if self.config.light_enabled && count >= self.config.light_threshold {
            let removed = self.compact_light(chunk_id).await?;

            if let Some(n) = removed {
                light_removed = n;

                // Re-analyze
                if n > 0 {
                    let (count_after, _, frag_after) = self.analyze_chunk(chunk_id).await?;
                    needs_heavy = self.config.heavy_enabled
                        && (frag_after >= self.config.heavy_fragment_threshold
                            || count_after >= self.config.heavy_slice_threshold);
                    // if remaining slices are extremely fragmented, force heavy
                    if frag_after >= self.config.heavy_force_fragment_threshold {
                        needs_heavy = true;
                    }
                    // If light removed all redundancy, skip heavy
                    if count_after <= 1 {
                        needs_heavy = false;
                    }
                }
            }
        }
        if needs_heavy {
            let new_slice_id = self.compact_heavy(chunk_id).await?;
            return Ok(CompactResult::Heavy { new_slice_id });
        }
        if light_removed > 0 {
            return Ok(CompactResult::Light {
                removed: light_removed,
            });
        }

        Ok(CompactResult::Skipped)
    }

    #[allow(dead_code)]
    pub async fn compact_chunk(&self, chunk_id: u64) -> Result<CompactResult, CompactorError> {
        self.compact_sequential(chunk_id).await
    }
    #[allow(dead_code)]
    pub async fn compact_light(&self, chunk_id: u64) -> Result<Option<usize>, CompactorError> {
        let slices = self.meta_store.get_slices(chunk_id).await?;
        self.compact_light_inner(&slices, chunk_id).await
    }

    async fn compact_light_inner(
        &self,
        slices: &[SliceDesc],
        chunk_id: u64,
    ) -> Result<Option<usize>, CompactorError> {
        if slices.len() <= 1 {
            return Ok(None);
        }

        let merged = SliceDesc::split_overlapped_views(slices);
        let replaced_ids = SliceDesc::find_replaced_ids(slices, &merged);

        if merged == slices {
            return Ok(None);
        }

        let delayed = SliceDesc::encode_delayed_data(slices, &replaced_ids);

        // Atomic: replace the visible slice list and stage delayed records only
        // for physical objects no longer referenced by any surviving view.
        self.meta_store
            .replace_slices_for_compact_with_version(chunk_id, &merged, &delayed, slices)
            .await?;

        let removed = replaced_ids.len();

        Ok(Some(removed))
    }

    /// Data-rewrite compaction: read all blocks, merge, write new slice.
    #[allow(dead_code)]
    pub async fn compact_heavy(&self, chunk_id: u64) -> Result<u64, CompactorError> {
        let slices = self.meta_store.get_slices(chunk_id).await?;
        self.compact_heavy_inner(&slices, chunk_id).await
    }

    async fn compact_heavy_inner(
        &self,
        slices: &[SliceDesc],
        chunk_id: u64,
    ) -> Result<u64, CompactorError> {
        let chunk_size = self.layout.chunk_size;
        let mut merged_data = vec![0u8; chunk_size as usize];

        self.read_and_merge_slices(slices, &mut merged_data).await?;

        let new_slice_id = self.meta_store.next_id(SLICE_ID_KEY).await? as u64;

        self.meta_store
            .record_uncommitted_slice(new_slice_id, chunk_id, chunk_size, "compact_heavy")
            .await
            .map_err(CompactorError::MetaError)?;

        self.write_merged_data(new_slice_id, &merged_data).await?;

        let new_slice = SliceDesc {
            slice_id: new_slice_id,
            chunk_id,
            offset: 0,
            length: chunk_size,
            object_offset: 0,
            object_size: chunk_size,
        };

        let all_ids: Vec<u64> = slices.iter().map(|s| s.slice_id).collect();
        let delayed = SliceDesc::encode_delayed_data(slices, &all_ids);

        match self
            .meta_store
            .replace_slices_for_compact_with_version(chunk_id, &[new_slice], &delayed, slices)
            .await
        {
            Ok(()) => {
                if let Err(e) = self.meta_store.confirm_slice_committed(new_slice_id).await {
                    warn!(
                        chunk_id,
                        new_slice_id,
                        error = %e,
                        "Failed to confirm slice committed, will be cleaned up by GC"
                    );
                }
                Ok(new_slice_id)
            }
            Err(MetaError::ContinueRetry(reason)) => {
                warn!(
                    chunk_id,
                    new_slice_id, "Compact heavy conflict detected, retry needed"
                );
                if let Err(cleanup_err) = self
                    .cleanup_uncommitted_slice(new_slice_id, chunk_size)
                    .await
                {
                    warn!(
                        chunk_id,
                        new_slice_id,
                        error = %cleanup_err,
                        "Failed to cleanup uncommitted slice after conflict"
                    );
                }
                Err(CompactorError::MetaError(MetaError::ContinueRetry(reason)))
            }
            Err(e) => {
                if let Err(cleanup_err) = self
                    .cleanup_uncommitted_slice(new_slice_id, chunk_size)
                    .await
                {
                    warn!(
                        chunk_id,
                        new_slice_id,
                        error = %cleanup_err,
                        "Failed to cleanup uncommitted slice after error"
                    );
                }
                Err(CompactorError::MetaError(e))
            }
        }
    }

    /// Read all slices and merge; newer slices (higher slice_id) overwrite older ones.
    async fn read_and_merge_slices(
        &self,
        slices: &[SliceDesc],
        merged_data: &mut [u8],
    ) -> Result<(), CompactorError> {
        let mut sorted: Vec<_> = slices.to_vec();
        sorted.sort_by_key(|s| s.slice_id);

        for slice in sorted {
            let start = slice.offset as usize;
            let end = start + slice.length as usize;

            if end > merged_data.len() {
                return Err(CompactorError::InvalidData(format!(
                    "Slice {} exceeds chunk bounds: offset={}, length={}, chunk_size={}",
                    slice.slice_id,
                    slice.offset,
                    slice.length,
                    merged_data.len()
                )));
            }

            self.read_slice_data_into(&slice, &mut merged_data[start..end])
                .await?;
        }

        Ok(())
    }

    async fn read_slice_data_into(
        &self,
        slice: &SliceDesc,
        dest: &mut [u8],
    ) -> Result<(), CompactorError> {
        let physical_start = slice.physical_offset_for(slice.offset);
        let spans: Vec<_> =
            block_span_iter_slice(SliceOffset(physical_start), slice.length, self.layout).collect();

        let mut pos = 0usize;
        for span in spans {
            let key: BlockKey = (slice.slice_id, span.index as u32);
            let take = span.len as usize;
            self.block_store
                .read_range(key, span.offset, &mut dest[pos..pos + take])
                .await
                .map_err(|e| CompactorError::BlockStoreError(e.to_string()))?;
            pos += take;
        }

        Ok(())
    }

    async fn write_merged_data(&self, slice_id: u64, data: &[u8]) -> Result<(), CompactorError> {
        let spans: Vec<_> =
            block_span_iter_chunk(0u64.into(), data.len() as u64, self.layout).collect();

        let mut offset = 0usize;
        for span in spans {
            let key: BlockKey = (slice_id, span.index as u32);
            let take = (span.len as usize).min(data.len() - offset);
            // Acquire a permit from the shared upload semaphore so compaction
            // yields bandwidth to foreground flush when the pool is contended.
            let _permit = upload_permit().await;
            self.block_store
                .write_fresh_range(key, span.offset, &data[offset..offset + take])
                .await
                .map_err(|e| CompactorError::BlockStoreError(e.to_string()))?;
            offset += take;
        }

        Ok(())
    }

    /// Clean up uncommitted slice data when compaction fails.
    /// This prevents orphan block data from accumulating.
    async fn cleanup_uncommitted_slice(
        &self,
        slice_id: u64,
        size: u64,
    ) -> Result<(), CompactorError> {
        // Delete block data from block store
        let num_blocks = size.div_ceil(self.layout.block_size as u64);
        if num_blocks > 0 {
            self.block_store
                .delete_range((slice_id, 0), num_blocks)
                .await
                .map_err(|e| {
                    CompactorError::BlockStoreError(format!(
                        "Failed to delete uncommitted blocks for slice {}: {}",
                        slice_id, e
                    ))
                })?;
        }

        // Note: The uncommitted_slice record in metadata will be cleaned up by
        // cleanup_orphan_uncommitted_slices during GC if it wasn't confirmed.
        // We don't delete it here because:
        // 1. It helps with crash recovery tracking
        // 2. GC will handle it based on age
        // 3. Avoid race conditions with concurrent operations

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CompactorError {
    #[error("MetaStore error: {0}")]
    MetaError(#[from] MetaError),
    #[error("BlockStore error: {0}")]
    BlockStoreError(String),
    #[error("Invalid data: {0}")]
    InvalidData(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

impl From<anyhow::Error> for CompactorError {
    fn from(e: anyhow::Error) -> Self {
        CompactorError::BlockStoreError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::reader::DataFetcher;
    use crate::chunk::store::InMemoryBlockStore;
    use crate::meta::factory::create_meta_store_from_url;
    use crate::vfs::backend::Backend;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_read_slice_data_into_uses_object_offset() {
        let layout = ChunkLayout {
            chunk_size: 8 * 1024,
            block_size: 1024,
        };
        let block_store = Arc::new(InMemoryBlockStore::new());
        let meta = create_meta_store_from_url("sqlite::memory:")
            .await
            .unwrap()
            .store();
        let compactor = Compactor::with_layout(meta, block_store.clone(), layout);

        let physical: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
        block_store
            .write_fresh_range((42, 0), 0, &physical[..1024])
            .await
            .unwrap();
        block_store
            .write_fresh_range((42, 1), 0, &physical[1024..])
            .await
            .unwrap();

        let slice = SliceDesc {
            slice_id: 42,
            chunk_id: 7,
            offset: 4096,
            length: 768,
            object_offset: 640,
            object_size: physical.len() as u64,
        };
        let mut dest = vec![0u8; slice.length as usize];

        compactor
            .read_slice_data_into(&slice, &mut dest)
            .await
            .unwrap();

        assert_eq!(dest, physical[640..1408]);
    }

    #[tokio::test]
    async fn test_compact_light_splits_partial_overlap_without_delaying_shared_object() {
        let layout = ChunkLayout {
            chunk_size: 8 * 1024,
            block_size: 1024,
        };
        let block_store = Arc::new(InMemoryBlockStore::new());
        let meta = create_meta_store_from_url("sqlite::memory:")
            .await
            .unwrap()
            .store();
        let compactor = Compactor::with_layout(meta.clone(), block_store, layout);
        let chunk_id = 7201;
        let old = SliceDesc {
            slice_id: 7,
            chunk_id,
            offset: 0,
            length: 4096,
            object_offset: 128,
            object_size: 8192,
        };
        let new = SliceDesc {
            slice_id: 8,
            chunk_id,
            offset: 1024,
            length: 1024,
            object_offset: 0,
            object_size: 1024,
        };
        let expected = vec![
            SliceDesc {
                slice_id: 7,
                chunk_id,
                offset: 0,
                length: 1024,
                object_offset: 128,
                object_size: 8192,
            },
            SliceDesc {
                slice_id: 7,
                chunk_id,
                offset: 2048,
                length: 2048,
                object_offset: 2176,
                object_size: 8192,
            },
            new,
        ];

        meta.append_slice(chunk_id, old).await.unwrap();
        meta.append_slice(chunk_id, new).await.unwrap();

        let removed = compactor.compact_light(chunk_id).await.unwrap();

        assert_eq!(removed, Some(0));
        assert_eq!(meta.get_slices(chunk_id).await.unwrap(), expected);
        assert!(meta.process_delayed_slices(10, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_compact_light_split_views_read_old_prefix_new_middle_old_suffix() {
        let layout = ChunkLayout {
            chunk_size: 8 * 1024,
            block_size: 1024,
        };
        let block_store = Arc::new(InMemoryBlockStore::new());
        let handle = create_meta_store_from_url("sqlite::memory:").await.unwrap();
        let meta = handle.store();
        let backend = Arc::new(Backend::new(block_store.clone(), handle.layer()));
        let compactor = Compactor::with_layout(meta.clone(), block_store.clone(), layout);
        let chunk_id = 7202;
        let old_data = vec![b'a'; 4096];
        let new_data = vec![b'b'; 1024];

        for (index, block) in old_data.chunks(layout.block_size as usize).enumerate() {
            block_store
                .write_fresh_range((7, index as u32), 0, block)
                .await
                .unwrap();
        }
        block_store
            .write_fresh_range((8, 0), 0, &new_data)
            .await
            .unwrap();

        let old = SliceDesc {
            slice_id: 7,
            chunk_id,
            offset: 0,
            length: old_data.len() as u64,
            object_offset: 0,
            object_size: old_data.len() as u64,
        };
        let new = SliceDesc {
            slice_id: 8,
            chunk_id,
            offset: 1024,
            length: new_data.len() as u64,
            object_offset: 0,
            object_size: new_data.len() as u64,
        };
        meta.append_slice(chunk_id, old).await.unwrap();
        meta.append_slice(chunk_id, new).await.unwrap();

        assert_eq!(compactor.compact_light(chunk_id).await.unwrap(), Some(0));

        let mut fetcher = DataFetcher::new(layout, chunk_id, backend.as_ref());
        fetcher.prepare_slices().await.unwrap();
        let data = fetcher.read_at(0u64.into(), old_data.len()).await.unwrap();

        assert_eq!(&data[..1024], vec![b'a'; 1024]);
        assert_eq!(&data[1024..2048], vec![b'b'; 1024]);
        assert_eq!(&data[2048..], vec![b'a'; 2048]);
    }
}

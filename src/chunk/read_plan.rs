//! Workspace-neutral resolved read plans and their block-store executor.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use super::{BlockKey, BlockStore, ChunkLayout, SliceOffset, block_span_iter_slice};
use crate::meta::store::MetaError;
use crate::utils::NumCastExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadPlanSegment {
    Data {
        logical_offset: u64,
        length: u64,
        slice_id: u64,
        slice_offset: u64,
    },
    Zero {
        logical_offset: u64,
        length: u64,
    },
}

impl ReadPlanSegment {
    fn logical_offset(&self) -> u64 {
        match self {
            Self::Data { logical_offset, .. } | Self::Zero { logical_offset, .. } => {
                *logical_offset
            }
        }
    }

    fn length(&self) -> u64 {
        match self {
            Self::Data { length, .. } | Self::Zero { length, .. } => *length,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedReadPlan {
    pub segments: Vec<ReadPlanSegment>,
}

impl ResolvedReadPlan {
    /// Adapt an existing v2/workspace plan into the generation-fenced plan
    /// used by packed v3.  The physical slice remains a legacy source until
    /// its provider is migrated to emit an upper block directly.
    pub fn into_unified(self, generation: ReadGeneration, logical_size: u64) -> UnifiedReadPlan {
        let segments = self
            .segments
            .into_iter()
            .map(|segment| match segment {
                ReadPlanSegment::Data {
                    logical_offset,
                    length,
                    slice_id,
                    slice_offset,
                } => LogicalSegment {
                    logical_offset,
                    length,
                    source: ReadSource::LegacySlice {
                        slice_id,
                        slice_offset,
                    },
                },
                ReadPlanSegment::Zero {
                    logical_offset,
                    length,
                } => LogicalSegment {
                    logical_offset,
                    length,
                    source: ReadSource::Hole,
                },
            })
            .collect();
        UnifiedReadPlan {
            generation,
            logical_size,
            segments,
        }
    }
}

/// The immutable lower and mutable upper versions observed while resolving a
/// read.  A plan is valid only for this pair; callers must discard it when
/// either side advances.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReadGeneration {
    pub workspace_head_epoch: u64,
    pub lower_snapshot: [u8; 32],
}

impl ReadGeneration {
    /// A read-only packed mount has no mutable workspace head.  Epoch zero is
    /// reserved for that case; the lower digest still identifies the exact
    /// immutable snapshot being read.
    pub const fn readonly(lower_snapshot: [u8; 32]) -> Self {
        Self {
            workspace_head_epoch: 0,
            lower_snapshot,
        }
    }
}

/// Physical source for one logical, non-overlapping read segment.  The
/// existing [`ReadPlanSegment`] remains the compatibility representation for
/// v2 and mutable workspace callers; this enum is the common vocabulary used
/// when a plan may combine upper data with a packed lower.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadSource {
    Hole,
    UpperBlock {
        key: BlockKey,
        block_offset: u64,
    },
    LegacySlice {
        slice_id: u64,
        slice_offset: u64,
    },
    PackedFrame {
        group_id: u64,
        container_ordinal: u32,
        frame_ordinal: u32,
        object_offset: u64,
        stored_len: u32,
        raw_offset: u32,
        raw_len: u32,
        size_class: u8,
        codec: u8,
        frame_digest: [u8; 16],
    },
    /// Immutable file bytes embedded in a packed GroupMeta page.  This source
    /// is already resident in the bounded metadata range and must not trigger
    /// a second object-store request.
    PackedInline {
        data: Arc<[u8]>,
        raw_offset: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalSegment {
    pub logical_offset: u64,
    pub length: u64,
    pub source: ReadSource,
}

/// A mutable upper interval supplied by the workspace resolver when composing
/// a view over an immutable lower snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayUpperSegment {
    pub logical_offset: u64,
    pub length: u64,
    pub key: BlockKey,
    pub block_offset: u64,
}

impl LogicalSegment {
    pub fn end(&self) -> Result<u64, ReadPlanError> {
        self.logical_offset
            .checked_add(self.length)
            .ok_or_else(|| ReadPlanError::Invalid("logical segment range overflows".into()))
    }
}

/// A generation-fenced plan shared by overlay resolution and packed readers.
/// It is intentionally additive so existing v2 plans can migrate without
/// changing the current FUSE executor in the same patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnifiedReadPlan {
    pub generation: ReadGeneration,
    pub logical_size: u64,
    pub segments: Vec<LogicalSegment>,
}

impl UnifiedReadPlan {
    pub fn validate(&self, requested_offset: u64, requested_len: u64) -> Result<(), ReadPlanError> {
        let requested_end = requested_offset
            .checked_add(requested_len)
            .ok_or_else(|| ReadPlanError::Invalid("requested range overflows".into()))?;
        if requested_end > self.logical_size {
            return Err(ReadPlanError::Invalid(
                "requested range exceeds logical file size".into(),
            ));
        }

        let mut previous_end = requested_offset;
        for segment in &self.segments {
            if segment.length == 0 {
                return Err(ReadPlanError::Invalid("zero-length logical segment".into()));
            }
            if segment.logical_offset < requested_offset {
                return Err(ReadPlanError::Invalid(
                    "logical segment starts before requested range".into(),
                ));
            }
            let end = segment.end()?;
            if end > requested_end {
                return Err(ReadPlanError::Invalid(
                    "logical segment ends after requested range".into(),
                ));
            }
            if segment.logical_offset < previous_end {
                return Err(ReadPlanError::Invalid(
                    "logical segments overlap or are not sorted".into(),
                ));
            }
            validate_source(&segment.source, segment.length)?;
            previous_end = end;
        }
        Ok(())
    }
}

/// Compose one generation-fenced logical plan from mutable upper intervals and
/// an immutable lower plan.  Every byte in the request receives exactly one
/// source; uncovered bytes become explicit holes so the executor can keep its
/// zero-fill rule simple.
pub fn compose_overlay_plan(
    generation: ReadGeneration,
    requested_offset: u64,
    requested_len: u64,
    lower: &UnifiedReadPlan,
    upper: impl IntoIterator<Item = OverlayUpperSegment>,
) -> Result<UnifiedReadPlan, ReadPlanError> {
    let requested_end = requested_offset
        .checked_add(requested_len)
        .ok_or_else(|| ReadPlanError::Invalid("overlay request range overflows".into()))?;
    if lower.generation != generation {
        return Err(ReadPlanError::Invalid(
            "overlay and lower plans belong to different generations".into(),
        ));
    }
    lower.validate(requested_offset, requested_len)?;

    let mut upper: Vec<_> = upper.into_iter().collect();
    upper.sort_by_key(|segment| segment.logical_offset);
    let mut previous_upper_end = requested_offset;
    let mut boundaries = vec![requested_offset, requested_end];
    for segment in &upper {
        if segment.length == 0 {
            return Err(ReadPlanError::Invalid(
                "overlay segment has zero length".into(),
            ));
        }
        let end = segment
            .logical_offset
            .checked_add(segment.length)
            .ok_or_else(|| ReadPlanError::Invalid("overlay segment range overflows".into()))?;
        if segment.logical_offset < requested_offset || end > requested_end {
            return Err(ReadPlanError::Invalid(
                "overlay segment is outside the requested range".into(),
            ));
        }
        if segment.logical_offset < previous_upper_end {
            return Err(ReadPlanError::Invalid(
                "overlay segments overlap or are not sorted".into(),
            ));
        }
        previous_upper_end = end;
        boundaries.push(segment.logical_offset);
        boundaries.push(end);
    }
    for segment in &lower.segments {
        boundaries.push(segment.logical_offset);
        boundaries.push(segment.end()?);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut segments = Vec::new();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if start >= end {
            continue;
        }
        let length = end - start;
        let source = if let Some(segment) = upper.iter().find(|segment| {
            segment.logical_offset <= start
                && segment
                    .logical_offset
                    .checked_add(segment.length)
                    .is_some_and(|segment_end| segment_end >= end)
        }) {
            ReadSource::UpperBlock {
                key: segment.key,
                block_offset: segment
                    .block_offset
                    .checked_add(start - segment.logical_offset)
                    .ok_or_else(|| {
                        ReadPlanError::Invalid("overlay source offset overflows".into())
                    })?,
            }
        } else if let Some(segment) = lower.segments.iter().find(|segment| {
            segment.logical_offset <= start
                && segment.end().is_ok_and(|segment_end| segment_end >= end)
        }) {
            shift_source(&segment.source, start - segment.logical_offset)?
        } else {
            ReadSource::Hole
        };
        segments.push(LogicalSegment {
            logical_offset: start,
            length,
            source,
        });
    }
    let plan = UnifiedReadPlan {
        generation,
        logical_size: lower.logical_size.max(requested_end),
        segments,
    };
    plan.validate(requested_offset, requested_len)?;
    Ok(plan)
}

fn shift_source(source: &ReadSource, delta: u64) -> Result<ReadSource, ReadPlanError> {
    match source {
        ReadSource::Hole => Ok(ReadSource::Hole),
        ReadSource::UpperBlock { key, block_offset } => Ok(ReadSource::UpperBlock {
            key: *key,
            block_offset: block_offset
                .checked_add(delta)
                .ok_or_else(|| ReadPlanError::Invalid("upper source offset overflows".into()))?,
        }),
        ReadSource::LegacySlice {
            slice_id,
            slice_offset,
        } => Ok(ReadSource::LegacySlice {
            slice_id: *slice_id,
            slice_offset: slice_offset
                .checked_add(delta)
                .ok_or_else(|| ReadPlanError::Invalid("legacy source offset overflows".into()))?,
        }),
        ReadSource::PackedFrame {
            group_id,
            container_ordinal,
            frame_ordinal,
            object_offset,
            stored_len,
            raw_offset,
            raw_len,
            size_class,
            codec,
            frame_digest,
        } => {
            let shifted = u64::from(*raw_offset)
                .checked_add(delta)
                .ok_or_else(|| ReadPlanError::Invalid("packed source offset overflows".into()))?;
            if shifted > u64::from(*raw_len) {
                return Err(ReadPlanError::Invalid(
                    "packed source offset exceeds frame length".into(),
                ));
            }
            Ok(ReadSource::PackedFrame {
                group_id: *group_id,
                container_ordinal: *container_ordinal,
                frame_ordinal: *frame_ordinal,
                object_offset: *object_offset,
                stored_len: *stored_len,
                raw_offset: u32::try_from(shifted).map_err(|_| {
                    ReadPlanError::Invalid("packed source offset exceeds u32".into())
                })?,
                raw_len: *raw_len,
                size_class: *size_class,
                codec: *codec,
                frame_digest: *frame_digest,
            })
        }
        ReadSource::PackedInline { data, raw_offset } => {
            let shifted = u64::from(*raw_offset)
                .checked_add(delta)
                .ok_or_else(|| ReadPlanError::Invalid("packed inline offset overflows".into()))?;
            if shifted > data.len() as u64 {
                return Err(ReadPlanError::Invalid(
                    "packed inline offset exceeds payload length".into(),
                ));
            }
            Ok(ReadSource::PackedInline {
                data: data.clone(),
                raw_offset: u32::try_from(shifted).map_err(|_| {
                    ReadPlanError::Invalid("packed inline offset exceeds u32".into())
                })?,
            })
        }
    }
}

fn validate_source(source: &ReadSource, logical_len: u64) -> Result<(), ReadPlanError> {
    match source {
        ReadSource::Hole => Ok(()),
        ReadSource::UpperBlock { block_offset, .. }
        | ReadSource::LegacySlice {
            slice_offset: block_offset,
            ..
        } => block_offset
            .checked_add(logical_len)
            .ok_or_else(|| ReadPlanError::Invalid("source range overflows".into()))
            .map(|_| ()),
        ReadSource::PackedFrame {
            object_offset,
            stored_len,
            raw_offset,
            raw_len,
            ..
        } => {
            if *stored_len == 0 || *raw_len == 0 {
                return Err(ReadPlanError::Invalid(
                    "packed frame lengths must be non-zero".into(),
                ));
            }
            let raw_end = u64::from(*raw_offset)
                .checked_add(logical_len)
                .ok_or_else(|| ReadPlanError::Invalid("packed raw range overflows".into()))?;
            if raw_end > u64::from(*raw_len) {
                return Err(ReadPlanError::Invalid(
                    "packed logical segment exceeds frame raw length".into(),
                ));
            }
            object_offset
                .checked_add(u64::from(*stored_len))
                .ok_or_else(|| ReadPlanError::Invalid("packed object range overflows".into()))?;
            Ok(())
        }
        ReadSource::PackedInline { data, raw_offset } => {
            let end = u64::from(*raw_offset)
                .checked_add(logical_len)
                .ok_or_else(|| ReadPlanError::Invalid("packed inline range overflows".into()))?;
            if end > data.len() as u64 {
                return Err(ReadPlanError::Invalid(
                    "packed inline range exceeds payload length".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Backend-specific source fetcher for the unified overlay/packed plan.
/// Coordinators can implement this with a block store, a v2 slice reader, or
/// a streaming packed frame reader without changing logical range validation.
#[async_trait]
pub trait UnifiedReadSourceFetcher: Send + Sync {
    async fn read_source(&self, source: &ReadSource, output: &mut [u8]) -> anyhow::Result<()>;

    async fn ensure_generation(&self, _generation: ReadGeneration) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Execute a unified plan into the caller's buffer.  Sources are dispatched
/// through one trait, while the fetcher remains free to coalesce packed
/// ranges internally.  The output is zero-filled first, so sparse gaps need
/// no explicit segment.
pub async fn execute_unified_into<F: UnifiedReadSourceFetcher + ?Sized>(
    fetcher: &F,
    requested_offset: u64,
    plan: &UnifiedReadPlan,
    output: &mut [u8],
) -> Result<(), ReadPlanError> {
    let requested_len = u64::try_from(output.len())
        .map_err(|_| ReadPlanError::Invalid("output length exceeds u64".into()))?;
    plan.validate(requested_offset, requested_len)?;
    fetcher
        .ensure_generation(plan.generation)
        .await
        .map_err(ReadPlanError::Backend)?;
    output.fill(0);
    for segment in &plan.segments {
        if matches!(&segment.source, ReadSource::Hole) {
            continue;
        }
        let output_start = usize::try_from(segment.logical_offset - requested_offset)
            .map_err(|_| ReadPlanError::Invalid("output offset exceeds usize".into()))?;
        let output_len = usize::try_from(segment.length)
            .map_err(|_| ReadPlanError::Invalid("segment length exceeds usize".into()))?;
        let output_end = output_start
            .checked_add(output_len)
            .ok_or_else(|| ReadPlanError::Invalid("output range overflows".into()))?;
        fetcher
            .read_source(&segment.source, &mut output[output_start..output_end])
            .await
            .map_err(ReadPlanError::Backend)?;
    }
    fetcher
        .ensure_generation(plan.generation)
        .await
        .map_err(ReadPlanError::Backend)?;
    Ok(())
}

#[async_trait]
pub trait WorkspaceReadPlanProvider: Send + Sync {
    async fn read_plan(
        &self,
        ino: i64,
        chunk_index: u64,
        offset: u64,
        len: u64,
    ) -> Result<ResolvedReadPlan, MetaError>;

    /// Generation-aware adapter used by the v3 overlay path. Existing
    /// providers can migrate incrementally: their legacy slices and holes are
    /// lifted into the shared source vocabulary until they emit upper or
    /// packed sources directly.
    async fn read_unified_plan(
        &self,
        ino: i64,
        chunk_index: u64,
        offset: u64,
        len: u64,
        generation: ReadGeneration,
        logical_size: u64,
    ) -> Result<UnifiedReadPlan, MetaError> {
        let plan = self.read_plan(ino, chunk_index, offset, len).await?;
        let plan = plan.into_unified(generation, logical_size.max(offset.saturating_add(len)));
        plan.validate(offset, len)
            .map_err(|error| MetaError::Internal(error.to_string()))?;
        Ok(plan)
    }

    async fn range_has_data(&self, ino: i64, offset: u64, len: u64) -> Result<bool, MetaError>;

    /// Persist an uploaded slice that could not be attached to the workspace head.
    ///
    /// Providers used only by tests may keep the no-op default. Production workspace
    /// providers override this so GC can reclaim data uploaded before a fencing failure.
    async fn record_orphan_slice(&self, _slice_id: u64, _slice_end: u64) -> Result<(), MetaError> {
        Ok(())
    }

    /// Replace a file range with logical zeroes in the effective workspace view.
    /// `keep_size` implements `FALLOC_FL_KEEP_SIZE`; otherwise the range may extend EOF.
    async fn apply_hole_range(
        &self,
        _ino: i64,
        _offset: u64,
        _len: u64,
        _keep_size: bool,
    ) -> Result<u64, MetaError> {
        Err(MetaError::NotSupported(
            "workspace hole mutations are unavailable".into(),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadPlanError {
    #[error("invalid read plan: {0}")]
    Invalid(String),
    #[error(transparent)]
    Backend(#[from] anyhow::Error),
}

struct SendBuf {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: `execute_into` creates each SendBuf from a disjoint output range and
// waits for every future before using the output again.
unsafe impl Send for SendBuf {}

impl SendBuf {
    unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: upheld by the constructor site in `execute_into`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

pub async fn execute_into<B: BlockStore + Sync>(
    store: &B,
    layout: ChunkLayout,
    requested_offset: u64,
    plan: &ResolvedReadPlan,
    output: &mut [u8],
) -> Result<(), ReadPlanError> {
    let requested_len = u64::try_from(output.len())
        .map_err(|_| ReadPlanError::Invalid("output length exceeds u64".into()))?;
    let requested_end = requested_offset
        .checked_add(requested_len)
        .ok_or_else(|| ReadPlanError::Invalid("requested range overflows".into()))?;

    let mut previous_end = requested_offset;
    for segment in &plan.segments {
        let start = segment.logical_offset();
        let length = segment.length();
        if length == 0 {
            return Err(ReadPlanError::Invalid("zero-length segment".into()));
        }
        let end = start
            .checked_add(length)
            .ok_or_else(|| ReadPlanError::Invalid("segment range overflows".into()))?;
        if start < requested_offset || end > requested_end {
            return Err(ReadPlanError::Invalid(
                "segment lies outside the requested range".into(),
            ));
        }
        if start < previous_end {
            return Err(ReadPlanError::Invalid(
                "segments overlap or are not sorted".into(),
            ));
        }
        previous_end = end;
        if let ReadPlanSegment::Data {
            slice_offset,
            length,
            ..
        } = segment
        {
            slice_offset
                .checked_add(*length)
                .ok_or_else(|| ReadPlanError::Invalid("slice range overflows".into()))?;
        }
    }

    output.fill(0);
    let mut futures = FuturesUnordered::new();
    for segment in &plan.segments {
        let ReadPlanSegment::Data {
            logical_offset,
            length,
            slice_id,
            slice_offset,
        } = *segment
        else {
            continue;
        };
        let output_start = usize::try_from(logical_offset - requested_offset)
            .map_err(|_| ReadPlanError::Invalid("output offset exceeds usize".into()))?;
        let output_len = usize::try_from(length)
            .map_err(|_| ReadPlanError::Invalid("segment length exceeds usize".into()))?;
        let output_end = output_start
            .checked_add(output_len)
            .ok_or_else(|| ReadPlanError::Invalid("output range overflows".into()))?;
        let segment_output = &mut output[output_start..output_end];
        let mut consumed = 0usize;
        for block in block_span_iter_slice(SliceOffset::from(slice_offset), length, layout) {
            let take = block.len.as_usize();
            let block_end = consumed
                .checked_add(take)
                .ok_or_else(|| ReadPlanError::Invalid("block span overflows".into()))?;
            let block_output = &mut segment_output[consumed..block_end];
            consumed = block_end;
            let mut send_buf = SendBuf {
                ptr: block_output.as_mut_ptr(),
                len: block_output.len(),
            };
            let key = (slice_id, block.index.as_u32());
            let block_offset = block.offset;
            futures.push(async move {
                // SAFETY: block spans and plan segments were validated as disjoint.
                store
                    .read_range(key, block_offset, unsafe { send_buf.as_mut_slice() })
                    .await
            });
        }
        if consumed != output_len {
            return Err(ReadPlanError::Invalid(
                "block spans do not cover the data segment".into(),
            ));
        }
    }
    while let Some(result) = futures.next().await {
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use bytes::Bytes;
    use tokio::sync::Mutex;

    use super::*;
    use crate::chunk::store::BlockKey;

    #[derive(Default)]
    struct TestStore {
        blocks: HashMap<BlockKey, Vec<u8>>,
        reads: Arc<Mutex<Vec<BlockKey>>>,
    }

    #[async_trait]
    impl BlockStore for TestStore {
        async fn write_fresh_range(
            &self,
            _key: BlockKey,
            _offset: u64,
            _data: &[u8],
        ) -> anyhow::Result<u64> {
            anyhow::bail!("unused")
        }

        async fn write_fresh_vectored(
            &self,
            _key: BlockKey,
            _offset: u64,
            _chunks: Vec<Bytes>,
        ) -> anyhow::Result<u64> {
            anyhow::bail!("unused")
        }

        async fn read_range(
            &self,
            key: BlockKey,
            offset: u64,
            buf: &mut [u8],
        ) -> anyhow::Result<()> {
            self.reads.lock().await.push(key);
            let block = self
                .blocks
                .get(&key)
                .ok_or_else(|| anyhow::anyhow!("missing"))?;
            let start = usize::try_from(offset)?;
            buf.copy_from_slice(&block[start..start + buf.len()]);
            Ok(())
        }

        async fn delete_range(&self, _key: BlockKey, _block_count: u64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn executor_reads_only_data_segments_and_preserves_zeroes() {
        let mut store = TestStore::default();
        store.blocks.insert((7, 0), b"abcdefgh".to_vec());
        let plan = ResolvedReadPlan {
            segments: vec![
                ReadPlanSegment::Zero {
                    logical_offset: 2,
                    length: 2,
                },
                ReadPlanSegment::Data {
                    logical_offset: 4,
                    length: 3,
                    slice_id: 7,
                    slice_offset: 1,
                },
                ReadPlanSegment::Zero {
                    logical_offset: 7,
                    length: 1,
                },
            ],
        };
        let mut output = [9; 6];
        execute_into(
            &store,
            ChunkLayout {
                chunk_size: 16,
                block_size: 8,
            },
            2,
            &plan,
            &mut output,
        )
        .await
        .unwrap();
        assert_eq!(&output, b"\0\0bcd\0");
        assert_eq!(store.reads.lock().await.as_slice(), &[(7, 0)]);
    }

    #[tokio::test]
    async fn executor_rejects_overlap_and_out_of_range_before_io() {
        let store = TestStore::default();
        let cases = [
            ResolvedReadPlan {
                segments: vec![
                    ReadPlanSegment::Zero {
                        logical_offset: 0,
                        length: 3,
                    },
                    ReadPlanSegment::Zero {
                        logical_offset: 2,
                        length: 1,
                    },
                ],
            },
            ResolvedReadPlan {
                segments: vec![ReadPlanSegment::Data {
                    logical_offset: 3,
                    length: 2,
                    slice_id: 1,
                    slice_offset: 0,
                }],
            },
        ];
        for plan in cases {
            let mut output = [0; 4];
            assert!(
                execute_into(&store, ChunkLayout::default(), 0, &plan, &mut output)
                    .await
                    .is_err()
            );
        }
        assert!(store.reads.lock().await.is_empty());
    }

    #[test]
    fn unified_plan_accepts_overlay_and_packed_sources_in_one_generation() {
        let plan = UnifiedReadPlan {
            generation: ReadGeneration {
                workspace_head_epoch: 9,
                lower_snapshot: [7; 32],
            },
            logical_size: 16,
            segments: vec![
                LogicalSegment {
                    logical_offset: 0,
                    length: 4,
                    source: ReadSource::UpperBlock {
                        key: (11, 0),
                        block_offset: 8,
                    },
                },
                LogicalSegment {
                    logical_offset: 4,
                    length: 4,
                    source: ReadSource::PackedFrame {
                        group_id: 3,
                        container_ordinal: 2,
                        frame_ordinal: 5,
                        object_offset: 4096,
                        stored_len: 128,
                        raw_offset: 12,
                        raw_len: 64,
                        size_class: 0,
                        codec: 0,
                        frame_digest: [1; 16],
                    },
                },
                LogicalSegment {
                    logical_offset: 8,
                    length: 8,
                    source: ReadSource::Hole,
                },
            ],
        };

        plan.validate(0, 16).unwrap();
        assert_eq!(plan.generation.workspace_head_epoch, 9);
    }

    #[test]
    fn unified_plan_rejects_packed_frame_overread_and_overlap() {
        let overread = UnifiedReadPlan {
            generation: ReadGeneration::readonly([3; 32]),
            logical_size: 8,
            segments: vec![LogicalSegment {
                logical_offset: 0,
                length: 5,
                source: ReadSource::PackedFrame {
                    group_id: 1,
                    container_ordinal: 0,
                    frame_ordinal: 0,
                    object_offset: 64,
                    stored_len: 8,
                    raw_offset: 4,
                    raw_len: 8,
                    size_class: 0,
                    codec: 0,
                    frame_digest: [0; 16],
                },
            }],
        };
        assert!(overread.validate(0, 5).is_err());

        let overlap = UnifiedReadPlan {
            generation: ReadGeneration::readonly([4; 32]),
            logical_size: 8,
            segments: vec![
                LogicalSegment {
                    logical_offset: 0,
                    length: 5,
                    source: ReadSource::Hole,
                },
                LogicalSegment {
                    logical_offset: 4,
                    length: 1,
                    source: ReadSource::Hole,
                },
            ],
        };
        assert!(overlap.validate(0, 8).is_err());
    }

    #[test]
    fn compose_overlay_plan_covers_upper_hole_and_lower_fallback() {
        let generation = ReadGeneration {
            workspace_head_epoch: 4,
            lower_snapshot: [6; 32],
        };
        let lower = UnifiedReadPlan {
            generation,
            logical_size: 16,
            segments: vec![
                LogicalSegment {
                    logical_offset: 0,
                    length: 8,
                    source: ReadSource::LegacySlice {
                        slice_id: 1,
                        slice_offset: 10,
                    },
                },
                LogicalSegment {
                    logical_offset: 8,
                    length: 4,
                    source: ReadSource::Hole,
                },
                LogicalSegment {
                    logical_offset: 12,
                    length: 4,
                    source: ReadSource::PackedFrame {
                        group_id: 2,
                        container_ordinal: 0,
                        frame_ordinal: 3,
                        object_offset: 4096,
                        stored_len: 64,
                        raw_offset: 0,
                        raw_len: 64,
                        size_class: 0,
                        codec: 0,
                        frame_digest: [9; 16],
                    },
                },
            ],
        };
        let composed = compose_overlay_plan(
            generation,
            0,
            16,
            &lower,
            [OverlayUpperSegment {
                logical_offset: 2,
                length: 3,
                key: (99, 0),
                block_offset: 7,
            }],
        )
        .unwrap();
        composed.validate(0, 16).unwrap();
        assert!(matches!(
            composed.segments[1].source,
            ReadSource::UpperBlock {
                key: (99, 0),
                block_offset: 7
            }
        ));
        assert!(matches!(composed.segments[3].source, ReadSource::Hole));
        assert!(matches!(
            composed.segments[4].source,
            ReadSource::PackedFrame { raw_offset: 0, .. }
        ));
    }

    #[test]
    fn compose_overlay_plan_rejects_generation_mismatch_and_overlap() {
        let lower = UnifiedReadPlan {
            generation: ReadGeneration::readonly([1; 32]),
            logical_size: 4,
            segments: vec![LogicalSegment {
                logical_offset: 0,
                length: 4,
                source: ReadSource::Hole,
            }],
        };
        assert!(
            compose_overlay_plan(
                ReadGeneration::readonly([2; 32]),
                0,
                4,
                &lower,
                std::iter::empty(),
            )
            .is_err()
        );
        assert!(
            compose_overlay_plan(
                ReadGeneration::readonly([1; 32]),
                0,
                4,
                &lower,
                [
                    OverlayUpperSegment {
                        logical_offset: 0,
                        length: 3,
                        key: (1, 0),
                        block_offset: 0,
                    },
                    OverlayUpperSegment {
                        logical_offset: 2,
                        length: 2,
                        key: (2, 0),
                        block_offset: 0,
                    },
                ],
            )
            .is_err()
        );
    }

    struct UnifiedTestFetcher;

    #[test]
    fn shifting_packed_inline_sources_shares_payload_storage() {
        let data: Arc<[u8]> = Arc::from(b"payload".as_slice());
        let source = ReadSource::PackedInline {
            data: data.clone(),
            raw_offset: 0,
        };
        let shifted = shift_source(&source, 2).unwrap();
        let ReadSource::PackedInline {
            data: shifted_data,
            raw_offset,
        } = shifted
        else {
            panic!("expected packed inline source");
        };
        assert_eq!(raw_offset, 2);
        assert!(Arc::ptr_eq(&data, &shifted_data));
    }

    #[async_trait]
    impl UnifiedReadSourceFetcher for UnifiedTestFetcher {
        async fn read_source(&self, source: &ReadSource, output: &mut [u8]) -> anyhow::Result<()> {
            match source {
                ReadSource::UpperBlock { .. } => output.fill(b'U'),
                ReadSource::LegacySlice { .. } => output.fill(b'L'),
                ReadSource::PackedFrame { .. } => output.fill(b'P'),
                ReadSource::PackedInline { data, raw_offset } => {
                    let start = *raw_offset as usize;
                    output.copy_from_slice(&data[start..start + output.len()]);
                }
                ReadSource::Hole => anyhow::bail!("holes are handled by the executor"),
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn unified_executor_dispatches_sources_and_preserves_holes() {
        let plan = UnifiedReadPlan {
            generation: ReadGeneration::readonly([5; 32]),
            logical_size: 8,
            segments: vec![
                LogicalSegment {
                    logical_offset: 0,
                    length: 2,
                    source: ReadSource::UpperBlock {
                        key: (1, 0),
                        block_offset: 0,
                    },
                },
                LogicalSegment {
                    logical_offset: 2,
                    length: 2,
                    source: ReadSource::PackedFrame {
                        group_id: 1,
                        container_ordinal: 0,
                        frame_ordinal: 0,
                        object_offset: 64,
                        stored_len: 8,
                        raw_offset: 0,
                        raw_len: 2,
                        size_class: 0,
                        codec: 0,
                        frame_digest: [0; 16],
                    },
                },
                LogicalSegment {
                    logical_offset: 6,
                    length: 2,
                    source: ReadSource::LegacySlice {
                        slice_id: 2,
                        slice_offset: 0,
                    },
                },
            ],
        };
        let mut output = [0xff; 8];
        execute_unified_into(&UnifiedTestFetcher, 0, &plan, &mut output)
            .await
            .unwrap();
        assert_eq!(&output, b"UUPP\0\0LL");
    }
}

//! Authenticated PM11 lower combined with native workspace Data/Hole coverage.
//! Store binding publication and whole-request retries are separate contracts.

use std::sync::Arc;

use async_trait::async_trait;

use crate::cadapter::client::ObjectBackend;
use crate::cadapter::read_observer::{OperationDelivery, TerminalGuard};
use crate::chunk::read_plan::{
    LogicalSegment, PreparedUnifiedRead, ReadGeneration, ReadPlanSegment, ReadSource,
    ReadViewChanged, ResolvedReadPlan, UnifiedReadPlan, UnifiedReadRequestFence,
    UnifiedReadSourceFetcher, WorkspaceReadPlanProvider, execute_into, prepare_overlay_plan,
};
use crate::chunk::{BlockStore, ChunkLayout};
use crate::meta::layer::MetaLayer;
use crate::meta::store::{FileType, MetaError};

use super::{WorkspaceMetaLayer, validate_fixed_layer_pair};
use crate::workspace_overlay::catalog::{
    ExtentQuery, HeadGuard, InodeQuery, PackedLowerBinding, WorkspaceStore,
};
use crate::workspace_overlay::error::WorkspaceError;
use crate::workspace_overlay::model::{ExtentKind, LayerRecord};
use crate::workspace_overlay::packed_v3::PackedV3ReadonlyMeta;
use crate::workspace_overlay::packed_v3::wire005::{
    V3BudgetPool, V3MountBudget, V3ObjectKind, V3OwnedPermit,
};
use crate::workspace_overlay::resolver::{
    Resolution, resolve_extent_coverage, resolve_inode_state,
};

const MAX_UPPER_EXTENT_ROWS: usize = 1024;
// Includes fixed-width backend rows plus sentinel, resolver intermediates,
// upper segments, gaps and per-gap owner bookkeeping. Child plan clones are
// admitted separately before cloning; child owners retain their own permits.
const UPPER_PREPARATION_BYTES: u64 = 4 << 20;
// The request retains a fixed guard, two layer records and shared lower
// owners. Chunk preparation keeps its separate row/segment admission.
const REQUEST_FENCE_BYTES: u64 = 4096;

pub(super) fn budget_to_meta(
    error: crate::workspace_overlay::packed_v3::PackedWireError,
) -> MetaError {
    if matches!(
        error,
        crate::workspace_overlay::packed_v3::PackedWireError::LimitExceeded(_)
    ) {
        MetaError::Io(std::io::Error::from_raw_os_error(libc::ENOMEM))
    } else {
        MetaError::Internal(error.to_string())
    }
}

fn read_workspace_error(error: WorkspaceError) -> anyhow::Error {
    match error {
        WorkspaceError::Busy => ReadViewChanged.into(),
        error => error.into(),
    }
}

fn read_workspace_to_meta(error: WorkspaceError) -> MetaError {
    MetaError::Anyhow(read_workspace_error(error))
}

fn fetch_to_meta(error: anyhow::Error) -> MetaError {
    MetaError::Anyhow(error)
}

/// An authority must validate the independent binding under the supplied
/// workspace/lease guard. Tests may inject a deterministic authority, which
/// does not establish persisted publication/GC correctness.
#[async_trait]
pub trait WorkspacePackedBindingAuthority: Send + Sync {
    fn retain_reader_request(
        &self,
    ) -> Result<
        Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
        WorkspaceError,
    > {
        Ok(None)
    }
    fn reader_session(
        &self,
    ) -> Option<Arc<dyn crate::workspace_overlay::packed_reader_lifecycle::PackedReaderSession>>
    {
        None
    }

    async fn validate(
        &self,
        guard: &HeadGuard,
        expected: &PackedLowerBinding,
    ) -> Result<(), WorkspaceError>;
}

pub struct CatalogPackedBindingAuthority<W>(pub Arc<W>);

#[async_trait]
impl<W: WorkspaceStore + 'static> WorkspacePackedBindingAuthority
    for CatalogPackedBindingAuthority<W>
{
    async fn validate(
        &self,
        guard: &HeadGuard,
        expected: &PackedLowerBinding,
    ) -> Result<(), WorkspaceError> {
        if self
            .0
            .load_packed_lower_binding(guard.clone())
            .await?
            .as_ref()
            != Some(expected)
        {
            return Err(WorkspaceError::Fenced);
        }
        Ok(())
    }
}

/// Current workspace authority plus independent persistent byte retention.
/// Retention renewal survives publication; mixed workspace reads still fence.
pub struct PinnedCatalogPackedBindingAuthority<W> {
    pub store: Arc<W>,
    pub reader: Arc<dyn crate::workspace_overlay::packed_reader_lifecycle::PackedReaderSession>,
}
#[async_trait]
impl<W: WorkspaceStore + 'static> WorkspacePackedBindingAuthority
    for PinnedCatalogPackedBindingAuthority<W>
{
    fn retain_reader_request(
        &self,
    ) -> Result<
        Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
        WorkspaceError,
    > {
        self.reader.retain_request().map(Some)
    }
    fn reader_session(
        &self,
    ) -> Option<Arc<dyn crate::workspace_overlay::packed_reader_lifecycle::PackedReaderSession>>
    {
        Some(self.reader.clone())
    }
    async fn validate(
        &self,
        guard: &HeadGuard,
        expected: &PackedLowerBinding,
    ) -> Result<(), WorkspaceError> {
        if self.reader.binding() != expected {
            return Err(WorkspaceError::Fenced);
        }
        self.reader.validate().await?;
        CatalogPackedBindingAuthority(self.store.clone())
            .validate(guard, expected)
            .await
    }
}

#[async_trait]
pub(crate) trait PackedLowerOperations: MetaLayer + WorkspaceReadPlanProvider {
    async fn drain_packed_transport(&self) -> Result<(), MetaError>;
    async fn frozen_reverse_names_page_owned(
        &self,
        inode: i64,
        after: Option<&[u8]>,
    ) -> Result<crate::workspace_overlay::packed_v3::PackedLowerReverseNames, MetaError>;
    async fn frozen_inode_metadata_owned(
        &self,
        inode: i64,
    ) -> Result<Option<crate::workspace_overlay::packed_v3::PackedLowerInodeMetadata>, MetaError>;
    async fn frozen_cold_attributes_owned(
        &self,
        inode: i64,
    ) -> Result<
        Option<
            crate::workspace_overlay::packed_v3::wire005::V3Owned<
                crate::workspace_overlay::packed_v3::wire005::V3ColdAttributes,
            >,
        >,
        MetaError,
    >;
}
#[async_trait]
impl<B: ObjectBackend + Clone + Send + Sync + 'static> PackedLowerOperations
    for PackedV3ReadonlyMeta<B>
{
    async fn frozen_reverse_names_page_owned(
        &self,
        inode: i64,
        after: Option<&[u8]>,
    ) -> Result<crate::workspace_overlay::packed_v3::PackedLowerReverseNames, MetaError> {
        PackedV3ReadonlyMeta::frozen_reverse_names_page_owned(self, inode, after).await
    }
    async fn frozen_inode_metadata_owned(
        &self,
        inode: i64,
    ) -> Result<Option<crate::workspace_overlay::packed_v3::PackedLowerInodeMetadata>, MetaError>
    {
        PackedV3ReadonlyMeta::frozen_inode_metadata_owned(self, inode).await
    }
    async fn drain_packed_transport(&self) -> Result<(), MetaError> {
        PackedV3ReadonlyMeta::drain_packed_transport(self).await
    }
    async fn frozen_cold_attributes_owned(
        &self,
        inode: i64,
    ) -> Result<
        Option<
            crate::workspace_overlay::packed_v3::wire005::V3Owned<
                crate::workspace_overlay::packed_v3::wire005::V3ColdAttributes,
            >,
        >,
        MetaError,
    > {
        PackedV3ReadonlyMeta::frozen_cold_attributes_owned(self, inode).await
    }
}

/// Native means native catalog layers; it is never an older packed decoder.
pub(super) enum WorkspaceLower {
    Native,
    PackedV3(Arc<WorkspacePackedLower>),
}

pub(crate) struct WorkspacePackedLower {
    pub binding: PackedLowerBinding,
    pub metadata: Arc<dyn PackedLowerOperations>,
    pub authority: Arc<dyn WorkspacePackedBindingAuthority>,
    pub upper: Arc<dyn UnifiedReadSourceFetcher>,
    pub budget: Arc<V3MountBudget>,
    upper_identity: usize,
    upper_layout: ChunkLayout,
}

impl WorkspacePackedLower {
    pub(super) fn new<B, S>(
        binding: PackedLowerBinding,
        metadata: Arc<PackedV3ReadonlyMeta<B>>,
        authority: Arc<dyn WorkspacePackedBindingAuthority>,
        upper: Arc<S>,
        layout: ChunkLayout,
    ) -> Result<Arc<Self>, MetaError>
    where
        B: ObjectBackend + Clone + Send + Sync + 'static,
        S: BlockStore + Send + Sync + 'static,
    {
        if binding.binding_version == 0
            || binding.manifest.kind != V3ObjectKind::Manifest
            || &binding.manifest != metadata.manifest_reference()
            || layout.chunk_size == 0
            || layout.block_size == 0
            || layout.chunk_size != metadata.chunk_size()
        {
            return Err(MetaError::Internal(
                "packed lower binding/layout mismatch".into(),
            ));
        }
        let budget = metadata.mount_budget();
        if let Some(reader) = authority.reader_session() {
            if reader.binding() != &binding {
                return Err(MetaError::Internal(
                    "packed lower transport binding mismatch".into(),
                ));
            }
            metadata.bind_reader_session(reader)?;
        }
        Ok(Arc::new(Self {
            binding,
            metadata,
            authority,
            upper_identity: Arc::as_ptr(&upper) as *const () as usize,
            upper_layout: layout,
            upper: Arc::new(NativeUpperFetcher {
                store: upper,
                layout,
            }),
            budget,
        }))
    }

    pub(super) fn begin_operation(&self, requested: u64) -> Option<TerminalGuard> {
        self.metadata.begin_unified_read_operation(requested)
    }

    pub(crate) fn frozen_upper_matches<S>(&self, upper: &Arc<S>, layout: ChunkLayout) -> bool {
        self.upper_identity == Arc::as_ptr(upper) as *const () as usize
            && self.upper_layout.chunk_size == layout.chunk_size
            && self.upper_layout.block_size == layout.block_size
    }
}

async fn capture_read_fence<W: WorkspaceStore + 'static>(
    meta: &WorkspaceMetaLayer<W>,
    lower: &WorkspacePackedLower,
) -> anyhow::Result<(
    HeadGuard,
    [LayerRecord; 2],
    Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
)> {
    let owner = lower
        .authority
        .retain_reader_request()
        .map_err(read_workspace_error)?;
    let guard = meta.guard().await;
    lower
        .authority
        .validate(&guard, &lower.binding)
        .await
        .map_err(read_workspace_error)?;
    let cached_chain = meta.chain().await?;
    let layers = [
        meta.store
            .load_layer(cached_chain[0].layer_id)
            .await
            .map_err(read_workspace_error)?,
        meta.store
            .load_layer(cached_chain[1].layer_id)
            .await
            .map_err(read_workspace_error)?,
    ];
    if layers[0].layer_id != guard.expected_head_layer_id
        || layers[1].layer_id != lower.binding.base_layer_id
    {
        return Err(WorkspaceError::Fenced.into());
    }
    meta.store
        .validate_read_fence(guard.clone(), layers.clone())
        .await
        .map_err(read_workspace_error)?;
    // A former writable head becoming sealed is a lost guard. Check
    // authority before reporting malformed fixed-pair metadata.
    validate_fixed_layer_pair(&layers).map_err(read_workspace_error)?;
    Ok((guard, layers, owner))
}

struct PackedReadRequestFence<W> {
    store: Arc<W>,
    guard: HeadGuard,
    layers: [LayerRecord; 2],
    lower: Arc<WorkspacePackedLower>,
    file_size: u64,
    _reader_owner:
        Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
    _permit: V3OwnedPermit,
}

#[async_trait]
impl<W: WorkspaceStore + 'static> UnifiedReadRequestFence for PackedReadRequestFence<W> {
    fn file_size(&self) -> u64 {
        self.file_size
    }

    async fn ensure_current(&self) -> anyhow::Result<()> {
        self.lower
            .authority
            .validate(&self.guard, &self.lower.binding)
            .await
            .map_err(read_workspace_error)?;
        self.store
            .validate_read_fence(self.guard.clone(), self.layers.clone())
            .await
            .map_err(read_workspace_error)
    }
}

pub(super) async fn begin_request<W: WorkspaceStore + 'static>(
    meta: &WorkspaceMetaLayer<W>,
    lower: Arc<WorkspacePackedLower>,
    ino: i64,
) -> anyhow::Result<Arc<dyn UnifiedReadRequestFence>> {
    let permit = lower
        .budget
        .admit(&[(V3BudgetPool::Control, REQUEST_FENCE_BYTES)])
        .map_err(budget_to_meta)?;
    let (guard, layers, reader_owner) = capture_read_fence(meta, &lower).await?;
    // Read direct upper rows before any cached EOF can suppress preparation.
    // Only true upper absence consults the authenticated readonly lower.
    let inodes = meta
        .store
        .get_inode_deltas(InodeQuery {
            layer_ids: layers.iter().map(|layer| layer.layer_id).collect(),
            ino,
        })
        .await
        .map_err(read_workspace_error)?;
    let file_size =
        match resolve_inode_state(&layers, &inodes, ino).map_err(read_workspace_error)? {
            Resolution::Present(inode) => {
                if inode.inode.kind != 0 {
                    return Err(MetaError::NotSupported(
                        "read request requires a regular upper file".into(),
                    )
                    .into());
                }
                inode.inode.size
            }
            Resolution::Masked => return Err(MetaError::NotFound(ino).into()),
            Resolution::Absent => {
                let attr = lower
                    .metadata
                    .stat_fresh(ino)
                    .await?
                    .ok_or(MetaError::NotFound(ino))?;
                if attr.kind != FileType::File {
                    return Err(MetaError::NotSupported(
                        "read request requires a regular lower file".into(),
                    )
                    .into());
                }
                attr.size
            }
        };
    let request = Arc::new(PackedReadRequestFence {
        store: meta.store.clone(),
        guard,
        layers,
        lower,
        file_size,
        _reader_owner: reader_owner,
        _permit: permit,
    });
    request.ensure_current().await?;
    Ok(request)
}

struct NativeUpperFetcher<S> {
    store: Arc<S>,
    layout: ChunkLayout,
}

#[async_trait]
impl<S: BlockStore + Send + Sync + 'static> UnifiedReadSourceFetcher for NativeUpperFetcher<S> {
    async fn read_source(&self, source: &ReadSource, output: &mut [u8]) -> anyhow::Result<()> {
        match source {
            ReadSource::Hole => output.fill(0),
            ReadSource::UpperBlock { key, block_offset } => {
                self.store.read_range(*key, *block_offset, output).await?;
            }
            ReadSource::LegacySlice {
                slice_id,
                slice_offset,
            } => {
                let plan = ResolvedReadPlan {
                    segments: vec![ReadPlanSegment::Data {
                        logical_offset: 0,
                        length: output.len() as u64,
                        slice_id: *slice_id,
                        slice_offset: *slice_offset,
                    }],
                };
                execute_into(self.store.as_ref(), self.layout, 0, &plan, output).await?;
            }
            ReadSource::PackedFrame { .. } | ReadSource::PackedInline { .. } => {
                anyhow::bail!("packed source sent to native upper fetcher");
            }
        }
        Ok(())
    }
}

/// Retains original child plans/fetchers, their readonly epoch0 and all permits.
/// Only the composed logical plan uses the outer workspace generation.
struct CompositeFetcher<W> {
    store: Arc<W>,
    guard: HeadGuard,
    layers: [LayerRecord; 2],
    generation: ReadGeneration,
    lower: Arc<WorkspacePackedLower>,
    children: Vec<PreparedUnifiedRead>,
    _reader_owner:
        Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
    _permits: Vec<V3OwnedPermit>,
}

#[async_trait]
impl<W: WorkspaceStore + 'static> UnifiedReadSourceFetcher for CompositeFetcher<W> {
    async fn ensure_generation(&self, generation: ReadGeneration) -> anyhow::Result<()> {
        if generation != self.generation {
            return Err(ReadViewChanged.into());
        }
        self.lower
            .authority
            .validate(&self.guard, &self.lower.binding)
            .await
            .map_err(read_workspace_error)?;
        self.store
            .validate_read_fence(self.guard.clone(), self.layers.clone())
            .await
            .map_err(read_workspace_error)?;
        for child in &self.children {
            child
                .fetcher
                .ensure_generation(child.plan.generation)
                .await?;
        }
        Ok(())
    }

    async fn read_source(&self, source: &ReadSource, output: &mut [u8]) -> anyhow::Result<()> {
        match source {
            ReadSource::Hole | ReadSource::UpperBlock { .. } | ReadSource::LegacySlice { .. } => {
                self.lower.upper.read_source(source, output).await
            }
            ReadSource::PackedFrame { .. } | ReadSource::PackedInline { .. } => {
                // Dispatch only a source in an actually retained authenticated
                // child. Never reconstruct a recipe from container offsets.
                let child = self
                    .children
                    .iter()
                    .find(|child| {
                        child.plan.segments.iter().any(|segment| {
                            &segment.source == source && segment.length == output.len() as u64
                        })
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("packed source has no retained authenticated owner")
                    })?;
                child
                    .fetcher
                    .ensure_generation(child.plan.generation)
                    .await?;
                child.fetcher.read_source(source, output).await
            }
        }
    }
}

pub(super) async fn prepare<W: WorkspaceStore + 'static>(
    meta: &WorkspaceMetaLayer<W>,
    lower: Arc<WorkspacePackedLower>,
    ino: i64,
    chunk_index: u64,
    offset: u64,
    len: u64,
    delivery: Option<Arc<OperationDelivery>>,
) -> Result<PreparedUnifiedRead, MetaError> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| MetaError::Internal("workspace read range overflows".into()))?;
    if end > meta.chunk_size || len > lower.budget.max_read_bytes() as u64 {
        return Err(MetaError::Internal(
            "workspace prepared range exceeds mount limit".into(),
        ));
    }
    let base = chunk_index
        .checked_mul(meta.chunk_size)
        .ok_or_else(|| MetaError::Internal("workspace chunk base overflows".into()))?;
    let (guard, layers, reader_owner) = capture_read_fence(meta, &lower)
        .await
        .map_err(fetch_to_meta)?;
    // Reserve all bounded upper preparation/bookkeeping before backend query.
    let mut permits = vec![
        lower
            .budget
            .admit(&[
                (V3BudgetPool::Plans, UPPER_PREPARATION_BYTES),
                (V3BudgetPool::Control, 4096),
            ])
            .map_err(budget_to_meta)?,
    ];
    let inodes = meta
        .store
        .get_inode_deltas(InodeQuery {
            layer_ids: layers.iter().map(|layer| layer.layer_id).collect(),
            ino,
        })
        .await
        .map_err(read_workspace_to_meta)?;
    let inode_state = resolve_inode_state(&layers, &inodes, ino).map_err(read_workspace_to_meta)?;
    let mut lower_attr = None;
    let visible_size = match inode_state {
        Resolution::Present(inode) => {
            if inode.inode.kind != 0 {
                return Err(MetaError::NotSupported(
                    "prepared read requires a regular upper file".into(),
                ));
            }
            inode.inode.size
        }
        Resolution::Masked => return Err(MetaError::NotFound(ino)),
        Resolution::Absent => {
            let attr = lower
                .metadata
                .stat_fresh(ino)
                .await?
                .ok_or(MetaError::NotFound(ino))?;
            if attr.kind != FileType::File {
                return Err(MetaError::NotSupported(
                    "prepared read requires a regular lower file".into(),
                ));
            }
            let size = attr.size;
            lower_attr = Some(attr);
            size
        }
    };
    let logical_size = visible_size.saturating_sub(base).min(meta.chunk_size);
    if end > logical_size {
        return Err(MetaError::Anyhow(
            crate::chunk::read_plan::ReadRequestBeyondView.into(),
        ));
    }
    let rows = meta
        .store
        .get_extent_deltas_bounded(
            ExtentQuery {
                layer_ids: layers.iter().map(|layer| layer.layer_id).collect(),
                ino,
                chunk_index,
                range_start: offset,
                range_end: end,
            },
            MAX_UPPER_EXTENT_ROWS,
        )
        .await
        .map_err(read_workspace_to_meta)?;
    let coverage = resolve_extent_coverage(&layers, &rows, ino, chunk_index, offset..end)
        .map_err(read_workspace_to_meta)?;
    let upper = coverage.covered.into_iter().map(|extent| LogicalSegment {
        logical_offset: extent.logical_offset,
        length: extent.length,
        source: match extent.kind {
            ExtentKind::Hole => ReadSource::Hole,
            ExtentKind::Data {
                slice_id,
                slice_offset,
            } => ReadSource::LegacySlice {
                slice_id,
                slice_offset,
            },
        },
    });
    let generation = ReadGeneration {
        workspace_head_epoch: guard.expected_head_epoch,
        workspace_mutation_sequence: layers[0].next_sequence,
        lower_snapshot: lower.binding.manifest.digest,
    };
    let preparation = prepare_overlay_plan(generation, logical_size, offset, len, upper)
        .map_err(|error| MetaError::Internal(error.to_string()))?;
    let mut children = Vec::new();
    let mut gap_plans = Vec::new();
    if !preparation.lower_gaps().is_empty() {
        // No lower metadata query whatsoever for complete upper Data/Hole
        // coverage with a present upper inode. Authenticate EOF/absence once,
        // then prepare only the exact gaps that intersect that lower inode.
        if lower_attr.is_none() {
            lower_attr = lower.metadata.stat_fresh(ino).await?;
        }
        if lower_attr
            .as_ref()
            .is_some_and(|attr| attr.kind != FileType::File)
        {
            return Err(MetaError::Internal(
                "packed lower file type disagrees with upper file".into(),
            ));
        }
        let lower_size = lower_attr.as_ref().map_or(0, |attr| {
            attr.size.saturating_sub(base).min(meta.chunk_size)
        });
        for gap in preparation.lower_gaps() {
            let mut gap_plan = UnifiedReadPlan {
                generation,
                logical_size,
                segments: Vec::new(),
            };
            let lower_end = gap.end.min(lower_size);
            if gap.start < lower_end {
                let child = lower
                    .metadata
                    .prepare_unified_read_observed(
                        ino,
                        chunk_index,
                        gap.start,
                        lower_end - gap.start,
                        delivery.clone(),
                    )
                    .await?
                    .ok_or_else(|| {
                        MetaError::Internal("packed lower supplied no prepared gap".into())
                    })?;
                if child.plan.generation != ReadGeneration::readonly(lower.binding.manifest.digest)
                    || child.plan.segments.iter().any(|segment| {
                        !matches!(
                            segment.source,
                            ReadSource::Hole
                                | ReadSource::PackedFrame { .. }
                                | ReadSource::PackedInline { .. }
                        )
                    })
                {
                    return Err(MetaError::Internal(
                        "packed child identity/source mismatch".into(),
                    ));
                }
                child
                    .plan
                    .validate(gap.start, lower_end - gap.start)
                    .map_err(|error| MetaError::Internal(error.to_string()))?;
                child
                    .fetcher
                    .ensure_generation(child.plan.generation)
                    .await
                    .map_err(fetch_to_meta)?;
                let clone_bytes = (child.plan.segments.len() as u64)
                    .checked_mul(2 * std::mem::size_of::<LogicalSegment>() as u64)
                    .ok_or_else(|| MetaError::Internal("composed plan charge overflows".into()))?;
                permits.push(
                    lower
                        .budget
                        .admit(&[(V3BudgetPool::Plans, clone_bytes)])
                        .map_err(budget_to_meta)?,
                );
                gap_plan.segments = child.plan.segments.clone();
                // Keep original child generation/owners intact. The clone is
                // lifted only after authenticated binding/fence validation.
                children.push(child);
            }
            gap_plans.push(gap_plan);
        }
    }
    let plan = preparation
        .finish(gap_plans)
        .map_err(|error| MetaError::Internal(error.to_string()))?;
    let fetcher = Arc::new(CompositeFetcher {
        store: meta.store.clone(),
        guard,
        layers,
        generation,
        lower,
        children,
        _reader_owner: reader_owner,
        _permits: permits,
    });
    fetcher
        .ensure_generation(generation)
        .await
        .map_err(fetch_to_meta)?;
    Ok(PreparedUnifiedRead { plan, fetcher })
}

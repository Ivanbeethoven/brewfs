//! Physical dependency audit, deliberately separate from publication authority.
//! Namespace joins, parent-edge claims, frame codecs, and external logical
//! digests still need verification before constructing a catalog proof.

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::cadapter::client::{ObjectBackend, ObjectClient};
use crate::workspace_overlay::packed_v3::wire::{PackedResult, PackedWireError, Reader};

use super::super::{
    AuthenticatedV3Snapshot, V3_FOOTER_LEN, V3_HEADER_LEN, V3BudgetPool, V3ColdAttributes,
    V3FrameDirectoryPage, V3GroupRef, V3IndexPage, V3IndexRecord, V3IndexValue, V3InodeLocation,
    V3LargeExtent, V3MountBudget, V3ObjectKind, V3ObjectRef, V3OwnedPermit, V3Placement,
    observer_backend_error, observer_validation_error, page_read_class,
};
use super::inventory::{InventoryLimits, PhysicalInventory};
use super::payload::{PayloadLimits, authenticate_payload};

#[derive(Clone, Copy, Debug)]
pub struct V3PhysicalAuditLimits {
    pub max_objects: u64,
    pub max_authenticated_bytes: u64,
    pub max_requested_bytes: u64,
    pub max_edges: u64,
    pub max_leaf_records: u64,
    pub max_disk_bytes: u64,
    pub sqlite_cache_bytes: u64,
    pub chunk_bytes: usize,
}

impl Default for V3PhysicalAuditLimits {
    fn default() -> Self {
        Self {
            max_objects: 1_000_000,
            max_authenticated_bytes: 1 << 40,
            max_requested_bytes: 1 << 40,
            max_edges: 4_000_000,
            max_leaf_records: 16_000_000,
            max_disk_bytes: 512 << 20,
            sqlite_cache_bytes: 256 << 10,
            chunk_bytes: 1 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct V3PhysicalAuditCounts {
    pub objects: u64,
    pub authenticated_bytes: u64,
    pub edges: u64,
    pub leaf_records: u64,
    pub requested_bytes: u64,
}

/// All reachable physical refs were fetched and authenticated. This result
/// cannot construct VerifiedPackedLower and cannot authorize catalog writes.
#[derive(Debug)]
pub struct V3PhysicalDependencyAudit {
    manifest: V3ObjectRef,
    inventory_digest: [u8; 32],
    counts: V3PhysicalAuditCounts,
    _roots: V3OwnedPermit,
}

impl V3PhysicalDependencyAudit {
    pub fn manifest_reference(&self) -> &V3ObjectRef {
        &self.manifest
    }
    pub fn inventory_digest(&self) -> [u8; 32] {
        self.inventory_digest
    }
    pub fn counts(&self) -> V3PhysicalAuditCounts {
        self.counts
    }
}

fn invalid(message: &str) -> PackedWireError {
    PackedWireError::Invalid(message.into())
}

fn live(budget: &V3MountBudget, cancel: &CancellationToken) -> PackedResult<()> {
    if cancel.is_cancelled() {
        return Err(PackedWireError::Backend(
            "physical dependency audit cancelled".into(),
        ));
    }
    if budget.state().closed {
        return Err(PackedWireError::LimitExceeded(
            "physical dependency audit budget closed".into(),
        ));
    }
    Ok(())
}

fn increment(value: &mut u64, amount: u64, limit: u64, label: &str) -> PackedResult<()> {
    *value = value
        .checked_add(amount)
        .filter(|v| *v <= limit)
        .ok_or_else(|| {
            PackedWireError::LimitExceeded(format!("physical dependency audit {label} limit"))
        })?;
    Ok(())
}

/// Enumerate physical refs through a disk-backed pending set, with one decoded
/// metadata page or one streaming payload summary live at a time. A full GET
/// is never used, including for physical-length queries.
pub async fn audit_v3_physical_dependencies<B: ObjectBackend + Clone>(
    client: &ObjectClient<B>,
    manifest: &V3ObjectRef,
    scratch: &Path,
    budget: Arc<V3MountBudget>,
    limits: V3PhysicalAuditLimits,
    cancel: CancellationToken,
) -> PackedResult<V3PhysicalDependencyAudit> {
    live(&budget, &cancel)?;
    if manifest.kind != V3ObjectKind::Manifest {
        return Err(invalid(
            "physical dependency audit requires a manifest reference",
        ));
    }
    if limits.chunk_bytes == 0
        || limits.chunk_bytes > 1 << 20
        || limits.max_edges == 0
        || limits.max_leaf_records == 0
        || limits.max_requested_bytes == 0
    {
        return Err(PackedWireError::LimitExceeded(
            "invalid physical dependency audit limits".into(),
        ));
    }
    let roots = budget.admit(&[(V3BudgetPool::Roots, 8192)])?;
    let mut inventory = PhysicalInventory::create(
        scratch,
        budget.clone(),
        InventoryLimits {
            max_objects: limits.max_objects,
            max_declared_bytes: limits.max_authenticated_bytes,
            max_disk_bytes: limits.max_disk_bytes,
            sqlite_cache_bytes: limits.sqlite_cache_bytes,
        },
        cancel.clone(),
    )
    .await?;
    let result = audit_inner(
        client,
        manifest,
        &budget,
        limits,
        &cancel,
        &mut inventory,
        roots,
    )
    .await;
    // Await real SQLite worker shutdown before returning success or releasing
    // its cache reservation. Drop retains these owners if this await is lost.
    finish_after_close(result, inventory.close(), &budget, &cancel).await
}

pub(super) async fn finish_after_close(
    result: PackedResult<V3PhysicalDependencyAudit>,
    close: impl std::future::Future<Output = PackedResult<()>>,
    budget: &V3MountBudget,
    cancel: &CancellationToken,
) -> PackedResult<V3PhysicalDependencyAudit> {
    close.await?;
    if result.is_ok() {
        live(budget, cancel)?;
    }
    result
}

async fn register(
    inventory: &mut PhysicalInventory,
    reference: &V3ObjectRef,
    counts: &mut V3PhysicalAuditCounts,
    limits: V3PhysicalAuditLimits,
) -> PackedResult<()> {
    increment(&mut counts.edges, 1, limits.max_edges, "edge")?;
    inventory.register(reference).await?;
    Ok(())
}

async fn audit_inner<B: ObjectBackend + Clone>(
    client: &ObjectClient<B>,
    manifest: &V3ObjectRef,
    budget: &Arc<V3MountBudget>,
    limits: V3PhysicalAuditLimits,
    cancel: &CancellationToken,
    inventory: &mut PhysicalInventory,
    roots: V3OwnedPermit,
) -> PackedResult<V3PhysicalDependencyAudit> {
    let mut counts = V3PhysicalAuditCounts::default();
    register(inventory, manifest, &mut counts, limits).await?;
    while let Some(reference) = inventory.next_pending().await? {
        live(budget, cancel)?;
        increment(
            &mut counts.requested_bytes,
            reference.object_len,
            limits.max_requested_bytes,
            "readback bytes",
        )?;
        if matches!(
            reference.kind,
            V3ObjectKind::GroupContainer | V3ObjectKind::LargeData
        ) {
            // Ordinal belongs to the later ContainerIndex/group relation join;
            // it is not encoded in the GC body or a physical object identity.
            let payload = authenticate_payload(
                client,
                &reference,
                0,
                budget,
                PayloadLimits {
                    chunk_bytes: limits.chunk_bytes,
                    ..PayloadLimits::default()
                },
                cancel,
            )
            .await?;
            drop(payload);
        } else {
            // The worst decoded bounded page, wire buffer, pending row and
            // per-record copies fit this reservation independently of depth.
            let _metadata = budget.admit(&[(V3BudgetPool::Metadata, 3 << 20)])?;
            let validation = client.begin_validation(page_read_class(reference.kind)?);
            let result = async {
                let bytes =
                    read_metadata(client, &reference, budget, limits.chunk_bytes, cancel).await?;
                inspect_metadata(inventory, &reference, &bytes, &mut counts, limits).await
            }
            .await;
            if let Some(validation) = validation {
                match &result {
                    Ok(_) => validation.succeed(),
                    Err(_) if cancel.is_cancelled() => drop(validation),
                    Err(error) => validation.fail(observer_validation_error(error.clone()).0),
                }
            }
            result?;
        }
        live(budget, cancel)?;
        inventory.mark_authenticated(&reference).await?;
    }
    let digest = inventory.final_digest().await?;
    let stats = inventory.stats();
    counts.objects = stats.authenticated_objects;
    counts.authenticated_bytes = stats.authenticated_bytes;
    live(budget, cancel)?;
    Ok(V3PhysicalDependencyAudit {
        manifest: manifest.clone(),
        inventory_digest: digest,
        counts,
        _roots: roots,
    })
}

pub(super) async fn read_metadata<B: ObjectBackend + Clone>(
    client: &ObjectClient<B>,
    reference: &V3ObjectRef,
    budget: &Arc<V3MountBudget>,
    chunk_bytes: usize,
    cancel: &CancellationToken,
) -> PackedResult<Vec<u8>> {
    let body_limit = if reference.kind == V3ObjectKind::Manifest {
        64 << 10
    } else {
        256 << 10
    };
    let maximum = (V3_HEADER_LEN + body_limit + V3_FOOTER_LEN) as u64;
    reference.encode_value()?;
    if reference.object_len > maximum {
        return Err(PackedWireError::LimitExceeded(
            "physical metadata page exceeds decode bound".into(),
        ));
    }
    let class = page_read_class(reference.kind)?;
    let _stored = budget.admit(&[(V3BudgetPool::Stored, (2 * chunk_bytes) as u64)])?;
    async {
        let size = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(PackedWireError::Backend("physical metadata read cancelled".into())),
            _ = budget.wait_closed() => return Err(PackedWireError::LimitExceeded("physical metadata budget closed".into())),
            size = client.typed_object_size(class, &reference.key) => size.map_err(observer_backend_error)?,
        };
        if size != Some(reference.object_len) {
            return Err(invalid("physical metadata length differs from authenticated reference"));
        }
        let mut bytes = Vec::with_capacity(reference.object_len as usize);
        while (bytes.len() as u64) < reference.object_len {
            live(budget, cancel)?;
            let length = (reference.object_len - bytes.len() as u64).min(chunk_bytes as u64);
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(PackedWireError::Backend("physical metadata read cancelled".into())),
                _ = budget.wait_closed() => return Err(PackedWireError::LimitExceeded("physical metadata budget closed".into())),
                chunk = client.typed_exact(class, &reference.key, bytes.len() as u64, length, chunk_bytes as u64, Ok) => chunk.map_err(observer_backend_error)?,
            };
            bytes.extend_from_slice(&chunk);
        }
        reference.verify(&bytes, body_limit)?;
        let size = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(PackedWireError::Backend("physical metadata read cancelled".into())),
            _ = budget.wait_closed() => return Err(PackedWireError::LimitExceeded("physical metadata budget closed".into())),
            size = client.typed_object_size(class, &reference.key) => size.map_err(observer_backend_error)?,
        };
        if size != Some(reference.object_len) {
            return Err(invalid("physical metadata length changed during authentication"));
        }
        live(budget, cancel)?;
        Ok(bytes)
    }.await
}

async fn inspect_metadata(
    inventory: &mut PhysicalInventory,
    reference: &V3ObjectRef,
    bytes: &[u8],
    counts: &mut V3PhysicalAuditCounts,
    limits: V3PhysicalAuditLimits,
) -> PackedResult<()> {
    match reference.kind {
        V3ObjectKind::Manifest => {
            let snapshot = AuthenticatedV3Snapshot::decode(reference, bytes)?;
            let manifest = snapshot.manifest();
            for root in &manifest.roots {
                register(inventory, root, counts, limits).await?;
            }
            if let Some(source) = &manifest.source {
                register(inventory, &source.allocations, counts, limits).await?;
            }
        }
        V3ObjectKind::ColdAttributes => {
            let body = reference.verify(bytes, 256 << 10)?;
            let mut r = Reader::new(body);
            r.take(4)?;
            V3ColdAttributes::decode(reference, bytes, r.u64()?)?;
        }
        V3ObjectKind::FrameDirectory => {
            V3FrameDirectoryPage::decode(reference, bytes)?;
        }
        _ => {
            let page = V3IndexPage::decode(reference, bytes)?;
            for record in &page.records {
                match &record.value {
                    V3IndexValue::Child { reference, .. } => {
                        register(inventory, reference, counts, limits).await?
                    }
                    V3IndexValue::Leaf(value) => {
                        increment(&mut counts.leaf_records, 1, limits.max_leaf_records, "leaf")?;
                        inspect_leaf(inventory, page.kind, record, value, counts, limits).await?;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn inspect_leaf(
    inventory: &mut PhysicalInventory,
    kind: V3ObjectKind,
    record: &V3IndexRecord,
    value: &[u8],
    counts: &mut V3PhysicalAuditCounts,
    limits: V3PhysicalAuditLimits,
) -> PackedResult<()> {
    match kind {
        V3ObjectKind::ContainerIndex | V3ObjectKind::FrameIndex | V3ObjectKind::ColdIndex => {
            let reference = V3ObjectRef::decode_value(value)?;
            let right_kind = match kind {
                V3ObjectKind::ContainerIndex => matches!(
                    reference.kind,
                    V3ObjectKind::GroupContainer | V3ObjectKind::LargeData
                ),
                V3ObjectKind::FrameIndex => reference.kind == V3ObjectKind::FrameDirectory,
                _ => reference.kind == V3ObjectKind::ColdAttributes,
            };
            if !right_kind {
                return Err(invalid("physical index leaf object has the wrong kind"));
            }
            register(inventory, &reference, counts, limits).await?;
        }
        V3ObjectKind::GroupIndex => {
            V3GroupRef::decode_value(value)?;
        }
        V3ObjectKind::InodeIndex | V3ObjectKind::ReverseIndex => {
            V3InodeLocation::decode_value(value)?;
        }
        V3ObjectKind::SourceStatsIndex => {
            let inode = u64::from_be_bytes(
                record
                    .first_key
                    .as_slice()
                    .try_into()
                    .map_err(|_| invalid("physical SI05 leaf key length"))?,
            );
            super::super::source_stat::decode_allocation(value, inode)?;
        }
        V3ObjectKind::LargeIndex => {
            let mut r = Reader::new(value);
            match r.take(4)? {
                b"PS09" => {
                    let inode = r.u64()?;
                    let size = r.u64()?;
                    if let V3Placement::External { extents, .. } =
                        V3Placement::decode(value, inode, size)?
                    {
                        register(inventory, &extents, counts, limits).await?;
                    }
                }
                b"LE09" => {
                    V3LargeExtent::decode_record(record, r.u64()?, i64::MAX as u64)?;
                }
                _ => {
                    return Err(PackedWireError::UnsupportedFormat(
                        "physical large-index leaf version".into(),
                    ));
                }
            }
        }
        _ => return Err(invalid("unexpected physical index leaf kind")),
    }
    Ok(())
}

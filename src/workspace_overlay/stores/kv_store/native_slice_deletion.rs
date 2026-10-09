//! Durable SID birth fence for the existing small-catalog native collector.
//! Root birth checks against Deleting ancestry are a required companion.
use super::super::kv_backend::KvReadLimits;
use super::*;
use crate::workspace_overlay::packed_v3::wire005::{V3BudgetPool, V3MountBudget, V3OwnedPermit};
use uuid::Uuid;

pub(super) const EXTENT_GENERATION_KEY: &[u8] = b"packed/v3/native-extent-generation";
const EXTENT_PREFIX: &[u8] = b"delta/extent/";
const DELETE_PREFIX: &str = "packed/v3/native-slice-delete/";
const MAX_TARGETS: usize = 120;
const MAX_SCAN_ROWS: usize = 4096;
const MAX_SCAN_BYTES: usize = 32 << 20;
const MAX_RESERVATION_BYTES: usize = 48 << 10;
const GC_PACKET_ITEMS: usize = 256;
const GC_PACKET_BYTES: usize = 256 << 10;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SliceDeletionReservation {
    slice: u64,
    end: u64,
    run: Uuid,
    volume: Uuid,
    header: Vec<u8>,
    inventory: Option<Vec<u8>>,
    // Exact Deleting bytes and canonical reverse build bytes identify the target.
    // Historical inventory is evidence only; each attempt checks current inventory.
    targets: Vec<(LayerId, Vec<u8>, Vec<u8>)>,
}

pub(super) fn slice_deletion_key(slice: u64) -> Vec<u8> {
    format!("{DELETE_PREFIX}{slice:016x}").into_bytes()
}

fn error(message: &str) -> WorkspaceError {
    WorkspaceError::CorruptMetadata(message.into())
}

fn extent_generation(raw: &Option<Vec<u8>>) -> Result<u64, WorkspaceError> {
    match raw {
        None => Ok(0),
        Some(raw) => {
            if raw.len() != ENVELOPE_MAGIC.len() + 8 {
                return Err(error("invalid native extent generation envelope"));
            }
            let generation = decode_open_value(raw, 16)?;
            if generation == 0 {
                return Err(error("zero persisted native extent generation"));
            }
            Ok(generation)
        }
    }
}

fn point_limits(records: usize) -> KvReadLimits {
    KvReadLimits {
        max_records: records,
        max_key_bytes: 1024,
        max_value_bytes: MAX_RESERVATION_BYTES,
        max_total_bytes: 112 << 10,
        max_response_bytes: 128 << 10,
        max_data_requests: records,
    }
}

fn merge(checks: &mut Vec<KvCheck>, next: KvCheck) -> Result<(), WorkspaceError> {
    if let Some(prior) = checks.iter().find(|prior| prior.key == next.key) {
        if prior.expected != next.expected {
            return Err(WorkspaceError::Busy);
        }
    } else {
        checks.push(next);
    }
    Ok(())
}

fn packet_bytes(checks: &[KvCheck], writes: &[KvWrite]) -> Result<usize, WorkspaceError> {
    let mut bytes = 0usize;
    for check in checks {
        bytes = bytes
            .checked_add(check.key.len())
            .and_then(|bytes| bytes.checked_add(check.expected.as_ref().map_or(0, Vec::len)))
            .ok_or(WorkspaceError::Busy)?;
    }
    for write in writes {
        let (key, value) = match write {
            KvWrite::Put { key, value } => (key, value.len()),
            KvWrite::Delete { key } => (key, 0),
        };
        bytes = bytes
            .checked_add(key.len())
            .and_then(|bytes| bytes.checked_add(value))
            .ok_or(WorkspaceError::Busy)?;
    }
    Ok(bytes)
}

impl<B: WorkspaceKvBackend> KvWorkspaceStore<B> {
    /// Mounted callers use their original ledger. Native/admin catalog users
    /// without a mounted ledger share one store-local auxiliary budget.
    pub(super) fn native_auxiliary_budget(&self) -> &Arc<V3MountBudget> {
        self.packed_reader_pin_budget.get().unwrap_or_else(|| {
            self.native_auxiliary_budget
                .get_or_init(V3MountBudget::defaults)
        })
    }

    async fn slice_fence_points(
        &self,
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>, WorkspaceError> {
        if keys.is_empty() || keys.len() > 32 {
            return Err(WorkspaceError::Busy);
        }
        let limits = point_limits(keys.len());
        limits.validate_keys(keys)?;
        let (values, _) = self
            .backend
            .get_many_consistent_with_time_bounded(keys, limits)
            .await?;
        if values.len() != keys.len() {
            return Err(error("short native slice fence point read"));
        }
        let bytes = keys
            .iter()
            .zip(&values)
            .try_fold(0usize, |bytes, (key, value)| {
                let size = value.as_ref().map_or(0, Vec::len);
                if size > limits.max_value_bytes {
                    return Err(WorkspaceError::Busy);
                }
                bytes
                    .checked_add(key.len())
                    .and_then(|bytes| bytes.checked_add(size))
                    .ok_or(WorkspaceError::Busy)
            })?;
        if bytes > limits.max_total_bytes {
            return Err(WorkspaceError::Busy);
        }
        Ok(values)
    }

    /// Prepare after final primary writes and before the genuine final CAS.
    /// The returned owner must live through that CAS, including cancellation.
    /// The caller's existing final packet envelope counts all added entries.
    pub(super) async fn prepare_native_extent_cas(
        &self,
        checks: &mut Vec<KvCheck>,
        writes: &mut Vec<KvWrite>,
    ) -> Result<Option<V3OwnedPermit>, WorkspaceError> {
        let count = writes
            .iter()
            .filter(|write| match write {
                KvWrite::Put { key, .. } | KvWrite::Delete { key } => {
                    key.starts_with(EXTENT_PREFIX)
                }
            })
            .count();
        if count == 0 {
            return Ok(None);
        }
        // Count without allocating keys, decoded rows or SID sets. Include
        // all retained packet bytes, derived tracking and one bounded window.
        let bytes = packet_bytes(checks, writes)?
            .checked_mul(2)
            .and_then(|bytes| {
                count
                    .checked_mul(512)
                    .and_then(|extra| bytes.checked_add(extra))
            })
            .and_then(|bytes| bytes.checked_add(2 << 20))
            .ok_or(WorkspaceError::Busy)?;
        let owner = self
            .native_auxiliary_budget()
            .admit(&[(
                V3BudgetPool::Metadata,
                u64::try_from(bytes).map_err(|_| WorkspaceError::Busy)?,
            )])
            .map_err(|error| WorkspaceError::InvalidReadPlan(error.to_string()))?;
        let mut primary_keys = BTreeSet::new();
        let mut slices = BTreeSet::new();
        for write in writes.iter() {
            let key = match write {
                KvWrite::Put { key, .. } | KvWrite::Delete { key } => key,
            };
            if !key.starts_with(EXTENT_PREFIX) {
                continue;
            }
            if !primary_keys.insert(key.as_slice()) {
                return Err(error("duplicate native extent packet key"));
            }
            if let KvWrite::Put { value, .. } = write {
                let row: DataExtentDelta = decode_open_value(value, 512)?;
                row.validate()?;
                if *key != extent_key(&row) {
                    return Err(error("native extent packet key/row disagree"));
                }
                if let ExtentKind::Data { slice_id, .. } = row.kind {
                    slices.insert(slice_id);
                }
            }
        }
        drop(primary_keys);
        let epoch_key = EXTENT_GENERATION_KEY.to_vec();
        let mut epoch_values = self
            .slice_fence_points(std::slice::from_ref(&epoch_key))
            .await?;
        let epoch = epoch_values.remove(0);
        let next = extent_generation(&epoch)?
            .checked_add(1)
            .ok_or(WorkspaceError::Busy)?;
        let mut window = Vec::with_capacity(32);
        for slice in slices {
            window.push(slice_deletion_key(slice));
            if window.len() == 32 {
                self.prepare_slice_absences(checks, &window).await?;
                window.clear();
            }
        }
        if !window.is_empty() {
            self.prepare_slice_absences(checks, &window).await?;
        }
        merge(
            checks,
            KvCheck {
                key: epoch_key.clone(),
                expected: epoch,
            },
        )?;
        if writes.iter().any(|write| match write {
            KvWrite::Put { key, .. } | KvWrite::Delete { key } => *key == epoch_key,
        }) {
            return Err(error("caller supplied native extent generation write"));
        }
        writes.push(put(epoch_key, &next)?);
        Ok(Some(owner))
    }

    async fn prepare_slice_absences(
        &self,
        checks: &mut Vec<KvCheck>,
        keys: &[Vec<u8>],
    ) -> Result<(), WorkspaceError> {
        let values = self.slice_fence_points(keys).await?;
        for (key, value) in keys.iter().zip(values) {
            if value.is_some() {
                return Err(WorkspaceError::Busy);
            }
            merge(
                checks,
                KvCheck {
                    key: key.clone(),
                    expected: None,
                },
            )?;
        }
        Ok(())
    }

    /// Reserve one SID after retaining its actual maximum object end. A
    /// matching permanent row may resume only the original Deleting target
    /// incarnation; GuardedBlocks decides which blocks are still undispatched.
    pub(super) async fn reserve_native_slice_deletion(
        &self,
        slice: u64,
        end: u64,
        deleted_layers: &[LayerId],
    ) -> Result<(), WorkspaceError> {
        self.require_admin_access()?;
        if slice == 0 || end == 0 || deleted_layers.is_empty() || deleted_layers.len() > MAX_TARGETS
        {
            return Err(WorkspaceError::Busy);
        }
        let _owner = self
            .native_auxiliary_budget()
            .admit(&[(V3BudgetPool::Metadata, 32 << 20)])
            .map_err(|error| WorkspaceError::InvalidReadPlan(error.to_string()))?;
        let targets = deleted_layers.iter().copied().collect::<BTreeSet<_>>();
        if targets.len() != deleted_layers.len() {
            return Err(WorkspaceError::Busy);
        }
        let sid_key = slice_deletion_key(slice);
        let keys = vec![
            VOLUME_HEADER_KEY.to_vec(),
            LAYER_INVENTORY_GENERATION_KEY.to_vec(),
            EXTENT_GENERATION_KEY.to_vec(),
            sid_key.clone(),
        ];
        let values = self.slice_fence_points(&keys).await?;
        let header_raw = values[0].as_ref().ok_or(WorkspaceError::Fenced)?;
        let header: VolumeHeader = decode_open_value(header_raw, OPEN_RECORD_MAX_BYTES)?;
        if header.volume_id.is_nil()
            || header.volume_format != VOLUME_FORMAT
            || header.schema_version != WORKSPACE_SCHEMA_VERSION
        {
            return Err(WorkspaceError::Fenced);
        }
        layer_inventory_generation(&values[1])?;
        extent_generation(&values[2])?;
        let old = values[3]
            .as_deref()
            .map(|raw| decode_open_value::<SliceDeletionReservation>(raw, MAX_RESERVATION_BYTES))
            .transpose()?;
        if let Some(old) = &old {
            if encode(old)?.as_slice() != values[3].as_deref().unwrap() {
                return Err(error("noncanonical native slice deletion reservation"));
            }
            layer_inventory_generation(&old.inventory)?;
        }
        let mut target_rows = Vec::with_capacity(targets.len());
        let mut checks = keys
            .into_iter()
            .zip(values.iter().cloned())
            .map(|(key, expected)| KvCheck { key, expected })
            .collect::<Vec<_>>();
        let target_ids = targets.iter().copied().collect::<Vec<_>>();
        for ids in target_ids.chunks(16) {
            let keys = ids
                .iter()
                .copied()
                .flat_map(|id| [hot_layer_key(id), native_reverse::state_key(id)])
                .collect::<Vec<_>>();
            let rows = self.slice_fence_points(&keys).await?;
            for (index, id) in ids.iter().enumerate() {
                let raw = rows[index * 2].as_ref().ok_or(WorkspaceError::Busy)?;
                let reverse = rows[index * 2 + 1].as_ref().ok_or(WorkspaceError::Busy)?;
                let layer: LayerRecord = decode_open_value(raw, OPEN_RECORD_MAX_BYTES)?;
                if layer.layer_id != *id
                    || layer.state != LayerState::Deleting
                    || layer.schema_version != WORKSPACE_SCHEMA_VERSION
                    || layer.next_sequence == 0
                    || encode(&layer)? != *raw
                {
                    return Err(WorkspaceError::Busy);
                }
                // Runtime never fabricates reverse completeness or an identity
                // for old Deleting layers. Only the explicit admin initializer
                // may install a missing Building state under exact authority.
                native_reverse::gc_incarnation(reverse, *id)?;
                target_rows.push((*id, raw.clone(), reverse.clone()));
                for offset in 0..2 {
                    merge(
                        &mut checks,
                        KvCheck {
                            key: keys[index * 2 + offset].clone(),
                            expected: rows[index * 2 + offset].clone(),
                        },
                    )?;
                }
            }
        }
        if let Some(old) = &old
            && (old.slice != slice
                || old.end == 0
                || old.run.is_nil()
                || old.volume != header.volume_id
                || old.header != *header_raw
                || old.targets != target_rows)
        {
            return Err(WorkspaceError::Busy);
        }
        // Retain the current root/topology authority in this exact reservation
        // CAS. Companion anti-birth checks forbid roots reconnecting Deleting
        // ancestry after this CAS, both on initial reserve and on restart.
        let (packed_roots, packed_checks, _packed_owners) =
            self.scan_packed_binding_roots().await?;
        for next in packed_checks {
            merge(&mut checks, next)?;
        }
        let (control, state, hot) = self.load_control_raw().await?;
        let mut reachable = reachable_layers(&state, i64::MIN);
        reachable.extend(reachable_from_roots(&state, packed_roots));
        if targets.iter().any(|target| reachable.contains(target)) {
            return Err(WorkspaceError::Busy);
        }
        merge(
            &mut checks,
            KvCheck {
                key: CONTROL_KEY.to_vec(),
                expected: control,
            },
        )?;
        for (key, expected) in hot {
            merge(&mut checks, KvCheck { key, expected })?;
        }
        // Scan the complete extent family. Never ignore an unselected layer,
        // even if it currently appears unreachable. The durable epoch closes
        // phantom insertion, overwrites and deletes between any pages and CAS.
        let mut after = None;
        let mut rows = 0usize;
        let mut bytes = 0usize;
        loop {
            if self.native_auxiliary_budget().state().closed {
                return Err(WorkspaceError::Busy);
            }
            let page = self
                .backend
                .scan_prefix_page_with_byte_limits(
                    EXTENT_PREFIX,
                    after.as_deref(),
                    KvReadLimits {
                        max_records: 4,
                        max_key_bytes: 1024,
                        max_value_bytes: 512,
                        max_total_bytes: 8 << 10,
                        max_response_bytes: 16 << 10,
                        max_data_requests: 4,
                    },
                )
                .await?;
            if page.len() > 4 {
                return Err(WorkspaceError::Busy);
            }
            if page.is_empty() {
                break;
            }
            for entry in &page {
                if !entry.key.starts_with(EXTENT_PREFIX)
                    || entry.key.len() > 1024
                    || entry.value.len() > 512
                    || after.as_ref().is_some_and(|key| entry.key <= *key)
                {
                    return Err(WorkspaceError::Fenced);
                }
                rows = rows.checked_add(1).ok_or(WorkspaceError::Busy)?;
                bytes = bytes
                    .checked_add(entry.key.len())
                    .and_then(|bytes| bytes.checked_add(entry.value.len()))
                    .ok_or(WorkspaceError::Busy)?;
                if rows > MAX_SCAN_ROWS || bytes > MAX_SCAN_BYTES {
                    return Err(WorkspaceError::Busy);
                }
                let row: DataExtentDelta = decode_open_value(&entry.value, 512)?;
                row.validate()?;
                if entry.key != extent_key(&row) {
                    return Err(error("GC extent key/row disagree"));
                }
                if let ExtentKind::Data {
                    slice_id,
                    slice_offset,
                } = row.kind
                    && slice_id == slice
                {
                    if !targets.contains(&row.layer_id) {
                        return Err(WorkspaceError::Busy);
                    }
                    if slice_offset
                        .checked_add(row.length)
                        .ok_or(WorkspaceError::Busy)?
                        > end.max(old.as_ref().map_or(0, |row| row.end))
                    {
                        return Err(WorkspaceError::Busy);
                    }
                }
                after = Some(entry.key.clone());
            }
        }
        let reservation = SliceDeletionReservation {
            slice,
            end: end.max(old.as_ref().map_or(0, |row| row.end)),
            run: old.as_ref().map_or_else(Uuid::new_v4, |row| row.run),
            volume: header.volume_id,
            header: header_raw.clone(),
            inventory: old
                .as_ref()
                .map_or_else(|| values[1].clone(), |row| row.inventory.clone()),
            targets: target_rows,
        };
        let value = encode(&reservation)?;
        if value.len() > MAX_RESERVATION_BYTES {
            return Err(WorkspaceError::Busy);
        }
        let writes = if old.as_ref() == Some(&reservation) {
            Vec::new()
        } else {
            vec![KvWrite::Put {
                key: sid_key,
                value,
            }]
        };
        if checks
            .len()
            .checked_add(writes.len())
            .is_none_or(|items| items > GC_PACKET_ITEMS)
            || packet_bytes(&checks, &writes)? > GC_PACKET_BYTES
        {
            return Err(WorkspaceError::Busy);
        }
        // Uncertain replies propagate unchanged: never resend the reservation
        // or physical DELETE in this call. A later run authenticates persisted
        // identity and GuardedBlocks preserves Dispatched quarantine.
        if !self.backend.compare_and_swap(&checks, &writes).await? {
            return Err(WorkspaceError::Busy);
        }
        Ok(())
    }
}

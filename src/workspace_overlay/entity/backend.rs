//! Bounded, atomic current-row transactions. There is no workspace head CAS.

use std::collections::BTreeMap;

use async_trait::async_trait;

use super::{EntityError, Result, codec::MAX_ROW_BYTES};

pub use super::memory::MemoryEntityBackend;

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;

pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_TRANSACTION_ROWS: usize = 4096;
pub const MAX_TRANSACTION_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowCheck {
    pub key: Vec<u8>,
    pub expected: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowWrite {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

impl RowWrite {
    pub fn key(&self) -> &[u8] {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityPlan {
    pub checks: Vec<RowCheck>,
    pub writes: Vec<RowWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowSnapshot {
    pub now_ns: i64,
    /// Positional results, including absent rows, in the requested key order.
    pub rows: Vec<Option<Vec<u8>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed,
    Conflict,
}

/// Created by lifecycle validation from exact canonical descriptor/lease rows.
/// Callers outside this module tree cannot invent a deadline or authority.
#[derive(Clone, Debug)]
pub struct TransactionAuthority {
    pub(super) checks: Vec<RowCheck>,
    pub(super) not_before_ns: Option<i64>,
    pub(super) expires_before_ns: Option<i64>,
}

impl TransactionAuthority {
    pub(super) fn from_validated(
        checks: Vec<RowCheck>,
        not_before_ns: Option<i64>,
        expires_before_ns: Option<i64>,
    ) -> Result<Self> {
        if checks.is_empty()
            || not_before_ns.is_some_and(|value| value < 0)
            || expires_before_ns.is_some_and(|value| value < 0)
            || matches!((not_before_ns, expires_before_ns), (Some(start), Some(end)) if start >= end)
        {
            return Err(EntityError::Invalid("invalid transaction authority".into()));
        }
        validate_checks(&checks)?;
        Ok(Self {
            checks,
            not_before_ns,
            expires_before_ns,
        })
    }

    pub(super) fn allows_time(&self, now_ns: i64) -> bool {
        now_ns >= 0
            && self.not_before_ns.is_none_or(|start| now_ns >= start)
            && self.expires_before_ns.is_none_or(|end| now_ns < end)
    }
}

#[async_trait]
pub trait EntityBackend: Send + Sync {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot>;
    /// Exact authority-row changes (including heartbeat renewal) are conflicts.
    /// The caller must obtain fresh lifecycle authority before retrying. Backend
    /// time outside the authorized interval is fenced, never silently extended.
    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome>;
    /// Increment an already initialized canonical decimal counter. Never replays
    /// an uncertain result; allocated IDs may have gaps and must never be reused.
    async fn allocate(&self, authority: &TransactionAuthority, counter_key: &[u8]) -> Result<u64>;
    /// Bounded prefix observation. A caller using this to decide a mutation must
    /// check its parent row, read before the scan, in the resulting transaction.
    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

pub(super) fn validate_key(key: &[u8]) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(EntityError::Invalid("entity key exceeds size bound".into()));
    }
    volume_prefix(key)?;
    Ok(())
}

fn volume_prefix(key: &[u8]) -> Result<&[u8]> {
    const START: &[u8] = b"{brewfs-entity:";
    let invalid = || EntityError::Invalid("invalid entity volume domain".into());
    let tail = key.strip_prefix(START).ok_or_else(invalid)?;
    let uuid_text =
        std::str::from_utf8(tail.get(..36).ok_or_else(invalid)?).map_err(|_| invalid())?;
    let volume = uuid::Uuid::parse_str(uuid_text).map_err(|_| invalid())?;
    if uuid_text != volume.hyphenated().to_string() || tail.get(36..38) != Some(b"}/") {
        return Err(invalid());
    }
    Ok(&key[..START.len() + 38])
}

pub(super) fn validate_same_volume<'a>(keys: impl IntoIterator<Item = &'a [u8]>) -> Result<()> {
    let mut domain = None;
    for key in keys {
        let current = volume_prefix(key)?;
        if domain.is_some_and(|previous| previous != current) {
            return Err(EntityError::Invalid(
                "transaction crosses entity volume domains".into(),
            ));
        }
        domain = Some(current);
    }
    Ok(())
}

pub(super) fn add_bytes(total: &mut usize, key: &[u8], value: Option<&[u8]>) -> Result<()> {
    validate_key(key)?;
    let value_len = value.map_or(0, <[u8]>::len);
    if value_len > MAX_ROW_BYTES {
        return Err(EntityError::Invalid("entity row exceeds size bound".into()));
    }
    *total = total
        .checked_add(key.len())
        .and_then(|value| value.checked_add(value_len))
        .ok_or(EntityError::Overflow)?;
    if *total > MAX_TRANSACTION_BYTES {
        return Err(EntityError::Invalid(
            "entity transaction exceeds byte bound".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_keys(keys: &[Vec<u8>]) -> Result<()> {
    if keys.len() > MAX_TRANSACTION_ROWS {
        return Err(EntityError::Invalid(
            "entity transaction exceeds row bound".into(),
        ));
    }
    validate_same_volume(keys.iter().map(Vec::as_slice))?;
    let mut total = 0;
    for key in keys {
        add_bytes(&mut total, key, None)?;
    }
    Ok(())
}

fn validate_checks(checks: &[RowCheck]) -> Result<()> {
    if checks.len() > MAX_TRANSACTION_ROWS {
        return Err(EntityError::Invalid(
            "entity transaction exceeds row bound".into(),
        ));
    }
    validate_same_volume(checks.iter().map(|check| check.key.as_slice()))?;
    let mut total = 0;
    let mut seen = BTreeMap::new();
    for check in checks {
        add_bytes(&mut total, &check.key, check.expected.as_deref())?;
        if seen
            .insert(&check.key, &check.expected)
            .is_some_and(|previous| previous != &check.expected)
        {
            return Err(EntityError::Invalid(
                "contradictory entity predicates".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_plan(authority: &TransactionAuthority, plan: &EntityPlan) -> Result<()> {
    validate_same_volume(
        authority
            .checks
            .iter()
            .chain(&plan.checks)
            .map(|check| check.key.as_slice())
            .chain(plan.writes.iter().map(RowWrite::key)),
    )?;
    let count = authority
        .checks
        .len()
        .checked_add(plan.checks.len())
        .and_then(|value| value.checked_add(plan.writes.len()))
        .ok_or(EntityError::Overflow)?;
    if count > MAX_TRANSACTION_ROWS {
        return Err(EntityError::Invalid(
            "entity transaction exceeds row bound".into(),
        ));
    }
    let mut total = 0;
    let mut checks = BTreeMap::new();
    for check in authority.checks.iter().chain(&plan.checks) {
        add_bytes(&mut total, &check.key, check.expected.as_deref())?;
        if checks
            .insert(&check.key, &check.expected)
            .is_some_and(|previous| previous != &check.expected)
        {
            return Err(EntityError::Invalid(
                "contradictory entity predicates".into(),
            ));
        }
    }
    let mut writes = std::collections::BTreeSet::new();
    for write in &plan.writes {
        if !writes.insert(write.key()) {
            return Err(EntityError::Invalid("duplicate entity write".into()));
        }
        let value = match write {
            RowWrite::Put { value, .. } => Some(value.as_slice()),
            RowWrite::Delete { .. } => None,
        };
        add_bytes(&mut total, write.key(), value)?;
    }
    Ok(())
}

pub(super) fn validate_scan(prefix: &[u8], limit: usize) -> Result<()> {
    validate_key(prefix)?;
    if limit == 0 || limit > MAX_TRANSACTION_ROWS {
        return Err(EntityError::Invalid("invalid entity scan row bound".into()));
    }
    Ok(())
}

pub(super) fn next_counter(bytes: &[u8]) -> Result<u64> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| EntityError::Corrupt("invalid entity counter".into()))?;
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(EntityError::Corrupt("invalid entity counter".into()));
    }
    let value = value
        .parse::<u64>()
        .map_err(|_| EntityError::Corrupt("invalid entity counter".into()))?;
    if value > i64::MAX as u64 {
        return Err(EntityError::Corrupt(
            "entity counter exceeds signed integer range".into(),
        ));
    }
    value
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or(EntityError::Exhausted)
}

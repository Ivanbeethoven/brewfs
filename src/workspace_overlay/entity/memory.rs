//! Atomic in-memory reference backend with a controllable backend clock.

use std::{collections::BTreeMap, sync::Mutex};

use async_trait::async_trait;

use super::{
    EntityError, Result,
    backend::{
        CommitOutcome, EntityBackend, EntityPlan, MAX_TRANSACTION_ROWS, RowCheck, RowSnapshot,
        RowWrite, TransactionAuthority, add_bytes, next_counter, validate_key, validate_keys,
        validate_plan, validate_same_volume, validate_scan,
    },
};

pub struct MemoryEntityBackend {
    state: Mutex<State>,
}

struct State {
    now_ns: i64,
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MemoryEntityBackend {
    pub fn new(now_ns: i64) -> Self {
        Self {
            state: Mutex::new(State {
                now_ns,
                rows: BTreeMap::new(),
            }),
        }
    }

    pub fn set_time(&self, now_ns: i64) -> Result<()> {
        if now_ns < 0 {
            return Err(EntityError::Invalid("negative backend time".into()));
        }
        self.state.lock().map_err(poisoned)?.now_ns = now_ns;
        Ok(())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> EntityError {
    EntityError::Backend("memory entity backend lock poisoned".into())
}

fn matches(rows: &BTreeMap<Vec<u8>, Vec<u8>>, checks: &[RowCheck]) -> bool {
    checks
        .iter()
        .all(|check| rows.get(&check.key) == check.expected.as_ref())
}

#[async_trait]
impl EntityBackend for MemoryEntityBackend {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot> {
        validate_keys(keys)?;
        let state = self.state.lock().map_err(poisoned)?;
        if state.now_ns < 0 {
            return Err(EntityError::Corrupt("negative backend time".into()));
        }
        let mut total = 0;
        for key in keys {
            add_bytes(&mut total, key, state.rows.get(key).map(Vec::as_slice))?;
        }
        Ok(RowSnapshot {
            now_ns: state.now_ns,
            rows: keys
                .iter()
                .map(|key| state.rows.get(key).cloned())
                .collect(),
        })
    }

    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome> {
        validate_plan(authority, plan)?;
        let mut state = self.state.lock().map_err(poisoned)?;
        let mut stored_bytes = 0;
        for key in authority
            .checks
            .iter()
            .chain(&plan.checks)
            .map(|check| check.key.as_slice())
            .chain(plan.writes.iter().map(RowWrite::key))
        {
            add_bytes(
                &mut stored_bytes,
                key,
                state.rows.get(key).map(Vec::as_slice),
            )?;
        }
        if !authority.allows_time(state.now_ns) {
            return Err(EntityError::Fenced);
        }
        if !matches(&state.rows, &authority.checks) || !matches(&state.rows, &plan.checks) {
            return Ok(CommitOutcome::Conflict);
        }
        for write in &plan.writes {
            match write {
                RowWrite::Put { key, value } => {
                    state.rows.insert(key.clone(), value.clone());
                }
                RowWrite::Delete { key } => {
                    state.rows.remove(key);
                }
            }
        }
        Ok(CommitOutcome::Committed)
    }

    async fn allocate(&self, authority: &TransactionAuthority, counter_key: &[u8]) -> Result<u64> {
        validate_key(counter_key)?;
        validate_plan(authority, &EntityPlan::default())?;
        validate_same_volume(
            authority
                .checks
                .iter()
                .map(|check| check.key.as_slice())
                .chain(std::iter::once(counter_key)),
        )?;
        if authority.checks.len() >= MAX_TRANSACTION_ROWS
            || authority
                .checks
                .iter()
                .any(|check| check.key == counter_key)
        {
            return Err(EntityError::Invalid(
                "allocator collides with authority row".into(),
            ));
        }
        let mut state = self.state.lock().map_err(poisoned)?;
        let mut stored_bytes = 0;
        for key in authority
            .checks
            .iter()
            .map(|check| check.key.as_slice())
            .chain(std::iter::once(counter_key))
        {
            add_bytes(
                &mut stored_bytes,
                key,
                state.rows.get(key).map(Vec::as_slice),
            )?;
        }
        if !authority.allows_time(state.now_ns) {
            return Err(EntityError::Fenced);
        }
        if !matches(&state.rows, &authority.checks) {
            return Err(EntityError::Conflict);
        }
        let current = state
            .rows
            .get(counter_key)
            .ok_or_else(|| EntityError::Corrupt("uninitialized entity counter".into()))?;
        let next = next_counter(current)?;
        state
            .rows
            .insert(counter_key.to_vec(), next.to_string().into_bytes());
        Ok(next)
    }

    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        validate_scan(prefix, limit)?;
        let state = self.state.lock().map_err(poisoned)?;
        let entries = state
            .rows
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix));
        let mut result = Vec::new();
        let mut total = 0;
        for (key, value) in entries {
            if result.len() == limit {
                return Err(EntityError::Invalid("entity scan exceeds row bound".into()));
            }
            add_bytes(&mut total, key, Some(value))?;
            result.push((key.clone(), value.clone()));
        }
        Ok(result)
    }
}

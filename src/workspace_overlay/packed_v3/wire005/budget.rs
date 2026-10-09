//! Shared mount accounting. Admission is atomic and never waits while holding
//! another resource. Owned permits outlive cache eviction and FUSE submission.

use crate::workspace_overlay::packed_v3::wire::{PackedResult, PackedWireError};
use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum V3BudgetPool {
    Roots,
    Metadata,
    Plans,
    Control,
    Stored,
    Raw,
    Workspace,
    Output,
}
impl V3BudgetPool {
    pub const ALL: [Self; 8] = [
        Self::Roots,
        Self::Metadata,
        Self::Plans,
        Self::Control,
        Self::Stored,
        Self::Raw,
        Self::Workspace,
        Self::Output,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Roots => "roots",
            Self::Metadata => "metadata_owned",
            Self::Plans => "plans",
            Self::Control => "control",
            Self::Stored => "stored",
            Self::Raw => "raw",
            Self::Workspace => "decode_workspace",
            Self::Output => "output",
        }
    }
}

#[derive(Clone, Debug)]
pub struct V3BudgetLimits {
    pub bytes: [u64; 8],
    pub max_read_bytes: usize,
}
impl Default for V3BudgetLimits {
    fn default() -> Self {
        Self {
            bytes: [
                16 << 20,
                256 << 20,
                32 << 20,
                1 << 20,
                32 << 20,
                32 << 20,
                8 << 20,
                32 << 20,
            ],
            max_read_bytes: 4 << 20,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct V3BudgetState {
    pub used: [u64; 8],
    pub peak: [u64; 8],
    pub rejections: u64,
    pub closed: bool,
}
#[derive(Debug)]
pub struct V3MountBudget {
    limits: V3BudgetLimits,
    state: Mutex<V3BudgetState>,
    changed: tokio::sync::Notify,
    observer: Mutex<Weak<crate::cadapter::read_observer::ReadObserver>>,
}
impl V3MountBudget {
    pub const REPLY_ALLOCATION_ALLOWANCE_BYTES: u64 = 1 << 20;
    pub const STATS_EXTENSION_RENDER_MAX_BYTES: usize = 32768;
    pub fn new(limits: V3BudgetLimits) -> PackedResult<Arc<Self>> {
        if limits.max_read_bytes == 0
            || limits.max_read_bytes > limits.bytes[V3BudgetPool::Output as usize] as usize
            || limits.bytes.contains(&0)
        {
            return Err(PackedWireError::LimitExceeded(
                "invalid mount memory budgets or max_read".into(),
            ));
        }
        Ok(Arc::new(Self {
            limits,
            state: Mutex::new(V3BudgetState::default()),
            changed: tokio::sync::Notify::new(),
            observer: Mutex::new(Weak::new()),
        }))
    }
    pub fn defaults() -> Arc<Self> {
        Self::new(V3BudgetLimits::default()).expect("valid defaults")
    }

    /// Source and candidate readers on one mount share its complete ledger.
    /// The weak slot avoids retaining an observer's permit through the budget
    /// it owns; the last real request/reader releases that ownership normally.
    pub fn read_observer(
        self: &Arc<Self>,
        supplied: Option<Arc<crate::cadapter::read_observer::ReadObserver>>,
    ) -> PackedResult<Arc<crate::cadapter::read_observer::ReadObserver>> {
        if self.state().closed {
            return Err(PackedWireError::LimitExceeded("mount budget closed".into()));
        }
        if supplied
            .as_ref()
            .is_some_and(|observer| !observer.owned_by_budget(Arc::as_ptr(self) as usize))
        {
            return Err(PackedWireError::Invalid(
                "read observer belongs to no mount budget or a different mount".into(),
            ));
        }
        let mut slot = self.observer.lock().unwrap();
        if let Some(observer) = slot.upgrade() {
            if supplied
                .as_ref()
                .is_some_and(|supplied| !Arc::ptr_eq(supplied, &observer))
            {
                return Err(PackedWireError::Invalid(
                    "mount already carries a different read observer".into(),
                ));
            }
            return Ok(observer);
        }
        let observer = match supplied {
            Some(observer) => observer,
            None => {
                Arc::new(crate::cadapter::read_observer::ReadObserver::with_mount_budget(self)?)
            }
        };
        *slot = Arc::downgrade(&observer);
        Ok(observer)
    }
    pub fn from_env() -> PackedResult<Arc<Self>> {
        let mut limits = V3BudgetLimits::default();
        for pool in V3BudgetPool::ALL {
            let key = format!(
                "BREWFS_PACKED_V3_{}_BUDGET_BYTES",
                pool.name().to_ascii_uppercase()
            );
            if let Some(value) = std::env::var_os(&key) {
                limits.bytes[pool as usize] =
                    value
                        .to_str()
                        .and_then(|value| value.parse().ok())
                        .ok_or_else(|| PackedWireError::LimitExceeded(format!("invalid {key}")))?;
            }
        }
        limits.max_read_bytes = (4 << 20).min(limits.bytes[V3BudgetPool::Output as usize] as usize);
        Self::new(limits)
    }
    pub fn max_read_bytes(&self) -> usize {
        self.limits.max_read_bytes
    }
    pub fn capacity(&self, pool: V3BudgetPool) -> u64 {
        self.limits.bytes[pool as usize]
    }
    pub fn render_into(&self, output: &mut dyn std::fmt::Write) {
        let state = self.state();
        for pool in V3BudgetPool::ALL {
            for (suffix, value) in [
                ("capacity_bytes", self.capacity(pool)),
                ("owned_bytes", state.used[pool as usize]),
                ("peak_owned_bytes", state.peak[pool as usize]),
            ] {
                let _ = writeln!(
                    output,
                    "brewfs_packed_v3_budget_{}_{suffix} {value}",
                    pool.name()
                );
            }
        }
        let _ = writeln!(
            output,
            "brewfs_packed_v3_budget_rejections_total {}",
            state.rejections
        );
        let _ = writeln!(
            output,
            "brewfs_packed_v3_budget_closed {}",
            u8::from(state.closed)
        );
    }
    pub fn state(&self) -> V3BudgetState {
        self.state.lock().unwrap().clone()
    }
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.changed.notify_waiters();
    }
    /// Observe shutdown without retaining a memory permit while waiting.
    /// Register before inspecting state so a concurrent close cannot be lost.
    pub(crate) async fn wait_closed(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.state().closed {
                return;
            }
            changed.await;
        }
    }
    pub fn validate_frame_capability(&self, maximum_raw: usize) -> PackedResult<()> {
        let workspace =
            super::super::codec::decode_workspace_bytes(super::super::PackedCodec::Zstd)? as u64;
        let output_required = (crate::cadapter::read_observer::ReadObserver::MAX_RENDER_BYTES
            as u64)
            .checked_add(crate::vfs::stats::FsStats::BASE_RENDER_MAX_BYTES as u64)
            .and_then(|n| n.checked_add(Self::STATS_EXTENSION_RENDER_MAX_BYTES as u64))
            .and_then(|n| n.checked_add(self.max_read_bytes() as u64))
            .and_then(|n| n.checked_add(2 * Self::REPLY_ALLOCATION_ALLOWANCE_BYTES))
            .ok_or_else(|| {
                PackedWireError::LimitExceeded("mount output admission bound overflows".into())
            })?;
        for (pool, required) in [
            (
                V3BudgetPool::Stored,
                super::super::remote::MAX_PACKED_STREAM_RANGE_BYTES * 2,
            ),
            (V3BudgetPool::Raw, maximum_raw as u64),
            (
                V3BudgetPool::Workspace,
                workspace
                    + crate::cadapter::read_observer::SharedRawCoverage::required_tracking_bytes(
                        maximum_raw as u64,
                    )
                    .map_err(|error| PackedWireError::LimitExceeded(error.to_string()))?,
            ),
            // One maximal index path (17 owned pages), request copies and
            // GroupMeta/descriptor temporary state must fit concurrently.
            (V3BudgetPool::Metadata, 44 << 20),
            (V3BudgetPool::Plans, 2 << 20),
            (V3BudgetPool::Control, 32 << 10),
            // One complete diagnostic snapshot must coexist with one normal
            // read and both FUSE reply allocation allowances.
            (V3BudgetPool::Output, output_required),
            (
                V3BudgetPool::Roots,
                (4 << 20)
                    + (256 << 10)
                    + crate::cadapter::read_observer::ReadObserver::MEMORY_BOUND_BYTES
                    + super::pipeline::queue_roots_bytes(
                        super::pipeline::V3PipelineLimits::default(),
                    )
                    + super::pipeline::registry_roots_bytes::<
                        V3Owned<super::pipeline::V3SharedFrame>,
                    >(super::pipeline::V3PipelineLimits::default()),
            ),
        ] {
            if self.limits.bytes[pool as usize] < required {
                return Err(PackedWireError::LimitExceeded(format!(
                    "mount {} budget cannot admit one valid profile frame",
                    pool.name()
                )));
            }
        }
        Ok(())
    }
    pub fn admit(self: &Arc<Self>, charges: &[(V3BudgetPool, u64)]) -> PackedResult<V3OwnedPermit> {
        let mut totals = [0u64; 8];
        for (pool, bytes) in charges {
            totals[*pool as usize] = totals[*pool as usize]
                .checked_add(*bytes)
                .ok_or_else(|| PackedWireError::LimitExceeded("mount admission overflow".into()))?;
        }
        let mut state = self.state.lock().unwrap();
        if state.closed
            || totals.iter().enumerate().any(|(i, bytes)| {
                state.used[i]
                    .checked_add(*bytes)
                    .is_none_or(|n| n > self.limits.bytes[i])
            })
        {
            state.rejections += 1;
            return Err(PackedWireError::LimitExceeded(if state.closed {
                "mount budget closed".into()
            } else {
                let (index, bytes) = totals
                    .iter()
                    .enumerate()
                    .find(|(i, bytes)| {
                        state.used[*i]
                            .checked_add(**bytes)
                            .is_none_or(|n| n > self.limits.bytes[*i])
                    })
                    .expect("failed admission has an exhausted pool");
                format!(
                    "mount {} budget temporarily exhausted: used={}, requested={}, limit={}",
                    V3BudgetPool::ALL[index].name(),
                    state.used[index],
                    bytes,
                    self.limits.bytes[index]
                )
            }));
        }
        for (i, bytes) in totals.iter().enumerate() {
            state.used[i] += bytes;
            state.peak[i] = state.peak[i].max(state.used[i]);
        }
        drop(state);
        Ok(V3OwnedPermit {
            budget: self.clone(),
            charges: totals,
        })
    }
    /// Wait without holding any partially acquired stored/raw/workspace bundle.
    /// A bundle that exceeds capacity is a permanent error, never a wait.
    pub(super) async fn admit_when_available(
        self: &Arc<Self>,
        charges: &[(V3BudgetPool, u64)],
    ) -> PackedResult<V3OwnedPermit> {
        let mut totals = [0u64; 8];
        for (pool, bytes) in charges {
            totals[*pool as usize] = totals[*pool as usize]
                .checked_add(*bytes)
                .ok_or_else(|| PackedWireError::LimitExceeded("mount admission overflow".into()))?;
        }
        if totals
            .iter()
            .enumerate()
            .any(|(i, n)| *n > self.limits.bytes[i])
        {
            return Err(PackedWireError::LimitExceeded(
                "requested bundle exceeds mount capacity".into(),
            ));
        }
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.admit(charges) {
                Ok(permit) => return Ok(permit),
                Err(error) if self.state().closed => return Err(error),
                Err(error @ PackedWireError::LimitExceeded(_)) => {
                    let state = self.state();
                    // Tracking receipts can depend on this complete read's
                    // own terminal event. Never wait for such a self-held
                    // resource; only body/raw consumer releases are waitable.
                    if V3BudgetPool::ALL.iter().any(|pool| {
                        !matches!(pool, V3BudgetPool::Stored | V3BudgetPool::Raw)
                            && state.used[*pool as usize]
                                .checked_add(totals[*pool as usize])
                                .is_none_or(|n| n > self.limits.bytes[*pool as usize])
                    }) {
                        return Err(error);
                    }
                    changed.await;
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn output(self: &Arc<Self>, length: usize) -> PackedResult<V3OwnedPermit> {
        if length > self.max_read_bytes() {
            return Err(PackedWireError::LimitExceeded(
                "read exceeds negotiated max_read".into(),
            ));
        }
        self.admit(&[
            (V3BudgetPool::Output, length as u64),
            (V3BudgetPool::Control, 1024),
        ])
    }
}

#[derive(Debug)]
pub struct V3OwnedPermit {
    budget: Arc<V3MountBudget>,
    charges: [u64; 8],
}
impl V3OwnedPermit {
    /// Transfer ownership without reacquiring a second reservation. A batch
    /// admits atomically, then distributes raw/tracking charges to consumers.
    pub(super) fn split(&mut self, pool: V3BudgetPool, bytes: u64) -> PackedResult<Self> {
        if bytes > self.charges[pool as usize] {
            return Err(PackedWireError::LimitExceeded(
                "permit transfer exceeds reservation".into(),
            ));
        }
        self.charges[pool as usize] -= bytes;
        let mut charges = [0; 8];
        charges[pool as usize] = bytes;
        Ok(Self {
            budget: self.budget.clone(),
            charges,
        })
    }
    /// Reduce a conservative pre-allocation reservation to measured owned
    /// capacity. Increasing after allocation would violate the contract.
    pub fn shrink(&mut self, pool: V3BudgetPool, bytes: u64) -> PackedResult<()> {
        let previous = self.charges[pool as usize];
        if bytes > previous {
            return Err(PackedWireError::LimitExceeded(
                "decoded allocation exceeded reserved bound".into(),
            ));
        }
        self.budget.state.lock().unwrap().used[pool as usize] -= previous - bytes;
        self.charges[pool as usize] = bytes;
        if previous != bytes {
            self.budget.changed.notify_waiters();
        }
        Ok(())
    }
}
impl Drop for V3OwnedPermit {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        for (i, bytes) in self.charges.iter().enumerate() {
            state.used[i] -= bytes;
        }
        drop(state);
        self.budget.changed.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) struct V3Owned<T> {
    value: T,
    _permit: V3OwnedPermit,
}
impl<T> V3Owned<T> {
    pub(super) fn new(value: T, permit: V3OwnedPermit) -> Self {
        Self {
            value,
            _permit: permit,
        }
    }
    pub(super) fn value_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
impl<T> Deref for V3Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

pub struct V3OwnedBytes {
    pub data: Vec<u8>,
    pub permit: V3OwnedPermit,
}
impl AsRef<[u8]> for V3OwnedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_observer_retains_one_charge_through_last_request_and_can_be_recreated() {
        use crate::cadapter::read_observer::{
            Engine, Ledger, Origin, Phase, ReadClass, ReadContext, ReadObserver,
        };
        let budget = V3MountBudget::defaults();
        let observer = budget.read_observer(None).unwrap();
        let reader = budget.read_observer(None).unwrap();
        assert_eq!(
            budget.state().used[V3BudgetPool::Roots as usize],
            ReadObserver::MEMORY_BOUND_BYTES
        );
        let request = observer.start(
            Ledger::LogicalOperation,
            ReadContext {
                engine: Engine::PackedV3,
                phase: Phase::Runtime,
                origin: Origin::Demand,
                class: ReadClass::InlinePayload,
            },
            1,
        );
        drop(observer);
        drop(reader);
        assert_eq!(
            budget.state().used[V3BudgetPool::Roots as usize],
            ReadObserver::MEMORY_BOUND_BYTES
        );
        drop(request);
        assert_eq!(budget.state().used, [0; 8]);
        let observer = budget.read_observer(None).unwrap();
        assert_eq!(
            budget.state().used[V3BudgetPool::Roots as usize],
            ReadObserver::MEMORY_BOUND_BYTES
        );
        drop(observer);
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[tokio::test]
    async fn wait_closed_ignores_permit_release_and_wakes_all_registered_waiters() {
        use futures_util::FutureExt;
        use std::time::Duration;

        let budget = V3MountBudget::defaults();
        let first = budget.wait_closed();
        let second = budget.wait_closed();
        tokio::pin!(first, second);
        assert!(first.as_mut().now_or_never().is_none());
        assert!(second.as_mut().now_or_never().is_none());
        let permit = budget.admit(&[(V3BudgetPool::Control, 1)]).unwrap();
        drop(permit);
        assert!(first.as_mut().now_or_never().is_none());
        assert!(second.as_mut().now_or_never().is_none());
        budget.close();
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(first, second);
            budget.wait_closed().await;
        })
        .await
        .expect("registered and late waiters must see closure");
        assert_eq!(budget.state().used, [0; 8]);
    }

    #[test]
    fn minimum_control_capability_remains_32_kib_and_one_byte_less_is_rejected() {
        let mut limits = V3BudgetLimits::default();
        limits.bytes[V3BudgetPool::Control as usize] = 32 << 10;
        let budget = V3MountBudget::new(limits.clone()).unwrap();
        budget.validate_frame_capability(8 << 20).unwrap();
        assert_eq!(budget.state().used, [0; 8]);
        limits.bytes[V3BudgetPool::Control as usize] -= 1;
        let smaller = V3MountBudget::new(limits).unwrap();
        assert!(smaller.validate_frame_capability(8 << 20).is_err());
        assert_eq!(smaller.state().used, [0; 8]);
    }
    #[test]
    fn startup_rejects_workspace_one_byte_below_shared_three_bitmap_decode() {
        let required = super::super::super::codec::decode_workspace_bytes(
            super::super::super::PackedCodec::Zstd,
        )
        .unwrap() as u64
            + crate::cadapter::read_observer::SharedRawCoverage::required_tracking_bytes(8 << 20)
                .unwrap();
        let mut limits = V3BudgetLimits::default();
        limits.bytes[V3BudgetPool::Workspace as usize] = required - 1;
        let budget = V3MountBudget::new(limits).unwrap();
        assert!(budget.validate_frame_capability(8 << 20).is_err());
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn startup_rejects_output_budget_that_cannot_render_complete_statistics() {
        let mut limits = V3BudgetLimits::default();
        limits.bytes[V3BudgetPool::Output as usize] = 2 << 20;
        limits.max_read_bytes = 1 << 20;
        let budget = V3MountBudget::new(limits).unwrap();
        assert!(budget.validate_frame_capability(8 << 20).is_err());
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn startup_rejects_output_one_byte_below_complete_statistics_and_read() {
        let mut limits = V3BudgetLimits::default();
        let stats_bytes = crate::cadapter::read_observer::ReadObserver::MAX_RENDER_BYTES as u64
            + crate::vfs::stats::FsStats::BASE_RENDER_MAX_BYTES as u64
            + V3MountBudget::STATS_EXTENSION_RENDER_MAX_BYTES as u64;
        limits.bytes[V3BudgetPool::Output as usize] = stats_bytes
            + limits.max_read_bytes as u64
            + 2 * V3MountBudget::REPLY_ALLOCATION_ALLOWANCE_BYTES
            - 1;
        let budget = V3MountBudget::new(limits).unwrap();
        assert!(matches!(
            budget.validate_frame_capability(8 << 20),
            Err(PackedWireError::LimitExceeded(message)) if message.contains("output budget")
        ));
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn minimum_output_admits_complete_statistics_and_a_normal_read_reply() {
        let mut limits = V3BudgetLimits::default();
        let stats_bytes = crate::cadapter::read_observer::ReadObserver::MAX_RENDER_BYTES as u64
            + crate::vfs::stats::FsStats::BASE_RENDER_MAX_BYTES as u64
            + V3MountBudget::STATS_EXTENSION_RENDER_MAX_BYTES as u64;
        let required = stats_bytes
            + limits.max_read_bytes as u64
            + 2 * V3MountBudget::REPLY_ALLOCATION_ALLOWANCE_BYTES;
        limits.bytes[V3BudgetPool::Output as usize] = required;
        let budget = V3MountBudget::new(limits).unwrap();
        budget.validate_frame_capability(8 << 20).unwrap();
        let read = budget.output(budget.max_read_bytes()).unwrap();
        let read_reply = budget
            .admit(&[(
                V3BudgetPool::Output,
                V3MountBudget::REPLY_ALLOCATION_ALLOWANCE_BYTES,
            )])
            .unwrap();
        let stats = budget
            .admit(&[(
                V3BudgetPool::Output,
                stats_bytes + V3MountBudget::REPLY_ALLOCATION_ALLOWANCE_BYTES,
            )])
            .unwrap();
        assert_eq!(budget.state().used[V3BudgetPool::Output as usize], required);
        assert!(budget.output(1).is_err());
        drop(stats);
        drop(read_reply);
        drop(read);
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn atomic_bundle_failure_cannot_leak_a_partial_resource() {
        let budget = V3MountBudget::defaults();
        let held = budget.admit(&[(V3BudgetPool::Raw, 32 << 20)]).unwrap();
        assert!(
            budget
                .admit(&[(V3BudgetPool::Stored, 1), (V3BudgetPool::Raw, 1)])
                .is_err()
        );
        assert_eq!(budget.state().used[V3BudgetPool::Stored as usize], 0);
        drop(held);
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn slow_reply_consumers_own_output_until_the_last_bytes_clone_drops() {
        let budget = V3MountBudget::defaults();
        let bytes = bytes::Bytes::from_owner(V3OwnedBytes {
            data: vec![7; 4096],
            permit: budget.output(4096).unwrap(),
        });
        let consumer = bytes.clone();
        drop(bytes);
        assert_eq!(budget.state().used[V3BudgetPool::Output as usize], 4096);
        assert_eq!(consumer[0], 7);
        drop(consumer);
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[test]
    fn shutdown_rejects_new_work_and_preserves_inflight_ownership() {
        let budget = V3MountBudget::defaults();
        let held = budget.output(4096).unwrap();
        budget.close();
        assert!(budget.output(1).is_err());
        assert_eq!(budget.state().used[V3BudgetPool::Output as usize], 4096);
        drop(held);
        assert_eq!(budget.state().used, [0; 8]);
    }
    #[tokio::test]
    async fn shutdown_wakes_blocked_stored_raw_admission_without_partial_charge() {
        let budget = V3MountBudget::defaults();
        let held = budget
            .admit(&[(V3BudgetPool::Raw, budget.capacity(V3BudgetPool::Raw))])
            .unwrap();
        let waiter_budget = budget.clone();
        let waiter = tokio::spawn(async move {
            waiter_budget
                .admit_when_available(&[(V3BudgetPool::Stored, 1), (V3BudgetPool::Raw, 1)])
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        budget.close();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("closed budget did not wake admission")
            .expect("admission task panicked");
        assert!(result.is_err());
        assert_eq!(budget.state().used[V3BudgetPool::Stored as usize], 0);
        assert_eq!(
            budget.state().used[V3BudgetPool::Raw as usize],
            budget.capacity(V3BudgetPool::Raw)
        );
        drop(held);
        assert_eq!(budget.state().used, [0; 8]);
    }

    #[tokio::test]
    async fn cancellation_during_backend_wait_releases_the_owned_bundle() {
        let budget = V3MountBudget::defaults();
        let clone = budget.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _permit = clone
                .admit(&[(V3BudgetPool::Stored, 4096), (V3BudgetPool::Raw, 8192)])
                .unwrap();
            entered.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(budget.state().used, [0; 8]);
    }
}

#[cfg(test)]
mod inline_root_layout_tests {
    use super::*;
    #[test]
    fn actual_v3_permit_fits_inline_storage_without_new_guard_arc() {
        use asyncfuse::raw::reply::InlineRootPermit;
        eprintln!(
            "BREWFS_ACTUAL_V3_INLINE_PERMIT_LAYOUT permit_bytes={} permit_align={} inline_bytes={} inline_align={}",
            std::mem::size_of::<V3OwnedPermit>(),
            std::mem::align_of::<V3OwnedPermit>(),
            std::mem::size_of::<InlineRootPermit>(),
            std::mem::align_of::<InlineRootPermit>()
        );
        assert_eq!(std::mem::size_of::<V3OwnedPermit>(), 72);
        assert_eq!(std::mem::align_of::<V3OwnedPermit>(), 8);
        assert_eq!(std::mem::size_of::<InlineRootPermit>(), 80);
        assert_eq!(std::mem::align_of::<InlineRootPermit>(), 8);
        let budget = V3MountBudget::defaults();
        let before = Arc::strong_count(&budget);
        let permit = budget.admit(&[(V3BudgetPool::Roots, 4096)]).unwrap();
        let inline = InlineRootPermit::try_new(permit).unwrap();
        assert_eq!(
            Arc::strong_count(&budget),
            before + 1,
            "existing budget Arc only"
        );
        assert_eq!(budget.state().used[V3BudgetPool::Roots as usize], 4096);
        drop(inline);
        assert_eq!(budget.state().used[V3BudgetPool::Roots as usize], 0);
        assert_eq!(Arc::strong_count(&budget), before);
    }
}

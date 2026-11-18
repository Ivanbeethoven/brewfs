use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub(crate) const MAX_MEMBERS: usize = 32;

/// One entry per open group. The caller includes inode, chunk and generation
/// in the key and submits only eligible 4 KiB writes.
pub(crate) struct Groups<K, T> {
    window: Duration,
    capacity: usize,
    by_key: BTreeMap<K, (Instant, Vec<T>)>,
    by_start: BTreeSet<(Instant, K)>,
}

impl<K: Ord + Clone, T> Groups<K, T> {
    #[cfg(test)]
    pub(crate) fn new(window: Duration) -> Self {
        Self::with_capacity(window, MAX_MEMBERS)
    }

    pub(crate) fn with_capacity(window: Duration, capacity: usize) -> Self {
        assert!((1..=MAX_MEMBERS).contains(&capacity));
        Self {
            window,
            capacity,
            by_key: BTreeMap::new(),
            by_start: BTreeSet::new(),
        }
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.by_start.first()?.0.checked_add(self.window)
    }

    fn take_oldest(&mut self) -> Option<Vec<T>> {
        let (_, key) = self.by_start.pop_first()?;
        Some(self.by_key.remove(&key).expect("open group").1)
    }

    pub(crate) fn seal_due(&mut self, now: Instant) -> Vec<Vec<T>> {
        let mut ready = Vec::new();
        while self.next_deadline().is_some_and(|time| time <= now) {
            ready.push(self.take_oldest().expect("group at deadline"));
        }
        ready
    }

    pub(crate) fn push(&mut self, key: K, now: Instant, value: T) -> Vec<Vec<T>> {
        if self.window.is_zero() {
            return vec![vec![value]];
        }
        let mut ready = self.seal_due(now);
        let (start, members) = self.by_key.entry(key.clone()).or_insert_with(|| {
            self.by_start.insert((now, key.clone()));
            (now, Vec::new())
        });
        members.push(value);
        if members.len() == self.capacity {
            self.by_start.remove(&(*start, key.clone()));
            ready.push(self.by_key.remove(&key).expect("full group").1);
        }
        ready
    }

    /// Detach every open group under the writer lock before awaiting uploads.
    pub(crate) fn seal_all(&mut self) -> Vec<Vec<T>> {
        let mut ready = Vec::with_capacity(self.by_key.len());
        while let Some(members) = self.take_oldest() {
            ready.push(members);
        }
        ready
    }
}

#[cfg(test)]
#[path = "native_groups_tests.rs"]
mod tests;

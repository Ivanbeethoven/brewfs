//! One raw decode union shared across independent complete read operations.
//! Ownership supplied by the mount must precede every bitmap/span allocation.

use super::{OperationDelivery, RawCoverage, ReadContext, ReadObserver};
use anyhow::Result;
use std::sync::{Arc, Mutex};

const MAX_RECEIPT_SPANS: usize = 4096;
const MAX_OPERATION_RECEIPTS: usize = 1024;
#[derive(Debug, thiserror::Error)]
#[error("shared raw admission limit: {0}")]
pub(crate) struct SharedRawLimit(&'static str);

struct SharedState {
    coverage: RawCoverage,
    delivered: Vec<u64>,
    delivered_union: u64,
}
pub(crate) struct SharedRawCoverage {
    observer: Arc<ReadObserver>,
    context: ReadContext,
    state: Mutex<SharedState>,
    _owner: Box<dyn Send + Sync>,
}
impl std::fmt::Debug for SharedRawCoverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRawCoverage")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}
impl SharedRawCoverage {
    pub(crate) fn request(&self, start: u64, length: u64) -> Result<()> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .coverage
            .request(start, length)
    }
    pub(crate) fn required_tracking_bytes(raw: u64) -> Result<u64> {
        if raw > 8 << 20 {
            return Err(SharedRawLimit("authenticated raw size").into());
        }
        RawCoverage::required_tracking_bytes(raw)?
            .checked_add(raw.div_ceil(64) * 8)
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(|| SharedRawLimit("tracking ownership overflow").into())
    }
    pub(crate) fn required_receipt_bytes(spans: usize) -> Result<u64> {
        if spans == 0 || spans > MAX_RECEIPT_SPANS {
            return Err(SharedRawLimit("receipt span count").into());
        }
        (spans as u64)
            .checked_mul(std::mem::size_of::<Span>() as u64 * 2)
            .and_then(|n| n.checked_add(2048))
            .ok_or_else(|| SharedRawLimit("receipt ownership overflow").into())
    }
    pub(crate) fn new(
        observer: Arc<ReadObserver>,
        context: ReadContext,
        raw: u64,
        tracking_budget: u64,
        owner: Box<dyn Send + Sync>,
    ) -> Result<Arc<Self>> {
        if tracking_budget < Self::required_tracking_bytes(raw)? {
            return Err(SharedRawLimit("tracker requires admitted ownership").into());
        }
        observer.enable_raw();
        Ok(Arc::new(Self {
            observer,
            context,
            state: Mutex::new(SharedState {
                coverage: RawCoverage::new(raw, tracking_budget)?,
                delivered: vec![0; raw.div_ceil(64) as usize],
                delivered_union: 0,
            }),
            _owner: owner,
        }))
    }
    pub(crate) fn attach(
        self: &Arc<Self>,
        delivery: &Arc<OperationDelivery>,
        requests: &[(u64, u64)],
        receipt_budget: u64,
        owner: Box<dyn Send + Sync>,
    ) -> Result<Arc<SharedRawReceipt>> {
        if receipt_budget < Self::required_receipt_bytes(requests.len())? {
            return Err(SharedRawLimit("receipt requires admitted ownership").into());
        }
        if !Arc::ptr_eq(&self.observer, &delivery.observer) {
            anyhow::bail!("shared raw receipt belongs to another mount observer");
        }
        let raw = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .coverage
            .raw_bytes;
        for (start, length) in requests {
            if *length == 0 || start.checked_add(*length).is_none_or(|end| end > raw) {
                anyhow::bail!("shared receipt range exceeds authenticated raw frame");
            }
        }
        let receipt = Arc::new(SharedRawReceipt {
            frame: self.clone(),
            state: Mutex::new(ReceiptState {
                terminal: None,
                spans: requests
                    .iter()
                    .map(|&(start, length)| Span {
                        start,
                        length,
                        copied: false,
                    })
                    .collect(),
            }),
            _owner: owner,
        });
        let mut delivery_state = delivery.state.lock().unwrap_or_else(|e| e.into_inner());
        if delivery_state.terminal.is_none()
            && delivery_state.shared.len() >= MAX_OPERATION_RECEIPTS
        {
            return Err(SharedRawLimit("complete operation receipt count").into());
        }
        let terminal = delivery_state.terminal;
        if terminal.is_none() {
            delivery_state.shared.push(receipt.clone());
        }
        drop(delivery_state);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for &(start, length) in requests {
                state.coverage.request(start, length)?;
            }
        }
        if let Some(delivered) = terminal {
            receipt.finish(delivered);
        }
        Ok(receipt)
    }
}
impl Drop for SharedRawCoverage {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        let mut summary = state.coverage.summary(false);
        summary.delivered_union = state.delivered_union;
        summary.undelivered_decoded_raw = summary.decoded_raw - state.delivered_union;
        self.observer.record_raw(self.context, summary);
    }
}

#[derive(Debug)]
struct Span {
    start: u64,
    length: u64,
    copied: bool,
}
#[derive(Debug)]
struct ReceiptState {
    terminal: Option<bool>,
    spans: Vec<Span>,
}
pub(crate) struct SharedRawReceipt {
    frame: Arc<SharedRawCoverage>,
    state: Mutex<ReceiptState>,
    _owner: Box<dyn Send + Sync>,
}
impl std::fmt::Debug for SharedRawReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRawReceipt")
            .field("frame", &self.frame)
            .finish_non_exhaustive()
    }
}
fn deliver(state: &mut SharedState, start: u64, length: u64) -> Result<()> {
    state.delivered_union += RawCoverage::mark(
        state.coverage.raw_bytes,
        &mut state.delivered,
        Some(&state.coverage.copied),
        start,
        length,
    )?;
    Ok(())
}
impl SharedRawReceipt {
    /// UnifiedReadPlan dispatches the same authenticated span in one copy call.
    /// A receipt may neither copy another operation's request nor add new spans.
    pub(crate) fn copied(&self, start: u64, length: u64) -> Result<()> {
        let mut receipt = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let delivered = receipt.terminal == Some(true);
        let span = receipt
            .spans
            .iter_mut()
            .find(|s| s.start == start && s.length == length && !s.copied)
            .ok_or_else(|| {
                anyhow::anyhow!("copied interval was not submitted by this shared raw receipt")
            })?;
        let mut state = self.frame.state.lock().unwrap_or_else(|e| e.into_inner());
        state.coverage.copied(start, length)?;
        if delivered {
            deliver(&mut state, start, length)?;
        }
        span.copied = true;
        Ok(())
    }
    pub(super) fn finish(&self, delivered: bool) {
        let mut receipt = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if receipt.terminal.is_some() {
            return;
        }
        receipt.terminal = Some(delivered);
        if delivered {
            let mut state = self.frame.state.lock().unwrap_or_else(|e| e.into_inner());
            for span in receipt.spans.iter().filter(|s| s.copied) {
                if deliver(&mut state, span.start, span.length).is_err() {
                    self.frame
                        .observer
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .overflowed = true;
                }
            }
        }
    }
}
impl Drop for SharedRawReceipt {
    fn drop(&mut self) {
        self.finish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Engine, FailureClass, Ledger, Origin, Phase, ReadClass};
    use super::*;
    fn tag() -> ReadContext {
        ReadContext {
            engine: Engine::Native,
            phase: Phase::Runtime,
            origin: Origin::Demand,
            class: ReadClass::PackedPayload,
        }
    }
    fn tracker(observer: Arc<ReadObserver>) -> Arc<SharedRawCoverage> {
        SharedRawCoverage::new(
            observer,
            tag(),
            256,
            SharedRawCoverage::required_tracking_bytes(256).unwrap(),
            Box::new(()),
        )
        .unwrap()
    }
    fn attach(
        frame: &Arc<SharedRawCoverage>,
        delivery: &Arc<OperationDelivery>,
        spans: &[(u64, u64)],
    ) -> Arc<SharedRawReceipt> {
        frame
            .attach(
                delivery,
                spans,
                SharedRawCoverage::required_receipt_bytes(spans.len()).unwrap(),
                Box::new(()),
            )
            .unwrap()
    }
    #[test]
    fn shared_decode_counts_once_and_commits_only_successful_receipt_union() {
        let observer = Arc::new(ReadObserver::default());
        let frame = tracker(observer.clone());
        let first = observer.start(Ledger::LogicalOperation, tag(), 64);
        let second = observer.start(Ledger::LogicalOperation, tag(), 96);
        let third = observer.start(Ledger::LogicalOperation, tag(), 64);
        let a = attach(&frame, &first.delivery_token().unwrap(), &[(0, 64)]);
        let b = attach(&frame, &second.delivery_token().unwrap(), &[(32, 96)]);
        let c = attach(&frame, &third.delivery_token().unwrap(), &[(128, 64)]);
        a.copied(0, 64).unwrap();
        b.copied(32, 96).unwrap();
        c.copied(128, 64).unwrap();
        first.deliver(64);
        second.deliver(96);
        third.fail(FailureClass::Generation);
        drop(frame);
        drop(a);
        drop(b);
        assert!(observer.snapshot().raw.is_empty());
        drop(c);
        let raw = observer.snapshot().raw[&tag()];
        assert_eq!(
            (
                raw.decoded_raw,
                raw.requested_union,
                raw.copied_union,
                raw.delivered_union
            ),
            (256, 192, 192, 128)
        );
        assert!(raw.conserved());
    }
    #[test]
    fn copied_union_cannot_use_a_different_operations_request() {
        let observer = Arc::new(ReadObserver::default());
        let frame = tracker(observer.clone());
        let first = observer.start(Ledger::LogicalOperation, tag(), 64);
        let second = observer.start(Ledger::LogicalOperation, tag(), 64);
        let a = attach(&frame, &first.delivery_token().unwrap(), &[(0, 64)]);
        let b = attach(&frame, &second.delivery_token().unwrap(), &[(128, 64)]);
        assert!(a.copied(128, 64).is_err());
        assert!(a.copied(0, 32).is_err());
        a.copied(0, 64).unwrap();
        assert!(a.copied(0, 64).is_err());
        drop(first);
        second.deliver(64);
        drop(a);
        drop(b);
        drop(frame);
        let raw = observer.snapshot().raw[&tag()];
        assert_eq!((raw.copied_union, raw.delivered_union), (64, 0));
        assert!(raw.conserved());
    }
    #[test]
    fn terminal_before_receipt_registration_and_tracking_owner_lifetime() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Owner(Arc<AtomicUsize>);
        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let observer = Arc::new(ReadObserver::default());
        let released = Arc::new(AtomicUsize::new(0));
        let frame = SharedRawCoverage::new(
            observer.clone(),
            tag(),
            256,
            SharedRawCoverage::required_tracking_bytes(256).unwrap(),
            Box::new(Owner(released.clone())),
        )
        .unwrap();
        let first = observer.start(Ledger::LogicalOperation, tag(), 64);
        let delivery = first.delivery_token().unwrap();
        first.deliver(64);
        let a = attach(&frame, &delivery, &[(0, 64)]);
        a.copied(0, 64).unwrap();
        drop(frame);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(a);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(observer.snapshot().raw[&tag()].delivered_union, 64);
    }
    #[test]
    fn receipt_and_tracking_admission_fail_before_mutation() {
        let observer = Arc::new(ReadObserver::default());
        assert!(SharedRawCoverage::new(observer.clone(), tag(), 256, 0, Box::new(())).is_err());
        let frame = tracker(observer.clone());
        let guard = observer.start(Ledger::LogicalOperation, tag(), 64);
        let delivery = guard.delivery_token().unwrap();
        assert!(
            frame
                .attach(&delivery, &[(0, 64)], 0, Box::new(()))
                .is_err()
        );
        assert!(
            frame
                .attach(&delivery, &[(255, 2)], 4096, Box::new(()))
                .is_err()
        );
        drop(frame);
        drop(guard);
        let raw = observer.snapshot().raw[&tag()];
        assert_eq!(
            (raw.requested_union, raw.copied_union, raw.delivered_union),
            (0, 0, 0)
        );
        assert!(raw.conserved());
    }
}

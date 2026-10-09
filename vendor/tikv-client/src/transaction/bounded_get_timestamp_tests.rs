//! Observe GetRequest emitted by the actual Transaction::..._until method.
//! This checks the SDK timestamp path, independently of a handcrafted Dispatch.

use super::*;
use crate::mock::{MockKvClient, MockPdClient};
use std::sync::Mutex;

type ObservedGets = Arc<Mutex<Vec<(Vec<u8>, u64)>>>;

fn fixture(readonly: bool) -> (Transaction<MockPdClient>, ObservedGets) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let client = MockKvClient::with_dispatch_hook(move |request| {
        let get = request
            .downcast_ref::<kvrpcpb::GetRequest>()
            .expect("bounded Get must not dispatch TSO, locks, CAS or resolver requests");
        captured
            .lock()
            .unwrap()
            .push((get.key.clone(), get.version));
        Ok(Box::new(kvrpcpb::GetResponse {
            value: vec![7],
            ..Default::default()
        }))
    });
    let options = if readonly {
        TransactionOptions::new_optimistic().read_only()
    } else {
        TransactionOptions::new_optimistic().drop_check(CheckLevel::None)
    };
    (
        Transaction::new(
            Timestamp::from_version(987654321),
            Arc::new(MockPdClient::new(client)),
            options,
            Keyspace::Disable,
        ),
        observed,
    )
}

#[tokio::test]
async fn actual_until_get_emits_same_original_timestamp_for_repeat_and_next_key() {
    let (mut transaction, observed) = fixture(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    for key in [b"a".as_slice(), b"a".as_slice(), b"b".as_slice()] {
        assert_eq!(
            transaction
                .get_uncached_single_region_until(key.to_vec(), deadline)
                .await
                .unwrap(),
            Some(vec![7])
        );
        assert_eq!(transaction.start_timestamp().version(), 987654321);
    }
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            (b"a".to_vec(), 987654321),
            (b"a".to_vec(), 987654321),
            (b"b".to_vec(), 987654321),
        ]
    );
}

#[tokio::test]
async fn actual_until_get_rejects_mutating_transaction_before_data_dispatch() {
    let (mut transaction, observed) = fixture(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let error = transaction
        .get_uncached_single_region_until(b"a".to_vec(), deadline)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::StringError(_)));
    assert!(observed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actual_until_get_rejects_expired_deadline_before_data_dispatch() {
    let (mut transaction, observed) = fixture(true);
    let error = transaction
        .get_uncached_single_region_until(b"a".to_vec(), tokio::time::Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::StringError(_)));
    assert!(observed.lock().unwrap().is_empty());
    assert_eq!(transaction.start_timestamp().version(), 987654321);
}

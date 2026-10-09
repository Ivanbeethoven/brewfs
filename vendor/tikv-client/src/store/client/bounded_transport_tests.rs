//! Exercise the actual generated client/tonic receive path, without a server.

use std::future::{ready, Ready};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use crate::proto::{kvrpcpb, tikvpb::tikv_client::TikvClient};
use prost::Message;
use tonic::codegen::{http, Body, Bytes, Service};
use tonic::{Code, Status};

#[derive(Clone)]
struct ProbeService {
    gzip: bool,
    polls: Arc<AtomicUsize>,
}

struct ProbeBody {
    gzip: bool,
    polls: Arc<AtomicUsize>,
}

impl Body for ProbeBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_data(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<Bytes, Status>>> {
        assert!(!self.gzip, "gzip body was read before encoding rejection");
        assert_eq!(
            self.polls.fetch_add(1, Ordering::SeqCst),
            0,
            "oversized body was read after its five-byte length header"
        );
        // Uncompressed message claims 16385 bytes; supply only its header.
        Poll::Ready(Some(Ok(Bytes::from_static(&[0, 0, 0, 64, 1]))))
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Status>> {
        panic!("oversized response reached trailer polling")
    }
}

impl Service<http::Request<tonic::body::BoxBody>> for ProbeService {
    type Response = http::Response<ProbeBody>;
    type Error = Status;
    type Future = Ready<Result<Self::Response, Status>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Status>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: http::Request<tonic::body::BoxBody>) -> Self::Future {
        let mut response = http::Response::new(ProbeBody {
            gzip: self.gzip,
            polls: self.polls.clone(),
        });
        response
            .headers_mut()
            .insert("content-type", "application/grpc".parse().unwrap());
        if self.gzip {
            response
                .headers_mut()
                .insert("grpc-encoding", "gzip".parse().unwrap());
        }
        ready(Ok(response))
    }
}

#[tokio::test]
async fn disabled_gzip_rejects_before_reading_compressed_body() {
    let polls = Arc::new(AtomicUsize::new(0));
    let mut client = TikvClient::new(ProbeService {
        gzip: true,
        polls: polls.clone(),
    })
    .max_decoding_message_size(16 << 10);
    let error = client
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unimplemented);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn message_hard_limit_rejects_header_before_body_materialization() {
    let polls = Arc::new(AtomicUsize::new(0));
    let mut client = TikvClient::new(ProbeService {
        gzip: false,
        polls: polls.clone(),
    })
    .max_decoding_message_size(16 << 10);
    let error = client
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::OutOfRange);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}

#[derive(Clone)]
struct FeedService(Bytes);
struct FeedBody(Option<Bytes>);

impl Body for FeedBody {
    type Data = Bytes;
    type Error = Status;
    fn poll_data(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Status>>> {
        Poll::Ready(self.0.take().map(Ok))
    }
    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Status>> {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        Poll::Ready(Ok(Some(trailers)))
    }
}

impl Service<http::Request<tonic::body::BoxBody>> for FeedService {
    type Response = http::Response<FeedBody>;
    type Error = Status;
    type Future = Ready<Result<Self::Response, Status>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Status>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: http::Request<tonic::body::BoxBody>) -> Self::Future {
        let mut response = http::Response::new(FeedBody(Some(self.0.clone())));
        response
            .headers_mut()
            .insert("content-type", "application/grpc".parse().unwrap());
        ready(Ok(response))
    }
}

fn feed(payload: Vec<u8>) -> FeedService {
    let mut frame = vec![0];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    FeedService(frame.into())
}

#[tokio::test]
async fn schema_preflight_rejects_empty_pair_expansion_before_prost() {
    // Empty KvPair uses two wire bytes but contains a large inline KeyError.
    let payload = [0x12, 0].repeat(512);
    let mut client = TikvClient::new(feed(payload))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true);
    let error = client
        .kv_scan(kvrpcpb::ScanRequest {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
}

#[tokio::test]
async fn schema_preflight_returns_a_fixed_error_for_nested_secondary_expansion() {
    let response = kvrpcpb::GetResponse {
        error: Some(kvrpcpb::KeyError {
            locked: Some(kvrpcpb::LockInfo {
                secondaries: vec![vec![]; 4096],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true);
    let error = client
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "bounded read returned a key or region error"
    );
}

#[tokio::test]
async fn schema_preflight_preserves_one_legal_pair_and_get_value() {
    let response = kvrpcpb::ScanResponse {
        pairs: vec![kvrpcpb::KvPair {
            key: b"a".to_vec(),
            value: b"value".to_vec(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true);
    let actual = client
        .kv_scan(kvrpcpb::ScanRequest {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(actual, response);
    let response = kvrpcpb::GetResponse {
        value: b"value".to_vec(),
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true);
    assert_eq!(
        client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap()
            .into_inner(),
        response
    );
}

#[tokio::test]
async fn schema_preflight_accepts_legal_error_as_bounded_fail_closed_status() {
    let response = kvrpcpb::GetResponse {
        error: Some(kvrpcpb::KeyError {
            abort: "transaction aborted".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true);
    assert_eq!(
        client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
}

#[tokio::test]
async fn pd_schema_preflight_caps_empty_member_expansion_and_keeps_legal_control() {
    use crate::proto::pdpb::{self, pd_client::PdClient};
    let mut client = PdClient::new(feed([0x12, 0].repeat(17)))
        .max_decoding_message_size(16 << 10)
        .bounded_topology_schema(true);
    assert_eq!(
        client
            .get_members(pdpb::GetMembersRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::ResourceExhausted
    );
    let response = pdpb::GetMembersResponse {
        members: vec![pdpb::Member {
            member_id: 1,
            client_urls: vec!["http://localhost:2379".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut client = PdClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_topology_schema(true);
    assert_eq!(
        client
            .get_members(pdpb::GetMembersRequest::default())
            .await
            .unwrap()
            .into_inner(),
        response
    );
}

#[tokio::test]
async fn pd_store_schema_accepts_real_thread_metrics_and_caps_expansion() {
    use crate::proto::pdpb::{self, pd_client::PdClient};
    // Isolated TiKV v8.5.3 reports 71 records in each of these arrays, even
    // with a two-thread gRPC pool. They fit comfortably in the 16 KiB envelope.
    let metrics = (0..71)
        .map(|thread| pdpb::RecordPair {
            key: format!("worker-{thread}"),
            value: thread,
        })
        .collect::<Vec<_>>();
    let response = pdpb::GetStoreResponse {
        stats: Some(pdpb::StoreStats {
            cpu_usages: metrics.clone(),
            read_io_rates: metrics.clone(),
            write_io_rates: metrics,
            ..Default::default()
        }),
        ..Default::default()
    };
    let payload = response.encode_to_vec();
    assert!(payload.len() < 16 << 10);
    let mut client = PdClient::new(feed(payload))
        .max_decoding_message_size(16 << 10)
        .bounded_topology_schema(true);
    assert_eq!(
        client
            .get_store(pdpb::GetStoreRequest::default())
            .await
            .unwrap()
            .into_inner(),
        response
    );

    // Tiny empty records still cannot amplify into arbitrarily many objects.
    for (cpu, read, write, latency) in [(129, 0, 0, 0), (128, 128, 128, 128)] {
        let response = pdpb::GetStoreResponse {
            stats: Some(pdpb::StoreStats {
                cpu_usages: vec![pdpb::RecordPair::default(); cpu],
                read_io_rates: vec![pdpb::RecordPair::default(); read],
                write_io_rates: vec![pdpb::RecordPair::default(); write],
                op_latencies: vec![pdpb::RecordPair::default(); latency],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut client = PdClient::new(feed(response.encode_to_vec()))
            .max_decoding_message_size(16 << 10)
            .bounded_topology_schema(true);
        let error = client
            .get_store(pdpb::GetStoreRequest::default())
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::ResourceExhausted);
        assert_eq!(error.message(), "bounded protobuf structure limit exceeded");
    }
}

fn diagnostic_field(field: u8, body: &[u8]) -> Vec<u8> {
    assert!(field < 32 && body.len() < 128);
    let tag = (u16::from(field) << 3) | 2;
    let mut value = if tag < 128 {
        vec![tag as u8]
    } else {
        vec![(tag as u8 & 127) | 128, (tag >> 7) as u8]
    };
    value.push(body.len() as u8);
    value.extend_from_slice(body);
    value
}

fn assert_fixed_diagnostic(error: Status, location: u8, family: u8, subtype: u8, shape: u8) {
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "bounded read returned a key or region error"
    );
    assert_eq!(
        error.details(),
        &[b'B', b'R', b'E', location, family, subtype, shape]
    );
}

#[tokio::test]
async fn schema_preflight_classifies_key_errors_without_decoding_nested_bodies() {
    // Invalid nested protobuf/UTF-8 would fail prost decoding if reached.
    let private_body = [0xff, 0xfe, 0xfd];
    for subtype in 1..=11 {
        let error = diagnostic_field(subtype, &private_body);
        let payload = diagnostic_field(2, &error);
        let mut client = TikvClient::new(feed(payload))
            .max_decoding_message_size(16 << 10)
            .bounded_get_scan_schema(true);
        let error = client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap_err();
        assert_fixed_diagnostic(error, 1, 2, subtype, 0);
    }
}

#[tokio::test]
async fn schema_preflight_classifies_region_errors_without_decoding_server_text() {
    for subtype in [2u8, 5, 6, 10, 15, 21] {
        let mut body = diagnostic_field(1, b"server-text-must-remain-private");
        body.extend(diagnostic_field(subtype, &[0xff]));
        let mut client = TikvClient::new(feed(diagnostic_field(1, &body)))
            .max_decoding_message_size(16 << 10)
            .bounded_get_scan_schema(true);
        let error = client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap_err();
        assert_fixed_diagnostic(error, 1, 1, subtype, 0);
    }
}

#[tokio::test]
async fn schema_preflight_distinguishes_scan_top_level_and_pair_errors() {
    let key_error = diagnostic_field(1, &[0xff]);
    let top_level = diagnostic_field(3, &key_error);
    let pair = diagnostic_field(2, &diagnostic_field(1, &key_error));
    for (payload, location) in [(top_level, 2), (pair, 3)] {
        let mut client = TikvClient::new(feed(payload))
            .max_decoding_message_size(16 << 10)
            .bounded_get_scan_schema(true);
        let error = client
            .kv_scan(kvrpcpb::ScanRequest {
                limit: 1,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_fixed_diagnostic(error, location, 2, 1, 0);
    }
}

#[tokio::test]
async fn schema_preflight_bounds_and_qualifies_incomplete_error_diagnostics() {
    let cases = [
        (vec![], 1),
        (vec![0x0a, 0, 0x22, 0], 2),
        (vec![0x0a, 0, 0x0a, 0], 2),
        (vec![0xa2, 1, 0], 1), // Unknown field 20.
        (vec![0], 3),
        (vec![0x0a, 1], 3),
        (vec![0x08, 1], 3), // Known message with a scalar wire type.
        (vec![0x80], 3),
        ([0x78, 0].repeat(32), 1), // Exactly 32 fields remains within the cap.
        ([0x78, 0].repeat(33), 4), // More than 32 shallow fields.
    ];
    for (body, shape) in cases {
        let mut client = TikvClient::new(feed(diagnostic_field(2, &body)))
            .max_decoding_message_size(16 << 10)
            .bounded_get_scan_schema(true);
        let error = client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap_err();
        assert_fixed_diagnostic(error, 1, 2, 0, shape);
    }
}

#[tokio::test]
async fn native_error_response_remains_decodable_when_schema_guard_is_disabled() {
    let response = kvrpcpb::GetResponse {
        error: Some(kvrpcpb::KeyError {
            abort: "native-client-error-content".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(false);
    assert_eq!(
        client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap()
            .into_inner(),
        response
    );
}

// Strict local classification runs through the generated Get client and tonic.
fn lock_wire_varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value >= 128 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}

fn lock_wire_bytes(field: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = lock_wire_varint((field << 3) | 2);
    bytes.extend(lock_wire_varint(body.len() as u64));
    bytes.extend_from_slice(body);
    bytes
}

fn lock_wire_scalar(field: u64, value: u64) -> Vec<u8> {
    let mut bytes = lock_wire_varint(field << 3);
    bytes.extend(lock_wire_varint(value));
    bytes
}

fn lock_wire_minimal(primary: &[u8], version: u64, key: &[u8]) -> Vec<u8> {
    [
        lock_wire_bytes(1, primary),
        lock_wire_scalar(2, version),
        lock_wire_bytes(3, key),
    ]
    .concat()
}

fn lock_wire_response(lock: &[u8]) -> Vec<u8> {
    lock_wire_bytes(2, &lock_wire_bytes(1, lock))
}

async fn lock_get_status(payload: Vec<u8>, policy: Option<usize>, wire_cap: usize) -> Status {
    TikvClient::new(feed(payload))
        .max_decoding_message_size(wire_cap)
        .bounded_get_scan_schema(true)
        .bounded_read_lock_conflicts(policy)
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err()
}

fn assert_lock_classification(error: Status, classified: bool) {
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "bounded read returned a key or region error"
    );
    assert_eq!(error.details(), &[b'B', b'R', b'E', 1, 2, 1, 0]);
    assert_eq!(
        crate::Error::GrpcAPI(error).is_bounded_read_lock_conflict(),
        classified
    );
}

#[tokio::test]
async fn validated_get_lock_requires_explicit_policy_and_keeps_fixed_diagnostic() {
    let payload = lock_wire_response(&lock_wire_minimal(b"primary", 17, b"key"));
    for (policy, classified) in [
        (None, false),
        (Some(64), true),
        (Some(0), false),
        (Some(4097), false),
    ] {
        assert_lock_classification(
            lock_get_status(payload.clone(), policy, 16 << 10).await,
            classified,
        );
    }
    assert!(!crate::Error::Unimplemented.is_bounded_read_lock_conflict());
    assert_eq!(
        crate::Config::default().bounded_read_lock_conflict_key_bytes,
        None
    );
    assert_eq!(
        crate::Config::default()
            .with_bounded_read_lock_conflicts(64)
            .bounded_read_lock_conflict_key_bytes,
        Some(64)
    );
    for invalid in [0, 4097, usize::MAX] {
        assert_eq!(
            crate::Config::default()
                .with_bounded_read_lock_conflicts(invalid)
                .bounded_read_lock_conflict_key_bytes,
            None
        );
    }
}

#[tokio::test]
async fn validated_get_lock_preserves_native_client_when_schema_guard_is_disabled() {
    let response = kvrpcpb::GetResponse {
        error: Some(kvrpcpb::KeyError {
            locked: Some(kvrpcpb::LockInfo {
                primary_lock: b"primary".to_vec(),
                lock_version: 17,
                key: b"key".to_vec(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut client = TikvClient::new(feed(response.encode_to_vec()))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(false)
        .bounded_read_lock_conflicts(Some(64));
    assert_eq!(
        client
            .kv_get(kvrpcpb::GetRequest::default())
            .await
            .unwrap()
            .into_inner(),
        response
    );
}

#[tokio::test]
async fn validated_get_lock_policy_cannot_bypass_gzip_or_response_length_caps() {
    for (gzip, expected_code, expected_polls) in
        [(true, Code::Unimplemented, 0), (false, Code::OutOfRange, 1)]
    {
        let polls = Arc::new(AtomicUsize::new(0));
        let error = TikvClient::new(ProbeService {
            gzip,
            polls: polls.clone(),
        })
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true)
        .bounded_read_lock_conflicts(Some(64))
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err();
        assert_eq!(error.code(), expected_code);
        assert_eq!(polls.load(Ordering::SeqCst), expected_polls);
        assert!(!crate::Error::GrpcAPI(error).is_bounded_read_lock_conflict());
    }
}

#[derive(Clone)]
struct RemoteLockDiagnosticService;

impl Service<http::Request<tonic::body::BoxBody>> for RemoteLockDiagnosticService {
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = Status;
    type Future = Ready<Result<Self::Response, Status>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Status>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: http::Request<tonic::body::BoxBody>) -> Self::Future {
        ready(Ok(Status::with_details(
            Code::FailedPrecondition,
            "bounded read returned a key or region error",
            Bytes::from_static(&[b'B', b'R', b'E', 1, 2, 1, 0]),
        )
        .to_http()))
    }
}

#[tokio::test]
async fn remote_matching_lock_diagnostic_cannot_forge_local_classification() {
    let error = TikvClient::new(RemoteLockDiagnosticService)
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true)
        .bounded_read_lock_conflicts(Some(64))
        .kv_get(kvrpcpb::GetRequest::default())
        .await
        .unwrap_err();
    assert_lock_classification(error, false);
}

#[tokio::test]
async fn validated_get_lock_rejects_nested_malformed_unknown_and_duplicate_fields() {
    let valid = lock_wire_minimal(b"primary", 17, b"key");
    let tails = [
        vec![0],
        vec![0x80],
        vec![0x22, 1],
        lock_wire_scalar(12, 0),
        lock_wire_bytes(100, &[]),
        lock_wire_scalar(1, 1),
        lock_wire_bytes(2, &[17]),
        lock_wire_bytes(1, b"duplicate"),
        lock_wire_scalar(2, 18),
        lock_wire_bytes(3, b"duplicate"),
        [lock_wire_scalar(4, 1), lock_wire_scalar(4, 2)].concat(),
        [lock_wire_scalar(8, 0), lock_wire_scalar(8, 0)].concat(),
        [lock_wire_scalar(100, 0), lock_wire_scalar(100, 0)].concat(),
        vec![0xa0, 0, 1],                      // Noncanonical field-4 tag.
        vec![0x20, 0x81, 0],                   // Noncanonical scalar.
        vec![0x52, 0x81, 0, b'x'],             // Noncanonical bytes length.
        [vec![0x20], vec![0xff; 10]].concat(), // Overwide u64.
    ];
    for tail in tails {
        let payload = lock_wire_response(&[valid.clone(), tail].concat());
        assert_lock_classification(lock_get_status(payload, Some(64), 16 << 10).await, false);
    }
    for lock in [
        [lock_wire_scalar(2, 17), lock_wire_bytes(3, b"key")].concat(),
        [lock_wire_bytes(1, b"primary"), lock_wire_bytes(3, b"key")].concat(),
        [lock_wire_bytes(1, b"primary"), lock_wire_scalar(2, 17)].concat(),
        lock_wire_minimal(b"", 17, b"key"),
        lock_wire_minimal(b"primary", 0, b"key"),
        lock_wire_minimal(b"primary", 17, b""),
    ] {
        assert_lock_classification(
            lock_get_status(lock_wire_response(&lock), Some(64), 16 << 10).await,
            false,
        );
    }
}

#[tokio::test]
async fn validated_get_lock_checks_operation_async_bool_and_file_transaction_fields() {
    let valid = lock_wire_minimal(b"primary", 17, b"key");
    for (tail, classified) in [
        (vec![], true),
        (lock_wire_scalar(6, 0), true),
        (lock_wire_scalar(6, 1), true),
        (lock_wire_scalar(6, 2), true),
        (lock_wire_scalar(6, 4), true),
        (
            [lock_wire_scalar(6, 5), lock_wire_scalar(7, 19)].concat(),
            true,
        ),
        (lock_wire_scalar(6, 3), false),
        (lock_wire_scalar(6, 6), false),
        (lock_wire_scalar(6, 7), false),
        (lock_wire_scalar(6, u64::MAX), false),
        (lock_wire_scalar(6, 5), false),
        (lock_wire_scalar(8, 2), false),
        (lock_wire_scalar(100, 0), true),
        (lock_wire_scalar(100, 1), false),
        (lock_wire_scalar(100, 2), false),
        (lock_wire_scalar(8, 1), false),
        (
            [lock_wire_scalar(8, 1), lock_wire_scalar(9, 17)].concat(),
            false,
        ),
        (
            [lock_wire_scalar(8, 1), lock_wire_scalar(9, 18)].concat(),
            true,
        ),
        (lock_wire_bytes(10, b"secondary"), false),
        (
            [
                lock_wire_scalar(4, u64::MAX),
                lock_wire_scalar(5, u64::MAX),
                lock_wire_scalar(11, u64::MAX),
            ]
            .concat(),
            true,
        ),
    ] {
        assert_lock_classification(
            lock_get_status(
                lock_wire_response(&[valid.clone(), tail].concat()),
                Some(64),
                16 << 10,
            )
            .await,
            classified,
        );
    }
}

#[tokio::test]
async fn validated_get_lock_caps_secondary_count_key_lengths_and_nested_body() {
    for (key_len, classified) in [(64, true), (65, false)] {
        let payload = lock_wire_response(&lock_wire_minimal(
            &vec![b'p'; key_len],
            17,
            &vec![b'k'; key_len],
        ));
        assert_lock_classification(
            lock_get_status(payload, Some(64), 16 << 10).await,
            classified,
        );
    }
    let payload = lock_wire_response(&lock_wire_minimal(&vec![b'p'; 4096], 17, &vec![b'k'; 4096]));
    assert_lock_classification(lock_get_status(payload, Some(4096), 16 << 10).await, true);
    for (secondary_len, classified) in [(64, true), (65, false)] {
        let mut lock = lock_wire_minimal(b"primary", 17, b"key");
        lock.extend(lock_wire_scalar(8, 1));
        lock.extend(lock_wire_scalar(9, 18));
        lock.extend(lock_wire_bytes(10, &vec![b's'; secondary_len]));
        assert_lock_classification(
            lock_get_status(lock_wire_response(&lock), Some(64), 16 << 10).await,
            classified,
        );
    }
    for (count, key, classified) in [
        (16, b"secondary".as_slice(), true),
        (17, b"secondary".as_slice(), false),
        (4096, b"".as_slice(), false),
        (1, b"".as_slice(), false),
    ] {
        let mut lock = lock_wire_minimal(b"primary", 17, b"key");
        lock.extend(lock_wire_scalar(8, 1));
        lock.extend(lock_wire_scalar(9, 18));
        for _ in 0..count {
            lock.extend(lock_wire_bytes(10, key));
        }
        assert_lock_classification(
            lock_get_status(lock_wire_response(&lock), Some(64), 16 << 10).await,
            classified,
        );
    }
    // A larger test-only tonic cap lets the independent nested cap be observed.
    let mut lock = lock_wire_minimal(&vec![b'p'; 4096], 17, &vec![b'k'; 4096]);
    lock.extend(lock_wire_scalar(8, 1));
    lock.extend(lock_wire_scalar(9, 18));
    for _ in 0..3 {
        lock.extend(lock_wire_bytes(10, &vec![b's'; 4096]));
    }
    assert!(lock.len() > 16 << 10);
    assert_lock_classification(
        lock_get_status(lock_wire_response(&lock), Some(4096), 32 << 10).await,
        false,
    );
}

#[tokio::test]
async fn validated_get_lock_requires_one_entire_error_only_response() {
    let lock = lock_wire_minimal(b"primary", 17, b"key");
    let key_error = lock_wire_bytes(1, &lock);
    let response = lock_wire_bytes(2, &key_error);
    for extra in [
        lock_wire_bytes(1, &[]),
        response.clone(),
        lock_wire_bytes(3, b"value"),
        lock_wire_scalar(4, 1),
        lock_wire_bytes(6, &[]),
        lock_wire_scalar(20, 0),
        vec![0x80],
    ] {
        assert_lock_classification(
            lock_get_status([response.clone(), extra].concat(), Some(64), 16 << 10).await,
            false,
        );
    }
    for extra in [
        lock_wire_bytes(1, &lock),
        lock_wire_bytes(3, b"abort"),
        lock_wire_scalar(20, 0),
    ] {
        let error = lock_get_status(
            lock_wire_bytes(2, &[key_error.clone(), extra].concat()),
            Some(64),
            16 << 10,
        )
        .await;
        assert!(!crate::Error::GrpcAPI(error).is_bounded_read_lock_conflict());
    }
    // Overlong outer tag and length must not produce a local certificate.
    let mut noncanonical_tag = vec![0x92, 0];
    noncanonical_tag.extend(lock_wire_varint(key_error.len() as u64));
    noncanonical_tag.extend_from_slice(&key_error);
    let mut noncanonical_len = vec![0x12, key_error.len() as u8 | 128, 0];
    noncanonical_len.extend_from_slice(&key_error);
    for payload in [noncanonical_tag, noncanonical_len] {
        assert_lock_classification(lock_get_status(payload, Some(64), 16 << 10).await, false);
    }
}

#[tokio::test]
async fn validated_get_lock_never_classifies_other_key_errors_or_scan_paths() {
    for body in [lock_wire_bytes(3, b"abort"), lock_wire_bytes(4, &[])] {
        let error = lock_get_status(lock_wire_bytes(2, &body), Some(64), 16 << 10).await;
        assert!(!crate::Error::GrpcAPI(error).is_bounded_read_lock_conflict());
    }
    let key_error = lock_wire_bytes(1, &lock_wire_minimal(b"primary", 17, b"key"));
    for (payload, location) in [
        (lock_wire_bytes(3, &key_error), 2),
        (lock_wire_bytes(2, &lock_wire_bytes(1, &key_error)), 3),
    ] {
        let error = TikvClient::new(feed(payload))
            .max_decoding_message_size(16 << 10)
            .bounded_get_scan_schema(true)
            .bounded_read_lock_conflicts(Some(64))
            .kv_scan(kvrpcpb::ScanRequest {
                limit: 1,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.details(), &[b'B', b'R', b'E', location, 2, 1, 0]);
        assert!(!crate::Error::GrpcAPI(error).is_bounded_read_lock_conflict());
    }
}

fn lock_wire_observed_time_details() -> Vec<u8> {
    // Real pinned TiKV observation01: Get tags 2,6; ExecDetailsV2 tags 1,4;
    // TimeDetail tag 4 and TimeDetailV2 tag 5, all canonical uint64 scalars.
    // Keys and timestamps are synthetic; the observed body is never retained.
    [
        lock_wire_bytes(1, &lock_wire_scalar(4, 123456)),
        lock_wire_bytes(4, &lock_wire_scalar(5, 123456)),
    ]
    .concat()
}

#[tokio::test]
async fn validated_get_lock_accepts_observed_bounded_timing_with_explicit_policy() {
    let error = lock_wire_response(&lock_wire_minimal(b"primary", 17, b"key"));
    let timing = lock_wire_bytes(6, &lock_wire_observed_time_details());
    for payload in [
        [error.clone(), timing.clone()].concat(),
        [timing.clone(), error.clone()].concat(),
    ] {
        assert_lock_classification(
            lock_get_status(payload.clone(), Some(64), 16 << 10).await,
            true,
        );
        assert_lock_classification(lock_get_status(payload, None, 16 << 10).await, false);
    }
}

#[tokio::test]
async fn validated_get_lock_accepts_only_fixed_uint64_time_schemas() {
    let error = lock_wire_response(&lock_wire_minimal(b"primary", 17, b"key"));
    let legacy = (1..=4)
        .rev()
        .flat_map(|n| lock_wire_scalar(n, u64::MAX))
        .collect::<Vec<_>>();
    let modern = (1..=5)
        .rev()
        .flat_map(|n| lock_wire_scalar(n, u64::MAX))
        .collect::<Vec<_>>();
    for timing in [
        lock_wire_bytes(1, &legacy),
        lock_wire_bytes(4, &modern),
        [lock_wire_bytes(4, &modern), lock_wire_bytes(1, &legacy)].concat(),
        lock_wire_bytes(1, &lock_wire_scalar(1, 0)),
    ] {
        assert!(timing.len() <= 128);
        assert_lock_classification(
            lock_get_status(
                [error.clone(), lock_wire_bytes(6, &timing)].concat(),
                Some(64),
                16 << 10,
            )
            .await,
            true,
        );
    }
}

#[tokio::test]
async fn validated_get_lock_rejects_unknown_empty_duplicate_and_oversized_timing() {
    let error = lock_wire_response(&lock_wire_minimal(b"primary", 17, b"key"));
    let timing = lock_wire_observed_time_details();
    for invalid in [
        Vec::new(),
        lock_wire_bytes(1, &[]),
        lock_wire_bytes(4, &[]),
        lock_wire_bytes(2, &lock_wire_scalar(1, 1)), // ScanDetailV2 is outside policy.
        lock_wire_bytes(3, &lock_wire_scalar(1, 1)), // WriteDetail is outside policy.
        lock_wire_bytes(5, &lock_wire_scalar(1, 1)),
        lock_wire_scalar(1, 1),
        lock_wire_scalar(4, 1),
        [timing.clone(), timing.clone()].concat(),
        [timing.clone(), vec![0x80]].concat(),
        [vec![0x8a, 0, 2], lock_wire_scalar(1, 1)].concat(), // Noncanonical tag.
        [vec![10, 0x82, 0], lock_wire_scalar(1, 1)].concat(), // Noncanonical length.
        lock_wire_bytes(1, &vec![0; 129]),
    ] {
        assert_lock_classification(
            lock_get_status(
                [error.clone(), lock_wire_bytes(6, &invalid)].concat(),
                Some(64),
                16 << 10,
            )
            .await,
            false,
        );
    }
}

#[tokio::test]
async fn validated_get_lock_rejects_malformed_noncanonical_and_duplicate_time_scalars() {
    let error = lock_wire_response(&lock_wire_minimal(b"primary", 17, b"key"));
    for child in [1, 4] {
        for invalid in [
            vec![0],
            vec![0x80],
            vec![8],
            vec![8, 0x81, 0],                   // Noncanonical value.
            vec![0x88, 0, 1],                   // Noncanonical tag.
            [vec![8], vec![0xff; 10]].concat(), // Overflow.
            [lock_wire_scalar(1, 1), lock_wire_scalar(1, 2)].concat(),
            lock_wire_bytes(1, &[1]),
            [vec![9], vec![0; 8]].concat(), // fixed64 is not uint64.
            lock_wire_scalar(if child == 1 { 5 } else { 6 }, 0),
        ] {
            assert_lock_classification(
                lock_get_status(
                    [
                        error.clone(),
                        lock_wire_bytes(6, &lock_wire_bytes(child, &invalid)),
                    ]
                    .concat(),
                    Some(64),
                    16 << 10,
                )
                .await,
                false,
            );
        }
    }
}

#[tokio::test]
async fn validated_get_lock_timing_cannot_certify_mixed_errors_or_payload() {
    let valid_lock = lock_wire_minimal(b"primary", 17, b"key");
    let timing = lock_wire_bytes(6, &lock_wire_observed_time_details());
    let valid_error = lock_wire_response(&valid_lock);
    for extra in [
        lock_wire_bytes(1, &[]),
        valid_error.clone(),
        lock_wire_bytes(3, b"value"),
        lock_wire_bytes(3, &[]),
        lock_wire_scalar(4, 1),
        lock_wire_scalar(4, 0),
        timing.clone(),
        lock_wire_scalar(20, 0),
    ] {
        let status = lock_get_status(
            [valid_error.clone(), timing.clone(), extra].concat(),
            Some(64),
            16 << 10,
        )
        .await;
        assert!(!crate::Error::GrpcAPI(status).is_bounded_read_lock_conflict());
    }
    for error in [
        lock_wire_bytes(3, b"abort"),
        [
            lock_wire_bytes(1, &valid_lock),
            lock_wire_bytes(3, b"abort"),
        ]
        .concat(),
        lock_wire_bytes(1, &lock_wire_minimal(b"primary", 0, b"key")),
        lock_wire_bytes(1, &[valid_lock.clone(), lock_wire_scalar(12, 0)].concat()),
    ] {
        let status = lock_get_status(
            [lock_wire_bytes(2, &error), timing.clone()].concat(),
            Some(64),
            16 << 10,
        )
        .await;
        assert!(!crate::Error::GrpcAPI(status).is_bounded_read_lock_conflict());
    }
}

#[tokio::test]
async fn validated_get_lock_timing_does_not_certify_scan_errors() {
    let key_error = lock_wire_bytes(1, &lock_wire_minimal(b"primary", 17, b"key"));
    let payload = [
        lock_wire_bytes(3, &key_error),
        lock_wire_bytes(4, &lock_wire_observed_time_details()),
    ]
    .concat();
    let status = TikvClient::new(feed(payload))
        .max_decoding_message_size(16 << 10)
        .bounded_get_scan_schema(true)
        .bounded_read_lock_conflicts(Some(64))
        .kv_scan(kvrpcpb::ScanRequest {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(!crate::Error::GrpcAPI(status).is_bounded_read_lock_conflict());
}

#[tokio::test]
async fn validated_get_lock_timing_counts_towards_the_entire_response_cap() {
    for (secondary_len, expected_size, classified) in
        [(4058, 16 << 10, true), (4059, (16 << 10) + 1, false)]
    {
        let mut lock = lock_wire_minimal(&vec![b'p'; 4096], 17, &vec![b'k'; 4096]);
        lock.extend(lock_wire_scalar(8, 1));
        lock.extend(lock_wire_scalar(9, 18));
        lock.extend(lock_wire_bytes(10, &vec![b's'; 4096]));
        lock.extend(lock_wire_bytes(10, &vec![b's'; secondary_len]));
        assert!(
            lock.len() < 16 << 10,
            "nested lock cap cannot explain rejection"
        );
        let payload = [
            lock_wire_response(&lock),
            lock_wire_bytes(6, &lock_wire_observed_time_details()),
        ]
        .concat();
        assert_eq!(payload.len(), expected_size);
        // A larger independent tonic cap allows observing the classifier's
        // entire-response bound, rather than a transport length rejection.
        assert_lock_classification(
            lock_get_status(payload, Some(4096), 32 << 10).await,
            classified,
        );
    }
}

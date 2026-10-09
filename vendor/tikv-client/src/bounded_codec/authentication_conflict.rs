//! Classify one failed, bounded normal-mode pessimistic locking request.
//! A codec shape is certified against the actual request before any caller may
//! treat it as an authentication conflict. No response value becomes successful.

use std::sync::Arc;

use tonic::Status;

use super::lock_conflict::{body, scalar};

const MAX_BODY_BYTES: usize = 16 << 10;
const MAX_KEY_BYTES: usize = 4096;
const MAX_RETRYABLE_BYTES: usize = 2048;
const MAX_EXEC_DETAILS_BYTES: usize = 128;

struct Candidate {
    start_ts: u64,
    conflict_commit_ts: u64,
    key: Vec<u8>,
    primary: Vec<u8>,
    response_bytes: usize,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded authentication write-conflict shape")
    }
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded authentication write-conflict shape")
    }
}

impl std::error::Error for Candidate {}

#[derive(Debug)]
struct Certified {
    response_bytes: usize,
}

impl std::fmt::Display for Certified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("validated bounded authentication write conflict")
    }
}

impl std::error::Error for Certified {}

struct BorrowedConflict<'a> {
    start_ts: u64,
    conflict_commit_ts: u64,
    key: &'a [u8],
    primary: &'a [u8],
}

fn conflict(mut bytes: &[u8], key_limit: usize) -> Option<BorrowedConflict<'_>> {
    let mut seen = 0u8;
    let mut start_ts = 0;
    let mut conflict_ts = 0;
    let mut conflict_commit_ts = 0;
    let mut reason = 0;
    let mut key = None;
    let mut primary = None;
    for _ in 0..6 {
        if bytes.is_empty() {
            break;
        }
        let tag = scalar(&mut bytes)?;
        let field = tag >> 3;
        if !(1..=6).contains(&field) || seen & (1 << field) != 0 {
            return None;
        }
        seen |= 1 << field;
        match field {
            3 | 4 => {
                if tag & 7 != 2 {
                    return None;
                }
                let value = body(&mut bytes)?;
                if value.is_empty() || value.len() > key_limit {
                    return None;
                }
                if field == 3 {
                    key = Some(value);
                } else {
                    primary = Some(value);
                }
            }
            _ => {
                if tag & 7 != 0 {
                    return None;
                }
                let value = scalar(&mut bytes)?;
                match field {
                    1 => start_ts = value,
                    2 => conflict_ts = value,
                    5 => conflict_commit_ts = value,
                    6 => reason = value,
                    _ => return None,
                }
            }
        }
    }
    // Only the normal locking request's definite PessimisticRetry failure.
    // Other WriteConflict reasons do not establish this restart authority.
    if !bytes.is_empty()
        || seen != 0b0111_1110
        || start_ts == 0
        || conflict_ts == 0
        || conflict_ts == start_ts
        || conflict_commit_ts <= conflict_ts
        || reason != 2
    {
        return None;
    }
    Some(BorrowedConflict {
        start_ts,
        conflict_commit_ts,
        key: key?,
        primary: primary?,
    })
}

fn key_error(mut bytes: &[u8], key_limit: usize) -> Option<BorrowedConflict<'_>> {
    let mut retryable = false;
    let mut parsed = None;
    for _ in 0..2 {
        if bytes.is_empty() {
            break;
        }
        match scalar(&mut bytes)? {
            18 if !retryable => {
                let text = body(&mut bytes)?;
                if text.is_empty() || text.len() > MAX_RETRYABLE_BYTES {
                    return None;
                }
                std::str::from_utf8(text).ok()?;
                retryable = true;
            }
            34 if parsed.is_none() => parsed = Some(conflict(body(&mut bytes)?, key_limit)?),
            _ => return None,
        }
    }
    (bytes.is_empty() && retryable).then_some(parsed?)
}

fn validate_execution_details(mut bytes: &[u8]) -> Option<()> {
    // TiKV v8.5.3 pins kvproto b6a98c6bf02d1864e74029f31ff99951719d96c1.
    // Its Lock replies carry all four fixed uint64-only ExecDetailsV2 schemas.
    // TimeDetailV2 fields 6/7 are gRPC process/wait nanoseconds in that schema;
    // the older generated client omits them. This policy is private to Lock:
    // the bounded Get classifier retains its existing timing-only policy.
    if bytes.is_empty() || bytes.len() > MAX_EXEC_DETAILS_BYTES {
        return None;
    }
    let mut seen = 0u8;
    for _ in 0..4 {
        if bytes.is_empty() {
            break;
        }
        let tag = scalar(&mut bytes)?;
        let max_field = match tag {
            10 => 4,  // TimeDetail.
            18 => 13, // ScanDetailV2.
            26 => 17, // WriteDetail.
            34 => 7,  // TimeDetailV2 at the pinned server schema.
            _ => return None,
        };
        let bit = 1 << (tag >> 3);
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
        let mut details = body(&mut bytes)?;
        if details.is_empty() {
            return None;
        }
        let mut scalars_seen = 0u32;
        for _ in 0..max_field {
            if details.is_empty() {
                break;
            }
            let tag = scalar(&mut details)?;
            let field = tag >> 3;
            if tag & 7 != 0 || field == 0 || field > max_field {
                return None;
            }
            let bit = 1u32 << field;
            if scalars_seen & bit != 0 {
                return None;
            }
            scalars_seen |= bit;
            scalar(&mut details)?;
        }
        if !details.is_empty() {
            return None;
        }
    }
    bytes.is_empty().then_some(())
}

pub(super) fn status_if_validated(mut response: &[u8], key_limit: usize) -> Option<Status> {
    if response.len() > MAX_BODY_BYTES || key_limit == 0 || key_limit > MAX_KEY_BYTES {
        return None;
    }
    let response_bytes = response.len();
    let mut parsed = None;
    let mut timing_seen = false;
    for _ in 0..2 {
        if response.is_empty() {
            break;
        }
        match scalar(&mut response)? {
            18 if parsed.is_none() => parsed = Some(key_error(body(&mut response)?, key_limit)?),
            58 if !timing_seen => {
                validate_execution_details(body(&mut response)?)?;
                timing_seen = true;
            }
            _ => return None,
        }
    }
    if !response.is_empty() {
        return None;
    }
    let parsed = parsed?;
    let mut status = Status::failed_precondition(
        "bounded authentication returned a validated locking write conflict",
    );
    status.set_source(Arc::new(Candidate {
        start_ts: parsed.start_ts,
        conflict_commit_ts: parsed.conflict_commit_ts,
        key: parsed.key.to_vec(),
        primary: parsed.primary.to_vec(),
        response_bytes,
    }));
    Some(status)
}

pub(super) fn certify_for_request(
    status: &mut Status,
    key: &[u8],
    primary: &[u8],
    start_ts: u64,
    for_update_ts: u64,
) {
    let Some(candidate) =
        std::error::Error::source(status).and_then(|source| source.downcast_ref::<Candidate>())
    else {
        return;
    };
    if candidate.start_ts != start_ts
        || candidate.key.as_slice() != key
        || candidate.primary.as_slice() != primary
        || for_update_ts < start_ts
        || candidate.conflict_commit_ts <= for_update_ts
    {
        return;
    }
    let response_bytes = candidate.response_bytes;
    status.set_source(Arc::new(Certified { response_bytes }));
}

pub(super) fn response_bytes(status: &Status) -> Option<usize> {
    std::error::Error::source(status)
        .and_then(|source| source.downcast_ref::<Certified>())
        .map(|source| source.response_bytes)
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;
    use crate::proto::kvrpcpb;

    fn reply() -> kvrpcpb::PessimisticLockResponse {
        kvrpcpb::PessimisticLockResponse {
            errors: vec![kvrpcpb::KeyError {
                retryable: "a bounded conflict diagnostic".into(),
                conflict: Some(kvrpcpb::WriteConflict {
                    start_ts: 10,
                    conflict_ts: 11,
                    key: b"key".to_vec(),
                    primary: b"primary".to_vec(),
                    conflict_commit_ts: 30,
                    reason: 2,
                }),
                ..Default::default()
            }],
            exec_details_v2: Some(kvrpcpb::ExecDetailsV2 {
                time_detail: Some(kvrpcpb::TimeDetail {
                    total_rpc_wall_time_ns: 1,
                    ..Default::default()
                }),
                time_detail_v2: Some(kvrpcpb::TimeDetailV2 {
                    total_rpc_wall_time_ns: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn push_varint(mut value: u64, bytes: &mut Vec<u8>) {
        while value >= 128 {
            bytes.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
    }

    fn scalar_field(field: u64, value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint(field << 3, &mut bytes);
        push_varint(value, &mut bytes);
        bytes
    }

    fn message_field(field: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint((field << 3) | 2, &mut bytes);
        push_varint(body.len() as u64, &mut bytes);
        bytes.extend_from_slice(body);
        bytes
    }

    fn reply_with_execution_details(details: &[u8]) -> Vec<u8> {
        let mut response = reply();
        response.exec_details_v2 = None;
        let mut bytes = response.encode_to_vec();
        bytes.extend_from_slice(&message_field(7, details));
        bytes
    }

    #[test]
    fn actual_lock_execution_shape_requires_exact_request_certification() {
        // Generated messages cover the known fields actually observed from
        // TiKV v8.5.3. Append its pinned-schema gRPC field 6 explicitly because
        // the generated client predates that field; never decode error values.
        let mut details = kvrpcpb::ExecDetailsV2 {
            time_detail: Some(kvrpcpb::TimeDetail {
                total_rpc_wall_time_ns: 1,
                ..Default::default()
            }),
            scan_detail_v2: Some(kvrpcpb::ScanDetailV2 {
                get_snapshot_nanos: 1,
                ..Default::default()
            }),
            write_detail: Some(kvrpcpb::WriteDetail {
                latch_wait_nanos: 1,
                process_nanos: 1,
                pessimistic_lock_wait_nanos: 1,
                ..Default::default()
            }),
            ..Default::default()
        }
        .encode_to_vec();
        let mut time = kvrpcpb::TimeDetailV2 {
            total_rpc_wall_time_ns: 1,
            ..Default::default()
        }
        .encode_to_vec();
        time.extend_from_slice(&scalar_field(6, 1));
        details.extend_from_slice(&message_field(4, &time));
        let decoded = kvrpcpb::ExecDetailsV2::decode(details.as_slice()).unwrap();
        assert_eq!(decoded.scan_detail_v2.unwrap().get_snapshot_nanos, 1);
        assert_eq!(decoded.write_detail.unwrap().pessimistic_lock_wait_nanos, 1);
        assert!(super::super::lock_conflict::validate_time_details(&details).is_none());

        let bytes = reply_with_execution_details(&details);
        let mut status = status_if_validated(&bytes, 4096).unwrap();
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"other", b"primary", 10, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"other", 10, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 9, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 10, 30);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 10, 20);
        assert_eq!(response_bytes(&status), Some(bytes.len()));
    }

    #[test]
    fn lock_execution_details_use_fixed_scalar_schemas_and_exact_byte_bound() {
        for (wrapper, last_field) in [(1, 4), (2, 13), (3, 17), (4, 7)] {
            let fields: Vec<u8> = (1..=last_field)
                .flat_map(|field| scalar_field(field, 1))
                .collect();
            let details = message_field(wrapper, &fields);
            assert!(status_if_validated(&reply_with_execution_details(&details), 4096).is_some());
        }
        // Seventeen distinct WriteDetail fields with canonical uint64 values.
        // Ten maximum-width values make the complete wrapper exactly 128B.
        let mut fields = Vec::new();
        for field in 1..=17 {
            let value = if field <= 10 { u64::MAX } else { 1 };
            fields.extend_from_slice(&scalar_field(field, value));
        }
        let details = message_field(3, &fields);
        assert_eq!(details.len(), 128);
        assert!(status_if_validated(&reply_with_execution_details(&details), 4096).is_some());
        let mut fields = Vec::new();
        for field in 1..=17 {
            let value = if field <= 10 {
                u64::MAX
            } else if field == 11 {
                128
            } else {
                1
            };
            fields.extend_from_slice(&scalar_field(field, value));
        }
        let details = message_field(3, &fields);
        assert_eq!(details.len(), 129);
        assert!(status_if_validated(&reply_with_execution_details(&details), 4096).is_none());
    }

    #[test]
    fn lock_execution_details_reject_unknown_duplicate_wire_and_malformed_fields() {
        let valid = message_field(1, &scalar_field(1, 1));
        let mut cases = vec![
            Vec::new(),
            message_field(5, &scalar_field(1, 1)),
            scalar_field(1, 1),
            message_field(1, &[]),
            [valid.clone(), valid].concat(),
            vec![0x8a, 0, 2, 8, 1],  // Noncanonical wrapper tag.
            vec![10, 0x82, 0, 8, 1], // Noncanonical body length.
            vec![10, 3, 8, 1],       // Truncated body.
            vec![10, 0x80],          // Truncated length.
        ];
        for (wrapper, last_field) in [(1, 4), (2, 13), (3, 17), (4, 7)] {
            let repeated = [scalar_field(1, 1), scalar_field(1, 2)].concat();
            let mut overflow = vec![8];
            overflow.extend_from_slice(&[0xff; 9]);
            overflow.push(2);
            for nested in [
                scalar_field(0, 1),
                scalar_field(last_field + 1, 1),
                message_field(1, &[0]),
                repeated,
                vec![0x88, 0, 1], // Noncanonical scalar tag.
                vec![8, 0x81, 0], // Noncanonical scalar value.
                vec![8, 0x80],    // Truncated scalar.
                overflow,
            ] {
                cases.push(message_field(wrapper, &nested));
            }
        }
        for details in cases {
            assert!(status_if_validated(&reply_with_execution_details(&details), 4096).is_none());
        }
    }

    #[test]
    fn protobuf_conflict_requires_request_identity_before_restart_authority() {
        let bytes = reply().encode_to_vec();
        let mut status = status_if_validated(&bytes, 4096).unwrap();
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"other", b"primary", 10, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"other", 10, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 9, 20);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 10, 30);
        assert_eq!(response_bytes(&status), None);
        certify_for_request(&mut status, b"key", b"primary", 10, 20);
        assert_eq!(response_bytes(&status), Some(bytes.len()));
        let mut remote = Status::failed_precondition(status.message());
        certify_for_request(&mut remote, b"key", b"primary", 10, 20);
        assert_eq!(response_bytes(&remote), None);
    }

    #[test]
    fn mixed_unknown_duplicate_oversize_and_other_reasons_never_classify() {
        for reason in [0, 1, 3, 4, 99] {
            let mut response = reply();
            response.errors[0].conflict.as_mut().unwrap().reason = reason;
            assert!(status_if_validated(&response.encode_to_vec(), 4096).is_none());
        }
        let mut mixed = reply();
        mixed.errors[0].locked = Some(kvrpcpb::LockInfo::default());
        assert!(status_if_validated(&mixed.encode_to_vec(), 4096).is_none());
        let mut duplicate = reply();
        duplicate.errors.push(duplicate.errors[0].clone());
        assert!(status_if_validated(&duplicate.encode_to_vec(), 4096).is_none());
        let mut oversized = reply();
        oversized.errors[0].retryable = "x".repeat(MAX_RETRYABLE_BYTES + 1);
        assert!(status_if_validated(&oversized.encode_to_vec(), 4096).is_none());
        let mut key_too_long = reply();
        key_too_long.errors[0].conflict.as_mut().unwrap().key = vec![b'k'; 4097];
        assert!(status_if_validated(&key_too_long.encode_to_vec(), 4096).is_none());
        let mut payload = reply();
        payload.values.push(b"value".to_vec());
        assert!(status_if_validated(&payload.encode_to_vec(), 4096).is_none());
        let mut region = reply();
        region.region_error = Some(Default::default());
        assert!(status_if_validated(&region.encode_to_vec(), 4096).is_none());
        for suffix in [&[0x7a, 0][..], &[0x12, 0], &[0x92, 0, 0]] {
            let mut bytes = reply().encode_to_vec();
            bytes.extend_from_slice(suffix);
            assert!(status_if_validated(&bytes, 4096).is_none());
        }
        assert!(status_if_validated(&[0x12, 0], 4096).is_none());
    }
}

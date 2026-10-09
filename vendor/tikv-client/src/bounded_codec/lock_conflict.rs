//! Validate one bounded Get lock error without materializing any protobuf body.
//! This module classifies a response; it never dispatches or resolves anything.

use std::sync::Arc;

use tonic::Status;

const MAX_BODY_BYTES: usize = 16 << 10;
const MAX_KEY_BYTES: usize = 4096;
const MAX_FIELDS: usize = 32;
const MAX_SECONDARIES: usize = 16;
const MAX_TIME_DETAILS_BYTES: usize = 128;

#[derive(Debug)]
struct ValidatedLockConflict;

impl std::fmt::Display for ValidatedLockConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("validated bounded Get lock conflict")
    }
}

impl std::error::Error for ValidatedLockConflict {}

pub(super) fn is_status(status: &Status) -> bool {
    std::error::Error::source(status).is_some_and(|source| source.is::<ValidatedLockConflict>())
}

pub(super) fn scalar(bytes: &mut &[u8]) -> Option<u64> {
    let before = bytes.len();
    let value = super::varint(bytes).ok()?;
    let used = before - bytes.len();
    // A local retry classification is stricter than generic protobuf decode.
    if used > 1 && value < (1u64 << (7 * (used - 1))) {
        return None;
    }
    Some(value)
}

pub(super) fn body<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = usize::try_from(scalar(bytes)?).ok()?;
    super::take(bytes, len).ok()
}

fn validate_lock(mut bytes: &[u8], key_limit: usize) -> Option<()> {
    if bytes.len() > MAX_BODY_BYTES || key_limit == 0 || key_limit > MAX_KEY_BYTES {
        return None;
    }
    let mut seen = 0u16;
    let mut secondary_count = 0;
    let mut lock_version = 0;
    let mut for_update_ts = 0;
    let mut min_commit_ts = 0;
    let mut lock_type = 0;
    let mut async_commit = false;
    for _ in 0..MAX_FIELDS {
        if bytes.is_empty() {
            break;
        }
        let tag = scalar(&mut bytes)?;
        let field = tag >> 3;
        let bit = match field {
            1..=11 => 1u16 << field,
            100 => 1 << 12,
            _ => return None,
        };
        if field != 10 && seen & bit != 0 {
            return None;
        }
        seen |= bit;
        match field {
            1 | 3 | 10 => {
                if tag & 7 != 2 {
                    return None;
                }
                let key = body(&mut bytes)?;
                if key.is_empty() || key.len() > key_limit {
                    return None;
                }
                if field == 10 {
                    secondary_count += 1;
                    if secondary_count > MAX_SECONDARIES {
                        return None;
                    }
                }
            }
            _ => {
                if tag & 7 != 0 {
                    return None;
                }
                let value = scalar(&mut bytes)?;
                match field {
                    2 => lock_version = value,
                    6 => {
                        // Actual lock operations; rollback/check-only records
                        // are not certified as retryable read lock conflicts.
                        if !matches!(value, 0 | 1 | 2 | 4 | 5) {
                            return None;
                        }
                        lock_type = value;
                    }
                    7 => for_update_ts = value,
                    8 => {
                        if value > 1 {
                            return None;
                        }
                        async_commit = value == 1;
                    }
                    9 => min_commit_ts = value,
                    100 => {
                        // File-based transactions are outside this policy.
                        if value != 0 {
                            return None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mandatory = (1 << 1) | (1 << 2) | (1 << 3);
    if !bytes.is_empty()
        || seen & mandatory != mandatory
        || lock_version == 0
        || (lock_type == 5 && for_update_ts == 0)
        || (async_commit && min_commit_ts <= lock_version)
        || (!async_commit && secondary_count != 0)
    {
        return None;
    }
    Some(())
}

pub(super) fn validate_time_details(mut bytes: &[u8]) -> Option<()> {
    // Real TiKV Get lock errors carry ExecDetailsV2's legacy and nanosecond
    // timing messages. Only these fixed scalar schemas are classified; scan,
    // write, empty, unknown and duplicate messages remain outside this policy.
    if bytes.is_empty() || bytes.len() > MAX_TIME_DETAILS_BYTES {
        return None;
    }
    let mut seen = 0u8;
    for _ in 0..2 {
        if bytes.is_empty() {
            break;
        }
        let tag = scalar(&mut bytes)?;
        let max_field = match tag {
            10 => 4, // TimeDetail: fields 1..=4, all uint64.
            34 => 5, // TimeDetailV2: fields 1..=5, all uint64.
            _ => return None,
        };
        let bit = 1 << (tag >> 3);
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
        let mut time = body(&mut bytes)?;
        if time.is_empty() {
            return None;
        }
        let mut scalars_seen = 0u8;
        for _ in 0..max_field {
            if time.is_empty() {
                break;
            }
            let tag = scalar(&mut time)?;
            let field = tag >> 3;
            if tag & 7 != 0 || field == 0 || field > max_field {
                return None;
            }
            let bit = 1 << field;
            if scalars_seen & bit != 0 {
                return None;
            }
            scalars_seen |= bit;
            scalar(&mut time)?;
        }
        if !time.is_empty() {
            return None;
        }
    }
    bytes.is_empty().then_some(())
}

pub(super) fn status_if_validated_get(mut response: &[u8], key_limit: usize) -> Option<Status> {
    // Require one entire error-only Get, optionally with the bounded timing
    // messages actually observed from TiKV. No region or payload is accepted.
    if response.len() > MAX_BODY_BYTES {
        return None;
    }
    let mut key_error = None;
    let mut timing_seen = false;
    for _ in 0..2 {
        if response.is_empty() {
            break;
        }
        match scalar(&mut response)? {
            18 if key_error.is_none() => key_error = Some(body(&mut response)?),
            50 if !timing_seen => {
                validate_time_details(body(&mut response)?)?;
                timing_seen = true;
            }
            _ => return None,
        }
    }
    if !response.is_empty() {
        return None;
    }
    let key_error = key_error?;
    let mut fields = key_error;
    if scalar(&mut fields)? != ((1 << 3) | 2) {
        return None;
    }
    let lock = body(&mut fields)?;
    if !fields.is_empty() {
        return None;
    }
    validate_lock(lock, key_limit)?;
    let mut status = super::read_error(super::Schema::Get, 2, key_error);
    status.set_source(Arc::new(ValidatedLockConflict));
    Some(status)
}

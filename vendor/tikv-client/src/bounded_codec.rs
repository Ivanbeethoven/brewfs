// Copyright 2026 TiKV Project Authors. Licensed under Apache-2.0.

//! Inspect bounded read protobuf structure before prost materializes vectors.
//! This borrows tonic's already byte-capped, contiguous message buffer.

use prost::bytes::Buf;
use prost::Message;
use tonic::codec::{Codec, DecodeBuf, Decoder, ProstCodec};
use tonic::Status;

mod authentication;
mod authentication_conflict;
mod lock_conflict;

pub(crate) fn is_lock_conflict(status: &Status) -> bool {
    lock_conflict::is_status(status)
}

pub(crate) fn certify_authentication_write_conflict(
    status: &mut Status,
    key: &[u8],
    primary: &[u8],
    start_ts: u64,
    for_update_ts: u64,
) {
    authentication_conflict::certify_for_request(status, key, primary, start_ts, for_update_ts);
}

pub(crate) fn authentication_write_conflict_response_bytes(status: &Status) -> Option<usize> {
    authentication_conflict::response_bytes(status)
}

#[derive(Clone, Copy)]
pub(crate) enum Schema {
    Get,
    PessimisticLock,
    Prewrite,
    Commit,
    Rollback,
    Scan,
    Pair,
    Members,
    Member,
    MemberMap,
    RegionReply,
    Region,
    StoreReply,
    Store,
    StoreStats,
    Stores,
    Tso,
    Header,
    Leaf,
}

pub(crate) struct BoundedCodec<T, U> {
    inner: ProstCodec<T, U>,
    schema: Option<Schema>,
    lock_conflict_key_bytes: Option<usize>,
}

impl<T, U> BoundedCodec<T, U> {
    pub(crate) fn new(enabled: bool, schema: Schema) -> Self {
        Self {
            inner: ProstCodec::default(),
            schema: enabled.then_some(schema),
            lock_conflict_key_bytes: None,
        }
    }

    pub(crate) fn with_lock_conflict_key_bytes(mut self, max_key_bytes: Option<usize>) -> Self {
        self.lock_conflict_key_bytes = max_key_bytes;
        self
    }
}

pub(crate) struct BoundedDecoder<D> {
    inner: D,
    schema: Option<Schema>,
    lock_conflict_key_bytes: Option<usize>,
}

impl<T: Message + Send + 'static, U: Message + Default + Send + 'static> Codec
    for BoundedCodec<T, U>
{
    type Encode = T;
    type Decode = U;
    type Encoder = <ProstCodec<T, U> as Codec>::Encoder;
    type Decoder = BoundedDecoder<<ProstCodec<T, U> as Codec>::Decoder>;
    fn encoder(&mut self) -> Self::Encoder {
        self.inner.encoder()
    }
    fn decoder(&mut self) -> Self::Decoder {
        BoundedDecoder {
            inner: self.inner.decoder(),
            schema: self.schema,
            lock_conflict_key_bytes: self.lock_conflict_key_bytes,
        }
    }
}

impl<D: Decoder<Error = Status>> Decoder for BoundedDecoder<D> {
    type Item = D::Item;
    type Error = Status;
    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Status> {
        if let Some(schema) = self.schema {
            if buf.chunk().len() != buf.remaining() {
                return Err(Status::internal(
                    "bounded protobuf buffer is not contiguous",
                ));
            }
            if matches!(schema, Schema::Get) {
                if let Some(key_bytes) = self.lock_conflict_key_bytes {
                    if let Some(status) =
                        lock_conflict::status_if_validated_get(buf.chunk(), key_bytes)
                    {
                        return Err(status);
                    }
                }
            }
            if matches!(schema, Schema::PessimisticLock) {
                if let Some(key_bytes) = self.lock_conflict_key_bytes {
                    if let Some(status) =
                        authentication_conflict::status_if_validated(buf.chunk(), key_bytes)
                    {
                        return Err(status);
                    }
                }
            }
            if matches!(
                schema,
                Schema::PessimisticLock | Schema::Prewrite | Schema::Commit | Schema::Rollback
            ) {
                authentication::inspect(buf.chunk(), schema)?;
            } else {
                inspect(buf.chunk(), schema, 0, &mut 0)?;
            }
        }
        self.inner.decode(buf)
    }
}

fn malformed() -> Status {
    Status::internal("malformed bounded protobuf response")
}
fn limit() -> Status {
    Status::resource_exhausted("bounded protobuf structure limit exceeded")
}

fn varint(bytes: &mut &[u8]) -> Result<u64, Status> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = bytes.split_first().ok_or_else(malformed)?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            return Err(malformed());
        }
        value |= ((byte & 127) as u64) << shift;
        if byte < 128 {
            return Ok(value);
        }
    }
    Err(malformed())
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8], Status> {
    if len > bytes.len() {
        return Err(malformed());
    }
    let (field, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(field)
}

// Fixed diagnostic metadata only. These bytes never include server strings,
// keys, lock bodies, or nested protobuf contents and never authorize a retry.
#[derive(Clone, Copy)]
#[repr(u8)]
enum ReadErrorFamily {
    Region = 1,
    Key = 2,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum ReadErrorShape {
    Complete = 0,
    Unknown = 1,
    Ambiguous = 2,
    Malformed = 3,
    FieldLimit = 4,
}

fn error_subtype(mut bytes: &[u8], family: ReadErrorFamily) -> (u8, ReadErrorShape) {
    let mut subtype = 0;
    let mut unknown = false;
    let mut ambiguous = false;
    // At most 32 shallow fields; nested bodies are borrowed and skipped.
    for _ in 0..32 {
        if bytes.is_empty() {
            return if unknown || subtype == 0 {
                (0, ReadErrorShape::Unknown)
            } else if ambiguous {
                (0, ReadErrorShape::Ambiguous)
            } else {
                (subtype, ReadErrorShape::Complete)
            };
        }
        let Ok(tag) = varint(&mut bytes) else {
            return (0, ReadErrorShape::Malformed);
        };
        let field = tag >> 3;
        if field == 0 || field > 0x1fff_ffff {
            return (0, ReadErrorShape::Malformed);
        }
        let known = match family {
            ReadErrorFamily::Region => (1..=21).contains(&field),
            ReadErrorFamily::Key => (1..=11).contains(&field),
        };
        // Every current region/key error field is length-delimited.
        if known && tag & 7 != 2 {
            return (0, ReadErrorShape::Malformed);
        }
        let skipped = match tag & 7 {
            0 => varint(&mut bytes).map(|_| ()),
            1 => take(&mut bytes, 8).map(|_| ()),
            5 => take(&mut bytes, 4).map(|_| ()),
            2 => varint(&mut bytes).and_then(|len| {
                let len = usize::try_from(len).map_err(|_| malformed())?;
                take(&mut bytes, len).map(|_| ())
            }),
            _ => return (0, ReadErrorShape::Malformed),
        };
        if skipped.is_err() {
            return (0, ReadErrorShape::Malformed);
        }
        if !known {
            unknown = true;
        } else if !matches!((family, field), (ReadErrorFamily::Region, 1)) {
            ambiguous |= subtype != 0;
            subtype = field as u8;
        }
    }
    if bytes.is_empty() {
        if unknown || subtype == 0 {
            (0, ReadErrorShape::Unknown)
        } else if ambiguous {
            (0, ReadErrorShape::Ambiguous)
        } else {
            (subtype, ReadErrorShape::Complete)
        }
    } else {
        (0, ReadErrorShape::FieldLimit)
    }
}

fn read_error(schema: Schema, field: u64, body: &[u8]) -> Status {
    let location = match schema {
        Schema::Get => 1,
        Schema::Scan => 2,
        Schema::Pair => 3,
        _ => 0,
    };
    let family = if matches!((schema, field), (Schema::Get | Schema::Scan, 1)) {
        ReadErrorFamily::Region
    } else {
        ReadErrorFamily::Key
    };
    let (subtype, shape) = error_subtype(body, family);
    let details = [
        b'B',
        b'R',
        b'E',
        location,
        family as u8,
        subtype,
        shape as u8,
    ];
    Status::with_details(
        tonic::Code::FailedPrecondition,
        "bounded read returned a key or region error",
        prost::bytes::Bytes::copy_from_slice(&details),
    )
}

fn inspect(
    mut bytes: &[u8],
    schema: Schema,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), Status> {
    // PD includes per-thread CPU/read/write metrics in GetStore. A normal
    // TiKV process can report more than 64 threads in each array. Keep both
    // per-array and aggregate structure bounds inside the 16 KiB envelope.
    if depth > 8 || *nodes >= 512 {
        return Err(limit());
    }
    *nodes += 1;
    let mut counts = [0u16; 32];
    while !bytes.is_empty() {
        let tag = varint(&mut bytes)?;
        let field = tag >> 3;
        if field == 0 {
            return Err(malformed());
        }
        if field < 32 {
            let count = &mut counts[field as usize];
            *count += 1;
            let maximum = match (schema, field) {
                (Schema::Scan, 2) => 1,
                (Schema::Members, 2 | 5) => 16,
                (Schema::Member, 3 | 4) => 4,
                (Schema::StoreStats, 16..=20 | 25) => 128,
                _ => 64,
            };
            if *count > maximum {
                return Err(limit());
            }
        }
        match tag & 7 {
            0 => {
                varint(&mut bytes)?;
            }
            1 => {
                take(&mut bytes, 8)?;
            }
            5 => {
                take(&mut bytes, 4)?;
            }
            2 => {
                let length = usize::try_from(varint(&mut bytes)?).map_err(|_| malformed())?;
                let body = take(&mut bytes, length)?;
                // Bounded reads fail on any lock/region error without cloning
                // or formatting arbitrary nested error vectors. Native retry
                // clients keep their original protobuf error behavior.
                if matches!(
                    (schema, field),
                    (Schema::Get, 1 | 2) | (Schema::Scan, 1 | 3) | (Schema::Pair, 1)
                ) {
                    return Err(read_error(schema, field, body));
                }
                let child = match (schema, field) {
                    (Schema::Scan, 2) => Some(Schema::Pair),
                    (Schema::Get, 6) => Some(Schema::Leaf),
                    (Schema::Members, 1)
                    | (Schema::RegionReply, 1)
                    | (Schema::StoreReply, 1)
                    | (Schema::Stores, 1)
                    | (Schema::Tso, 1) => Some(Schema::Header),
                    (Schema::Members, 2 | 3 | 4) | (Schema::MemberMap, 2) => Some(Schema::Member),
                    (Schema::Members, 5) => Some(Schema::MemberMap),
                    (Schema::RegionReply, 2) => Some(Schema::Region),
                    (Schema::RegionReply, 3 | 5 | 6) => Some(Schema::Leaf),
                    (Schema::RegionReply, 7) => return Err(limit()), // need_buckets is false
                    (Schema::Region, 4 | 5 | 6) => Some(Schema::Leaf),
                    (Schema::StoreReply, 2) | (Schema::Stores, 2) => Some(Schema::Store),
                    (Schema::StoreReply, 3) => Some(Schema::StoreStats),
                    (Schema::Store, 4) => Some(Schema::Leaf),
                    (Schema::StoreStats, 15..=21 | 25 | 26) => Some(Schema::Leaf),
                    (Schema::Tso, 3) => Some(Schema::Leaf),
                    (Schema::Header, 2) => {
                        return Err(Status::failed_precondition(
                            "bounded PD request returned an error",
                        ))
                    }
                    _ => None,
                };
                if let Some(child) = child {
                    inspect(body, child, depth + 1, nodes)?;
                }
            }
            _ => return Err(malformed()),
        }
    }
    Ok(())
}

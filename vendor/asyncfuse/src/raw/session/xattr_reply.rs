//! One encoder for worker and serial extended-attribute replies.
use crate::helper::*;
use crate::raw::abi::*;
use crate::raw::reply::ReplyXAttr;
use bincode::Options;
#[cfg(test)]
use bytes::Bytes;
use futures_util::future::Either;

pub(super) fn encode_xattr_reply(
    reply: ReplyXAttr,
    unique: u64,
    requested: u32,
) -> super::FuseData {
    let error_reply = || {
        let header = fuse_out_header {
            len: FUSE_OUT_HEADER_SIZE as u32,
            error: -libc::ERANGE,
            unique,
        };
        let mut data = Vec::with_capacity(FUSE_OUT_HEADER_SIZE);
        get_bincode_config()
            .serialize_into(&mut data, &header)
            .expect("serialize xattr error header");
        Either::Left(data)
    };
    match reply {
        ReplyXAttr::Size(size) if requested == 0 => {
            let header = fuse_out_header {
                len: (FUSE_OUT_HEADER_SIZE + FUSE_GETXATTR_OUT_SIZE) as u32,
                error: 0,
                unique,
            };
            let mut data = Vec::with_capacity(FUSE_OUT_HEADER_SIZE + FUSE_GETXATTR_OUT_SIZE);
            get_bincode_config()
                .serialize_into(&mut data, &header)
                .expect("serialize xattr size header");
            get_bincode_config()
                .serialize_into(&mut data, &fuse_getxattr_out { size, _padding: 0 })
                .expect("serialize xattr size");
            Either::Left(data)
        }
        ReplyXAttr::Data(payload) if requested > 0 && payload.len() <= requested as usize => {
            let Some(length) = payload
                .len()
                .checked_add(FUSE_OUT_HEADER_SIZE)
                .and_then(|length| u32::try_from(length).ok())
            else {
                return error_reply();
            };
            let header = fuse_out_header {
                len: length,
                error: 0,
                unique,
            };
            let mut data = Vec::with_capacity(FUSE_OUT_HEADER_SIZE);
            get_bincode_config()
                .serialize_into(&mut data, &header)
                .expect("serialize xattr data header");
            Either::Right((data, payload.into()))
        }
        _ => error_reply(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xattr_binary_payload_is_sent_once_with_exact_header_length() {
        for payload in [
            Bytes::new(),
            Bytes::from_static(b"\0\xffcold"),
            Bytes::from_static(b"user.test\0user.other\0"),
        ] {
            let Either::Right((header, data)) =
                encode_xattr_reply(ReplyXAttr::Data(payload.clone()), 123, 4096)
            else {
                panic!("expected data reply");
            };
            assert_eq!(header.len(), FUSE_OUT_HEADER_SIZE);
            assert_eq!(
                u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize,
                header.len() + data.len()
            );
            assert_eq!(i32::from_le_bytes(header[4..8].try_into().unwrap()), 0);
            assert_eq!(u64::from_le_bytes(header[8..16].try_into().unwrap()), 123);
            assert_eq!(data, payload);
        }
    }
    #[test]
    fn xattr_size_probe_is_a_successful_size_reply() {
        let Either::Left(bytes) = encode_xattr_reply(ReplyXAttr::Size(6), 77, 0) else {
            panic!("expected size reply");
        };
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 6);
    }
    #[test]
    fn xattr_small_buffer_is_a_negative_erange_without_payload() {
        for reply in [
            ReplyXAttr::Data(Bytes::from_static(b"long")),
            ReplyXAttr::Size(4),
        ] {
            let Either::Left(bytes) = encode_xattr_reply(reply, 42, 2) else {
                panic!("expected error reply");
            };
            assert_eq!(bytes.len(), FUSE_OUT_HEADER_SIZE);
            assert_eq!(
                i32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                -libc::ERANGE
            );
        }
    }
}

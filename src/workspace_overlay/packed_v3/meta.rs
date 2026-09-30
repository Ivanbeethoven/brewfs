//! Canonical, bounded metadata pages stored inside a packed group.
//!
//! A group is deliberately small enough to page, but it must not require a
//! namespace lookup for every file.  `GroupMeta` therefore stores the sorted
//! directory entries and the data locator for each file in one authenticated
//! byte string.  Names use front coding against the previous name; all other
//! fields are fixed width so a decoder never has to allocate based on an
//! untrusted varint.

use std::sync::Arc;

use super::wire::{PackedResult, PackedWireError, Reader, Writer};

// GM06 is the current packed-v3 metadata format. GM04 was used by the
// development codec when each entry carried one extent; accepting it here
// would make the decoder interpret the old trailing fields as an extent
// count and could silently produce a wrong read plan. Keep the version
// marker strict until an explicit migration decoder exists. GM05 remains a
// read-only compatibility format because it has no inline-payload field.
const GROUP_META_MAGIC: &[u8; 4] = b"GM06";
const LEGACY_GROUP_META_MAGIC: &[u8; 4] = b"GM05";
pub(crate) const GROUP_META_HEADER_LEN: usize = 12;
// Prefix/suffix lengths, kind/flags, mode, inode, size, POSIX hot
// attributes, and extent count.  Keeping this lower bound explicit lets the
// decoder reject a forged entry count before allocating its vector.
const GROUP_META_MIN_ENTRY_BYTES: usize = 76;
const MAX_GROUP_META_ENTRIES: u32 = 1_048_576;
const MAX_ENTRY_EXTENTS: usize = 1024;
const MAX_NAME_LEN: usize = 1024;
pub(crate) const MAX_GROUP_META_BYTES: usize = 256 * 1024;
/// Files below this threshold may be stored directly in the authenticated
/// GroupMeta range.  The strict inequality leaves the envelope and record
/// headers outside the payload budget.
pub const INLINE_FILE_MAX_BYTES: usize = 256 * 1024;
/// Keep enough room for names and POSIX hot attributes when a group contains
/// more than one inline file.  A group that exceeds this budget continues to
/// use ordinary dynamic frames for the remaining files.
pub(crate) const INLINE_GROUP_DATA_BUDGET_BYTES: usize = 224 * 1024;
pub const INLINE_DATA_FLAG: u8 = 0x01;

/// One physical extent belonging to a file in a group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupMetaExtent {
    pub file_offset: u64,
    pub logical_len: u32,
    pub frame_ordinal: u32,
    pub raw_offset: u32,
    pub raw_len: u32,
}

/// One authenticated directory entry in a group metadata page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupMetaEntry {
    pub name: Vec<u8>,
    pub inode: u64,
    /// POSIX file kind (the same compact kind used by the frozen catalog).
    pub kind: u8,
    pub mode: u32,
    /// Owner and device metadata needed by a read-only POSIX mount.
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub nlink: u32,
    pub atime_ns: i64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub size: u64,
    pub flags: u8,
    /// Contents embedded in the GroupMeta page for one small immutable file.
    /// An empty vector means the file is represented by `extents` (or is a
    /// zero-length file).  The flag is persisted separately so a malformed
    /// record cannot silently reinterpret an empty payload.
    pub inline_data: Arc<[u8]>,
    pub extents: Vec<GroupMetaExtent>,
}

impl GroupMetaEntry {
    fn validate(&self) -> PackedResult<()> {
        validate_name(&self.name)?;
        if self.extents.len() > MAX_ENTRY_EXTENTS {
            return Err(PackedWireError::LimitExceeded(
                "group entry extent count exceeds limit".into(),
            ));
        }
        if !self.inline_data.is_empty() {
            if self.kind != 1 {
                return Err(PackedWireError::Invalid(
                    "inline group metadata payload requires a regular file".into(),
                ));
            }
            if self.inline_data.len() >= INLINE_FILE_MAX_BYTES {
                return Err(PackedWireError::LimitExceeded(
                    "inline group metadata file exceeds 256 KiB".into(),
                ));
            }
            if self.size != self.inline_data.len() as u64 || !self.extents.is_empty() {
                return Err(PackedWireError::Invalid(
                    "inline group metadata file must have an exact size and no extents".into(),
                ));
            }
            if self.flags & INLINE_DATA_FLAG == 0 {
                return Err(PackedWireError::Invalid(
                    "inline group metadata file is missing its inline flag".into(),
                ));
            }
        } else if self.flags & INLINE_DATA_FLAG != 0 && self.kind != 1 {
            return Err(PackedWireError::Invalid(
                "inline group metadata flag requires a regular file".into(),
            ));
        } else if self.flags & INLINE_DATA_FLAG != 0 && self.size != 0 {
            return Err(PackedWireError::Invalid(
                "non-empty group metadata file has an inline flag without payload".into(),
            ));
        }
        let mut previous_end = 0u64;
        for extent in &self.extents {
            if extent.logical_len == 0 || extent.raw_len == 0 {
                return Err(PackedWireError::Invalid(
                    "group entry extent lengths must be non-zero".into(),
                ));
            }
            let file_end = extent
                .file_offset
                .checked_add(u64::from(extent.logical_len))
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("group entry extent overflows".into())
                })?;
            if file_end > self.size || extent.file_offset < previous_end {
                return Err(PackedWireError::Invalid(
                    "group entry extents overlap or exceed file size".into(),
                ));
            }
            let raw_end = extent
                .raw_offset
                .checked_add(extent.logical_len)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("group entry raw extent overflows".into())
                })?;
            if raw_end > extent.raw_len || u64::from(extent.raw_len) > 8 * 1024 * 1024 {
                return Err(PackedWireError::Invalid(
                    "group entry extent exceeds the frame limit".into(),
                ));
            }
            previous_end = file_end;
        }
        Ok(())
    }
}

/// Canonical metadata for one directory group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupMeta {
    entries: Vec<GroupMetaEntry>,
}

impl GroupMeta {
    pub fn new(entries: Vec<GroupMetaEntry>) -> PackedResult<Self> {
        let meta = Self { entries };
        meta.validate()?;
        Ok(meta)
    }

    pub fn entries(&self) -> &[GroupMetaEntry] {
        &self.entries
    }

    pub(crate) fn entries_mut(&mut self) -> &mut [GroupMetaEntry] {
        &mut self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Approximate the retained decoded footprint for byte-budgeted caches.
    /// The estimate intentionally includes owned names, inline payloads and
    /// extent vectors, which are the parts that dominate a large snapshot.
    pub(crate) fn decoded_weight(&self) -> u32 {
        let bytes = self.entries.iter().fold(0usize, |total, entry| {
            total
                .saturating_add(96)
                .saturating_add(entry.name.len())
                .saturating_add(entry.inline_data.len())
                .saturating_add(entry.extents.len().saturating_mul(28))
        });
        u32::try_from(bytes.min(u32::MAX as usize)).unwrap_or(u32::MAX)
    }

    /// Return a bounded page without copying the complete group.
    pub fn page(&self, start: usize, limit: usize) -> &[GroupMetaEntry] {
        if start >= self.entries.len() || limit == 0 {
            return &[];
        }
        let end = start.saturating_add(limit).min(self.entries.len());
        &self.entries[start..end]
    }

    pub fn lookup(&self, name: &[u8]) -> Option<&GroupMetaEntry> {
        self.entries
            .binary_search_by(|entry| entry.name.as_slice().cmp(name))
            .ok()
            .map(|index| &self.entries[index])
    }

    pub fn encode(&self) -> PackedResult<Vec<u8>> {
        self.validate()?;
        let mut writer = Writer::default();
        writer.bytes(GROUP_META_MAGIC);
        writer.u32(self.entries.len() as u32);
        writer.u32(0);
        let mut previous = Vec::new();
        for entry in &self.entries {
            let prefix = common_prefix_len(&previous, &entry.name);
            let suffix = &entry.name[prefix..];
            let prefix = u16::try_from(prefix).map_err(|_| {
                PackedWireError::LimitExceeded("group metadata name prefix exceeds u16".into())
            })?;
            let suffix_len = u16::try_from(suffix.len()).map_err(|_| {
                PackedWireError::LimitExceeded("group metadata name suffix exceeds u16".into())
            })?;
            writer.u16(prefix);
            writer.u16(suffix_len);
            writer.bytes(suffix);
            writer.u8(entry.kind);
            writer.u8(entry.flags);
            writer.u32(entry.mode);
            writer.u32(entry.uid);
            writer.u32(entry.gid);
            writer.u64(entry.rdev);
            writer.u32(entry.nlink);
            writer.i64(entry.atime_ns);
            writer.i64(entry.mtime_ns);
            writer.i64(entry.ctime_ns);
            writer.u64(entry.inode);
            writer.u64(entry.size);
            writer.u16(u16::try_from(entry.extents.len()).map_err(|_| {
                PackedWireError::LimitExceeded("group entry extent count exceeds u16".into())
            })?);
            for extent in &entry.extents {
                writer.u64(extent.file_offset);
                writer.u32(extent.logical_len);
                writer.u32(extent.frame_ordinal);
                writer.u32(extent.raw_offset);
                writer.u32(extent.raw_len);
            }
            writer.u32(u32::try_from(entry.inline_data.len()).map_err(|_| {
                PackedWireError::LimitExceeded("inline group metadata payload exceeds u32".into())
            })?);
            writer.bytes(entry.inline_data.as_ref());
            previous.clear();
            previous.extend_from_slice(&entry.name);
        }
        let bytes = writer.finish();
        if bytes.len() < GROUP_META_HEADER_LEN || bytes.len() > MAX_GROUP_META_BYTES {
            return Err(PackedWireError::LimitExceeded(
                "group metadata page exceeds 256 KiB".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> PackedResult<Self> {
        if bytes.len() < GROUP_META_HEADER_LEN {
            return Err(PackedWireError::Truncated {
                what: "group metadata header",
                need: GROUP_META_HEADER_LEN,
                have: bytes.len(),
            });
        }
        if bytes.len() > MAX_GROUP_META_BYTES {
            return Err(PackedWireError::LimitExceeded(
                "group metadata page exceeds 256 KiB".into(),
            ));
        }
        let mut reader = Reader::new(bytes);
        let magic = reader.take(4)?;
        let has_inline_payload = if magic == GROUP_META_MAGIC {
            true
        } else if magic == LEGACY_GROUP_META_MAGIC {
            false
        } else {
            return Err(PackedWireError::UnsupportedFormat(
                "group metadata payload version mismatch".into(),
            ));
        };
        let count = reader.u32()?;
        if count > MAX_GROUP_META_ENTRIES {
            return Err(PackedWireError::LimitExceeded(
                "group metadata entry count exceeds limit".into(),
            ));
        }
        let available = bytes.len().saturating_sub(GROUP_META_HEADER_LEN);
        let min_entry_bytes = if has_inline_payload {
            GROUP_META_MIN_ENTRY_BYTES
        } else {
            // GM05 did not carry the inline-length field.
            GROUP_META_MIN_ENTRY_BYTES - 4
        };
        if count as usize > available / min_entry_bytes {
            return Err(PackedWireError::Invalid(
                "group metadata entry count exceeds the payload budget".into(),
            ));
        }
        reader.skip_zeroes(4)?;
        let mut entries = Vec::with_capacity(count as usize);
        let mut previous = Vec::new();
        for _ in 0..count {
            let prefix = usize::from(reader.u16()?);
            let suffix_len = usize::from(reader.u16()?);
            if prefix > previous.len() || prefix + suffix_len > MAX_NAME_LEN {
                return Err(PackedWireError::Invalid(
                    "group metadata name prefix/suffix is invalid".into(),
                ));
            }
            let suffix = reader.bytes(suffix_len)?;
            let mut name = Vec::with_capacity(prefix + suffix_len);
            name.extend_from_slice(&previous[..prefix]);
            name.extend_from_slice(suffix);
            if prefix != common_prefix_len(&previous, &name) {
                return Err(PackedWireError::Invalid(
                    "group metadata name prefix is not canonical".into(),
                ));
            }
            let kind = reader.u8()?;
            let flags = reader.u8()?;
            let mode = reader.u32()?;
            let uid = reader.u32()?;
            let gid = reader.u32()?;
            let rdev = reader.u64()?;
            let nlink = reader.u32()?;
            let atime_ns = reader.i64()?;
            let mtime_ns = reader.i64()?;
            let ctime_ns = reader.i64()?;
            let inode = reader.u64()?;
            let size = reader.u64()?;
            let extent_count = usize::from(reader.u16()?);
            if extent_count > MAX_ENTRY_EXTENTS {
                return Err(PackedWireError::LimitExceeded(
                    "group entry extent count exceeds limit".into(),
                ));
            }
            let mut extents = Vec::with_capacity(extent_count);
            for _ in 0..extent_count {
                extents.push(GroupMetaExtent {
                    file_offset: reader.u64()?,
                    logical_len: reader.u32()?,
                    frame_ordinal: reader.u32()?,
                    raw_offset: reader.u32()?,
                    raw_len: reader.u32()?,
                });
            }
            let inline_data = if has_inline_payload {
                let length = usize::try_from(reader.u32()?).map_err(|_| {
                    PackedWireError::LimitExceeded(
                        "inline group metadata length exceeds usize".into(),
                    )
                })?;
                if length >= INLINE_FILE_MAX_BYTES {
                    return Err(PackedWireError::LimitExceeded(
                        "inline group metadata file exceeds 256 KiB".into(),
                    ));
                }
                Arc::from(reader.bytes(length)?)
            } else {
                Arc::from([])
            };
            let entry = GroupMetaEntry {
                name,
                kind,
                flags,
                mode,
                uid,
                gid,
                rdev,
                nlink,
                atime_ns,
                mtime_ns,
                ctime_ns,
                inode,
                size,
                inline_data,
                extents,
            };
            entry.validate()?;
            if !previous.is_empty() && previous.as_slice() >= entry.name.as_slice() {
                return Err(PackedWireError::Invalid(
                    "group metadata names are not strictly sorted".into(),
                ));
            }
            previous = entry.name.clone();
            entries.push(entry);
        }
        if !reader.is_empty() {
            return Err(PackedWireError::Invalid(
                "group metadata has trailing bytes".into(),
            ));
        }
        Self::new(entries)
    }

    fn validate(&self) -> PackedResult<()> {
        if self.entries.len() as u64 > u64::from(MAX_GROUP_META_ENTRIES) {
            return Err(PackedWireError::LimitExceeded(
                "group metadata entry count exceeds limit".into(),
            ));
        }
        let mut previous: Option<&[u8]> = None;
        for entry in &self.entries {
            entry.validate()?;
            if let Some(previous) = previous {
                if previous >= entry.name.as_slice() {
                    return Err(PackedWireError::Invalid(
                        "group metadata names must be strictly sorted".into(),
                    ));
                }
            }
            previous = Some(&entry.name);
        }
        Ok(())
    }
}

fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

fn validate_name(name: &[u8]) -> PackedResult<()> {
    if name.is_empty() || name.len() > MAX_NAME_LEN || name.contains(&0) || name.contains(&b'/') {
        return Err(PackedWireError::Invalid(
            "group metadata name is empty, too long, or contains NUL/slash".into(),
        ));
    }
    if name == b"." || name == b".." {
        return Err(PackedWireError::Invalid(
            "group metadata name cannot be a dot component".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &[u8], inode: u64) -> GroupMetaEntry {
        GroupMetaEntry {
            name: name.to_vec(),
            inode,
            kind: 1,
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            nlink: 1,
            atime_ns: 11,
            mtime_ns: 12,
            ctime_ns: 13,
            size: 7,
            flags: 0,
            inline_data: Arc::from([]),
            extents: vec![GroupMetaExtent {
                file_offset: 0,
                logical_len: 7,
                frame_ordinal: 0,
                raw_offset: 0,
                raw_len: 7,
            }],
        }
    }

    #[test]
    fn front_coded_metadata_round_trips_and_pages() {
        let meta = GroupMeta::new(vec![entry(b"alpha", 1), entry(b"alphabet", 2)]).unwrap();
        let encoded = meta.encode().unwrap();
        assert!(encoded.len() < 256 * 1024);
        let decoded = GroupMeta::decode(&encoded).unwrap();
        assert_eq!(decoded, meta);
        assert_eq!(decoded.lookup(b"alphabet").unwrap().inode, 2);
        assert_eq!(decoded.page(1, 1)[0].name, b"alphabet");
    }

    #[test]
    fn inline_payload_round_trips_without_an_extent() {
        let payload = b"inline-small-file".to_vec();
        let meta = GroupMeta::new(vec![GroupMetaEntry {
            name: b"tiny.bin".to_vec(),
            inode: 3,
            kind: 1,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            rdev: 0,
            nlink: 1,
            atime_ns: 0,
            mtime_ns: 0,
            ctime_ns: 0,
            size: payload.len() as u64,
            flags: INLINE_DATA_FLAG,
            inline_data: Arc::from(payload.clone()),
            extents: Vec::new(),
        }])
        .unwrap();
        let decoded = GroupMeta::decode(&meta.encode().unwrap()).unwrap();
        assert_eq!(
            decoded.entries()[0].inline_data.as_ref(),
            payload.as_slice()
        );
        assert!(decoded.entries()[0].extents.is_empty());
    }

    #[test]
    fn legacy_gm05_payload_remains_readable() {
        let mut writer = Writer::default();
        writer.bytes(LEGACY_GROUP_META_MAGIC);
        writer.u32(1);
        writer.u32(0);
        writer.u16(0);
        writer.u16(1);
        writer.bytes(b"a");
        writer.u8(1);
        writer.u8(0);
        writer.u32(0o100644);
        writer.u32(1000);
        writer.u32(1000);
        writer.u64(0);
        writer.u32(1);
        writer.i64(11);
        writer.i64(12);
        writer.i64(13);
        writer.u64(7);
        writer.u64(7);
        writer.u16(1);
        writer.u64(0);
        writer.u32(7);
        writer.u32(0);
        writer.u32(0);
        writer.u32(7);

        let decoded = GroupMeta::decode(&writer.finish()).unwrap();
        assert_eq!(decoded.entries()[0].name, b"a");
        assert!(decoded.entries()[0].inline_data.is_empty());
        assert_eq!(decoded.entries()[0].extents[0].logical_len, 7);
    }

    #[test]
    fn metadata_rejects_unsorted_and_path_names() {
        assert!(GroupMeta::new(vec![entry(b"z", 1), entry(b"a", 2)]).is_err());
        assert!(GroupMeta::new(vec![entry(b"a/b", 1)]).is_err());
        assert!(GroupMeta::new(vec![entry(b".", 1)]).is_err());
    }

    #[test]
    fn metadata_rejects_trailing_bytes() {
        let meta = GroupMeta::new(vec![entry(b"a", 1)]).unwrap();
        let mut encoded = meta.encode().unwrap();
        encoded.push(0);
        assert!(GroupMeta::decode(&encoded).is_err());
    }

    #[test]
    fn metadata_rejects_the_incompatible_single_extent_magic() {
        let meta = GroupMeta::new(vec![entry(b"a", 1)]).unwrap();
        let mut encoded = meta.encode().unwrap();
        encoded[..4].copy_from_slice(b"GM04");
        assert!(matches!(
            GroupMeta::decode(&encoded),
            Err(PackedWireError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn metadata_rejects_oversized_input_before_allocating_entries() {
        let bytes = vec![0u8; MAX_GROUP_META_BYTES + 1];
        assert!(matches!(
            GroupMeta::decode(&bytes),
            Err(PackedWireError::LimitExceeded(_))
        ));
    }

    #[test]
    fn metadata_round_trips_multiple_extents_and_sparse_hole() {
        let meta = GroupMeta::new(vec![GroupMetaEntry {
            name: b"large.bin".to_vec(),
            inode: 9,
            kind: 1,
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            nlink: 1,
            atime_ns: 11,
            mtime_ns: 12,
            ctime_ns: 13,
            size: 12,
            flags: 0,
            inline_data: Arc::from([]),
            extents: vec![
                GroupMetaExtent {
                    file_offset: 0,
                    logical_len: 4,
                    frame_ordinal: 0,
                    raw_offset: 0,
                    raw_len: 4,
                },
                GroupMetaExtent {
                    file_offset: 8,
                    logical_len: 4,
                    frame_ordinal: 1,
                    raw_offset: 0,
                    raw_len: 4,
                },
            ],
        }])
        .unwrap();
        assert_eq!(GroupMeta::decode(&meta.encode().unwrap()).unwrap(), meta);
    }
}

//! Canonical, bounded metadata pages stored inside a packed group.
//!
//! A group is deliberately small enough to page, but it must not require a
//! namespace lookup for every file.  `GroupMeta` therefore stores the sorted
//! directory entries and the data locator for each file in one authenticated
//! byte string.  Names use front coding against the previous name; all other
//! fields are fixed width so a decoder never has to allocate based on an
//! untrusted varint.

use super::wire::{PackedResult, PackedWireError, Reader, Writer};

// GM05 is the first packed-v3 metadata format. GM04 was used by the
// development codec when each entry carried one extent; accepting it here
// would make the decoder interpret the old trailing fields as an extent
// count and could silently produce a wrong read plan. Keep the version
// marker strict until an explicit migration decoder exists.
const GROUP_META_MAGIC: &[u8; 4] = b"GM05";
const GROUP_META_HEADER_LEN: usize = 12;
// Prefix/suffix lengths, kind/flags, mode, inode, size, POSIX hot
// attributes, and extent count.  Keeping this lower bound explicit lets the
// decoder reject a forged entry count before allocating its vector.
const GROUP_META_MIN_ENTRY_BYTES: usize = 72;
const MAX_GROUP_META_ENTRIES: u32 = 1_048_576;
const MAX_ENTRY_EXTENTS: usize = 1024;
const MAX_NAME_LEN: usize = 1024;
pub(crate) const MAX_GROUP_META_BYTES: usize = 256 * 1024;

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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
        if reader.take(4)? != GROUP_META_MAGIC {
            return Err(PackedWireError::UnsupportedFormat(
                "group metadata payload version mismatch".into(),
            ));
        }
        let count = reader.u32()?;
        if count > MAX_GROUP_META_ENTRIES {
            return Err(PackedWireError::LimitExceeded(
                "group metadata entry count exceeds limit".into(),
            ));
        }
        let available = bytes.len().saturating_sub(GROUP_META_HEADER_LEN);
        if count as usize > available / GROUP_META_MIN_ENTRY_BYTES {
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
            let entry = GroupMetaEntry {
                name,
                kind: reader.u8()?,
                flags: reader.u8()?,
                mode: reader.u32()?,
                uid: reader.u32()?,
                gid: reader.u32()?,
                rdev: reader.u64()?,
                nlink: reader.u32()?,
                atime_ns: reader.i64()?,
                mtime_ns: reader.i64()?,
                ctime_ns: reader.i64()?,
                inode: reader.u64()?,
                size: reader.u64()?,
                extents: {
                    let count = usize::from(reader.u16()?);
                    if count > MAX_ENTRY_EXTENTS {
                        return Err(PackedWireError::LimitExceeded(
                            "group entry extent count exceeds limit".into(),
                        ));
                    }
                    let mut extents = Vec::with_capacity(count);
                    for _ in 0..count {
                        extents.push(GroupMetaExtent {
                            file_offset: reader.u64()?,
                            logical_len: reader.u32()?,
                            frame_ordinal: reader.u32()?,
                            raw_offset: reader.u32()?,
                            raw_len: reader.u32()?,
                        });
                    }
                    extents
                },
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

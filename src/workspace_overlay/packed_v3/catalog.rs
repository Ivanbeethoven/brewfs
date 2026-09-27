//! Read-only catalog facade for a pinned packed v3 manifest.

use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::cadapter::client::{ObjectBackend, ObjectClient};
use crate::chunk::read_plan::{LogicalSegment, ReadGeneration, ReadSource, UnifiedReadPlan};

use super::group::PackedFrameDescriptor;
use super::index::{PackedGroupIndexPage, PackedInodeIndexPage};
use super::meta::{GroupMeta, GroupMetaEntry, MAX_GROUP_META_BYTES};
use super::remote::{RemotePackedObject, read_exact_range};
use super::wire::{
    PACKED_FOOTER_LEN, PACKED_HEADER_LEN, PackedGroupRef, PackedObjectKind, PackedResult,
    PackedSnapshotManifest, PackedWireError,
};

const MAX_GROUPS_PER_PAGE: usize = 4096;

/// A catalog intentionally keeps only the immutable manifest and no decoded
/// group metadata.  Callers may layer a byte-budgeted cache above it; strict
/// cold reads can simply discard the returned `GroupMeta` after the request.
#[derive(Clone)]
pub struct RemoteGroupCatalog<B: ObjectBackend + Clone> {
    client: ObjectClient<B>,
    manifest: Arc<PackedSnapshotManifest>,
    group_indexes: Arc<HashMap<u64, usize>>,
    parent_indexes: Arc<HashMap<[u8; 32], Vec<usize>>>,
}

impl<B: ObjectBackend + Clone> RemoteGroupCatalog<B> {
    pub fn new(client: ObjectClient<B>, manifest: PackedSnapshotManifest) -> Self {
        let mut group_indexes = HashMap::with_capacity(manifest.groups.len());
        let mut parent_indexes: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for (index, group) in manifest.groups.iter().enumerate() {
            group_indexes.entry(group.group_id).or_insert(index);
            parent_indexes
                .entry(group.parent_dir_key)
                .or_default()
                .push(index);
        }
        for indexes in parent_indexes.values_mut() {
            indexes.sort_by(|left, right| {
                let left = &manifest.groups[*left];
                let right = &manifest.groups[*right];
                left.first_name
                    .cmp(&right.first_name)
                    .then_with(|| left.group_id.cmp(&right.group_id))
            });
        }
        Self {
            client,
            manifest: Arc::new(manifest),
            group_indexes: Arc::new(group_indexes),
            parent_indexes: Arc::new(parent_indexes),
        }
    }

    pub fn manifest(&self) -> &PackedSnapshotManifest {
        self.manifest.as_ref()
    }

    pub fn client(&self) -> &ObjectClient<B> {
        &self.client
    }

    pub fn group_ref(&self, group_id: u64) -> PackedResult<&PackedGroupRef> {
        self.group_indexes
            .get(&group_id)
            .and_then(|index| self.manifest.groups.get(*index))
            .ok_or_else(|| PackedWireError::Invalid("packed group id is missing".into()))
    }

    /// Return one bounded page of groups belonging to a directory.  The
    /// per-parent order is built once when the catalog is created, so paging
    /// never scans or clones the complete directory.
    pub fn groups_for_parent(
        &self,
        parent_dir_key: [u8; 32],
        start: usize,
        limit: usize,
    ) -> Vec<PackedGroupRef> {
        if limit == 0 {
            return Vec::new();
        }
        self.parent_indexes
            .get(&parent_dir_key)
            .into_iter()
            .flat_map(|indexes| {
                indexes
                    .iter()
                    .skip(start)
                    .take(limit.min(MAX_GROUPS_PER_PAGE))
            })
            .filter_map(|index| self.manifest.groups.get(*index).cloned())
            .collect()
    }

    /// Locate the only group whose name range can contain `name`.
    pub fn group_for_name(&self, parent_dir_key: [u8; 32], name: &[u8]) -> Option<&PackedGroupRef> {
        let indexes = self.parent_indexes.get(&parent_dir_key)?;
        let candidate = indexes
            .partition_point(|index| self.manifest.groups[*index].last_name.as_slice() < name);
        let group = self.manifest.groups.get(*indexes.get(candidate)?)?;
        (group.first_name.as_slice() <= name && group.last_name.as_slice() >= name).then_some(group)
    }

    /// Resolve a group from one remote index page without materializing the
    /// manifest's complete group table.  Large snapshots leave
    /// `manifest.groups` empty and publish these pages instead; callers that
    /// already know the page ordinal (for example from a directory cursor)
    /// can keep the lookup strictly bounded to one page.
    pub async fn group_ref_from_index_page(
        &self,
        page_ordinal: usize,
        group_id: u64,
    ) -> PackedResult<Option<PackedGroupRef>> {
        if self.manifest.group_index_pages.is_empty() {
            return Ok(self
                .group_indexes
                .get(&group_id)
                .and_then(|index| self.manifest.groups.get(*index).cloned()));
        }
        Ok(self
            .load_group_index_page(page_ordinal)
            .await?
            .groups
            .into_iter()
            .find(|group| group.group_id == group_id))
    }

    /// Resolve a name range from one remote index page.  The page is decoded
    /// and dropped before the returned group is used, so memory remains
    /// bounded by one index page plus the cloned descriptor.
    pub async fn group_for_name_from_index_page(
        &self,
        page_ordinal: usize,
        parent_dir_key: [u8; 32],
        name: &[u8],
    ) -> PackedResult<Option<PackedGroupRef>> {
        if self.manifest.group_index_pages.is_empty() {
            return Ok(self.group_for_name(parent_dir_key, name).cloned());
        }
        Ok(self
            .load_group_index_page(page_ordinal)
            .await?
            .groups
            .into_iter()
            .find(|group| {
                group.parent_dir_key == parent_dir_key
                    && group.first_name.as_slice() <= name
                    && group.last_name.as_slice() >= name
            }))
    }

    /// Perform a dentry lookup using one pageable group-index page.  This is
    /// the page-backed counterpart to [`Self::lookup_entry`]; it is useful for
    /// million-file snapshots where keeping all group descriptors in RAM
    /// would defeat the format's paging contract.
    pub async fn lookup_entry_from_index_page(
        &self,
        page_ordinal: usize,
        parent_dir_key: [u8; 32],
        name: &[u8],
    ) -> PackedResult<Option<GroupMetaEntry>> {
        let Some(group) = self
            .group_for_name_from_index_page(page_ordinal, parent_dir_key, name)
            .await?
        else {
            return Ok(None);
        };
        Ok(self
            .load_group_meta_for_ref(&group)
            .await?
            .lookup(name)
            .cloned())
    }

    /// Resolve one directory entry without a per-entry namespace lookup.
    pub async fn lookup_entry(
        &self,
        parent_dir_key: [u8; 32],
        name: &[u8],
    ) -> PackedResult<Option<GroupMetaEntry>> {
        let Some(group_id) = self
            .group_for_name(parent_dir_key, name)
            .map(|group| group.group_id)
        else {
            return Ok(None);
        };
        Ok(self.load_group_meta(group_id).await?.lookup(name).cloned())
    }

    pub async fn load_group_meta(&self, group_id: u64) -> PackedResult<GroupMeta> {
        let group = self.group_ref(group_id)?.clone();
        self.load_group_meta_for_ref(&group).await
    }

    async fn load_group_meta_for_ref(&self, group: &PackedGroupRef) -> PackedResult<GroupMeta> {
        let container = self
            .manifest
            .containers
            .get(group.container_ordinal as usize)
            .ok_or_else(|| PackedWireError::Invalid("packed group container is missing".into()))?;
        if usize::try_from(group.meta_len).is_ok_and(|len| len > MAX_GROUP_META_BYTES) {
            return Err(PackedWireError::LimitExceeded(
                "packed group metadata exceeds 256 KiB".into(),
            ));
        }
        let remote = RemotePackedObject::open(
            &self.client,
            std::str::from_utf8(&container.object_key).map_err(|_| {
                PackedWireError::Invalid("packed container object key is not UTF-8".into())
            })?,
            container.object_len,
            PackedObjectKind::GroupContainer,
        )
        .await?;
        let bytes = remote
            .read_range(u64::from(group.meta_offset), u64::from(group.meta_len))
            .await?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != group.metadata_digest {
            return Err(PackedWireError::Invalid(
                "packed group metadata digest mismatch".into(),
            ));
        }
        GroupMeta::decode(&bytes)
    }

    pub async fn load_group_meta_page(
        &self,
        group_id: u64,
        start: usize,
        limit: usize,
    ) -> PackedResult<Vec<super::meta::GroupMetaEntry>> {
        Ok(self
            .load_group_meta(group_id)
            .await?
            .page(start, limit.min(MAX_GROUPS_PER_PAGE))
            .to_vec())
    }

    /// Resolve one file range directly from GroupMeta into the common read
    /// plan.  The plan contains only immutable packed sources and explicit
    /// holes; no namespace or KV lookup is needed after this call.  Frame
    /// descriptors are fetched from the bounded container directory and the
    /// payload itself is left to `PackedFrameSourceFetcher`.
    pub async fn read_unified_plan(
        &self,
        group_id: u64,
        name: &[u8],
        offset: u64,
        length: u64,
    ) -> PackedResult<UnifiedReadPlan> {
        let group = self.group_ref(group_id)?.clone();
        self.read_unified_plan_for_ref(&group, name, offset, length)
            .await
    }

    /// Generate a read plan from one pageable group-index page.  This is the
    /// data-path counterpart to [`Self::lookup_entry_from_index_page`]: large
    /// snapshots can keep `manifest.groups` empty and still resolve metadata,
    /// frame descriptors, and data ranges without loading the full index.
    pub async fn read_unified_plan_from_index_page(
        &self,
        page_ordinal: usize,
        group_id: u64,
        name: &[u8],
        offset: u64,
        length: u64,
    ) -> PackedResult<UnifiedReadPlan> {
        let Some(group) = self
            .group_ref_from_index_page(page_ordinal, group_id)
            .await?
        else {
            return Err(PackedWireError::Invalid(
                "packed group id is missing from the index page".into(),
            ));
        };
        self.read_unified_plan_for_ref(&group, name, offset, length)
            .await
    }

    async fn read_unified_plan_for_ref(
        &self,
        group: &PackedGroupRef,
        name: &[u8],
        offset: u64,
        length: u64,
    ) -> PackedResult<UnifiedReadPlan> {
        let entry = self
            .load_group_meta_for_ref(group)
            .await?
            .lookup(name)
            .cloned()
            .ok_or_else(|| PackedWireError::Invalid("packed group entry is missing".into()))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| PackedWireError::LimitExceeded("packed read range overflows".into()))?;
        if end > entry.size {
            return Err(PackedWireError::Invalid(
                "packed read range exceeds file size".into(),
            ));
        }
        let generation = ReadGeneration::readonly(self.manifest.snapshot_id);
        if length == 0 {
            return Ok(UnifiedReadPlan {
                generation,
                logical_size: entry.size,
                segments: Vec::new(),
            });
        }

        let frame_ordinals = entry.extents.iter().map(|extent| extent.frame_ordinal);
        let frames = self
            .load_frame_descriptors(group.container_ordinal, frame_ordinals)
            .await?
            .into_iter()
            .map(|frame| (frame.frame_ordinal, frame))
            .collect::<HashMap<_, _>>();
        let mut segments = Vec::new();
        for extent in entry.extents {
            let extent_end = extent
                .file_offset
                .checked_add(u64::from(extent.logical_len))
                .ok_or_else(|| PackedWireError::LimitExceeded("packed extent overflows".into()))?;
            let start = offset.max(extent.file_offset);
            let segment_end = end.min(extent_end);
            if start >= segment_end {
                continue;
            }
            let frame = frames
                .get(&extent.frame_ordinal)
                .ok_or_else(|| PackedWireError::Invalid("packed extent frame is missing".into()))?;
            if frame.raw_len != extent.raw_len {
                return Err(PackedWireError::Invalid(
                    "packed extent and frame lengths disagree".into(),
                ));
            }
            let raw_offset = u64::from(extent.raw_offset)
                .checked_add(start - extent.file_offset)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("packed raw offset overflows".into())
                })?;
            let segment_len = segment_end - start;
            if raw_offset
                .checked_add(segment_len)
                .is_none_or(|raw_end| raw_end > u64::from(frame.raw_len))
            {
                return Err(PackedWireError::Invalid(
                    "packed extent exceeds frame raw length".into(),
                ));
            }
            segments.push(LogicalSegment {
                logical_offset: start,
                length: segment_len,
                source: ReadSource::PackedFrame {
                    group_id: group.group_id,
                    container_ordinal: group.container_ordinal,
                    frame_ordinal: frame.frame_ordinal,
                    object_offset: frame.object_offset,
                    stored_len: frame.stored_len,
                    raw_offset: u32::try_from(raw_offset).map_err(|_| {
                        PackedWireError::LimitExceeded("packed raw offset exceeds u32".into())
                    })?,
                    raw_len: frame.raw_len,
                    size_class: frame.size_class as u8,
                    codec: frame.codec,
                    frame_digest: frame.frame_digest,
                },
            });
        }
        segments.sort_by_key(|segment| segment.logical_offset);
        let plan = UnifiedReadPlan {
            generation,
            logical_size: entry.size,
            segments,
        };
        plan.validate(offset, length)
            .map_err(|error| PackedWireError::Invalid(error.to_string()))?;
        Ok(plan)
    }

    pub async fn load_group_index_page(
        &self,
        page_ordinal: usize,
    ) -> PackedResult<PackedGroupIndexPage> {
        let reference = self
            .manifest
            .group_index_pages
            .get(page_ordinal)
            .ok_or_else(|| PackedWireError::Invalid("packed group index page is missing".into()))?;
        let object = self
            .read_index_object(reference, PackedObjectKind::GroupIndex)
            .await?;
        let page = PackedGroupIndexPage::decode(object)?;
        if page.snapshot_id != self.manifest.snapshot_id
            || page.page_ordinal != page_ordinal as u32
            || page.total_pages != self.manifest.group_index_pages.len() as u32
        {
            return Err(PackedWireError::Invalid(
                "packed group index page does not match the manifest".into(),
            ));
        }
        Ok(page)
    }

    pub async fn load_inode_index_page(
        &self,
        page_ordinal: usize,
    ) -> PackedResult<PackedInodeIndexPage> {
        let reference = self
            .manifest
            .inode_index_pages
            .get(page_ordinal)
            .ok_or_else(|| PackedWireError::Invalid("packed inode index page is missing".into()))?;
        let object = self
            .read_index_object(reference, PackedObjectKind::InodeIndex)
            .await?;
        let page = PackedInodeIndexPage::decode(object)?;
        if page.snapshot_id != self.manifest.snapshot_id
            || page.page_ordinal != page_ordinal as u32
            || page.total_pages != self.manifest.inode_index_pages.len() as u32
        {
            return Err(PackedWireError::Invalid(
                "packed inode index page does not match the manifest".into(),
            ));
        }
        Ok(page)
    }

    /// Look up one inode in a single remote inode-index page.  The caller
    /// supplies the page ordinal from its inode cursor/routing layer; only
    /// that bounded page is downloaded and decoded.
    pub async fn inode_from_index_page(
        &self,
        page_ordinal: usize,
        inode: u64,
    ) -> PackedResult<Option<super::index::PackedInodeIndexEntry>> {
        if self.manifest.inode_index_pages.is_empty() {
            return Ok(None);
        }
        Ok(self
            .load_inode_index_page(page_ordinal)
            .await?
            .entries
            .into_iter()
            .find(|entry| entry.inode == inode))
    }

    async fn read_index_object(
        &self,
        reference: &super::wire::PackedContainerRef,
        kind: PackedObjectKind,
    ) -> PackedResult<Vec<u8>> {
        let key = std::str::from_utf8(&reference.object_key)
            .map_err(|_| PackedWireError::Invalid("packed index object key is not UTF-8".into()))?;
        let remote =
            RemotePackedObject::open(&self.client, key, reference.object_len, kind).await?;
        let body = remote
            .read_range(
                PACKED_HEADER_LEN as u64,
                u64::from(remote.header().body_stored_len),
            )
            .await?;
        let footer = read_exact_range(
            &self.client,
            key,
            reference
                .object_len
                .saturating_sub(PACKED_FOOTER_LEN as u64),
            PACKED_FOOTER_LEN as u64,
        )
        .await?;
        let mut object = Vec::with_capacity(reference.object_len as usize);
        object.extend_from_slice(&remote.header().encode());
        object.extend_from_slice(&body);
        object.extend_from_slice(&footer);
        let digest: [u8; 32] = Sha256::digest(&object).into();
        if digest != reference.object_digest {
            return Err(PackedWireError::Invalid(
                "packed index object digest mismatch".into(),
            ));
        }
        Ok(object)
    }

    async fn load_frame_descriptors(
        &self,
        container_ordinal: u32,
        ordinals: impl IntoIterator<Item = u32>,
    ) -> PackedResult<Vec<PackedFrameDescriptor>> {
        let container = self
            .manifest
            .containers
            .get(container_ordinal as usize)
            .ok_or_else(|| PackedWireError::Invalid("packed container is missing".into()))?;
        let key = std::str::from_utf8(&container.object_key).map_err(|_| {
            PackedWireError::Invalid("packed container object key is not UTF-8".into())
        })?;
        let remote = RemotePackedObject::open(
            &self.client,
            key,
            container.object_len,
            PackedObjectKind::GroupContainer,
        )
        .await?;
        remote.read_frame_descriptors(ordinals).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadapter::localfs::LocalFsBackend;
    use crate::workspace_overlay::packed_v3::{
        AccessProfile, GroupMetaEntry, GroupMetaExtent, PackedContainerRef, PackedFrameInput,
        PackedGroupContainer, PackedGroupInput, PackedSnapshotManifest, SizeClass,
    };
    use tempfile::tempdir;

    fn meta_bytes() -> Vec<u8> {
        super::super::meta::GroupMeta::new(vec![GroupMetaEntry {
            name: b"sample.bin".to_vec(),
            inode: 7,
            kind: 1,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            rdev: 0,
            nlink: 1,
            atime_ns: 0,
            mtime_ns: 0,
            ctime_ns: 0,
            size: 7,
            flags: 0,
            extents: vec![GroupMetaExtent {
                file_offset: 0,
                logical_len: 7,
                frame_ordinal: 0,
                raw_offset: 0,
                raw_len: 7,
            }],
        }])
        .unwrap()
        .encode()
        .unwrap()
    }

    #[tokio::test]
    async fn catalog_fetches_only_the_group_metadata_range() {
        let temp = tempdir().unwrap();
        let backend = LocalFsBackend::new(temp.path());
        let client = ObjectClient::new(backend.clone());
        let metadata = meta_bytes();
        let object = PackedGroupContainer::build(
            11,
            AccessProfile::RandomSmallFile,
            vec![PackedGroupInput {
                group_id: 1,
                parent_dir_key: [3; 32],
                metadata: metadata.clone(),
                frame_ordinals: vec![0],
                entry_count: 1,
                file_count: 1,
                layout_profile: AccessProfile::RandomSmallFile,
            }],
            vec![PackedFrameInput {
                raw: b"payload".to_vec(),
                size_class: SizeClass::Tiny,
                codec: 0,
                first_file_slot: 0,
                last_file_slot: 0,
            }],
        )
        .unwrap();
        let opened = PackedGroupContainer::open(object.clone()).unwrap();
        client.put_object("container", &object).await.unwrap();
        let manifest = PackedSnapshotManifest {
            snapshot_id: [9; 32],
            root_dir_key: [8; 32],
            root_inode: 1,
            layout_profile: AccessProfile::RandomSmallFile,
            size_classes: Default::default(),
            groups: vec![super::super::wire::PackedGroupRef {
                group_id: 1,
                container_ordinal: 0,
                parent_dir_key: [3; 32],
                first_name: b"sample.bin".to_vec(),
                last_name: b"sample.bin".to_vec(),
                meta_offset: opened.groups()[0].metadata_offset,
                meta_len: opened.groups()[0].metadata_len,
                data_offset: opened.groups()[0].data_offset,
                data_len: opened.groups()[0].data_len,
                entry_count: 1,
                file_count: 1,
                frame_count: 1,
                layout_profile: AccessProfile::RandomSmallFile,
                metadata_digest: Sha256::digest(&metadata).into(),
                data_digest: opened.groups()[0].data_digest,
            }],
            containers: vec![PackedContainerRef {
                object_key: b"container".to_vec(),
                object_len: opened.object_len(),
                object_digest: opened.object_digest(),
            }],
            group_index_pages: Vec::new(),
            inode_index_pages: Vec::new(),
        };
        let catalog = RemoteGroupCatalog::new(client, manifest);
        let page = catalog.load_group_meta_page(1, 0, 1).await.unwrap();
        assert_eq!(page[0].name, b"sample.bin");
        let entry = catalog
            .lookup_entry([3; 32], b"sample.bin")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.inode, 7);
        let plan = catalog
            .read_unified_plan(1, b"sample.bin", 1, 4)
            .await
            .unwrap();
        assert_eq!(
            plan.generation,
            crate::chunk::read_plan::ReadGeneration::readonly([9; 32])
        );
        assert_eq!(plan.segments.len(), 1);
        let remote = super::super::remote::RemotePackedObject::open(
            catalog.client(),
            "container",
            opened.object_len(),
            PackedObjectKind::GroupContainer,
        )
        .await
        .unwrap();
        let fetcher = super::super::remote::PackedFrameSourceFetcher::from_object(remote, 0);
        let mut bytes = [0u8; 4];
        crate::chunk::read_plan::execute_unified_into(&fetcher, 1, &plan, &mut bytes)
            .await
            .unwrap();
        assert_eq!(&bytes, b"aylo");
        assert!(
            catalog
                .lookup_entry([3; 32], b"missing.bin")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn catalog_resolves_entries_from_a_pageable_group_index() {
        let temp = tempdir().unwrap();
        let backend = LocalFsBackend::new(temp.path());
        let client = ObjectClient::new(backend.clone());
        let metadata = meta_bytes();
        let object = PackedGroupContainer::build(
            12,
            AccessProfile::RandomSmallFile,
            vec![PackedGroupInput {
                group_id: 9,
                parent_dir_key: [8; 32],
                metadata: metadata.clone(),
                frame_ordinals: vec![0],
                entry_count: 1,
                file_count: 1,
                layout_profile: AccessProfile::RandomSmallFile,
            }],
            vec![PackedFrameInput {
                raw: b"payload".to_vec(),
                size_class: SizeClass::Tiny,
                codec: 0,
                first_file_slot: 0,
                last_file_slot: 0,
            }],
        )
        .unwrap();
        let opened = PackedGroupContainer::open(object.clone()).unwrap();
        client.put_object("container", &object).await.unwrap();

        let group = super::super::wire::PackedGroupRef {
            group_id: 9,
            container_ordinal: 0,
            parent_dir_key: [8; 32],
            first_name: b"sample.bin".to_vec(),
            last_name: b"sample.bin".to_vec(),
            meta_offset: opened.groups()[0].metadata_offset,
            meta_len: opened.groups()[0].metadata_len,
            data_offset: opened.groups()[0].data_offset,
            data_len: opened.groups()[0].data_len,
            entry_count: 1,
            file_count: 1,
            frame_count: 1,
            layout_profile: AccessProfile::RandomSmallFile,
            metadata_digest: Sha256::digest(&metadata).into(),
            data_digest: opened.groups()[0].data_digest,
        };
        let page = PackedGroupIndexPage {
            snapshot_id: [7; 32],
            page_ordinal: 0,
            total_pages: 1,
            groups: vec![group],
        }
        .encode()
        .unwrap();
        client.put_object("group-index", &page).await.unwrap();
        let manifest = PackedSnapshotManifest {
            snapshot_id: [7; 32],
            root_dir_key: [8; 32],
            root_inode: 1,
            layout_profile: AccessProfile::RandomSmallFile,
            size_classes: Default::default(),
            // The complete group table is intentionally omitted.  This is
            // the shape used by large snapshots to keep manifest memory
            // independent of file count.
            groups: Vec::new(),
            containers: vec![PackedContainerRef {
                object_key: b"container".to_vec(),
                object_len: opened.object_len(),
                object_digest: opened.object_digest(),
            }],
            group_index_pages: vec![PackedContainerRef {
                object_key: b"group-index".to_vec(),
                object_len: page.len() as u64,
                object_digest: Sha256::digest(&page).into(),
            }],
            inode_index_pages: Vec::new(),
        };
        let catalog = RemoteGroupCatalog::new(client, manifest);
        let group = catalog
            .group_for_name_from_index_page(0, [8; 32], b"sample.bin")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(group.group_id, 9);
        let entry = catalog
            .lookup_entry_from_index_page(0, [8; 32], b"sample.bin")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.inode, 7);
        let plan = catalog
            .read_unified_plan_from_index_page(0, 9, b"sample.bin", 1, 4)
            .await
            .unwrap();
        let remote = super::super::remote::RemotePackedObject::open(
            catalog.client(),
            "container",
            opened.object_len(),
            PackedObjectKind::GroupContainer,
        )
        .await
        .unwrap();
        let fetcher = super::super::remote::PackedFrameSourceFetcher::from_object(remote, 0);
        let mut bytes = [0u8; 4];
        crate::chunk::read_plan::execute_unified_into(&fetcher, 1, &plan, &mut bytes)
            .await
            .unwrap();
        assert_eq!(&bytes, b"aylo");
        assert!(
            catalog
                .group_ref_from_index_page(0, 99)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn catalog_paginates_parent_groups_in_name_order() {
        let client = ObjectClient::new(LocalFsBackend::new(tempdir().unwrap().path()));
        let group = |group_id: u64, name: &[u8]| super::super::wire::PackedGroupRef {
            group_id,
            container_ordinal: 0,
            parent_dir_key: [4; 32],
            first_name: name.to_vec(),
            last_name: name.to_vec(),
            meta_offset: 0,
            meta_len: 0,
            data_offset: 0,
            data_len: 0,
            entry_count: 0,
            file_count: 0,
            frame_count: 0,
            layout_profile: AccessProfile::RandomSmallFile,
            metadata_digest: [0; 32],
            data_digest: [0; 32],
        };
        let catalog = RemoteGroupCatalog::new(
            client,
            PackedSnapshotManifest {
                snapshot_id: [0; 32],
                root_dir_key: [8; 32],
                root_inode: 1,
                layout_profile: AccessProfile::RandomSmallFile,
                size_classes: Default::default(),
                groups: vec![group(2, b"z"), group(1, b"a")],
                containers: vec![PackedContainerRef {
                    object_key: b"container".to_vec(),
                    object_len: 128,
                    object_digest: [0; 32],
                }],
                group_index_pages: Vec::new(),
                inode_index_pages: Vec::new(),
            },
        );
        assert_eq!(catalog.groups_for_parent([4; 32], 0, 1)[0].group_id, 1);
        assert_eq!(catalog.groups_for_parent([4; 32], 1, 1)[0].group_id, 2);
    }
}

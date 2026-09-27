//! Read-only VFS adapters for the packed-v3 snapshot.
//!
//! The packed catalog owns namespace and frame resolution.  These adapters
//! deliberately keep the existing VFS contract: metadata exposes immutable
//! `SliceDesc` rows and the block store translates those synthetic rows back
//! into a bounded packed frame read.  No Redis/TiKV metadata client or loose
//! block cache is involved.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;

use crate::cadapter::client::ObjectBackend;
use crate::chunk::{BlockKey, BlockStore, SliceDesc};
use crate::meta::client::MetaClientMetrics;
use crate::meta::client::session::SessionInfo;
use crate::meta::file_lock::{FileLockInfo, FileLockQuery, FileLockRange, FileLockType};
use crate::meta::layer::MetaLayer;
use crate::meta::store::{
    AclRule, CreateEntryResult, DirEntry, FileAttr, FileType, MetaError, OpenFlags, SetAttrFlags,
    SetAttrRequest, StatFsSnapshot, stat_fs_snapshot_from_usage,
};
use crate::vfs::handles::{DirHandle, DirectoryPageSource, RawDirEntry};
use crate::vfs::{chunk_id_for, extract_ino_and_chunk_index};

use super::catalog::{RemoteGroupCatalog, directory_key};
use super::meta::GroupMetaEntry;
use super::wire::{PackedSnapshotManifest, PackedWireError};

const READ_ONLY_ERROR: &str = "packed metadata v3 snapshot is read-only";

fn map_error(error: PackedWireError) -> MetaError {
    MetaError::Internal(error.to_string())
}

fn file_type(kind: u8, mode: u32) -> FileType {
    match kind {
        1 => FileType::File,
        2 => FileType::Dir,
        3 => FileType::Symlink,
        4 => FileType::Fifo,
        5 => FileType::Socket,
        6 => FileType::CharDevice,
        7 => FileType::BlockDevice,
        _ => FileType::from_mode(mode),
    }
}

fn attr(inode: u64, entry: &GroupMetaEntry) -> FileAttr {
    FileAttr {
        ino: inode as i64,
        size: entry.size,
        blocks: entry.size.div_ceil(512),
        kind: file_type(entry.kind, entry.mode),
        mode: entry.mode,
        rdev: entry.rdev.min(u64::from(u32::MAX)) as u32,
        uid: entry.uid,
        gid: entry.gid,
        atime: entry.atime_ns,
        mtime: entry.mtime_ns,
        ctime: entry.ctime_ns,
        nlink: entry.nlink,
    }
}

/// A block-store facade over immutable packed frames.
#[derive(Clone)]
pub struct PackedV3BlockStore<B: ObjectBackend + Clone> {
    catalog: Arc<RemoteGroupCatalog<B>>,
    chunk_size: u64,
    block_size: u64,
}

impl<B: ObjectBackend + Clone> PackedV3BlockStore<B> {
    pub fn new(
        catalog: Arc<RemoteGroupCatalog<B>>,
        chunk_size: u64,
        block_size: u32,
    ) -> Result<Self, MetaError> {
        if chunk_size == 0 || block_size == 0 || chunk_size < u64::from(block_size) {
            return Err(MetaError::Internal("invalid packed v3 block layout".into()));
        }
        Ok(Self {
            catalog,
            chunk_size,
            block_size: u64::from(block_size),
        })
    }

    fn range_for(
        &self,
        key: BlockKey,
        offset: u64,
        len: usize,
    ) -> Result<(u64, u64), anyhow::Error> {
        let (ino, chunk_index) = extract_ino_and_chunk_index(key.0);
        if ino <= 0 || chunk_id_for(ino, chunk_index)? != key.0 {
            anyhow::bail!("invalid packed v3 chunk id {}", key.0);
        }
        let absolute = chunk_index
            .checked_mul(self.chunk_size)
            .and_then(|value| value.checked_add(u64::from(key.1).checked_mul(self.block_size)?))
            .and_then(|value| value.checked_add(offset))
            .ok_or_else(|| anyhow::anyhow!("packed v3 read offset overflows"))?;
        let length = u64::try_from(len).map_err(|_| anyhow::anyhow!("read length exceeds u64"))?;
        Ok((
            u64::try_from(ino).unwrap(),
            absolute
                .checked_add(length)
                .ok_or_else(|| anyhow::anyhow!("packed v3 read end overflows"))?,
        ))
    }
}

#[async_trait]
impl<B> BlockStore for PackedV3BlockStore<B>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    async fn write_fresh_range(
        &self,
        _key: BlockKey,
        _offset: u64,
        _data: &[u8],
    ) -> anyhow::Result<u64> {
        anyhow::bail!(READ_ONLY_ERROR)
    }

    async fn read_range(&self, key: BlockKey, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let (inode, end) = self.range_for(key, offset, buf.len())?;
        let start = end - u64::try_from(buf.len())?;
        self.catalog
            .read_inode_range(inode, start, buf)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    async fn delete_range(&self, _key: BlockKey, _block_count: u64) -> anyhow::Result<()> {
        anyhow::bail!(READ_ONLY_ERROR)
    }
}

struct PackedDirectoryPageSource<B: ObjectBackend + Clone> {
    catalog: Arc<RemoteGroupCatalog<B>>,
    manifest: Arc<PackedSnapshotManifest>,
}

#[async_trait]
impl<B> DirectoryPageSource for PackedDirectoryPageSource<B>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    async fn read_page(
        &self,
        ino: i64,
        child_offset: u64,
        max_entries: usize,
    ) -> Result<Vec<RawDirEntry>, MetaError> {
        let parent = u64::try_from(ino).map_err(|_| MetaError::NotFound(ino))?;
        let key = if parent == self.manifest.root_inode {
            self.manifest.root_dir_key
        } else {
            directory_key(self.manifest.snapshot_id, parent)
        };
        let entries = self
            .catalog
            .readdir_page(
                key,
                usize::try_from(child_offset)
                    .map_err(|_| MetaError::Internal("directory offset exceeds usize".into()))?,
                max_entries,
            )
            .await
            .map_err(map_error)?;
        entries
            .into_iter()
            .map(|entry| {
                Ok(RawDirEntry {
                    name: entry.name,
                    ino: i64::try_from(entry.inode)
                        .map_err(|_| MetaError::Internal("packed inode exceeds i64".into()))?,
                    kind: file_type(entry.kind, entry.mode),
                })
            })
            .collect()
    }
}

/// Immutable metadata facade for a packed-v3 manifest.
pub struct PackedV3ReadonlyMeta<B: ObjectBackend + Clone> {
    catalog: Arc<RemoteGroupCatalog<B>>,
    manifest: Arc<PackedSnapshotManifest>,
    root: AtomicI64,
    chunk_size: u64,
}

impl<B: ObjectBackend + Clone + 'static> PackedV3ReadonlyMeta<B> {
    pub fn new(catalog: Arc<RemoteGroupCatalog<B>>, chunk_size: u64) -> Self {
        let manifest = Arc::new(catalog.manifest().clone());
        Self {
            root: AtomicI64::new(manifest.root_inode as i64),
            catalog,
            manifest,
            chunk_size,
        }
    }

    pub fn catalog(&self) -> &Arc<RemoteGroupCatalog<B>> {
        &self.catalog
    }

    fn inode(ino: i64) -> Result<u64, MetaError> {
        u64::try_from(ino).map_err(|_| MetaError::NotFound(ino))
    }

    fn parent_key(&self, inode: u64) -> [u8; 32] {
        if inode == self.manifest.root_inode {
            self.manifest.root_dir_key
        } else {
            directory_key(self.manifest.snapshot_id, inode)
        }
    }

    async fn entry(
        &self,
        inode: u64,
    ) -> Result<Option<(super::wire::PackedGroupRef, GroupMetaEntry)>, MetaError> {
        self.catalog
            .lookup_inode_entry(inode)
            .await
            .map_err(map_error)
    }

    async fn readonly<T>() -> Result<T, MetaError> {
        Err(MetaError::NotSupported(READ_ONLY_ERROR.into()))
    }

    fn chunk_slices(
        &self,
        chunk_id: u64,
        entry: &GroupMetaEntry,
        chunk_index: u64,
    ) -> Result<Vec<SliceDesc>, MetaError> {
        let chunk_start = chunk_index
            .checked_mul(self.chunk_size)
            .ok_or_else(|| MetaError::Internal("packed chunk offset overflows".into()))?;
        let chunk_end = chunk_start.saturating_add(self.chunk_size);
        let mut slices = Vec::new();
        for extent in &entry.extents {
            let extent_end = extent
                .file_offset
                .saturating_add(u64::from(extent.logical_len));
            let start = extent.file_offset.max(chunk_start);
            let end = extent_end.min(chunk_end).min(entry.size);
            if start < end {
                slices.push(SliceDesc {
                    slice_id: chunk_id,
                    chunk_id,
                    offset: start - chunk_start,
                    length: end - start,
                });
            }
        }
        Ok(slices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadapter::client::ObjectClient;
    use crate::cadapter::localfs::LocalFsBackend;
    use crate::workspace_overlay::packed_v3::{
        AccessProfile, GroupMeta, GroupMetaExtent, PackedContainerRef, PackedFrameInput,
        PackedGroupContainer, PackedGroupIndexPage, PackedGroupIndexPageRef, PackedGroupInput,
        PackedGroupRef, PackedInodeIndexEntry, PackedInodeIndexPage, PackedInodeIndexPageRef,
        SizeClass, SizeClassTable,
    };
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[tokio::test]
    async fn readonly_meta_and_block_store_read_one_packed_file() {
        let temp = tempdir().unwrap();
        let client = ObjectClient::new(LocalFsBackend::new(temp.path()));
        let root_key = [7; 32];
        let metadata = GroupMeta::new(vec![GroupMetaEntry {
            name: b"file".to_vec(),
            inode: 2,
            kind: 1,
            mode: 0o100644,
            uid: 1,
            gid: 2,
            rdev: 0,
            nlink: 1,
            atime_ns: 0,
            mtime_ns: 0,
            ctime_ns: 0,
            size: 5,
            flags: 0,
            extents: vec![GroupMetaExtent {
                file_offset: 0,
                logical_len: 5,
                frame_ordinal: 0,
                raw_offset: 0,
                raw_len: 5,
            }],
        }])
        .unwrap();
        let container = PackedGroupContainer::build(
            1,
            AccessProfile::RandomSmallFile,
            vec![PackedGroupInput {
                group_id: 1,
                parent_dir_key: root_key,
                metadata: metadata.encode().unwrap(),
                frame_ordinals: vec![0],
                entry_count: 1,
                file_count: 1,
                layout_profile: AccessProfile::RandomSmallFile,
            }],
            vec![PackedFrameInput {
                raw: b"hello".to_vec(),
                size_class: SizeClass::Tiny,
                codec: 0,
                first_file_slot: 0,
                last_file_slot: 0,
            }],
        )
        .unwrap();
        client.put_object("container", &container).await.unwrap();
        let opened = PackedGroupContainer::open(container.clone()).unwrap();
        let descriptor = &opened.groups()[0];
        let group = PackedGroupRef {
            group_id: 1,
            container_ordinal: 0,
            parent_dir_key: root_key,
            first_name: b"file".to_vec(),
            last_name: b"file".to_vec(),
            meta_offset: descriptor.metadata_offset,
            meta_len: descriptor.metadata_len,
            data_offset: descriptor.data_offset,
            data_len: descriptor.data_len,
            entry_count: descriptor.entry_count,
            file_count: descriptor.file_count,
            frame_count: 1,
            layout_profile: descriptor.layout_profile,
            metadata_digest: descriptor.metadata_digest,
            data_digest: descriptor.data_digest,
        };
        let group_page = PackedGroupIndexPage {
            snapshot_id: [9; 32],
            page_ordinal: 0,
            total_pages: 1,
            groups: vec![group.clone()],
        }
        .encode()
        .unwrap();
        client.put_object("group-index", &group_page).await.unwrap();
        let inode_page = PackedInodeIndexPage {
            snapshot_id: [9; 32],
            page_ordinal: 0,
            total_pages: 1,
            entries: vec![PackedInodeIndexEntry {
                inode: 2,
                parent_inode: 1,
                parent_dir_key: root_key,
                group_id: 1,
                entry_ordinal: 0,
                name: b"file".to_vec(),
                kind: 1,
                mode: 0o100644,
                uid: 1,
                gid: 2,
                rdev: 0,
                nlink: 1,
                atime_ns: 0,
                mtime_ns: 0,
                ctime_ns: 0,
                size: 5,
            }],
        }
        .encode()
        .unwrap();
        client.put_object("inode-index", &inode_page).await.unwrap();
        let manifest = PackedSnapshotManifest {
            snapshot_id: [9; 32],
            root_dir_key: root_key,
            root_inode: 1,
            layout_profile: AccessProfile::RandomSmallFile,
            size_classes: SizeClassTable::default(),
            groups: Vec::new(),
            containers: vec![PackedContainerRef {
                object_key: b"container".to_vec(),
                object_len: opened.object_len(),
                object_digest: opened.object_digest(),
            }],
            group_index_pages: vec![PackedGroupIndexPageRef {
                object: PackedContainerRef {
                    object_key: b"group-index".to_vec(),
                    object_len: group_page.len() as u64,
                    object_digest: Sha256::digest(&group_page).into(),
                },
                first_parent_dir_key: root_key,
                first_name: b"file".to_vec(),
                last_parent_dir_key: root_key,
                last_name: b"file".to_vec(),
            }],
            inode_index_pages: vec![PackedInodeIndexPageRef {
                object: PackedContainerRef {
                    object_key: b"inode-index".to_vec(),
                    object_len: inode_page.len() as u64,
                    object_digest: Sha256::digest(&inode_page).into(),
                },
                first_inode: 2,
                last_inode: 2,
            }],
        };
        let catalog = Arc::new(RemoteGroupCatalog::new(client, manifest));
        let meta = PackedV3ReadonlyMeta::new(Arc::clone(&catalog), 4096);
        assert_eq!(meta.lookup(1, "file").await.unwrap(), Some(2));
        assert_eq!(meta.stat(2).await.unwrap().unwrap().size, 5);
        assert_eq!(
            meta.get_paths(2).await.unwrap(),
            vec![String::from("/file")]
        );
        let store = PackedV3BlockStore::new(catalog, 4096, 4096).unwrap();
        let key = (chunk_id_for(2, 0).unwrap(), 0);
        let mut output = [0; 5];
        store.read_range(key, 0, &mut output).await.unwrap();
        assert_eq!(&output, b"hello");
    }
}

#[async_trait]
impl<B> MetaLayer for PackedV3ReadonlyMeta<B>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &'static str {
        "packed-metadata-v3-readonly"
    }
    fn metrics(&self) -> Option<Arc<MetaClientMetrics>> {
        None
    }
    fn root_ino(&self) -> i64 {
        self.root.load(Ordering::Acquire)
    }
    fn chroot(&self, inode: i64) {
        self.root.store(inode, Ordering::Release);
    }
    async fn initialize(&self) -> Result<(), MetaError> {
        Ok(())
    }

    async fn stat_fs(&self) -> Result<StatFsSnapshot, MetaError> {
        // PM06 keeps aggregate usage out of the hot manifest so opening a
        // mount never requires scanning every inode page. Report the stable
        // default capacity until a future manifest adds authenticated totals.
        Ok(stat_fs_snapshot_from_usage(0, 0))
    }

    async fn stat(&self, ino: i64) -> Result<Option<FileAttr>, MetaError> {
        self.stat_fresh(ino).await
    }
    async fn stat_fresh(&self, ino: i64) -> Result<Option<FileAttr>, MetaError> {
        let inode = Self::inode(ino)?;
        if inode == self.manifest.root_inode {
            return Ok(Some(FileAttr {
                ino,
                size: 0,
                blocks: 0,
                kind: FileType::Dir,
                mode: 0o040755,
                rdev: 0,
                uid: 0,
                gid: 0,
                atime: 0,
                mtime: 0,
                ctime: 0,
                nlink: 2,
            }));
        }
        Ok(self
            .entry(inode)
            .await?
            .map(|(_, entry)| attr(inode, &entry)))
    }
    async fn lookup(&self, parent: i64, name: &str) -> Result<Option<i64>, MetaError> {
        let parent = Self::inode(parent)?;
        let entry = self
            .catalog
            .lookup_entry_paged(self.parent_key(parent), name.as_bytes())
            .await
            .map_err(map_error)?;
        entry
            .map(|entry| {
                i64::try_from(entry.inode)
                    .map_err(|_| MetaError::Internal("packed inode exceeds i64".into()))
            })
            .transpose()
    }
    async fn lookup_with_attr(
        &self,
        parent: i64,
        name: &str,
    ) -> Result<Option<(i64, FileAttr)>, MetaError> {
        let parent = Self::inode(parent)?;
        let Some(entry) = self
            .catalog
            .lookup_entry_paged(self.parent_key(parent), name.as_bytes())
            .await
            .map_err(map_error)?
        else {
            return Ok(None);
        };
        let inode = i64::try_from(entry.inode)
            .map_err(|_| MetaError::Internal("packed inode exceeds i64".into()))?;
        Ok(Some((inode, attr(entry.inode, &entry))))
    }
    async fn lookup_with_attr_bytes(
        &self,
        parent: i64,
        name: &[u8],
    ) -> Result<Option<(i64, FileAttr)>, MetaError> {
        let parent = Self::inode(parent)?;
        let Some(entry) = self
            .catalog
            .lookup_entry_paged(self.parent_key(parent), name)
            .await
            .map_err(map_error)?
        else {
            return Ok(None);
        };
        let inode = i64::try_from(entry.inode)
            .map_err(|_| MetaError::Internal("packed inode exceeds i64".into()))?;
        Ok(Some((inode, attr(entry.inode, &entry))))
    }
    async fn lookup_path(&self, path: &str) -> Result<Option<(i64, FileType)>, MetaError> {
        if path.is_empty() || !path.starts_with('/') {
            return Err(MetaError::InvalidPath(path.into()));
        }
        let mut inode = self.root_ino();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            if component == "." {
                continue;
            }
            if component == ".." {
                inode = self.get_dir_parent(inode).await?.unwrap_or(self.root_ino());
                continue;
            }
            let Some(next) = self.lookup(inode, component).await? else {
                return Ok(None);
            };
            inode = next;
        }
        let Some(file) = self.stat(inode).await? else {
            return Ok(None);
        };
        Ok(Some((inode, file.kind)))
    }
    async fn readdir(&self, ino: i64) -> Result<Vec<DirEntry>, MetaError> {
        let mut offset = 0usize;
        let mut result = Vec::new();
        loop {
            let page = self.readdir_page_raw(ino, offset, 256).await?;
            if page.is_empty() {
                break;
            }
            offset += page.len();
            for entry in page {
                result.push(DirEntry {
                    name: String::from_utf8(entry.name).map_err(|_| MetaError::InvalidFilename)?,
                    ino: i64::try_from(entry.inode)
                        .map_err(|_| MetaError::Internal("packed inode exceeds i64".into()))?,
                    kind: file_type(entry.kind, entry.mode),
                });
            }
        }
        Ok(result)
    }
    async fn opendir(&self, ino: i64) -> Result<DirHandle, MetaError> {
        let inode = Self::inode(ino)?;
        let is_dir = if inode == self.manifest.root_inode {
            true
        } else {
            self.entry(inode)
                .await?
                .map(|(_, entry)| file_type(entry.kind, entry.mode).is_dir())
                .ok_or(MetaError::NotFound(ino))?
        };
        if !is_dir {
            return Err(MetaError::NotDirectory(ino));
        }
        Ok(DirHandle::new_paged(
            ino,
            Arc::new(PackedDirectoryPageSource {
                catalog: Arc::clone(&self.catalog),
                manifest: Arc::clone(&self.manifest),
            }),
        ))
    }
    async fn mkdir(&self, _parent: i64, _name: String) -> Result<i64, MetaError> {
        Self::readonly().await
    }
    async fn rmdir(&self, _parent: i64, _name: &str) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn create_file(&self, _parent: i64, _name: String) -> Result<i64, MetaError> {
        Self::readonly().await
    }
    async fn create_file_with_attr(
        &self,
        _parent: i64,
        _name: String,
    ) -> Result<CreateEntryResult, MetaError> {
        Self::readonly().await
    }
    async fn create_node(
        &self,
        _parent: i64,
        _name: String,
        _kind: FileType,
        _mode: u32,
        _uid: u32,
        _gid: u32,
        _rdev: u32,
    ) -> Result<i64, MetaError> {
        Self::readonly().await
    }
    async fn link(&self, _ino: i64, _parent: i64, _name: &str) -> Result<FileAttr, MetaError> {
        Self::readonly().await
    }
    async fn symlink(
        &self,
        _parent: i64,
        _name: &str,
        _target: &str,
    ) -> Result<(i64, FileAttr), MetaError> {
        Self::readonly().await
    }
    async fn unlink(&self, _parent: i64, _name: &str) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn rename(
        &self,
        _old_parent: i64,
        _old_name: &str,
        _new_parent: i64,
        _new_name: String,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn rename_noreplace(
        &self,
        _old_parent: i64,
        _old_name: &str,
        _new_parent: i64,
        _new_name: String,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn rename_exchange(
        &self,
        _old_parent: i64,
        _old_name: &str,
        _new_parent: i64,
        _new_name: &str,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn set_file_size(&self, _ino: i64, _size: u64) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn extend_file_size(&self, _ino: i64, _size: u64) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn truncate(&self, _ino: i64, _size: u64, _chunk_size: u64) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_names(&self, ino: i64) -> Result<Vec<(Option<i64>, String)>, MetaError> {
        let inode = Self::inode(ino)?;
        let Some(index) = self.catalog.inode_paged(inode).await.map_err(map_error)? else {
            return Ok(Vec::new());
        };
        Ok(vec![(
            Some(index.parent_inode as i64),
            String::from_utf8(index.name).map_err(|_| MetaError::InvalidFilename)?,
        )])
    }
    async fn get_dentries(&self, ino: i64) -> Result<Vec<(i64, String)>, MetaError> {
        Ok(self
            .get_names(ino)
            .await?
            .into_iter()
            .filter_map(|(parent, name)| parent.map(|parent| (parent, name)))
            .collect())
    }
    async fn get_dir_parent(&self, dir_ino: i64) -> Result<Option<i64>, MetaError> {
        let inode = Self::inode(dir_ino)?;
        Ok(self
            .catalog
            .inode_paged(inode)
            .await
            .map_err(map_error)?
            .map(|entry| entry.parent_inode as i64))
    }
    async fn get_paths(&self, ino: i64) -> Result<Vec<String>, MetaError> {
        let inode = Self::inode(ino)?;
        if inode == self.manifest.root_inode {
            return Ok(vec![String::from("/")]);
        }

        // The inode index already carries the parent and raw dentry name.
        // Walk only this inode's ancestor chain; do not scan directory groups
        // or materialize a reverse path table for the whole snapshot.
        let mut components = Vec::new();
        let mut current = inode;
        let mut depth = 0usize;
        while current != self.manifest.root_inode {
            depth = depth.saturating_add(1);
            if depth > 1024 {
                return Err(MetaError::Internal(
                    "packed v3 inode path exceeds maximum depth".into(),
                ));
            }
            let Some(entry) = self.catalog.inode_paged(current).await.map_err(map_error)? else {
                return Ok(Vec::new());
            };
            if entry.name.is_empty()
                || entry.name == b"."
                || entry.name == b".."
                || entry.name.contains(&b'/')
            {
                return Err(MetaError::InvalidFilename);
            }
            components.push(entry.name);
            current = entry.parent_inode;
        }

        components.reverse();
        let mut path = String::new();
        for component in components {
            let component = String::from_utf8(component).map_err(|_| MetaError::InvalidFilename)?;
            path.push('/');
            path.push_str(&component);
        }
        Ok(vec![if path.is_empty() {
            String::from("/")
        } else {
            path
        }])
    }
    async fn read_symlink(&self, _ino: i64) -> Result<String, MetaError> {
        Err(MetaError::NotSupported(
            "packed v3 symlink targets are not present in GM05".into(),
        ))
    }
    async fn set_attr(
        &self,
        _ino: i64,
        _req: &SetAttrRequest,
        _flags: SetAttrFlags,
    ) -> Result<FileAttr, MetaError> {
        Self::readonly().await
    }
    async fn open(&self, ino: i64, flags: OpenFlags) -> Result<FileAttr, MetaError> {
        if flags.intersects(
            OpenFlags::WRONLY
                | OpenFlags::RDWR
                | OpenFlags::APPEND
                | OpenFlags::TRUNC
                | OpenFlags::CREATE,
        ) {
            return Self::readonly().await;
        }
        self.stat_fresh(ino).await?.ok_or(MetaError::NotFound(ino))
    }
    async fn close(&self, _ino: i64) -> Result<(), MetaError> {
        Ok(())
    }
    async fn write(
        &self,
        _ino: i64,
        _chunk_id: u64,
        _slice: SliceDesc,
        _new_size: u64,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_deleted_files(&self) -> Result<Vec<i64>, MetaError> {
        Ok(Vec::new())
    }
    async fn remove_file_metadata(&self, _ino: i64) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_slices(&self, chunk_id: u64) -> Result<Vec<SliceDesc>, MetaError> {
        let (ino, chunk_index) = extract_ino_and_chunk_index(chunk_id);
        if ino <= 0
            || chunk_id_for(ino, chunk_index)
                .map_err(|error| MetaError::Internal(error.to_string()))?
                != chunk_id
        {
            return Err(MetaError::Internal("invalid packed v3 chunk id".into()));
        }
        let Some((_, entry)) = self.entry(ino as u64).await? else {
            return Err(MetaError::NotFound(ino));
        };
        self.chunk_slices(chunk_id, &entry, chunk_index)
    }
    async fn append_slice(&self, _chunk_id: u64, _slice: SliceDesc) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn next_id(&self, _key: &str) -> Result<i64, MetaError> {
        Self::readonly().await
    }
    async fn start_session(&self, _session_info: SessionInfo) -> Result<(), MetaError> {
        Ok(())
    }
    async fn shutdown_session(&self) -> Result<(), MetaError> {
        Ok(())
    }
    async fn get_plock(
        &self,
        _inode: i64,
        _query: &FileLockQuery,
    ) -> Result<FileLockInfo, MetaError> {
        Self::readonly().await
    }
    async fn set_plock(
        &self,
        _inode: i64,
        _owner: i64,
        _block: bool,
        _lock_type: FileLockType,
        _range: FileLockRange,
        _pid: u32,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_flock(&self, _inode: i64, _owner: i64) -> Result<FileLockType, MetaError> {
        Self::readonly().await
    }
    async fn set_flock(
        &self,
        _inode: i64,
        _owner: i64,
        _block: bool,
        _lock_type: FileLockType,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn set_xattr(
        &self,
        _inode: i64,
        _name: &str,
        _value: &[u8],
        _flags: u32,
    ) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_xattr(&self, _inode: i64, _name: &str) -> Result<Option<Vec<u8>>, MetaError> {
        Ok(None)
    }
    async fn list_xattr(&self, _inode: i64) -> Result<Vec<String>, MetaError> {
        Ok(Vec::new())
    }
    async fn remove_xattr(&self, _inode: i64, _name: &str) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn set_acl(&self, _inode: i64, _rule: AclRule) -> Result<(), MetaError> {
        Self::readonly().await
    }
    async fn get_acl(
        &self,
        _inode: i64,
        _acl_type: u8,
        _acl_id: u32,
    ) -> Result<Option<AclRule>, MetaError> {
        Ok(None)
    }
}

impl<B: ObjectBackend + Clone + 'static> PackedV3ReadonlyMeta<B> {
    async fn readdir_page_raw(
        &self,
        ino: i64,
        child_offset: usize,
        limit: usize,
    ) -> Result<Vec<GroupMetaEntry>, MetaError> {
        let parent = Self::inode(ino)?;
        self.catalog
            .readdir_page(self.parent_key(parent), child_offset, limit)
            .await
            .map_err(map_error)
    }
}

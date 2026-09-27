//! Build a local packed-metadata v3 snapshot for cold-read tests.
//!
//! The fixture intentionally uses one immutable group container per directory
//! and publishes pageable group/inode indexes. This keeps generation simple
//! while exercising the same bounded lookup path used by large snapshots.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use sha2::{Digest, Sha256};

use brewfs::cadapter::client::ObjectClient;
use brewfs::cadapter::localfs::LocalFsBackend;
use brewfs::workspace_overlay::packed_v3::{
    AccessProfile, GroupMeta, PackedContainerRef, PackedFileInput, PackedGroupContainer,
    PackedGroupIndexPage, PackedGroupIndexPageRef, PackedGroupRef, PackedInodeIndexEntry,
    PackedInodeIndexPage, PackedInodeIndexPageRef, PackedSnapshotManifest, SizeClassTable,
    directory_key, pack_group_files,
};

#[derive(Debug, Parser)]
#[command(about = "build a local packed-metadata v3 fixture")]
struct Args {
    #[arg(long, default_value = "packed-v3-objects")]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 2)]
    dir_levels: u32,
    #[arg(long, default_value_t = 10)]
    dirs_per_level: u64,
    #[arg(long, default_value_t = 1000)]
    files_per_dir: u64,
    #[arg(long, default_value_t = 102_400)]
    small_file_size: u64,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    chunk_size: u64,
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    block_size: u32,
    #[arg(long, default_value = "packed-v3-manifest-key.txt")]
    manifest_output: PathBuf,
}

#[derive(Clone)]
struct Directory {
    inode: u64,
    parent_inode: u64,
    name: Vec<u8>,
}

fn fill_pattern(size: usize, file_index: u64) -> Vec<u8> {
    let mut data = vec![0u8; size];
    let seed = file_index.wrapping_add(1).to_le_bytes();
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = seed[index % seed.len()] ^ (index as u64).rotate_left(7) as u8;
    }
    data
}

fn page_ref(object_key: Vec<u8>, object: &[u8]) -> PackedContainerRef {
    PackedContainerRef {
        object_key,
        object_len: object.len() as u64,
        object_digest: Sha256::digest(object).into(),
    }
}

fn build_tree(
    levels: u32,
    fanout: u64,
    next_inode: &mut u64,
    parent: Directory,
    output: &mut Vec<Directory>,
) {
    output.push(parent.clone());
    if levels == 0 {
        return;
    }
    for index in 0..fanout {
        let inode = *next_inode;
        *next_inode += 1;
        let name = format!("d{index:03}").into_bytes();
        let child = Directory {
            inode,
            parent_inode: parent.inode,
            name,
        };
        build_tree(levels - 1, fanout, next_inode, child, output);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.dir_levels > 8 || args.dirs_per_level == 0 || args.files_per_dir == 0 {
        bail!("dir-levels must be <= 8 and fanout/files-per-dir must be non-zero");
    }
    if args.small_file_size == 0 || args.small_file_size > 4 * 1024 * 1024 {
        bail!("small-file-size must be in 1..=4 MiB");
    }
    if args.chunk_size < u64::from(args.block_size) || args.block_size == 0 {
        bail!("chunk-size must be >= block-size and block-size must be non-zero");
    }
    std::fs::create_dir_all(&args.output_dir)?;
    let backend = LocalFsBackend::new(&args.output_dir);
    let client = ObjectClient::new(backend);

    let snapshot_id: [u8; 32] = Sha256::digest(b"brewfs-packed-v3-fixture").into();
    let root_key = directory_key(snapshot_id, 1);
    let root = Directory {
        inode: 1,
        parent_inode: 0,
        name: Vec::new(),
    };
    let mut next_inode = 2u64;
    let mut directories = Vec::new();
    build_tree(
        args.dir_levels,
        args.dirs_per_level,
        &mut next_inode,
        root,
        &mut directories,
    );

    let mut groups = Vec::with_capacity(directories.len());
    let mut containers = Vec::with_capacity(directories.len());
    let mut inode_entries = Vec::new();
    let mut file_index = 0u64;

    for directory in &directories {
        let group_parent_key = if directory.inode == 1 {
            root_key
        } else {
            directory_key(snapshot_id, directory.inode)
        };
        let mut files = Vec::with_capacity(args.files_per_dir as usize + 8);
        for child in directories
            .iter()
            .filter(|child| child.parent_inode == directory.inode)
        {
            files.push(PackedFileInput {
                name: child.name.clone(),
                inode: child.inode,
                kind: 2,
                mode: 0o040755,
                uid: 0,
                gid: 0,
                rdev: 0,
                nlink: 2,
                atime_ns: 0,
                mtime_ns: 0,
                ctime_ns: 0,
                flags: 0,
                data: Vec::new(),
            });
        }
        for index in 0..args.files_per_dir {
            files.push(PackedFileInput {
                name: format!("f{index:06}").into_bytes(),
                inode: next_inode,
                kind: 1,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                rdev: 0,
                nlink: 1,
                atime_ns: 0,
                mtime_ns: 0,
                ctime_ns: 0,
                flags: 0,
                data: fill_pattern(args.small_file_size as usize, file_index),
            });
            next_inode += 1;
            file_index += 1;
        }
        let (group_input, frames) = pack_group_files(
            directory.inode,
            group_parent_key,
            files,
            AccessProfile::RandomSmallFile,
            SizeClassTable::default(),
            Some(args.small_file_size),
        )?;
        let group_id = directory.inode;
        let container_bytes = PackedGroupContainer::build(
            group_id,
            AccessProfile::RandomSmallFile,
            vec![group_input.clone()],
            frames,
        )?;
        let container_key = format!("containers/{group_id:016x}.brfgc");
        client.put_object(&container_key, &container_bytes).await?;
        let opened = PackedGroupContainer::open(container_bytes.clone())?;
        let descriptor = &opened.groups()[0];
        let metadata = GroupMeta::decode(&group_input.metadata)?;
        let first_name = metadata
            .entries()
            .first()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        let last_name = metadata
            .entries()
            .last()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        groups.push(PackedGroupRef {
            group_id,
            container_ordinal: containers.len() as u32,
            parent_dir_key: group_parent_key,
            first_name,
            last_name,
            meta_offset: descriptor.metadata_offset,
            meta_len: descriptor.metadata_len,
            data_offset: descriptor.data_offset,
            data_len: descriptor.data_len,
            entry_count: descriptor.entry_count,
            file_count: descriptor.file_count,
            frame_count: descriptor.frame_ordinals.len() as u32,
            layout_profile: descriptor.layout_profile,
            metadata_digest: descriptor.metadata_digest,
            data_digest: descriptor.data_digest,
        });
        containers.push(page_ref(container_key.into_bytes(), &container_bytes));
        for (entry_ordinal, entry) in metadata.entries().iter().enumerate() {
            inode_entries.push(PackedInodeIndexEntry {
                inode: entry.inode,
                parent_inode: directory.inode,
                parent_dir_key: group_parent_key,
                group_id,
                entry_ordinal: entry_ordinal as u32,
                name: entry.name.clone(),
                kind: entry.kind,
                mode: entry.mode,
                uid: entry.uid,
                gid: entry.gid,
                rdev: entry.rdev,
                nlink: entry.nlink,
                atime_ns: entry.atime_ns,
                mtime_ns: entry.mtime_ns,
                ctime_ns: entry.ctime_ns,
                size: entry.size,
            });
        }
    }

    groups.sort_by(|left, right| {
        left.parent_dir_key
            .cmp(&right.parent_dir_key)
            .then_with(|| left.first_name.cmp(&right.first_name))
    });
    inode_entries.sort_by_key(|entry| entry.inode);

    let mut group_pages = Vec::new();
    for (page_ordinal, chunk) in groups.chunks(4096).enumerate() {
        let page = PackedGroupIndexPage {
            snapshot_id,
            page_ordinal: page_ordinal as u32,
            total_pages: groups.len().div_ceil(4096) as u32,
            groups: chunk.to_vec(),
        };
        let bytes = page.encode()?;
        let key = format!("indexes/groups-{page_ordinal:08}.brfgi");
        client.put_object(&key, &bytes).await?;
        let first = chunk.first().context("empty group index page")?;
        let last = chunk.last().context("empty group index page")?;
        group_pages.push(PackedGroupIndexPageRef {
            object: page_ref(key.into_bytes(), &bytes),
            first_parent_dir_key: first.parent_dir_key,
            first_name: first.first_name.clone(),
            last_parent_dir_key: last.parent_dir_key,
            last_name: last.last_name.clone(),
        });
    }

    let mut inode_pages = Vec::new();
    for (page_ordinal, chunk) in inode_entries.chunks(4096).enumerate() {
        let page = PackedInodeIndexPage {
            snapshot_id,
            page_ordinal: page_ordinal as u32,
            total_pages: inode_entries.len().div_ceil(4096) as u32,
            entries: chunk.to_vec(),
        };
        let bytes = page.encode()?;
        let key = format!("indexes/inodes-{page_ordinal:08}.brfii");
        client.put_object(&key, &bytes).await?;
        let first = chunk.first().context("empty inode index page")?;
        let last = chunk.last().context("empty inode index page")?;
        inode_pages.push(PackedInodeIndexPageRef {
            object: page_ref(key.into_bytes(), &bytes),
            first_inode: first.inode,
            last_inode: last.inode,
        });
    }

    let manifest = PackedSnapshotManifest {
        snapshot_id,
        root_dir_key: root_key,
        root_inode: 1,
        layout_profile: AccessProfile::RandomSmallFile,
        size_classes: SizeClassTable::default(),
        groups: Vec::new(),
        containers,
        group_index_pages: group_pages,
        inode_index_pages: inode_pages,
    };
    let manifest_bytes = manifest.encode()?;
    client.put_object("manifest.brpm", &manifest_bytes).await?;
    std::fs::write(&args.manifest_output, "manifest.brpm\n")?;
    println!(
        "manifest_key=manifest.brpm directories={} files={} manifest_bytes={}",
        directories.len(),
        file_index,
        manifest_bytes.len()
    );
    Ok(())
}

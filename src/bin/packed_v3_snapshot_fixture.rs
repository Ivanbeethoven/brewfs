//! Build a local packed-metadata v3 snapshot for cold-read tests.
//!
//! The fixture shards large directories into bounded immutable groups and
//! publishes pageable group/inode indexes, matching the read path used by
//! large snapshots.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use clap::Parser;
use sha2::{Digest, Sha256};
use tokio::{sync::Semaphore, task::JoinSet};

use brewfs::cadapter::client::{ObjectBackend, ObjectClient};
use brewfs::cadapter::localfs::LocalFsBackend;
use brewfs::cadapter::s3::{S3Backend, S3Config};
use brewfs::workspace_overlay::packed_v3::{
    AccessProfile, ContainerPackingLimits, GroupMeta, GroupPackingLimits, PackedContainerRef,
    PackedFileInput, PackedFrameInput, PackedGroupContainer, PackedGroupIndexPage,
    PackedGroupIndexPageRef, PackedGroupInput, PackedGroupRef, PackedInodeIndexEntry,
    PackedInodeIndexPage, PackedInodeIndexPageRef, PackedSnapshotManifest, SizeClassTable,
    directory_key, pack_group_file_shards, pack_group_shard_containers,
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
    /// Optional deterministic size range. When omitted, `small_file_size`
    /// keeps the historical fixed-size fixture behavior.
    #[arg(long)]
    small_file_min_size: Option<u64>,
    #[arg(long)]
    small_file_max_size: Option<u64>,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    chunk_size: u64,
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    block_size: u32,
    #[arg(long, default_value = "packed-v3-manifest-key.txt")]
    manifest_output: PathBuf,
    #[arg(long)]
    bucket: Option<String>,
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    region: Option<String>,
    #[arg(long, default_value = "")]
    prefix: String,
    /// Packing profile for the immutable payload layout. Keep the default
    /// random profile for compatibility; sequential scans should opt into
    /// `sequential-small-file` so containers are grouped for read-ahead.
    #[arg(long, default_value = "random-small-file")]
    access_profile: String,
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    force_path_style: bool,
    /// Upload the deterministic file tree as individual raw objects instead
    /// of building packed metadata. This is the JuiceFS comparison source.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    raw_only: bool,
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

fn file_size_for_index(file_index: u64, min_size: u64, max_size: u64) -> usize {
    let span = max_size.saturating_sub(min_size).saturating_add(1);
    let mixed = file_index
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    min_size.saturating_add(if span == 0 { 0 } else { mixed % span }) as usize
}

fn file_size_range(args: &Args) -> Result<(u64, u64)> {
    let min_size = args.small_file_min_size.unwrap_or(args.small_file_size);
    let max_size = args.small_file_max_size.unwrap_or(args.small_file_size);
    if min_size == 0 || min_size > max_size || max_size > 4 * 1024 * 1024 {
        bail!("small file size range must satisfy 0 < min <= max <= 4 MiB");
    }
    Ok((min_size, max_size))
}

fn access_profile(value: &str) -> Result<AccessProfile> {
    match value.trim().to_ascii_lowercase().as_str() {
        "random" | "random-small-file" | "random_small_file" => Ok(AccessProfile::RandomSmallFile),
        "sequential" | "sequential-small-file" | "sequential_small_file" => {
            Ok(AccessProfile::SequentialSmallFile)
        }
        "mixed" => Ok(AccessProfile::Mixed),
        other => bail!(
            "unsupported access profile {other}; expected random-small-file, sequential-small-file, or mixed"
        ),
    }
}

fn leaf_path(leaf_index: u64, levels: u32, fanout: u64) -> String {
    let mut components = Vec::with_capacity(levels as usize);
    let mut value = leaf_index;
    for _ in 0..levels {
        components.push(format!("d{:03}", value % fanout));
        value /= fanout;
    }
    components.reverse();
    components.join("/")
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

fn object_key(prefix: &str, relative: &str) -> String {
    if prefix.is_empty() {
        relative.to_owned()
    } else {
        format!("{prefix}/{relative}")
    }
}

/// Publish a bounded batch of group shards before the next directory is built.
///
/// The fixture is also used for 100k+ file cloud tests. Holding every frame in
/// `pending_groups` until the manifest is assembled would make the builder's
/// memory usage proportional to the entire logical dataset. Container packing
/// is intentionally incremental so the live payload stays near one container.
async fn publish_container_batch<B>(
    client: &ObjectClient<B>,
    prefix: &str,
    pending_groups: &mut Vec<(PackedGroupInput, Vec<PackedFrameInput>)>,
    next_container_id: &mut u64,
    groups: &mut Vec<PackedGroupRef>,
    containers: &mut Vec<PackedContainerRef>,
    inode_entries: &mut Vec<PackedInodeIndexEntry>,
    group_parent_inodes: &HashMap<u64, u64>,
    profile: AccessProfile,
) -> Result<()>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    if pending_groups.is_empty() {
        return Ok(());
    }
    let packed_containers = pack_group_shard_containers(
        *next_container_id,
        std::mem::take(pending_groups),
        ContainerPackingLimits::for_profile(profile),
    )?;
    let batch_count = packed_containers.len() as u64;
    let upload_limit = Arc::new(Semaphore::new(4));
    let mut uploads = JoinSet::new();
    for container in packed_containers {
        let container_key = object_key(
            prefix,
            &format!("containers/{:016x}.brfgc", container.container_id),
        );
        let container_bytes = PackedGroupContainer::build(
            container.container_id,
            profile,
            container.groups.clone(),
            container.frames,
        )?;
        let opened = PackedGroupContainer::open(container_bytes.clone())?;
        let container_ordinal = containers.len() as u32;
        for (group_input, descriptor) in container.groups.iter().zip(opened.groups()) {
            let group_id = group_input.group_id;
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
            let parent_dir_inode = *group_parent_inodes
                .get(&group_id)
                .context("packed group parent inode is missing")?;
            groups.push(PackedGroupRef {
                group_id,
                container_ordinal,
                parent_dir_key: descriptor.parent_dir_key,
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
            for (entry_ordinal, entry) in metadata.entries().iter().enumerate() {
                inode_entries.push(PackedInodeIndexEntry {
                    inode: entry.inode,
                    parent_inode: parent_dir_inode,
                    parent_dir_key: descriptor.parent_dir_key,
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
        containers.push(page_ref(
            container_key.clone().into_bytes(),
            &container_bytes,
        ));

        let permit = upload_limit
            .clone()
            .acquire_owned()
            .await
            .context("packed container upload semaphore closed")?;
        let upload_client = client.clone();
        uploads.spawn(async move {
            let _permit = permit;
            upload_client
                .put_object(&container_key, &container_bytes)
                .await
                .with_context(|| format!("upload packed container {container_key}"))
        });
        if uploads.len() >= 4 {
            let _ = uploads
                .join_next()
                .await
                .context("packed container upload task disappeared")??;
        }
    }
    while let Some(result) = uploads.join_next().await {
        result??;
    }
    *next_container_id = (*next_container_id)
        .checked_add(batch_count)
        .ok_or_else(|| anyhow::anyhow!("packed container id exceeds u64"))?;
    Ok(())
}

/// Upload the same deterministic tree as one object per file. The bounded
/// JoinSet keeps SDK upload memory independent of the total dataset size.
async fn build_raw_fixture<B>(args: &Args, client: ObjectClient<B>, prefix: &str) -> Result<()>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    let (min_file_size, max_file_size) = file_size_range(args)?;
    let leaf_count = args
        .dirs_per_level
        .checked_pow(args.dir_levels)
        .context("directory count overflows u64")?;
    let file_count = leaf_count
        .checked_mul(args.files_per_dir)
        .context("file count overflows u64")?;
    let semaphore = Arc::new(Semaphore::new(64));
    let mut uploads = JoinSet::new();
    let mut manifest = Vec::with_capacity(file_count as usize * 64);
    let mut logical_bytes = 0u64;

    for leaf_index in 0..leaf_count {
        let directory = leaf_path(leaf_index, args.dir_levels, args.dirs_per_level);
        for file_index in 0..args.files_per_dir {
            let global_file_index = leaf_index
                .checked_mul(args.files_per_dir)
                .and_then(|value| value.checked_add(file_index))
                .context("file index overflows u64")?;
            let size = file_size_for_index(global_file_index, min_file_size, max_file_size);
            let key = object_key(prefix, &format!("{directory}/f{file_index:06}"));
            manifest.extend_from_slice(format!("{key}\t{global_file_index}\t{size}\n").as_bytes());
            logical_bytes = logical_bytes.saturating_add(size as u64);
            let permit = semaphore.clone().acquire_owned().await?;
            let upload_client = client.clone();
            uploads.spawn(async move {
                let _permit = permit;
                let data = fill_pattern(size, global_file_index);
                upload_client.put_object(&key, &data).await
            });
            if uploads.len() >= 64 {
                let _ = uploads
                    .join_next()
                    .await
                    .context("raw SDK upload task disappeared")??;
            }
        }
    }
    while let Some(result) = uploads.join_next().await {
        result??;
    }

    let manifest_key = object_key(prefix, "raw-manifest.tsv");
    client.put_object(&manifest_key, &manifest).await?;
    std::fs::write(&args.manifest_output, format!("{manifest_key}\n"))?;
    println!(
        "raw_manifest_key={manifest_key} files={file_count} min_file_size={min_file_size} max_file_size={max_file_size} logical_bytes={logical_bytes} manifest_bytes={} prefix={prefix}",
        manifest.len()
    );
    Ok(())
}

async fn build_fixture<B>(args: &Args, client: ObjectClient<B>, prefix: &str) -> Result<()>
where
    B: ObjectBackend + Clone + Send + Sync + 'static,
{
    let (min_file_size, max_file_size) = file_size_range(args)?;
    let profile = access_profile(&args.access_profile)?;
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
    let mut pending_groups = Vec::new();
    let mut group_parent_inodes = HashMap::new();
    let container_limits = ContainerPackingLimits::for_profile(profile);
    // Accumulate several target-sized containers so the bounded uploader can
    // keep multiple OSS requests in flight without retaining the full fixture.
    let publish_target_bytes = container_limits.target_body_bytes.saturating_mul(4);
    let mut next_container_id = 1u64;
    let mut pending_bytes = 0usize;
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
        let is_leaf = !directories
            .iter()
            .any(|child| child.parent_inode == directory.inode);
        if is_leaf {
            for index in 0..args.files_per_dir {
                let size = file_size_for_index(file_index, min_file_size, max_file_size);
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
                    data: fill_pattern(size, file_index),
                });
                next_inode += 1;
                file_index += 1;
            }
        }
        // Keep the high bits tied to the directory and use the low bits for
        // its lexicographic shard ordinal. This preserves stable, collision-
        // free group ids when a large directory is split.
        let group_id_base = directory
            .inode
            .checked_shl(32)
            .context("directory inode cannot be encoded as a group id")?;
        let shards = pack_group_file_shards(
            group_id_base,
            group_parent_key,
            files,
            profile,
            SizeClassTable::default(),
            None,
            GroupPackingLimits::for_profile(profile),
        )?;
        for (group_input, frames) in shards {
            group_parent_inodes.insert(group_input.group_id, directory.inode);
            pending_bytes = pending_bytes.saturating_add(
                group_input.metadata.len()
                    + frames.iter().map(|frame| frame.raw.len()).sum::<usize>(),
            );
            pending_groups.push((group_input, frames));
            if pending_bytes >= publish_target_bytes {
                publish_container_batch(
                    &client,
                    prefix,
                    &mut pending_groups,
                    &mut next_container_id,
                    &mut groups,
                    &mut containers,
                    &mut inode_entries,
                    &group_parent_inodes,
                    profile,
                )
                .await?;
                pending_bytes = 0;
            }
        }
    }
    publish_container_batch(
        &client,
        prefix,
        &mut pending_groups,
        &mut next_container_id,
        &mut groups,
        &mut containers,
        &mut inode_entries,
        &group_parent_inodes,
        profile,
    )
    .await?;

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
        let key = object_key(prefix, &format!("indexes/groups-{page_ordinal:08}.brfgi"));
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
        let key = object_key(prefix, &format!("indexes/inodes-{page_ordinal:08}.brfii"));
        client.put_object(&key, &bytes).await?;
        let first = chunk.first().context("empty inode index page")?;
        let last = chunk.last().context("empty inode index page")?;
        inode_pages.push(PackedInodeIndexPageRef {
            object: page_ref(key.into_bytes(), &bytes),
            first_inode: first.inode,
            last_inode: last.inode,
        });
    }

    let group_count = groups.len();
    let container_count = containers.len();
    let manifest = PackedSnapshotManifest {
        snapshot_id,
        root_dir_key: root_key,
        root_inode: 1,
        layout_profile: profile,
        size_classes: SizeClassTable::default(),
        groups: Vec::new(),
        containers,
        group_index_pages: group_pages,
        inode_index_pages: inode_pages,
    };
    let manifest_bytes = manifest.encode()?;
    let manifest_key = object_key(prefix, "manifest.brpm");
    client.put_object(&manifest_key, &manifest_bytes).await?;
    std::fs::write(&args.manifest_output, format!("{manifest_key}\n"))?;
    let logical_bytes: u64 = inode_entries.iter().map(|entry| entry.size).sum();
    println!(
        "manifest_key={manifest_key} directories={} groups={} containers={} files={} min_file_size={} max_file_size={} logical_bytes={} manifest_bytes={} prefix={prefix}",
        directories.len(),
        group_count,
        container_count,
        file_index,
        min_file_size,
        max_file_size,
        logical_bytes,
        manifest_bytes.len()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.dir_levels > 8 || args.dirs_per_level == 0 || args.files_per_dir == 0 {
        bail!("dir-levels must be <= 8 and fanout/files-per-dir must be non-zero");
    }
    file_size_range(&args)?;
    access_profile(&args.access_profile)?;
    if args.chunk_size < u64::from(args.block_size) || args.block_size == 0 {
        bail!("chunk-size must be >= block-size and block-size must be non-zero");
    }
    let prefix = args.prefix.trim_matches('/');
    if let Some(bucket) = args.bucket.as_deref() {
        let backend = S3Backend::with_config(S3Config {
            bucket: bucket.to_owned(),
            endpoint: args.endpoint.clone(),
            region: args.region.clone(),
            force_path_style: args.force_path_style,
            part_size: 16 * 1024 * 1024,
            max_concurrency: 32,
            disable_payload_checksum: true,
            ..Default::default()
        })
        .await?;
        let client = ObjectClient::new(backend);
        if args.raw_only {
            build_raw_fixture(&args, client, prefix).await
        } else {
            build_fixture(&args, client, prefix).await
        }
    } else {
        std::fs::create_dir_all(&args.output_dir)?;
        let backend = LocalFsBackend::new(&args.output_dir);
        let client = ObjectClient::new(backend);
        if args.raw_only {
            build_raw_fixture(&args, client, prefix).await
        } else {
            build_fixture(&args, client, prefix).await
        }
    }
}

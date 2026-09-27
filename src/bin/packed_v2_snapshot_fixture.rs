//! Build and publish a small immutable packed-metadata v2 fixture.
//!
//! The fixture is deliberately generated from a local directory and passed
//! through the production v2 ingest/publication path.  Each small file gets a
//! file-specific byte pattern; no shared-content shortcut is used by the
//! producer or the benchmark.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::Parser;

use brewfs::cadapter::client::ObjectClient;
use brewfs::cadapter::s3::{S3Backend, S3Config};
use brewfs::native_base::ingest::ConsistencyPolicy;
use brewfs::workspace_overlay::clustered_snapshot::{
    build_local_directory_cluster, build_single_cluster_manifest,
};

#[derive(Debug, Parser)]
#[command(about = "publish a packed-metadata v2 benchmark fixture")]
struct Args {
    #[arg(long, default_value = "brewfs-data")]
    bucket: String,
    #[arg(long, default_value = "http://127.0.0.1:19000")]
    endpoint: String,
    #[arg(long, default_value = "us-east-1")]
    region: String,
    #[arg(long, default_value = "packed-v2-fixture")]
    prefix: String,
    #[arg(long, default_value_t = 2)]
    dir_levels: u32,
    #[arg(long, default_value_t = 10)]
    dirs_per_level: u64,
    #[arg(long, default_value_t = 10)]
    files_per_dir: u64,
    #[arg(long, default_value_t = 102_400)]
    small_file_size: u64,
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    fio_file_size: u64,
    /// Use path-style S3 requests. Alibaba OSS normally uses virtual-host
    /// addressing, so this remains disabled unless explicitly requested.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    force_path_style: bool,
    #[arg(long, default_value = "packed-v2-manifest-key.txt")]
    manifest_output: PathBuf,
}

fn identity(seed: &str, label: &str) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"BrewFS.aliyun-packed-v2-fixture");
    hasher.update(seed.as_bytes());
    hasher.update(label.as_bytes());
    hasher.finalize().as_bytes()[..16]
        .try_into()
        .expect("blake3 prefix length")
}

fn fill_pattern(buffer: &mut [u8], file_index: u64, offset: u64) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        let position = offset.wrapping_add(index as u64);
        let mixed = file_index
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(position.rotate_left(17));
        *byte = (mixed ^ (position >> 11)) as u8;
    }
    if offset == 0 {
        let header = file_index.wrapping_add(1).to_le_bytes();
        let count = buffer.len().min(header.len());
        buffer[..count].copy_from_slice(&header[..count]);
    }
}

fn write_pattern(path: &Path, length: u64, file_index: u64) -> Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("create fixture file {}", path.display()))?;
    let mut offset = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    while offset < length {
        let count = (length - offset).min(buffer.len() as u64) as usize;
        fill_pattern(&mut buffer[..count], file_index, offset);
        file.write_all(&buffer[..count])?;
        offset += count as u64;
    }
    file.sync_all()?;
    Ok(())
}

fn build_leaf_tree(
    root: &Path,
    level: u32,
    levels: u32,
    fanout: u64,
    components: &mut Vec<String>,
    leaves: &mut Vec<PathBuf>,
) -> Result<()> {
    if level == levels {
        leaves.push(
            components
                .iter()
                .fold(root.to_path_buf(), |path, component| path.join(component)),
        );
        return Ok(());
    }
    for index in 0..fanout {
        let name = format!("d{index:03}");
        let path = components
            .iter()
            .fold(root.to_path_buf(), |path, component| path.join(component));
        let path = path.join(&name);
        fs::create_dir_all(&path)?;
        components.push(name);
        build_leaf_tree(root, level + 1, levels, fanout, components, leaves)?;
        components.pop();
    }
    Ok(())
}

fn create_source_tree(args: &Args, root: &Path) -> Result<u64> {
    if args.dir_levels == 0 || args.dirs_per_level == 0 || args.files_per_dir == 0 {
        bail!("directory levels, fanout, and files per directory must be non-zero");
    }
    if args.dir_levels > 8 {
        bail!("directory levels are capped at 8 for the remote smoke test");
    }
    if args.small_file_size == 0 || args.small_file_size > 4 * 1024 * 1024 {
        bail!("small-file-size must be in the range 1..=4 MiB");
    }
    if args.fio_file_size == 0 || !args.fio_file_size.is_multiple_of(1 << 20) {
        bail!("fio-file-size must be a positive multiple of 1 MiB");
    }

    fs::create_dir_all(root)?;
    let mut leaves = Vec::new();
    build_leaf_tree(
        root,
        0,
        args.dir_levels,
        args.dirs_per_level,
        &mut Vec::new(),
        &mut leaves,
    )?;
    if leaves.is_empty() {
        bail!("directory tree produced no leaves");
    }
    let mut file_index = 0u64;
    for leaf in &leaves {
        for index in 0..args.files_per_dir {
            let path = leaf.join(format!("f{index:05}"));
            write_pattern(&path, args.small_file_size, file_index)?;
            file_index += 1;
        }
    }

    let bench = root.join("bench");
    fs::create_dir_all(&bench)?;
    write_pattern(&bench.join("read.bin"), args.fio_file_size, file_index)?;
    Ok(file_index)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_nanos();
    let source_root = std::env::temp_dir().join(format!("brewfs-packed-v2-{unique}"));
    let volume_id = identity(&args.prefix, "volume");
    let cluster_id = identity(&args.prefix, "cluster");
    let snapshot_id = identity(&args.prefix, "snapshot");
    let route_seed = identity(&args.prefix, "route");

    let result = async {
        let small_file_count = create_source_tree(&args, &source_root)?;
        let built = build_local_directory_cluster(
            &source_root,
            ConsistencyPolicy::SnapshotBacked,
            cluster_id,
            volume_id,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let data = built
            .data
            .as_ref()
            .context("v2 ingest did not produce DataPack/Data Seal")?;
        let bundle = build_single_cluster_manifest(&built, snapshot_id, route_seed)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let config = S3Config {
            bucket: args.bucket.clone(),
            region: Some(args.region.clone()),
            endpoint: Some(args.endpoint.clone()),
            force_path_style: args.force_path_style,
            part_size: 16 * 1024 * 1024,
            max_concurrency: 16,
            disable_payload_checksum: true,
            ..Default::default()
        };
        let backend = S3Backend::with_config(config).await?;
        let client = ObjectClient::new(backend);
        let metadata_key = format!("clusters/{}/metadata.brfc", hex::encode(cluster_id));
        let seal_key = format!("clusters/{}/data/seal.brfds", hex::encode(cluster_id));
        let manifest_key = String::from_utf8(bundle.manifest_ref.key.clone())
            .context("v2 manifest key is not UTF-8")?;
        client
            .put_object(&metadata_key, &built.cluster.bytes)
            .await?;
        client.put_object(&data.object_key, &data.data_pack).await?;
        client.put_object(&seal_key, &data.data_seal).await?;
        client.put_object(&manifest_key, &bundle.bytes).await?;
        fs::write(&args.manifest_output, format!("{manifest_key}\n"))?;
        println!("manifest_key={manifest_key}");
        println!(
            "format=packed-metadata-v2 files={small_file_count} file_size={} levels={} fanout={} files_per_leaf={} fio_file_size={} data_pack_bytes={} data_seal_bytes={} metadata_bytes={} manifest_bytes={}",
            args.small_file_size,
            args.dir_levels,
            args.dirs_per_level,
            args.files_per_dir,
            args.fio_file_size,
            data.data_pack.len(),
            data.data_seal.len(),
            built.cluster.bytes.len(),
            bundle.bytes.len(),
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = fs::remove_dir_all(&source_root);
    result
}

//! Minimal fresh-only wiring for the explicitly opted-in entity experiment.
//! This is not production admission or an existing-workspace recovery path.

use anyhow::Context;
use tokio::task::JoinHandle;

use super::{FuseConcurrencyConfig, MountConfig, S3Backend, S3Config, shutdown_signal};
use crate::fuse::entity::{EntityFuse, mount_privileged, mount_unprivileged};
use crate::workspace_overlay::entity::diagnostics;
use crate::workspace_overlay::entity::experiment::{FreshMountOptions, RedisEntityExperiment};

const LEASE_TTL_NS: i64 = 60_000_000_000;

// Only the renewal loop is abortable. Entity operations retain their existing
// owned-task/drain semantics throughout shutdown.
struct RenewalTask(JoinHandle<()>);
impl Drop for RenewalTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn mount(args: MountConfig) -> anyhow::Result<()> {
    // Diagnostic snapshots are opt-in and contain only cumulative counters.
    // Register before mounting so a runner can safely request phase boundaries.
    let _diagnostic_task = if diagnostics::enabled() {
        let mut signals =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
        Some(RenewalTask(tokio::spawn(async move {
            while signals.recv().await.is_some() {
                println!("entity_diagnostics={}", diagnostics::snapshot_json());
            }
        })))
    } else {
        None
    };
    std::fs::create_dir_all(&args.mount_point)?;
    if !args.mount_point.is_dir() {
        anyhow::bail!("mount point must be a directory");
    }
    if args.chunk_size == 0
        || args.chunk_size > crate::workspace_overlay::entity::data::MAX_IO_BYTES as u64
    {
        anyhow::bail!("experimental entity chunk size exceeds the bounded data I/O geometry");
    }
    if args.s3_part_size == 0 || args.s3_max_concurrency == 0 {
        anyhow::bail!("S3 part size and concurrency must be positive");
    }
    let objects = S3Backend::with_gc_config(S3Config {
        bucket: args
            .s3_bucket
            .clone()
            .filter(|s| !s.is_empty())
            .context("S3 bucket required")?,
        region: args.s3_region.clone(),
        endpoint: args.s3_endpoint.clone(),
        part_size: args.s3_part_size,
        max_concurrency: args.s3_max_concurrency,
        force_path_style: args.s3_force_path_style,
        disable_payload_checksum: args.s3_disable_payload_checksum,
        ..Default::default()
    })
    .await?;
    let factory = RedisEntityExperiment::connect(
        &args.meta_url,
        &args.workspace_namespace,
        args.entity_volume_id
            .context("entity volume UUID required")?,
        objects,
    )
    .await?;
    let mounted = factory
        .create_fresh_from_manifest(
            args.packed_manifest_key
                .as_deref()
                .context("packed manifest key required")?,
            FreshMountOptions {
                namespace_id: args.workspace,
                lease_ttl_ns: LEASE_TTL_NS,
                chunk_size: args.chunk_size,
            },
        )
        .await?;
    let fs = EntityFuse::new(
        mounted.namespace().clone(),
        mounted.data().clone(),
        mounted.root_ino(),
    )?;
    let stop = fs.clone();
    let (failed_tx, mut failed_rx) = tokio::sync::oneshot::channel();
    let renewal_mount = mounted.clone();
    let renewal = RenewalTask(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = renewal_mount.renew(LEASE_TTL_NS).await {
                let _ = failed_tx.send(error);
                break;
            }
        }
    }));
    let concurrency = FuseConcurrencyConfig {
        worker_count: args.fuse_workers,
        max_background: args.fuse_max_background,
    };
    let handle = if args.privileged {
        mount_privileged(fs, &args.mount_point, concurrency).await
    } else {
        mount_unprivileged(fs, &args.mount_point, concurrency).await
    };
    let outcome = match handle {
        Err(error) => Err(anyhow::Error::from(error)),
        Ok(mut handle) => {
            println!(
                "entity_experiment={}",
                serde_json::json!({
                    "namespace_id": mounted.keys().namespace_id(),
                    "volume_id": mounted.keys().volume_id(),
                    "mount_point": args.mount_point,
                    "production_admission": false,
                    "fresh_only": true,
                })
            );
            enum Stop {
                Signal(std::io::Result<()>),
                Session(std::io::Result<()>),
                Lease(String),
            }
            let reason = tokio::select! {
                signal = shutdown_signal() => Stop::Signal(signal),
                result = &mut handle => Stop::Session(result),
                failure = &mut failed_rx => Stop::Lease(match failure {
                    Ok(error) => error.to_string(),
                    Err(error) => format!("renewal task ended: {error}"),
                }),
            };
            stop.stop_admission();
            match reason {
                Stop::Session(result) => result.map_err(Into::into),
                Stop::Signal(signal) => {
                    let unmount = handle.unmount().await;
                    signal.and(unmount).map_err(Into::into)
                }
                Stop::Lease(error) => {
                    let unmount = handle.unmount().await;
                    Err(anyhow::anyhow!(
                        "entity lease renewal failed: {error}; unmount={unmount:?}"
                    ))
                }
            }
        }
    };
    stop.stop_admission();
    let drained = mounted.orderly_close().await;
    renewal.0.abort();
    // Join renewal before leaving the runtime; do not cancel owned data I/O.
    let mut task = renewal;
    let _ = (&mut task.0).await;
    if diagnostics::enabled() {
        println!("entity_diagnostics={}", diagnostics::snapshot_json());
    }
    if drained.is_ok() {
        println!(
            "entity_stopped={}",
            serde_json::json!({
                "namespace_id": mounted.keys().namespace_id(),
                "orderly_local_drain": true,
                "upper_data_retained": true,
                "production_admission": false,
            })
        );
    }
    outcome?;
    drained?;
    Ok(())
}

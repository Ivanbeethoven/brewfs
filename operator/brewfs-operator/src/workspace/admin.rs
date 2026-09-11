//! Backend-neutral workspace control-plane facade used by the Kubernetes controllers.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _};
use async_trait::async_trait;
use brewfs::workspace_overlay::catalog::{
    CreateSnapshot, CreateVolumeRoot, CreateWorkspace, MarkDeleting, WorkspaceStore,
};
use brewfs::workspace_overlay::error::WorkspaceError;
use brewfs::workspace_overlay::ids::{LayerId, SnapshotId, WorkspaceId};
use brewfs::workspace_overlay::lifecycle::{
    NoopDurableRemoteBarrier, WorkspaceLifecycle, WorkspaceMountSession,
};
use brewfs::workspace_overlay::model::{
    BaseRevision, LayerState, LeaseState, SnapshotLease, WorkspaceRecord, WorkspaceState,
    WORKSPACE_SCHEMA_VERSION,
};
use brewfs::workspace_overlay::stores::kv_store::KvWorkspaceStore;
use brewfs::workspace_overlay::stores::redis::RedisWorkspaceBackend;
use brewfs::workspace_overlay::stores::tikv::TiKvWorkspaceBackend;
use uuid::Uuid;

use super::crd::{WorkspaceCatalogBackend, WorkspaceClusterSpec, WorkspaceRevision};

#[derive(Clone, Debug)]
pub struct VolumeIdentity {
    pub volume_id: Uuid,
    pub root_workspace_id: WorkspaceId,
    pub root_layer_id: LayerId,
    pub writable_layer_id: LayerId,
    pub root_snapshot_id: SnapshotId,
    pub owner_id: String,
}

#[derive(Clone, Debug)]
pub struct VolumeView {
    pub volume_id: Uuid,
    pub root_snapshot_id: SnapshotId,
    pub root_revision: BaseRevision,
}

#[derive(Clone, Debug)]
pub struct EnsureWorkspaceRequest {
    pub workspace_id: WorkspaceId,
    pub head_layer_id: LayerId,
    pub base_revision: BaseRevision,
    pub owner_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceView {
    pub record: WorkspaceRecord,
    pub base_revision: BaseRevision,
    pub layer_depth: u32,
    pub leases: Vec<SnapshotLease>,
}

#[derive(Clone, Debug)]
pub struct EnsureSnapshotRequest {
    pub snapshot_id: SnapshotId,
    pub name: String,
    pub revision: BaseRevision,
    pub owner_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SnapshotView {
    pub snapshot_id: SnapshotId,
    pub revision: BaseRevision,
}

#[async_trait]
pub trait WorkspaceAdmin: Send + Sync {
    async fn ensure_volume(&self, identity: VolumeIdentity) -> anyhow::Result<VolumeView>;
    async fn ensure_workspace(
        &self,
        request: EnsureWorkspaceRequest,
    ) -> anyhow::Result<WorkspaceView>;
    async fn inspect_workspace(&self, id: WorkspaceId) -> anyhow::Result<WorkspaceView>;
    async fn verify_revision(&self, revision: &BaseRevision) -> anyhow::Result<()>;
    async fn ensure_snapshot(&self, request: EnsureSnapshotRequest)
        -> anyhow::Result<SnapshotView>;
    async fn load_snapshot(&self, id: SnapshotId) -> anyhow::Result<SnapshotView>;
    async fn seal_and_snapshot(
        &self,
        workspace_id: WorkspaceId,
        expected_head_epoch: u64,
        holder_generation: u64,
        ttl_seconds: u32,
        heartbeat_seconds: u32,
        request: EnsureSnapshotRequest,
    ) -> anyhow::Result<(WorkspaceView, SnapshotView)>;
    async fn delete_snapshot(&self, id: SnapshotId) -> anyhow::Result<()>;
    async fn mark_workspace_deleting(&self, id: WorkspaceId, force: bool) -> anyhow::Result<()>;
    async fn recover_incomplete_seals(&self) -> anyhow::Result<()>;
    async fn reap_expired_leases(&self) -> anyhow::Result<u64>;
    async fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceRecord>>;
    async fn list_snapshots(&self) -> anyhow::Result<Vec<SnapshotView>>;
}

pub async fn connect_workspace_admin(
    cluster_name: &str,
    kubernetes_namespace: &str,
    redis_port: i32,
    spec: &WorkspaceClusterSpec,
    redis_password: Option<&str>,
) -> anyhow::Result<Arc<dyn WorkspaceAdmin>> {
    spec.validate().map_err(|error| anyhow!(error))?;
    let catalog_namespace = catalog_namespace(cluster_name, kubernetes_namespace, spec);
    match spec.catalog_backend {
        WorkspaceCatalogBackend::Redis => {
            let password = redis_password
                .filter(|password| !password.is_empty())
                .ok_or_else(|| anyhow!("Redis workspace catalog password is missing"))?;
            let url = format!(
                "redis://:{password}@{cluster_name}-workspace-redis.{kubernetes_namespace}.svc.cluster.local:{redis_port}/"
            );
            let backend = RedisWorkspaceBackend::connect(&url, &catalog_namespace)
                .await
                .context("connect authenticated Redis workspace catalog")?;
            Ok(Arc::new(StoreWorkspaceAdmin::new(KvWorkspaceStore::new(
                backend,
            ))))
        }
        WorkspaceCatalogBackend::TiKv => {
            let backend =
                TiKvWorkspaceBackend::connect(spec.tikv_pd_endpoints.clone(), &catalog_namespace)
                    .await
                    .context("connect TiKV workspace catalog")?;
            Ok(Arc::new(StoreWorkspaceAdmin::new(KvWorkspaceStore::new(
                backend,
            ))))
        }
    }
}

pub fn catalog_namespace(
    cluster_name: &str,
    kubernetes_namespace: &str,
    spec: &WorkspaceClusterSpec,
) -> String {
    spec.namespace
        .clone()
        .unwrap_or_else(|| format!("{kubernetes_namespace}-{cluster_name}"))
}

struct StoreWorkspaceAdmin<W> {
    store: Arc<W>,
}

impl<W> StoreWorkspaceAdmin<W> {
    fn new(store: W) -> Self {
        Self {
            store: Arc::new(store),
        }
    }
}

#[async_trait]
impl<W> WorkspaceAdmin for StoreWorkspaceAdmin<W>
where
    W: WorkspaceStore + 'static,
{
    async fn ensure_volume(&self, identity: VolumeIdentity) -> anyhow::Result<VolumeView> {
        self.store.initialize_workspace_schema().await?;
        match self.store.load_volume_header().await? {
            Some(header) => {
                if header.volume_id != identity.volume_id {
                    bail!(
                        "workspace catalog is owned by volume {}, expected {}",
                        header.volume_id,
                        identity.volume_id
                    );
                }
                if header.volume_format != "workspace-v1"
                    || header.schema_version != WORKSPACE_SCHEMA_VERSION
                {
                    bail!(
                        "workspace volume format mismatch: format={} schemaVersion={}",
                        header.volume_format,
                        header.schema_version
                    );
                }
            }
            None => {
                self.store
                    .create_volume_root(CreateVolumeRoot {
                        volume_id: identity.volume_id,
                        workspace_id: identity.root_workspace_id,
                        root_layer_id: identity.root_layer_id,
                        writable_layer_id: identity.writable_layer_id,
                        owner_id: Some(identity.owner_id.clone()),
                    })
                    .await?;
            }
        }

        let root = self
            .inspect_workspace(identity.root_workspace_id)
            .await
            .context("inspect reserved root workspace")?;
        self.ensure_snapshot(EnsureSnapshotRequest {
            snapshot_id: identity.root_snapshot_id,
            name: format!("{}-root", identity.owner_id),
            revision: root.base_revision.clone(),
            owner_id: Some(identity.owner_id),
        })
        .await?;
        Ok(VolumeView {
            volume_id: identity.volume_id,
            root_snapshot_id: identity.root_snapshot_id,
            root_revision: root.base_revision,
        })
    }

    async fn ensure_workspace(
        &self,
        request: EnsureWorkspaceRequest,
    ) -> anyhow::Result<WorkspaceView> {
        match self.store.load_workspace(request.workspace_id).await {
            Ok(record) => {
                if record.owner_id != request.owner_id {
                    bail!("deterministic workspace ID exists with a different owner");
                }
            }
            Err(WorkspaceError::WorkspaceNotFound(_)) => {
                match self
                    .store
                    .create_workspace(CreateWorkspace {
                        workspace_id: request.workspace_id,
                        head_layer_id: request.head_layer_id,
                        base_revision: request.base_revision.clone(),
                        owner_id: request.owner_id.clone(),
                    })
                    .await
                {
                    Ok(_) => {}
                    Err(error) => {
                        self.store
                            .load_workspace(request.workspace_id)
                            .await
                            .map_err(|_| error)?;
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
        let view = self.inspect_workspace(request.workspace_id).await?;
        if view.record.head_epoch == 0 && view.base_revision != request.base_revision {
            bail!("new workspace base revision does not match the requested source");
        }
        Ok(view)
    }

    async fn inspect_workspace(&self, id: WorkspaceId) -> anyhow::Result<WorkspaceView> {
        let record = self.store.load_workspace(id).await?;
        let chain = self.store.load_layer_chain(record.head_layer_id).await?;
        let [head, base] = chain.as_slice() else {
            bail!("workspace {id} does not contain exactly one writable and one sealed layer");
        };
        if head.state != LayerState::Writable
            || head.owner_workspace_id != Some(id)
            || head.parent_layer_id != Some(base.layer_id)
            || head.depth != 2
            || base.state != LayerState::Sealed
            || base.owner_workspace_id.is_some()
            || base.parent_layer_id.is_some()
            || base.depth != 1
        {
            bail!("workspace {id} violates the fixed two-layer invariant");
        }
        let base_revision = revision_from_layer(base)?;
        let leases = self.store.list_leases(id).await?;
        Ok(WorkspaceView {
            record,
            base_revision,
            layer_depth: head.depth,
            leases,
        })
    }

    async fn verify_revision(&self, revision: &BaseRevision) -> anyhow::Result<()> {
        let layer = self.store.load_layer(revision.layer_id).await?;
        let actual = revision_from_layer(&layer)?;
        if &actual != revision {
            bail!("sealed revision tuple changed");
        }
        Ok(())
    }

    async fn ensure_snapshot(
        &self,
        request: EnsureSnapshotRequest,
    ) -> anyhow::Result<SnapshotView> {
        self.verify_revision(&request.revision).await?;
        if let Some(existing) = self
            .store
            .list_snapshots()
            .await?
            .into_iter()
            .find(|snapshot| snapshot.snapshot_id == request.snapshot_id)
        {
            if existing.revision != request.revision
                || existing.name.as_deref() != Some(&request.name)
            {
                bail!("deterministic snapshot ID exists with different content");
            }
            return Ok(SnapshotView {
                snapshot_id: existing.snapshot_id,
                revision: existing.revision,
            });
        }
        let snapshot = self
            .store
            .create_snapshot(CreateSnapshot {
                snapshot_id: request.snapshot_id,
                name: Some(request.name),
                revision: request.revision,
                owner_id: request.owner_id,
            })
            .await?;
        Ok(SnapshotView {
            snapshot_id: snapshot.snapshot_id,
            revision: snapshot.revision,
        })
    }

    async fn load_snapshot(&self, id: SnapshotId) -> anyhow::Result<SnapshotView> {
        let snapshot = self.store.load_snapshot(id).await?;
        self.verify_revision(&snapshot.revision).await?;
        Ok(SnapshotView {
            snapshot_id: snapshot.snapshot_id,
            revision: snapshot.revision,
        })
    }

    async fn seal_and_snapshot(
        &self,
        workspace_id: WorkspaceId,
        expected_head_epoch: u64,
        holder_generation: u64,
        ttl_seconds: u32,
        heartbeat_seconds: u32,
        request: EnsureSnapshotRequest,
    ) -> anyhow::Result<(WorkspaceView, SnapshotView)> {
        let before = self.inspect_workspace(workspace_id).await?;
        if before
            .leases
            .iter()
            .any(|lease| lease.state == LeaseState::Active)
        {
            bail!("workspace still has an active writable lease");
        }
        let session = WorkspaceMountSession::acquire(
            self.store.clone(),
            workspace_id,
            holder_generation,
            std::time::Duration::from_secs(u64::from(ttl_seconds)),
            std::time::Duration::from_secs(u64::from(heartbeat_seconds)),
        )
        .await?;
        if session.view.head_epoch != expected_head_epoch {
            let actual_head_epoch = session.view.head_epoch;
            session.release().await?;
            bail!(
                "workspace head changed before seal: expected epoch {expected_head_epoch}, actual epoch {actual_head_epoch}"
            );
        }
        let lifecycle = WorkspaceLifecycle::new(self.store.clone());
        let seal_result = lifecycle
            .seal(&session.view, &NoopDurableRemoteBarrier)
            .await;
        let release_result = session.release().await;
        let revision = seal_result?.revision;
        release_result?;
        let snapshot = self
            .ensure_snapshot(EnsureSnapshotRequest {
                revision: revision.clone(),
                ..request
            })
            .await?;
        let workspace = self.inspect_workspace(workspace_id).await?;
        Ok((workspace, snapshot))
    }

    async fn delete_snapshot(&self, id: SnapshotId) -> anyhow::Result<()> {
        self.store.delete_snapshot(id).await?;
        Ok(())
    }

    async fn mark_workspace_deleting(&self, id: WorkspaceId, force: bool) -> anyhow::Result<()> {
        match self
            .store
            .mark_workspace_deleting(MarkDeleting {
                workspace_id: id,
                force_fence_lease: force,
            })
            .await
        {
            Ok(()) => Ok(()),
            Err(WorkspaceError::InvalidStateTransition { .. }) => {
                let workspace = self.store.load_workspace(id).await?;
                if workspace.state == WorkspaceState::Deleting {
                    Ok(())
                } else {
                    Err(anyhow!("workspace deletion transition was rejected"))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn recover_incomplete_seals(&self) -> anyhow::Result<()> {
        WorkspaceLifecycle::new(self.store.clone())
            .recover_incomplete_seals()
            .await?;
        Ok(())
    }

    async fn reap_expired_leases(&self) -> anyhow::Result<u64> {
        Ok(self.store.reap_expired_leases().await?)
    }

    async fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceRecord>> {
        Ok(self.store.list_workspaces().await?)
    }

    async fn list_snapshots(&self) -> anyhow::Result<Vec<SnapshotView>> {
        Ok(self
            .store
            .list_snapshots()
            .await?
            .into_iter()
            .map(|snapshot| SnapshotView {
                snapshot_id: snapshot.snapshot_id,
                revision: snapshot.revision,
            })
            .collect())
    }
}

fn revision_from_layer(
    layer: &brewfs::workspace_overlay::model::LayerRecord,
) -> anyhow::Result<BaseRevision> {
    if layer.state != LayerState::Sealed
        || layer.owner_workspace_id.is_some()
        || layer.parent_layer_id.is_some()
        || layer.depth != 1
    {
        bail!(
            "layer {} is not an immutable flat sealed base",
            layer.layer_id
        );
    }
    Ok(BaseRevision {
        layer_id: layer.layer_id,
        sealed_version: layer
            .sealed_version
            .ok_or_else(|| anyhow!("sealed layer has no version"))?,
        root_hash: layer
            .root_hash
            .ok_or_else(|| anyhow!("sealed layer has no root hash"))?,
    })
}

pub fn revision_to_status(revision: &BaseRevision) -> WorkspaceRevision {
    WorkspaceRevision {
        layer_id: revision.layer_id.to_string(),
        sealed_version: revision.sealed_version,
        root_hash: hex::encode(revision.root_hash),
    }
}

pub fn revision_from_status(revision: &WorkspaceRevision) -> anyhow::Result<BaseRevision> {
    let decoded = hex::decode(&revision.root_hash).context("decode workspace root hash")?;
    let root_hash: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow!("workspace root hash must be 32 bytes"))?;
    Ok(BaseRevision {
        layer_id: revision
            .layer_id
            .parse()
            .context("parse workspace layer ID")?,
        sealed_version: revision.sealed_version,
        root_hash,
    })
}

pub fn deterministic_uuid(scope: Uuid, value: &str) -> Uuid {
    Uuid::new_v5(&scope, value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_status_round_trips_exact_identity() {
        let revision = BaseRevision {
            layer_id: LayerId::from_uuid(Uuid::from_u128(7)),
            sealed_version: 9,
            root_hash: [0xab; 32],
        };
        let status = revision_to_status(&revision);
        assert_eq!(revision_from_status(&status).unwrap(), revision);
    }

    #[test]
    fn deterministic_ids_are_stable_and_scoped() {
        let scope = Uuid::from_u128(11);
        assert_eq!(
            deterministic_uuid(scope, "workspace"),
            deterministic_uuid(scope, "workspace")
        );
        assert_ne!(
            deterministic_uuid(scope, "workspace"),
            deterministic_uuid(scope, "snapshot")
        );
    }
}

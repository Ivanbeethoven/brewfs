use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use brewfs::workspace_overlay::error::WorkspaceError;
use brewfs::workspace_overlay::ids::{LayerId, SnapshotId, WorkspaceId};
use brewfs::workspace_overlay::model::LeaseState;
use chrono::Utc;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::{Client, Resource, ResourceExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

use crate::crd::BrewFSCluster;
use crate::reconciler::OperatorContext;

use super::admin::{
    catalog_namespace, connect_workspace_admin, deterministic_uuid, revision_from_status,
    revision_to_status, EnsureSnapshotRequest, EnsureWorkspaceRequest, VolumeIdentity,
    WorkspaceAdmin,
};
use super::crd::{
    BrewFSWorkspace, BrewFSWorkspaceMount, BrewFSWorkspaceSnapshot, BrewFSWorkspaceSnapshotStatus,
    BrewFSWorkspaceStatus, WorkspaceCondition, WorkspaceDeletionPolicy, WorkspaceDesiredState,
    WorkspaceRevision, WorkspaceSourceKind,
};
use super::workload::{
    delete_workspace_mount_workload, load_workspace_catalog_password, patch_mount_backend_status,
    reconcile_workspace_catalog, reconcile_workspace_mount_workload,
};

const WORKSPACE_FINALIZER: &str = "storage.brewfs.io/workspace-protection";
const MOUNT_FINALIZER: &str = "storage.brewfs.io/workspace-mount-protection";
const SNAPSHOT_FINALIZER: &str = "storage.brewfs.io/workspace-snapshot-protection";
const CLUSTER_FINALIZER: &str = "storage.brewfs.io/workspace-cluster-protection";
const FORCE_DELETE_ANNOTATION: &str = "storage.brewfs.io/force-delete";
const FORCE_DELETE_REASON_ANNOTATION: &str = "storage.brewfs.io/force-delete-reason";
const ID_NAMESPACE: Uuid = Uuid::from_u128(0xb7e6_f500_9a2d_4d17_8b4d_7f24_4c45_a011);
const SEAL_HEAD_EPOCH_ADVANCE: u64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotResumeAction {
    Seal,
    PinRecoveredRevision,
    Conflict,
}

fn snapshot_resume_action(source_head_epoch: u64, current_head_epoch: u64) -> SnapshotResumeAction {
    if current_head_epoch == source_head_epoch {
        SnapshotResumeAction::Seal
    } else if source_head_epoch
        .checked_add(SEAL_HEAD_EPOCH_ADVANCE)
        .is_some_and(|sealed_epoch| sealed_epoch == current_head_epoch)
    {
        SnapshotResumeAction::PinRecoveredRevision
    } else {
        SnapshotResumeAction::Conflict
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceReconcileError {
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

pub async fn guard_cluster_workspace_lifecycle(
    cluster: &BrewFSCluster,
    client: &Client,
    namespace: &str,
) -> anyhow::Result<bool> {
    let enabled = cluster
        .spec
        .workspace
        .as_ref()
        .is_some_and(|spec| spec.enabled);
    let protected = cluster
        .meta()
        .finalizers
        .as_ref()
        .is_some_and(|finalizers| finalizers.iter().any(|value| value == CLUSTER_FINALIZER));
    if !enabled && !protected {
        return Ok(false);
    }
    let api: Api<BrewFSCluster> = Api::namespaced(client.clone(), namespace);
    if cluster.meta().deletion_timestamp.is_none() {
        if !enabled {
            bail!("workspace support cannot be disabled after the catalog was initialized");
        }
        return ensure_finalizer(&api, &cluster.name_any(), cluster, CLUSTER_FINALIZER).await;
    }

    let workspaces = Api::<BrewFSWorkspace>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await?
        .into_iter()
        .filter(|workspace| workspace.spec.cluster_ref.name == cluster.name_any())
        .collect::<Vec<_>>();
    let workspace_names = workspaces
        .iter()
        .map(ResourceExt::name_any)
        .collect::<std::collections::BTreeSet<_>>();
    let mount_count = Api::<BrewFSWorkspaceMount>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await?
        .into_iter()
        .filter(|mount| workspace_names.contains(&mount.spec.workspace_ref.name))
        .count();
    let snapshot_count = Api::<BrewFSWorkspaceSnapshot>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await?
        .into_iter()
        .filter(|snapshot| snapshot.spec.cluster_ref.name == cluster.name_any())
        .count();
    if !workspaces.is_empty() || mount_count != 0 || snapshot_count != 0 {
        bail!(
            "cluster deletion is blocked by {} workspaces, {} mounts, and {} snapshots",
            workspaces.len(),
            mount_count,
            snapshot_count
        );
    }
    if let Some(workspace_status) = cluster
        .status
        .as_ref()
        .and_then(|status| status.workspace.as_ref())
    {
        let spec = cluster
            .spec
            .workspace
            .as_ref()
            .ok_or_else(|| anyhow!("restore spec.workspace to complete protected deletion"))?;
        let redis_password =
            load_workspace_catalog_password(client, namespace, cluster, spec).await?;
        let admin = connect_workspace_admin(
            &cluster.name_any(),
            namespace,
            cluster.spec.redis.port,
            spec,
            redis_password.as_deref(),
        )
        .await?;
        let root_workspace_id = WorkspaceId::from_uuid(deterministic_uuid(
            resource_uuid(&resource_uid(cluster)?),
            "root-workspace",
        ));
        let retained = admin
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| {
                workspace.workspace_id != root_workspace_id
                    && workspace.state != brewfs::workspace_overlay::model::WorkspaceState::Deleting
            })
            .count();
        if retained != 0 {
            bail!("cluster deletion is blocked by {retained} retained backend workspaces");
        }
        let root_snapshot_id: SnapshotId = workspace_status
            .root_snapshot_id
            .parse()
            .context("parse cluster root snapshot ID")?;
        let retained_snapshots = admin
            .list_snapshots()
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.snapshot_id != root_snapshot_id)
            .count();
        if retained_snapshots != 0 {
            bail!("cluster deletion is blocked by {retained_snapshots} retained backend snapshots");
        }
    }
    remove_finalizer(&api, &cluster.name_any(), cluster, CLUSTER_FINALIZER).await?;
    Ok(true)
}

pub async fn reconcile_cluster_workspace(
    cluster: &BrewFSCluster,
    client: &Client,
    kubernetes_namespace: &str,
) -> anyhow::Result<()> {
    let Some(spec) = cluster.spec.workspace.as_ref() else {
        patch_cluster_workspace_status(client, kubernetes_namespace, cluster, None).await?;
        return Ok(());
    };
    if !spec.enabled {
        patch_cluster_workspace_status(client, kubernetes_namespace, cluster, None).await?;
        return Ok(());
    }
    spec.validate().map_err(|error| anyhow!(error))?;
    let uid = resource_uid(cluster)?;
    let scope = resource_uuid(&uid);
    let volume_id = deterministic_uuid(scope, "volume");
    reconcile_workspace_catalog(client, kubernetes_namespace, cluster, spec).await?;
    let redis_password =
        load_workspace_catalog_password(client, kubernetes_namespace, cluster, spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        kubernetes_namespace,
        cluster.spec.redis.port,
        spec,
        redis_password.as_deref(),
    )
    .await?;
    let volume = admin
        .ensure_volume(VolumeIdentity {
            volume_id,
            root_workspace_id: WorkspaceId::from_uuid(deterministic_uuid(scope, "root-workspace")),
            root_layer_id: LayerId::from_uuid(deterministic_uuid(scope, "root-layer")),
            writable_layer_id: LayerId::from_uuid(deterministic_uuid(scope, "root-head")),
            root_snapshot_id: SnapshotId::from_uuid(deterministic_uuid(scope, "root-snapshot")),
            owner_id: format!("k8s-cluster/{kubernetes_namespace}/{}", cluster.name_any()),
        })
        .await?;
    let workspace_status = super::crd::WorkspaceClusterStatus {
        volume_id: volume.volume_id.to_string(),
        schema_version: brewfs::workspace_overlay::model::WORKSPACE_SCHEMA_VERSION,
        catalog_backend: spec.catalog_backend,
        catalog_namespace: catalog_namespace(&cluster.name_any(), kubernetes_namespace, spec),
        root_snapshot_id: volume.root_snapshot_id.to_string(),
        root_revision: revision_to_status(&volume.root_revision),
    };
    patch_cluster_workspace_status(
        client,
        kubernetes_namespace,
        cluster,
        Some(workspace_status),
    )
    .await
}

pub async fn reconcile_workspace(
    workspace: Arc<BrewFSWorkspace>,
    ctx: Arc<OperatorContext>,
) -> Result<Action, WorkspaceReconcileError> {
    let namespace = workspace
        .namespace()
        .ok_or_else(|| anyhow!("BrewFSWorkspace must be namespaced"))?;
    let api: Api<BrewFSWorkspace> = Api::namespaced(ctx.client.clone(), &namespace);
    let name = workspace.name_any();

    if workspace.meta().deletion_timestamp.is_some() {
        cleanup_workspace(&ctx.client, &namespace, &workspace).await?;
        remove_finalizer(&api, &name, &workspace, WORKSPACE_FINALIZER).await?;
        return Ok(Action::await_change());
    }
    if ensure_finalizer(&api, &name, &workspace, WORKSPACE_FINALIZER).await? {
        return Ok(Action::await_change());
    }
    if let Err(error) = workspace.spec.validate() {
        patch_workspace_status(
            &api,
            &workspace,
            workspace_status(&workspace, "Failed", &error, None, None),
        )
        .await?;
        return Ok(Action::await_change());
    }

    let cluster = load_cluster(&ctx.client, &namespace, &workspace.spec.cluster_ref.name).await?;
    let Some(cluster_workspace) = cluster
        .status
        .as_ref()
        .and_then(|status| status.workspace.as_ref())
    else {
        patch_workspace_status(
            &api,
            &workspace,
            workspace_status(
                &workspace,
                "Pending",
                "waiting for WorkspaceCatalogReady",
                None,
                None,
            ),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    };
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    let redis_password =
        load_workspace_catalog_password(&ctx.client, &namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        &namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    admin.recover_incomplete_seals().await?;

    let source = resolve_workspace_source(
        &ctx.client,
        &namespace,
        &workspace,
        &cluster,
        admin.as_ref(),
    )
    .await?;
    admin.verify_revision(&source).await?;
    if workspace
        .status
        .as_ref()
        .and_then(|status| status.origin_revision.as_ref())
        .is_some_and(|origin| *origin != revision_to_status(&source))
    {
        return Err(anyhow!("workspace source is immutable after creation").into());
    }

    let volume_id = Uuid::parse_str(&cluster_workspace.volume_id)
        .context("parse cluster workspace volume ID")?;
    let workspace_uid = resource_uid(&*workspace)?;
    let workspace_uuid = deterministic_uuid(volume_id, &format!("workspace/{workspace_uid}"));
    let workspace_id = WorkspaceId::from_uuid(workspace_uuid);
    if workspace
        .status
        .as_ref()
        .and_then(|status| status.workspace_id.as_deref())
        .is_some_and(|existing| existing != workspace_id.to_string())
    {
        return Err(anyhow!("clusterRef is immutable after workspace creation").into());
    }
    let view = admin
        .ensure_workspace(EnsureWorkspaceRequest {
            workspace_id,
            head_layer_id: LayerId::from_uuid(deterministic_uuid(workspace_uuid, "head/0")),
            base_revision: source.clone(),
            owner_id: workspace
                .spec
                .owner_id
                .clone()
                .or_else(|| Some(format!("k8s-workspace/{namespace}/{name}"))),
        })
        .await?;
    if view.layer_depth != 2 {
        return Err(anyhow!("workspace backend returned a non-canonical layer depth").into());
    }

    let active_lease = view
        .leases
        .iter()
        .any(|lease| lease.state == LeaseState::Active);
    let (phase, message) = match workspace.spec.desired_state {
        WorkspaceDesiredState::Active => ("Active", "workspace is available for mounting"),
        WorkspaceDesiredState::Suspended if active_lease => (
            "Quiescing",
            "waiting for the writable mount lease to be released",
        ),
        WorkspaceDesiredState::Suspended => ("Suspended", "workspace has no active writer"),
    };
    let mut status = workspace_status(
        &workspace,
        phase,
        message,
        Some(workspace_id.to_string()),
        Some(revision_to_status(&view.base_revision)),
    );
    status.origin_revision = workspace
        .status
        .as_ref()
        .and_then(|status| status.origin_revision.clone())
        .or_else(|| Some(revision_to_status(&source)));
    status.head_layer_id = Some(view.record.head_layer_id.to_string());
    status.head_epoch = Some(view.record.head_epoch);
    status.active_mount_ref = if active_lease {
        active_mount_name(&ctx.client, &namespace, &workspace).await?
    } else {
        None
    };
    if !active_lease {
        status.last_clean_release_epoch = Some(view.record.head_epoch);
    }
    let mut base_verified = condition(
        "BaseVerified",
        "True",
        "ExactRevisionMatched",
        "the sealed lower revision tuple and fixed two-layer view were verified",
        workspace.metadata.generation,
    );
    if let Some(existing) = workspace.status.as_ref().and_then(|status| {
        status.conditions.iter().find(|condition| {
            condition.condition_type == base_verified.condition_type
                && condition.status == base_verified.status
                && condition.reason == base_verified.reason
                && condition.message == base_verified.message
        })
    }) {
        base_verified.last_transition_time = existing.last_transition_time;
    }
    status.conditions = vec![base_verified];
    patch_workspace_status(&api, &workspace, status).await?;
    Ok(Action::requeue(Duration::from_secs(30)))
}

pub async fn reconcile_workspace_mount(
    mount: Arc<BrewFSWorkspaceMount>,
    ctx: Arc<OperatorContext>,
) -> Result<Action, WorkspaceReconcileError> {
    let namespace = mount
        .namespace()
        .ok_or_else(|| anyhow!("BrewFSWorkspaceMount must be namespaced"))?;
    let api: Api<BrewFSWorkspaceMount> = Api::namespaced(ctx.client.clone(), &namespace);
    let name = mount.name_any();
    if mount.meta().deletion_timestamp.is_some() {
        delete_workspace_mount_workload(&ctx.client, &namespace, &mount).await?;
        verify_mount_lease_released(&ctx.client, &namespace, &mount).await?;
        remove_finalizer(&api, &name, &mount, MOUNT_FINALIZER).await?;
        return Ok(Action::await_change());
    }
    if ensure_finalizer(&api, &name, &mount, MOUNT_FINALIZER).await? {
        return Ok(Action::await_change());
    }

    let workspace_api: Api<BrewFSWorkspace> = Api::namespaced(ctx.client.clone(), &namespace);
    let Some(workspace) = workspace_api
        .get_opt(&mount.spec.workspace_ref.name)
        .await
        .context("load referenced BrewFSWorkspace")?
    else {
        super::workload::patch_mount_status(
            &api,
            &mount,
            "WaitingForWorkspace",
            "referenced workspace does not exist",
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    };
    if workspace.meta().deletion_timestamp.is_some() {
        delete_workspace_mount_workload(&ctx.client, &namespace, &mount).await?;
        super::workload::patch_mount_status(
            &api,
            &mount,
            "Releasing",
            "workspace deletion is draining this mount",
            workspace.status.as_ref(),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(5)));
    }
    let cluster = load_cluster(&ctx.client, &namespace, &workspace.spec.cluster_ref.name).await?;
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    if let Err(error) = mount.spec.validate(cluster_spec.lease_ttl_seconds) {
        super::workload::patch_mount_status(&api, &mount, "Failed", &error, None).await?;
        return Ok(Action::await_change());
    }
    if workspace.spec.desired_state == WorkspaceDesiredState::Suspended {
        delete_workspace_mount_workload(&ctx.client, &namespace, &mount).await?;
        super::workload::patch_mount_status(
            &api,
            &mount,
            "Released",
            "workspace is suspended; mount workload is stopped",
            workspace.status.as_ref(),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    }
    if workspace
        .status
        .as_ref()
        .is_none_or(|status| status.phase != "Active" || status.workspace_id.is_none())
    {
        super::workload::patch_mount_status(
            &api,
            &mount,
            "WaitingForWorkspace",
            "workspace is not Active",
            workspace.status.as_ref(),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    }

    ensure_single_mount(&ctx.client, &namespace, &mount).await?;
    reconcile_workspace_mount_workload(
        &ctx.client,
        &namespace,
        &mount,
        &workspace,
        &cluster,
        cluster_spec,
    )
    .await?;
    let redis_password =
        load_workspace_catalog_password(&ctx.client, &namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        &namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    admin.reap_expired_leases().await?;
    let workspace_id: WorkspaceId = workspace
        .status
        .as_ref()
        .and_then(|status| status.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("active workspace has no backend identity"))?
        .parse()
        .context("parse active workspace backend ID")?;
    let backend = admin.inspect_workspace(workspace_id).await?;
    patch_mount_backend_status(&api, &mount, &backend).await?;
    Ok(Action::requeue(Duration::from_secs(15)))
}

async fn active_mount_name(
    client: &Client,
    namespace: &str,
    workspace: &BrewFSWorkspace,
) -> anyhow::Result<Option<String>> {
    let mounts = Api::<BrewFSWorkspaceMount>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await?;
    Ok(mounts
        .into_iter()
        .filter(|mount| {
            mount.spec.workspace_ref.name == workspace.name_any()
                && mount.meta().deletion_timestamp.is_none()
        })
        .map(|mount| mount.name_any())
        .min())
}

pub async fn reconcile_workspace_snapshot(
    snapshot: Arc<BrewFSWorkspaceSnapshot>,
    ctx: Arc<OperatorContext>,
) -> Result<Action, WorkspaceReconcileError> {
    let namespace = snapshot
        .namespace()
        .ok_or_else(|| anyhow!("BrewFSWorkspaceSnapshot must be namespaced"))?;
    let api: Api<BrewFSWorkspaceSnapshot> = Api::namespaced(ctx.client.clone(), &namespace);
    let name = snapshot.name_any();
    if snapshot.meta().deletion_timestamp.is_some() {
        cleanup_snapshot(&ctx.client, &namespace, &snapshot).await?;
        remove_finalizer(&api, &name, &snapshot, SNAPSHOT_FINALIZER).await?;
        return Ok(Action::await_change());
    }
    if ensure_finalizer(&api, &name, &snapshot, SNAPSHOT_FINALIZER).await? {
        return Ok(Action::await_change());
    }
    if let Err(error) = snapshot.spec.validate() {
        patch_snapshot_status(&api, &snapshot, "Failed", &error, None, None, None).await?;
        return Ok(Action::await_change());
    }

    let workspace_api: Api<BrewFSWorkspace> = Api::namespaced(ctx.client.clone(), &namespace);
    let workspace = workspace_api
        .get(&snapshot.spec.workspace_ref.name)
        .await
        .context("load snapshot source workspace")?;
    if workspace.spec.cluster_ref.name != snapshot.spec.cluster_ref.name {
        return Err(
            anyhow!("snapshot and source workspace must reference the same cluster").into(),
        );
    }
    if workspace.meta().deletion_timestamp.is_none()
        && (workspace.spec.desired_state != WorkspaceDesiredState::Suspended
            || workspace
                .status
                .as_ref()
                .is_none_or(|status| status.phase != "Suspended"))
    {
        patch_snapshot_status(
            &api,
            &snapshot,
            "Pending",
            "source workspace must be fully Suspended before snapshot",
            None,
            None,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    }
    let cluster = load_cluster(&ctx.client, &namespace, &snapshot.spec.cluster_ref.name).await?;
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    let cluster_workspace = cluster
        .status
        .as_ref()
        .and_then(|status| status.workspace.as_ref())
        .ok_or_else(|| anyhow!("cluster workspace catalog is not ready"))?;
    let workspace_id: WorkspaceId = workspace
        .status
        .as_ref()
        .and_then(|status| status.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("source workspace has no backend identity"))?
        .parse()
        .context("parse source workspace ID")?;
    let redis_password =
        load_workspace_catalog_password(&ctx.client, &namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        &namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    admin.recover_incomplete_seals().await?;
    let volume_id = Uuid::parse_str(&cluster_workspace.volume_id)
        .context("parse cluster workspace volume ID")?;
    let snapshot_uid = resource_uid(&*snapshot)?;
    let snapshot_id = SnapshotId::from_uuid(deterministic_uuid(
        volume_id,
        &format!("snapshot/{snapshot_uid}"),
    ));
    let current = admin.inspect_workspace(workspace_id).await?;
    let recorded_source = snapshot.status.as_ref().and_then(|status| {
        status
            .source_workspace_id
            .clone()
            .zip(status.source_head_epoch)
    });
    if let Some((recorded_workspace_id, _)) = recorded_source.as_ref() {
        if recorded_workspace_id != &workspace_id.to_string() {
            patch_snapshot_status(
                &api,
                &snapshot,
                "Failed",
                "snapshot intent belongs to a different backend workspace",
                None,
                None,
                recorded_source,
            )
            .await?;
            return Ok(Action::await_change());
        }
    }
    match admin.load_snapshot(snapshot_id).await {
        Ok(existing) => {
            let source = recorded_source.or_else(|| {
                current
                    .record
                    .head_epoch
                    .checked_sub(SEAL_HEAD_EPOCH_ADVANCE)
                    .map(|epoch| (workspace_id.to_string(), epoch))
            });
            patch_snapshot_status(
                &api,
                &snapshot,
                "Ready",
                "immutable revision is pinned",
                Some(existing.snapshot_id.to_string()),
                Some(revision_to_status(&existing.revision)),
                source,
            )
            .await?;
            return Ok(Action::requeue(Duration::from_secs(300)));
        }
        Err(error)
            if error
                .downcast_ref::<WorkspaceError>()
                .is_some_and(|error| matches!(error, WorkspaceError::SnapshotNotFound(_))) => {}
        Err(error) => return Err(error.into()),
    }
    let Some((_, source_head_epoch)) = recorded_source else {
        patch_snapshot_status(
            &api,
            &snapshot,
            "Sealing",
            "snapshot intent recorded; waiting to seal the source head",
            None,
            None,
            Some((workspace_id.to_string(), current.record.head_epoch)),
        )
        .await?;
        return Ok(Action::await_change());
    };
    let generation = holder_generation(resource_uuid(&snapshot_uid));
    let request = EnsureSnapshotRequest {
        snapshot_id,
        name: format!("{namespace}/{name}"),
        revision: current.base_revision.clone(),
        owner_id: workspace.spec.owner_id.clone(),
    };
    let pinned = match snapshot_resume_action(source_head_epoch, current.record.head_epoch) {
        SnapshotResumeAction::Seal => {
            admin
                .seal_and_snapshot(
                    workspace_id,
                    source_head_epoch,
                    generation,
                    cluster_spec.lease_ttl_seconds,
                    cluster_spec.heartbeat_seconds,
                    request,
                )
                .await?
                .1
        }
        SnapshotResumeAction::PinRecoveredRevision => admin.ensure_snapshot(request).await?,
        SnapshotResumeAction::Conflict => {
            patch_snapshot_status(
                &api,
                &snapshot,
                "Failed",
                &format!(
                    "source head changed unexpectedly: intended epoch {source_head_epoch}, current epoch {}",
                    current.record.head_epoch
                ),
                None,
                None,
                Some((workspace_id.to_string(), source_head_epoch)),
            )
            .await?;
            return Ok(Action::await_change());
        }
    };
    patch_snapshot_status(
        &api,
        &snapshot,
        "Ready",
        "workspace was sealed and the immutable revision was pinned",
        Some(pinned.snapshot_id.to_string()),
        Some(revision_to_status(&pinned.revision)),
        Some((workspace_id.to_string(), source_head_epoch)),
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

pub fn error_policy_workspace(
    _object: Arc<BrewFSWorkspace>,
    _error: &WorkspaceReconcileError,
    _ctx: Arc<OperatorContext>,
) -> Action {
    Action::requeue(Duration::from_secs(15))
}

pub fn error_policy_mount(
    _object: Arc<BrewFSWorkspaceMount>,
    _error: &WorkspaceReconcileError,
    _ctx: Arc<OperatorContext>,
) -> Action {
    Action::requeue(Duration::from_secs(15))
}

pub fn error_policy_snapshot(
    _object: Arc<BrewFSWorkspaceSnapshot>,
    _error: &WorkspaceReconcileError,
    _ctx: Arc<OperatorContext>,
) -> Action {
    Action::requeue(Duration::from_secs(15))
}

async fn resolve_workspace_source(
    client: &Client,
    namespace: &str,
    workspace: &BrewFSWorkspace,
    cluster: &BrewFSCluster,
    admin: &dyn WorkspaceAdmin,
) -> anyhow::Result<brewfs::workspace_overlay::model::BaseRevision> {
    match workspace.spec.source.kind {
        WorkspaceSourceKind::ClusterRoot => {
            let revision = cluster
                .status
                .as_ref()
                .and_then(|status| status.workspace.as_ref())
                .map(|status| &status.root_revision)
                .ok_or_else(|| anyhow!("cluster root revision is not ready"))?;
            revision_from_status(revision)
        }
        WorkspaceSourceKind::Snapshot => {
            let name = workspace
                .spec
                .source
                .name
                .as_deref()
                .ok_or_else(|| anyhow!("snapshot source name is missing"))?;
            let api: Api<BrewFSWorkspaceSnapshot> = Api::namespaced(client.clone(), namespace);
            let snapshot = api
                .get(name)
                .await
                .with_context(|| format!("load BrewFSWorkspaceSnapshot {name}"))?;
            if snapshot.spec.cluster_ref.name != workspace.spec.cluster_ref.name {
                bail!("snapshot source belongs to another BrewFSCluster");
            }
            let status = snapshot
                .status
                .as_ref()
                .filter(|status| status.phase == "Ready")
                .ok_or_else(|| anyhow!("snapshot source is not Ready"))?;
            let snapshot_id: SnapshotId = status
                .snapshot_id
                .as_deref()
                .ok_or_else(|| anyhow!("snapshot status has no backend ID"))?
                .parse()
                .context("parse snapshot backend ID")?;
            let backend = admin.load_snapshot(snapshot_id).await?;
            let status_revision = status
                .revision
                .as_ref()
                .ok_or_else(|| anyhow!("snapshot status has no revision"))?;
            if revision_to_status(&backend.revision) != *status_revision {
                bail!("snapshot status does not match the backend revision");
            }
            Ok(backend.revision)
        }
    }
}

async fn cleanup_workspace(
    client: &Client,
    namespace: &str,
    workspace: &BrewFSWorkspace,
) -> anyhow::Result<()> {
    let mount_api: Api<BrewFSWorkspaceMount> = Api::namespaced(client.clone(), namespace);
    let mounts = mount_api.list(&ListParams::default()).await?;
    let mut remaining = false;
    for mount in mounts {
        if mount.spec.workspace_ref.name == workspace.name_any() {
            remaining = true;
            if mount.meta().deletion_timestamp.is_none() {
                mount_api
                    .delete(&mount.name_any(), &DeleteParams::default())
                    .await?;
            }
        }
    }
    if remaining {
        bail!("waiting for BrewFSWorkspaceMount finalizers");
    }
    let cluster = load_cluster(client, namespace, &workspace.spec.cluster_ref.name).await?;
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    let redis_password =
        load_workspace_catalog_password(client, namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    let workspace_id: WorkspaceId = workspace
        .status
        .as_ref()
        .and_then(|status| status.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("cannot safely delete a workspace without its backend ID"))?
        .parse()?;
    admin.reap_expired_leases().await?;
    let view = admin.inspect_workspace(workspace_id).await?;
    if view
        .leases
        .iter()
        .any(|lease| lease.state == LeaseState::Active)
    {
        bail!("waiting for the workspace writable lease to be released");
    }
    if workspace.spec.deletion_policy == WorkspaceDeletionPolicy::Retain {
        return Ok(());
    }
    let force = force_delete_requested(workspace)?;
    if workspace.spec.deletion_policy == WorkspaceDeletionPolicy::SnapshotAndDelete {
        ensure_final_snapshot(client, namespace, workspace).await?;
    }
    admin.mark_workspace_deleting(workspace_id, force).await
}

async fn ensure_final_snapshot(
    client: &Client,
    namespace: &str,
    workspace: &BrewFSWorkspace,
) -> anyhow::Result<()> {
    let uid = resource_uid(workspace)?;
    let suffix = uid.replace('-', "").chars().take(12).collect::<String>();
    let prefix = workspace.name_any().chars().take(40).collect::<String>();
    let name = format!("{prefix}-final-{suffix}");
    let api: Api<BrewFSWorkspaceSnapshot> = Api::namespaced(client.clone(), namespace);
    let snapshot = match api.get_opt(&name).await? {
        Some(snapshot) => snapshot,
        None => {
            api.create(
                &PostParams::default(),
                &BrewFSWorkspaceSnapshot::new(
                    &name,
                    super::crd::BrewFSWorkspaceSnapshotSpec {
                        cluster_ref: workspace.spec.cluster_ref.clone(),
                        workspace_ref: super::crd::NamespacedNameRef {
                            name: workspace.name_any(),
                        },
                    },
                ),
            )
            .await?
        }
    };
    if snapshot
        .status
        .as_ref()
        .is_none_or(|status| status.phase != "Ready")
    {
        bail!("waiting for final snapshot {name} to become Ready");
    }
    Ok(())
}

async fn verify_mount_lease_released(
    client: &Client,
    namespace: &str,
    mount: &BrewFSWorkspaceMount,
) -> anyhow::Result<()> {
    let workspace_api: Api<BrewFSWorkspace> = Api::namespaced(client.clone(), namespace);
    let workspace = workspace_api
        .get(&mount.spec.workspace_ref.name)
        .await
        .context("load workspace while releasing mount")?;
    let cluster = load_cluster(client, namespace, &workspace.spec.cluster_ref.name).await?;
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    let redis_password =
        load_workspace_catalog_password(client, namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    admin.reap_expired_leases().await?;
    let workspace_id: WorkspaceId = workspace
        .status
        .as_ref()
        .and_then(|status| status.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("workspace has no backend identity"))?
        .parse()?;
    let view = admin.inspect_workspace(workspace_id).await?;
    if view
        .leases
        .iter()
        .any(|lease| lease.state == LeaseState::Active)
    {
        bail!("waiting for backend confirmation that the writable lease is released");
    }
    Ok(())
}

async fn cleanup_snapshot(
    client: &Client,
    namespace: &str,
    snapshot: &BrewFSWorkspaceSnapshot,
) -> anyhow::Result<()> {
    let workspace_api: Api<BrewFSWorkspace> = Api::namespaced(client.clone(), namespace);
    if workspace_api
        .list(&ListParams::default())
        .await?
        .iter()
        .any(|workspace| {
            workspace.spec.source.kind == WorkspaceSourceKind::Snapshot
                && workspace.spec.source.name.as_deref() == Some(&snapshot.name_any())
        })
    {
        bail!("snapshot is still referenced by a BrewFSWorkspace");
    }
    let Some(snapshot_id) = snapshot
        .status
        .as_ref()
        .and_then(|status| status.snapshot_id.as_deref())
    else {
        return Ok(());
    };
    let cluster = load_cluster(client, namespace, &snapshot.spec.cluster_ref.name).await?;
    let cluster_spec = enabled_cluster_workspace_spec(&cluster)?;
    let redis_password =
        load_workspace_catalog_password(client, namespace, &cluster, cluster_spec).await?;
    let admin = connect_workspace_admin(
        &cluster.name_any(),
        namespace,
        cluster.spec.redis.port,
        cluster_spec,
        redis_password.as_deref(),
    )
    .await?;
    admin.delete_snapshot(snapshot_id.parse()?).await
}

async fn ensure_single_mount(
    client: &Client,
    namespace: &str,
    mount: &BrewFSWorkspaceMount,
) -> anyhow::Result<()> {
    let api: Api<BrewFSWorkspaceMount> = Api::namespaced(client.clone(), namespace);
    let current_uid = resource_uid(mount)?;
    let conflict = api
        .list(&ListParams::default())
        .await?
        .into_iter()
        .any(|candidate| {
            candidate.spec.workspace_ref.name == mount.spec.workspace_ref.name
                && candidate.meta().deletion_timestamp.is_none()
                && resource_uid(&candidate).is_ok_and(|uid| uid < current_uid)
        });
    if conflict {
        bail!("another BrewFSWorkspaceMount owns the writable mount slot");
    }
    Ok(())
}

async fn load_cluster(
    client: &Client,
    namespace: &str,
    name: &str,
) -> anyhow::Result<BrewFSCluster> {
    Api::<BrewFSCluster>::namespaced(client.clone(), namespace)
        .get(name)
        .await
        .with_context(|| format!("load BrewFSCluster {name}"))
}

fn enabled_cluster_workspace_spec(
    cluster: &BrewFSCluster,
) -> anyhow::Result<&super::crd::WorkspaceClusterSpec> {
    cluster
        .spec
        .workspace
        .as_ref()
        .filter(|spec| spec.enabled)
        .ok_or_else(|| anyhow!("BrewFSCluster does not have workspace support enabled"))
}

fn workspace_status(
    workspace: &BrewFSWorkspace,
    phase: &str,
    message: &str,
    workspace_id: Option<String>,
    base_revision: Option<WorkspaceRevision>,
) -> BrewFSWorkspaceStatus {
    BrewFSWorkspaceStatus {
        observed_generation: workspace.metadata.generation,
        phase: phase.into(),
        message: message.into(),
        workspace_id,
        origin_revision: None,
        current_base_revision: base_revision,
        head_layer_id: None,
        head_epoch: None,
        active_mount_ref: None,
        last_clean_release_epoch: None,
        conditions: Vec::new(),
    }
}

fn condition(
    condition_type: &str,
    status: &str,
    reason: &str,
    message: &str,
    observed_generation: Option<i64>,
) -> WorkspaceCondition {
    WorkspaceCondition {
        condition_type: condition_type.into(),
        status: status.into(),
        reason: reason.into(),
        message: message.into(),
        observed_generation,
        last_transition_time: Utc::now(),
    }
}

async fn patch_workspace_status(
    api: &Api<BrewFSWorkspace>,
    workspace: &BrewFSWorkspace,
    status: BrewFSWorkspaceStatus,
) -> anyhow::Result<()> {
    if workspace.status.as_ref() == Some(&status) {
        return Ok(());
    }
    patch_status(api, &workspace.name_any(), "BrewFSWorkspace", status).await
}

async fn patch_snapshot_status(
    api: &Api<BrewFSWorkspaceSnapshot>,
    snapshot: &BrewFSWorkspaceSnapshot,
    phase: &str,
    message: &str,
    snapshot_id: Option<String>,
    revision: Option<WorkspaceRevision>,
    source: Option<(String, u64)>,
) -> anyhow::Result<()> {
    let (source_workspace_id, source_head_epoch) = source
        .map(|(workspace_id, head_epoch)| (Some(workspace_id), Some(head_epoch)))
        .unwrap_or((None, None));
    let status = BrewFSWorkspaceSnapshotStatus {
        observed_generation: snapshot.metadata.generation,
        phase: phase.into(),
        message: message.into(),
        snapshot_id,
        source_workspace_id,
        source_head_epoch,
        revision,
        conditions: Vec::new(),
    };
    if snapshot.status.as_ref() == Some(&status) {
        return Ok(());
    }
    patch_status(api, &snapshot.name_any(), "BrewFSWorkspaceSnapshot", status).await
}

async fn patch_cluster_workspace_status(
    client: &Client,
    namespace: &str,
    cluster: &BrewFSCluster,
    workspace: Option<super::crd::WorkspaceClusterStatus>,
) -> anyhow::Result<()> {
    if cluster
        .status
        .as_ref()
        .and_then(|status| status.workspace.as_ref())
        == workspace.as_ref()
    {
        return Ok(());
    }
    let api: Api<BrewFSCluster> = Api::namespaced(client.clone(), namespace);
    api.patch_status(
        &cluster.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({ "status": { "workspace": workspace } })),
    )
    .await?;
    Ok(())
}

async fn patch_status<K, S>(api: &Api<K>, name: &str, kind: &str, status: S) -> anyhow::Result<()>
where
    K: Clone + std::fmt::Debug + DeserializeOwned + Serialize + kube::Resource<DynamicType = ()>,
    S: Serialize,
{
    api.patch_status(
        name,
        &PatchParams::apply("brewfs-workspace-operator").force(),
        &Patch::Apply(json!({
            "apiVersion": "storage.brewfs.io/v1alpha1",
            "kind": kind,
            "status": status,
        })),
    )
    .await?;
    Ok(())
}

async fn ensure_finalizer<K>(
    api: &Api<K>,
    name: &str,
    object: &K,
    finalizer: &str,
) -> anyhow::Result<bool>
where
    K: Clone + std::fmt::Debug + DeserializeOwned + Serialize + kube::Resource<DynamicType = ()>,
{
    let mut finalizers = object.meta().finalizers.clone().unwrap_or_default();
    if finalizers.iter().any(|value| value == finalizer) {
        return Ok(false);
    }
    finalizers.push(finalizer.into());
    api.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(json!({ "metadata": { "finalizers": finalizers } })),
    )
    .await?;
    Ok(true)
}

async fn remove_finalizer<K>(
    api: &Api<K>,
    name: &str,
    object: &K,
    finalizer: &str,
) -> anyhow::Result<()>
where
    K: Clone + std::fmt::Debug + DeserializeOwned + Serialize + kube::Resource<DynamicType = ()>,
{
    let finalizers = object
        .meta()
        .finalizers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value != finalizer)
        .collect::<Vec<_>>();
    api.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(json!({ "metadata": { "finalizers": finalizers } })),
    )
    .await?;
    Ok(())
}

fn force_delete_requested(workspace: &BrewFSWorkspace) -> anyhow::Result<bool> {
    let annotations = workspace.annotations();
    if annotations
        .get(FORCE_DELETE_ANNOTATION)
        .is_none_or(|value| value != "true")
    {
        return Ok(false);
    }
    if annotations
        .get(FORCE_DELETE_REASON_ANNOTATION)
        .is_none_or(|reason| reason.trim().is_empty())
    {
        bail!("force-delete requires a non-empty force-delete-reason annotation");
    }
    Ok(true)
}

fn resource_uid<K: kube::ResourceExt>(resource: &K) -> anyhow::Result<String> {
    resource
        .uid()
        .ok_or_else(|| anyhow!("{} has no Kubernetes UID", resource.name_any()))
}

fn resource_uuid(uid: &str) -> Uuid {
    Uuid::parse_str(uid).unwrap_or_else(|_| deterministic_uuid(ID_NAMESPACE, uid))
}

fn holder_generation(id: Uuid) -> u64 {
    let bytes: [u8; 8] = id.as_bytes()[8..]
        .try_into()
        .expect("UUID suffix always contains eight bytes");
    (u64::from_be_bytes(bytes) & i64::MAX as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::ObjectMeta;

    #[test]
    fn fallback_resource_uuid_is_stable() {
        assert_eq!(resource_uuid("not-a-uuid"), resource_uuid("not-a-uuid"));
        assert_ne!(resource_uuid("not-a-uuid"), resource_uuid("another-uid"));
    }

    #[test]
    fn snapshot_retry_only_accepts_the_original_or_fully_sealed_epoch() {
        assert_eq!(snapshot_resume_action(7, 7), SnapshotResumeAction::Seal);
        assert_eq!(
            snapshot_resume_action(7, 9),
            SnapshotResumeAction::PinRecoveredRevision
        );
        assert_eq!(snapshot_resume_action(7, 8), SnapshotResumeAction::Conflict);
        assert_eq!(
            snapshot_resume_action(7, 10),
            SnapshotResumeAction::Conflict
        );
        assert_eq!(
            snapshot_resume_action(u64::MAX, 0),
            SnapshotResumeAction::Conflict
        );
    }

    #[test]
    fn force_delete_requires_an_audit_reason() {
        let mut workspace = BrewFSWorkspace::new(
            "test",
            super::super::crd::BrewFSWorkspaceSpec {
                cluster_ref: super::super::crd::NamespacedNameRef { name: "c".into() },
                source: super::super::crd::WorkspaceSourceSpec {
                    kind: WorkspaceSourceKind::ClusterRoot,
                    name: None,
                },
                desired_state: WorkspaceDesiredState::Active,
                owner_id: None,
                deletion_policy: WorkspaceDeletionPolicy::Delete,
            },
        );
        workspace.metadata = ObjectMeta {
            annotations: Some(std::collections::BTreeMap::from([(
                FORCE_DELETE_ANNOTATION.into(),
                "true".into(),
            )])),
            ..ObjectMeta::default()
        };
        assert!(force_delete_requested(&workspace).is_err());
    }
}

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{ConsumerContainerSpec, ConsumerVolumeSpec};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceClusterSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub catalog_backend: WorkspaceCatalogBackend,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub tikv_pd_endpoints: Vec<String>,
    #[serde(default = "default_lease_ttl_seconds")]
    pub lease_ttl_seconds: u32,
    #[serde(default = "default_heartbeat_seconds")]
    pub heartbeat_seconds: u32,
    #[serde(default = "default_gc_grace_seconds")]
    pub gc_grace_seconds: u32,
    #[serde(default = "default_catalog_storage_size")]
    pub catalog_storage_size: String,
}

impl Default for WorkspaceClusterSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            catalog_backend: WorkspaceCatalogBackend::Redis,
            namespace: None,
            tikv_pd_endpoints: Vec::new(),
            lease_ttl_seconds: default_lease_ttl_seconds(),
            heartbeat_seconds: default_heartbeat_seconds(),
            gc_grace_seconds: default_gc_grace_seconds(),
            catalog_storage_size: default_catalog_storage_size(),
        }
    }
}

impl WorkspaceClusterSpec {
    pub fn validate(&self) -> Result<(), String> {
        if !(10..=300).contains(&self.lease_ttl_seconds) {
            return Err("leaseTtlSeconds must be between 10 and 300".into());
        }
        if self.heartbeat_seconds == 0
            || self.heartbeat_seconds.saturating_mul(2) >= self.lease_ttl_seconds
        {
            return Err(
                "heartbeatSeconds must be greater than zero and less than half the lease TTL"
                    .into(),
            );
        }
        if self.gc_grace_seconds < self.lease_ttl_seconds {
            return Err("gcGraceSeconds must be at least leaseTtlSeconds".into());
        }
        if self.catalog_storage_size.trim().is_empty() {
            return Err("catalogStorageSize must not be empty".into());
        }
        if self.catalog_backend == WorkspaceCatalogBackend::TiKv
            && self.tikv_pd_endpoints.is_empty()
        {
            return Err("tikvPdEndpoints is required for the TiKV catalog backend".into());
        }
        if self
            .namespace
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("workspace namespace must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceCatalogBackend {
    #[default]
    Redis,
    TiKv,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceClusterStatus {
    pub volume_id: String,
    pub schema_version: u32,
    pub catalog_backend: WorkspaceCatalogBackend,
    pub catalog_namespace: String,
    pub root_snapshot_id: String,
    pub root_revision: WorkspaceRevision,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRevision {
    pub layer_id: String,
    pub sealed_version: u64,
    pub root_hash: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamespacedNameRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceSourceKind {
    ClusterRoot,
    Snapshot,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSourceSpec {
    pub kind: WorkspaceSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl WorkspaceSourceSpec {
    pub fn validate(&self) -> Result<(), String> {
        match (self.kind, self.name.as_deref()) {
            (WorkspaceSourceKind::ClusterRoot, None) => Ok(()),
            (WorkspaceSourceKind::Snapshot, Some(name)) if !name.trim().is_empty() => Ok(()),
            (WorkspaceSourceKind::ClusterRoot, Some(_)) => {
                Err("ClusterRoot source must not set name".into())
            }
            (WorkspaceSourceKind::Snapshot, _) => {
                Err("Snapshot source requires a non-empty name".into())
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceDesiredState {
    #[default]
    Active,
    Suspended,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceDeletionPolicy {
    #[default]
    Delete,
    Retain,
    SnapshotAndDelete,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "storage.brewfs.io",
    version = "v1alpha1",
    kind = "BrewFSWorkspace",
    plural = "brewfsworkspaces",
    namespaced,
    status = "BrewFSWorkspaceStatus",
    shortname = "bfws"
)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceSpec {
    pub cluster_ref: NamespacedNameRef,
    pub source: WorkspaceSourceSpec,
    #[serde(default)]
    pub desired_state: WorkspaceDesiredState,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub deletion_policy: WorkspaceDeletionPolicy,
}

impl BrewFSWorkspaceSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.cluster_ref.name.trim().is_empty() {
            return Err("clusterRef.name must not be empty".into());
        }
        if self
            .owner_id
            .as_deref()
            .is_some_and(|owner| owner.trim().is_empty())
        {
            return Err("ownerId must not be empty when set".into());
        }
        self.source.validate()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_revision: Option<WorkspaceRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_base_revision: Option<WorkspaceRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_layer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_mount_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_clean_release_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<WorkspaceCondition>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCondition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    pub last_transition_time: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceCacheMode {
    WriteThrough,
    #[default]
    WriteBack,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspaceCacheReclaimPolicy {
    #[default]
    Delete,
    Retain,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCacheSpec {
    #[serde(default)]
    pub mode: WorkspaceCacheMode,
    #[serde(default)]
    pub storage_class_name: Option<String>,
    #[serde(default = "default_cache_size")]
    pub size: String,
    #[serde(default)]
    pub reclaim_policy: WorkspaceCacheReclaimPolicy,
}

impl Default for WorkspaceCacheSpec {
    fn default() -> Self {
        Self {
            mode: WorkspaceCacheMode::WriteBack,
            storage_class_name: None,
            size: default_cache_size(),
            reclaim_policy: WorkspaceCacheReclaimPolicy::Delete,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentSpec {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub containers: Vec<ConsumerContainerSpec>,
    #[serde(default)]
    pub init_containers: Vec<ConsumerContainerSpec>,
    #[serde(default)]
    pub volumes: Vec<ConsumerVolumeSpec>,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "storage.brewfs.io",
    version = "v1alpha1",
    kind = "BrewFSWorkspaceMount",
    plural = "brewfsworkspacemounts",
    namespaced,
    status = "BrewFSWorkspaceMountStatus",
    shortname = "bfwsm"
)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceMountSpec {
    pub workspace_ref: NamespacedNameRef,
    #[serde(default = "default_workspace_mount_path")]
    pub mount_path: String,
    #[serde(default = "default_workspace_image")]
    pub image: String,
    #[serde(default = "default_image_pull_policy")]
    pub image_pull_policy: String,
    #[serde(default)]
    pub cache: WorkspaceCacheSpec,
    #[serde(default)]
    pub agent: Option<WorkspaceAgentSpec>,
    #[serde(default = "default_termination_grace_period_seconds")]
    pub termination_grace_period_seconds: u32,
    #[serde(default)]
    pub service_account_name: Option<String>,
    #[serde(default)]
    pub node_selector: BTreeMap<String, String>,
}

impl BrewFSWorkspaceMountSpec {
    pub fn validate(&self, lease_ttl_seconds: u32) -> Result<(), String> {
        if self.workspace_ref.name.trim().is_empty() {
            return Err("workspaceRef.name must not be empty".into());
        }
        validate_mount_path(&self.mount_path)?;
        if self.cache.mode == WorkspaceCacheMode::WriteBack && self.cache.size.trim().is_empty() {
            return Err("WriteBack cache requires a non-empty persistent volume size".into());
        }
        if self.termination_grace_period_seconds < lease_ttl_seconds.saturating_add(30) {
            return Err("terminationGracePeriodSeconds must cover lease TTL plus the 30 second drain timeout".into());
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| agent.containers.is_empty())
        {
            return Err("agent.containers must not be empty when agent is configured".into());
        }
        if let Some(agent) = &self.agent {
            for volume in &agent.volumes {
                if matches!(
                    volume.name.as_str(),
                    "workspace-mount" | "workspace-cache" | "workspace-config" | "fuse-device"
                ) {
                    return Err(format!(
                        "agent volume name {:?} is reserved by the workspace runtime",
                        volume.name
                    ));
                }
                if volume.host_path.is_some() || volume.secret_name.is_some() {
                    return Err("workspace agents cannot request hostPath or Secret volumes".into());
                }
            }
            for container in agent.containers.iter().chain(&agent.init_containers) {
                if container.volume_mounts.iter().any(|mount| {
                    matches!(
                        mount.name.as_str(),
                        "workspace-mount" | "workspace-cache" | "workspace-config" | "fuse-device"
                    )
                }) {
                    return Err(
                        "agent containers cannot mount workspace runtime internal volumes".into(),
                    );
                }
                if container
                    .security_context
                    .as_ref()
                    .and_then(|security| security.privileged)
                    == Some(true)
                    || container
                        .security_context
                        .as_ref()
                        .is_some_and(|security| !security.capabilities_add.is_empty())
                {
                    return Err(
                        "workspace agents cannot be privileged or add Linux capabilities".into(),
                    );
                }
                if container
                    .env_from
                    .iter()
                    .any(|source| source.secret_name.is_some())
                {
                    return Err("workspace agents cannot import Secret envFrom sources".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceMountStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pod_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted_head_layer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted_head_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted_base_revision: Option<WorkspaceRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<WorkspaceCondition>,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "storage.brewfs.io",
    version = "v1alpha1",
    kind = "BrewFSWorkspaceSnapshot",
    plural = "brewfsworkspacesnapshots",
    namespaced,
    status = "BrewFSWorkspaceSnapshotStatus",
    shortname = "bfwss"
)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceSnapshotSpec {
    pub cluster_ref: NamespacedNameRef,
    pub workspace_ref: NamespacedNameRef,
}

impl BrewFSWorkspaceSnapshotSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.cluster_ref.name.trim().is_empty() {
            return Err("clusterRef.name must not be empty".into());
        }
        if self.workspace_ref.name.trim().is_empty() {
            return Err("workspaceRef.name must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrewFSWorkspaceSnapshotStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_head_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<WorkspaceRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<WorkspaceCondition>,
}

fn validate_mount_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err("mountPath must be absolute".into());
    }
    let normalized = path.trim_end_matches('/');
    if normalized.is_empty()
        || ["/proc", "/sys", "/dev", "/var/run/secrets"]
            .iter()
            .any(|reserved| {
                normalized == *reserved
                    || normalized
                        .strip_prefix(reserved)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
    {
        return Err(format!("mountPath {path:?} is reserved"));
    }
    if !path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
    {
        return Err("mountPath contains unsupported characters".into());
    }
    if normalized
        .split('/')
        .skip(1)
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err("mountPath must not contain empty, '.' or '..' components".into());
    }
    Ok(())
}

fn default_lease_ttl_seconds() -> u32 {
    30
}

fn default_heartbeat_seconds() -> u32 {
    10
}

fn default_gc_grace_seconds() -> u32 {
    60
}

fn default_catalog_storage_size() -> String {
    "5Gi".into()
}

fn default_cache_size() -> String {
    "20Gi".into()
}

fn default_workspace_mount_path() -> String {
    "/workspace".into()
}

fn default_workspace_image() -> String {
    "brewfs:local".into()
}

fn default_image_pull_policy() -> String {
    "IfNotPresent".into()
}

fn default_termination_grace_period_seconds() -> u32 {
    90
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_union_is_strict() {
        assert!(WorkspaceSourceSpec {
            kind: WorkspaceSourceKind::ClusterRoot,
            name: None,
        }
        .validate()
        .is_ok());
        assert!(WorkspaceSourceSpec {
            kind: WorkspaceSourceKind::ClusterRoot,
            name: Some("unexpected".into()),
        }
        .validate()
        .is_err());
        assert!(WorkspaceSourceSpec {
            kind: WorkspaceSourceKind::Snapshot,
            name: None,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn cluster_timing_constraints_are_fail_closed() {
        let mut spec = WorkspaceClusterSpec {
            enabled: true,
            ..WorkspaceClusterSpec::default()
        };
        assert!(spec.validate().is_ok());
        spec.heartbeat_seconds = 15;
        assert!(spec.validate().is_err());
        spec.heartbeat_seconds = 10;
        spec.gc_grace_seconds = 20;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn workspace_mount_rejects_reserved_paths_and_short_shutdown() {
        let mut spec = BrewFSWorkspaceMountSpec {
            workspace_ref: NamespacedNameRef {
                name: "agent".into(),
            },
            mount_path: "/proc".into(),
            image: default_workspace_image(),
            image_pull_policy: default_image_pull_policy(),
            cache: WorkspaceCacheSpec::default(),
            agent: None,
            termination_grace_period_seconds: 90,
            service_account_name: None,
            node_selector: BTreeMap::new(),
        };
        assert!(spec.validate(30).is_err());
        spec.mount_path = "/proc/self/fd".into();
        assert!(spec.validate(30).is_err());
        spec.mount_path = "/workspace;touch /tmp/pwned".into();
        assert!(spec.validate(30).is_err());
        spec.mount_path = "/workspace/../dev".into();
        assert!(spec.validate(30).is_err());
        spec.mount_path = "/workspace".into();
        spec.termination_grace_period_seconds = 59;
        assert!(spec.validate(30).is_err());
        spec.termination_grace_period_seconds = 60;
        assert!(spec.validate(30).is_ok());
    }

    #[test]
    fn workspace_agent_cannot_mount_runtime_internal_volumes() {
        let spec: BrewFSWorkspaceMountSpec = serde_json::from_value(serde_json::json!({
            "workspaceRef": { "name": "agent" },
            "mountPath": "/workspace",
            "terminationGracePeriodSeconds": 90,
            "agent": {
                "containers": [{
                    "name": "agent",
                    "image": "example/agent:test",
                    "volumeMounts": [{
                        "name": "fuse-device",
                        "mountPath": "/dev/fuse"
                    }]
                }]
            }
        }))
        .unwrap();

        assert!(spec.validate(30).is_err());
    }
}

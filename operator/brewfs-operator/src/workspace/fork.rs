use anyhow::{anyhow, bail, Result};
use k8s_openapi::api::storage::v1::StorageClass;
use kube::api::{DeleteParams, ObjectMeta, Preconditions};
use kube::{Resource, ResourceExt};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::crd::{
    BrewFSWorkspace, BrewFSWorkspaceFork, BrewFSWorkspaceForkIdentity, BrewFSWorkspaceForkStatus,
    BrewFSWorkspaceSnapshot, BrewFSWorkspaceSnapshotSpec, NamespacedNameRef, WorkspaceSourceKind,
};

const FORK_UID: &str = "storage.brewfs.io/fork-uid";
const FORK_ID: &str = "storage.brewfs.io/fork-id";

pub(super) fn new_identity(
    fork: &BrewFSWorkspaceFork,
    source: &BrewFSWorkspace,
) -> Result<BrewFSWorkspaceForkIdentity> {
    let mut identity = BrewFSWorkspaceForkIdentity {
        namespace: fork
            .namespace()
            .ok_or_else(|| anyhow!("fork must be namespaced"))?,
        fork_name: fork.name_any(),
        fork_uid: uid(fork)?,
        cluster_name: fork.spec.cluster_ref.name.clone(),
        source_workspace_name: fork.spec.source_workspace_ref.name.clone(),
        source_workspace_uid: uid(source)?,
        source_workspace_id: source
            .status
            .as_ref()
            .and_then(|s| s.workspace_id.clone())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("source workspace has no backend identity"))?,
        workspace_name: fork.spec.workspace_name.clone(),
        snapshot_name: format!("{}-snapshot", fork.name_any()),
        storage_class_name: String::new(),
    };
    if identity.snapshot_name.len() > 253 {
        bail!("fork name is too long for its snapshot name");
    }
    // JSON encodes component boundaries, so ambiguous concatenations cannot collide.
    identity.storage_class_name = storage_class_name(
        &identity.namespace,
        &identity.workspace_name,
        &serde_json::to_string(&identity)?,
    );
    Ok(identity)
}

pub(super) fn persisted_identity(
    fork: &BrewFSWorkspaceFork,
) -> Result<&BrewFSWorkspaceForkIdentity> {
    let identity = fork
        .status
        .as_ref()
        .and_then(|s| s.identity.as_ref())
        .ok_or_else(|| {
            anyhow!("fork has no persisted identity; legacy forks require manual recovery")
        })?;
    if identity.fork_uid != uid(fork)?
        || Some(identity.namespace.as_str()) != fork.namespace().as_deref()
        || identity.fork_name != fork.name_any()
    {
        bail!("persisted fork identity belongs to a different fork");
    }
    Ok(identity)
}

pub(super) fn updated_status(
    fork: &BrewFSWorkspaceFork,
    phase: &str,
    message: &str,
    workspace_id: Option<String>,
    storage_class_name: Option<String>,
    snapshot_id: Option<String>,
) -> Result<BrewFSWorkspaceForkStatus> {
    let mut status = fork.status.clone().unwrap_or_default();
    status.observed_generation = fork.metadata.generation;
    status.phase = phase.into();
    status.message = message.into();
    for (recorded, next) in [
        (&mut status.workspace_id, workspace_id),
        (&mut status.storage_class_name, storage_class_name),
        (&mut status.snapshot_id, snapshot_id),
    ] {
        if let Some(next) = next {
            if recorded.as_ref().is_some_and(|old| old != &next) {
                bail!("fork backend identity cannot change once recorded");
            }
            *recorded = Some(next);
        }
    }
    Ok(status)
}

pub(super) fn validate_source(fork: &BrewFSWorkspaceFork, source: &BrewFSWorkspace) -> Result<()> {
    let identity = persisted_identity(fork)?;
    if identity.cluster_name != fork.spec.cluster_ref.name
        || identity.source_workspace_name != fork.spec.source_workspace_ref.name
        || identity.workspace_name != fork.spec.workspace_name
        || source.name_any() != identity.source_workspace_name
        || source.spec.cluster_ref.name != identity.cluster_name
        || uid(source)? != identity.source_workspace_uid
        || source
            .status
            .as_ref()
            .and_then(|s| s.workspace_id.as_deref())
            != Some(identity.source_workspace_id.as_str())
    {
        bail!("fork spec or source workspace differs from persisted identity");
    }
    Ok(())
}

fn validate_owner(fork: &BrewFSWorkspaceFork, resource: &impl ResourceExt) -> Result<()> {
    let owner = fork
        .controller_owner_ref(&())
        .ok_or_else(|| anyhow!("fork has no owner UID"))?;
    if resource.meta().owner_references.as_deref() != Some(std::slice::from_ref(&owner)) {
        bail!(
            "fork resource {} has a conflicting owner",
            resource.name_any()
        );
    }
    Ok(())
}

pub(super) fn desired_snapshot(fork: &BrewFSWorkspaceFork) -> Result<BrewFSWorkspaceSnapshot> {
    let identity = persisted_identity(fork)?;
    let mut snapshot = BrewFSWorkspaceSnapshot::new(
        &identity.snapshot_name,
        BrewFSWorkspaceSnapshotSpec {
            cluster_ref: NamespacedNameRef {
                name: identity.cluster_name.clone(),
            },
            workspace_ref: NamespacedNameRef {
                name: identity.source_workspace_name.clone(),
            },
        },
    );
    snapshot.metadata.owner_references = Some(vec![fork
        .controller_owner_ref(&())
        .ok_or_else(|| anyhow!("fork has no owner UID"))?]);
    Ok(snapshot)
}

pub(super) fn validate_snapshot(
    fork: &BrewFSWorkspaceFork,
    snapshot: &BrewFSWorkspaceSnapshot,
    source_id: &str,
) -> Result<()> {
    let identity = persisted_identity(fork)?;
    validate_owner(fork, snapshot)?;
    if snapshot.name_any() != identity.snapshot_name
        || snapshot.spec.workspace_ref.name != identity.source_workspace_name
        || snapshot.spec.cluster_ref.name != identity.cluster_name
        || source_id != identity.source_workspace_id
    {
        bail!("fork snapshot already exists with a different source");
    }
    if let Some(status) = &snapshot.status {
        if (status.phase == "Ready" || status.source_workspace_id.is_some())
            && status.source_workspace_id.as_deref() != Some(source_id)
        {
            bail!("fork snapshot has a different backend source identity");
        }
        if status.phase == "Ready" && status.snapshot_id.as_deref().is_none_or(str::is_empty) {
            bail!("fork snapshot is Ready without a backend identity");
        }
        if let Some(recorded) = fork.status.as_ref().and_then(|s| s.snapshot_id.as_ref()) {
            if status.snapshot_id.as_ref() != Some(recorded) {
                bail!("fork snapshot backend identity changed");
            }
        }
    } else if fork
        .status
        .as_ref()
        .is_some_and(|s| s.snapshot_id.is_some())
    {
        bail!("fork snapshot lost its backend identity");
    }
    Ok(())
}

pub(super) fn validate_child(
    fork: &BrewFSWorkspaceFork,
    snapshot_name: &str,
    child: &BrewFSWorkspace,
) -> Result<()> {
    let identity = persisted_identity(fork)?;
    validate_owner(fork, child)?;
    let owner_id = format!(
        "workspace-fork/{}/{}",
        identity.namespace, identity.fork_name
    );
    if child.name_any() != identity.workspace_name
        || child.spec.cluster_ref.name != identity.cluster_name
        || snapshot_name != identity.snapshot_name
        || child.spec.source.kind != WorkspaceSourceKind::Snapshot
        || child.spec.source.name.as_deref() != Some(snapshot_name)
        || child.spec.owner_id.as_deref() != Some(&owner_id)
    {
        bail!("fork child workspace already exists with different identity");
    }
    if let Some(recorded) = fork.status.as_ref().and_then(|s| s.workspace_id.as_ref()) {
        if child.status.as_ref().and_then(|s| s.workspace_id.as_ref()) != Some(recorded) {
            bail!("fork child workspace backend identity changed");
        }
    }
    Ok(())
}

pub(super) fn desired_storage_class(fork: &BrewFSWorkspaceFork) -> Result<StorageClass> {
    let identity = persisted_identity(fork)?;
    let workspace_id = fork
        .status
        .as_ref()
        .and_then(|s| s.workspace_id.clone())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("fork has no persisted child backend identity"))?;
    Ok(StorageClass {
        metadata: ObjectMeta {
            name: Some(identity.storage_class_name.clone()),
            // Cluster-scoped objects cannot have namespaced owner references.
            // The finalizer owns this lifecycle using the explicit UID and fork ID.
            annotations: Some(BTreeMap::from([
                (FORK_UID.into(), identity.fork_uid.clone()),
                (
                    FORK_ID.into(),
                    format!("{}/{}", identity.namespace, identity.fork_name),
                ),
            ])),
            ..Default::default()
        },
        provisioner: "csi.brewfs.io".into(),
        reclaim_policy: Some("Retain".into()),
        volume_binding_mode: Some("Immediate".into()),
        allow_volume_expansion: Some(false),
        parameters: Some(BTreeMap::from([
            ("cluster".into(), identity.cluster_name.clone()),
            ("workspace".into(), workspace_id),
            ("config".into(), "/etc/brewfs/config.yaml".into()),
            ("binary".into(), "/usr/local/bin/brewfs".into()),
            ("mount_root".into(), "/var/lib/brewfs-csi/mounts".into()),
            ("volume_format".into(), "workspace-v1".into()),
            (
                "source_workspace".into(),
                identity.source_workspace_id.clone(),
            ),
        ])),
        ..Default::default()
    })
}

pub(super) fn validate_storage_class(
    existing: &StorageClass,
    desired: &StorageClass,
) -> Result<()> {
    if existing.name_any() != desired.name_any()
        || [FORK_UID, FORK_ID].iter().any(|key| {
            desired.annotations().get(*key).is_none_or(String::is_empty)
                || existing.annotations().get(*key) != desired.annotations().get(*key)
        })
        || existing.metadata.owner_references != desired.metadata.owner_references
        || existing.provisioner != desired.provisioner
        || existing.parameters != desired.parameters
        || existing.reclaim_policy != desired.reclaim_policy
        || existing.volume_binding_mode != desired.volume_binding_mode
        || existing.allow_volume_expansion != desired.allow_volume_expansion
        || existing.mount_options != desired.mount_options
        || existing.allowed_topologies != desired.allowed_topologies
    {
        bail!(
            "fork StorageClass identity conflict: owner, parameters or provisioning policy differ"
        );
    }
    Ok(())
}

pub(super) fn delete_params(resource: &impl ResourceExt) -> Result<DeleteParams> {
    Ok(DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(uid(resource)?),
            resource_version: Some(
                resource
                    .resource_version()
                    .filter(|version| !version.is_empty())
                    .ok_or_else(|| anyhow!("{} has no resourceVersion", resource.name_any()))?,
            ),
        }),
        ..Default::default()
    })
}

fn uid(resource: &impl ResourceExt) -> Result<String> {
    resource
        .uid()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| anyhow!("{} has no Kubernetes UID", resource.name_any()))
}

pub(super) fn storage_class_name(
    namespace: &str,
    workspace: &str,
    source_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    for component in [namespace, workspace, source_identity] {
        hash.update((component.len() as u64).to_be_bytes());
        hash.update(component.as_bytes());
    }
    // 80 bits keep names compact. Ownership and complete identity checks remain
    // mandatory even if two full identities ever produce the same suffix.
    let suffix = hex::encode(&hash.finalize()[..10]);
    let mut value = format!("brewfs-{namespace}-{workspace}")
        .to_ascii_lowercase()
        .replace('_', "-");
    value.truncate(63 - suffix.len() - 1);
    format!("{}-{suffix}", value.trim_end_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::crd::*;
    use kube::{CustomResourceExt, Resource};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn fork() -> BrewFSWorkspaceFork {
        let mut fork = BrewFSWorkspaceFork::new(
            "fork",
            BrewFSWorkspaceForkSpec {
                cluster_ref: NamespacedNameRef {
                    name: "cluster".into(),
                },
                source_workspace_ref: NamespacedNameRef {
                    name: "parent".into(),
                },
                workspace_name: "child".into(),
            },
        );
        fork.metadata.namespace = Some("default".into());
        fork.metadata.uid = Some("fork-uid".into());
        fork.status = Some(BrewFSWorkspaceForkStatus {
            identity: Some(new_identity(&fork, &source()).unwrap()),
            ..Default::default()
        });
        fork
    }

    fn source() -> BrewFSWorkspace {
        let mut source = BrewFSWorkspace::new(
            "parent",
            BrewFSWorkspaceSpec {
                cluster_ref: NamespacedNameRef {
                    name: "cluster".into(),
                },
                source: WorkspaceSourceSpec {
                    kind: WorkspaceSourceKind::ClusterRoot,
                    name: None,
                },
                desired_state: WorkspaceDesiredState::Suspended,
                owner_id: None,
                deletion_policy: WorkspaceDeletionPolicy::Delete,
            },
        );
        source.metadata.uid = Some("source-uid".into());
        source.status = Some(BrewFSWorkspaceStatus {
            workspace_id: Some("source-id".into()),
            ..Default::default()
        });
        source
    }

    fn snapshot(fork: &BrewFSWorkspaceFork) -> BrewFSWorkspaceSnapshot {
        let mut snapshot = BrewFSWorkspaceSnapshot::new(
            "fork-snapshot",
            BrewFSWorkspaceSnapshotSpec {
                cluster_ref: fork.spec.cluster_ref.clone(),
                workspace_ref: fork.spec.source_workspace_ref.clone(),
            },
        );
        snapshot.metadata.owner_references = Some(vec![fork.controller_owner_ref(&()).unwrap()]);
        snapshot.status = Some(BrewFSWorkspaceSnapshotStatus {
            phase: "Ready".into(),
            source_workspace_id: Some("source-id".into()),
            snapshot_id: Some("snapshot-id".into()),
            ..Default::default()
        });
        snapshot
    }

    fn child(fork: &BrewFSWorkspaceFork) -> BrewFSWorkspace {
        let mut child = BrewFSWorkspace::new(
            "child",
            BrewFSWorkspaceSpec {
                cluster_ref: fork.spec.cluster_ref.clone(),
                source: WorkspaceSourceSpec {
                    kind: WorkspaceSourceKind::Snapshot,
                    name: Some("fork-snapshot".into()),
                },
                desired_state: WorkspaceDesiredState::Active,
                owner_id: Some("workspace-fork/default/fork".into()),
                deletion_policy: WorkspaceDeletionPolicy::Delete,
            },
        );
        child.metadata.owner_references = Some(vec![fork.controller_owner_ref(&()).unwrap()]);
        child
    }

    fn storage_class() -> StorageClass {
        serde_json::from_value(json!({
            "metadata": {
                "name": "brewfs-default-child",
                "uid": "storage-uid",
                "resourceVersion": "42",
                "annotations": {
                    "storage.brewfs.io/fork-uid": "fork-uid",
                    "storage.brewfs.io/fork-id": "default/fork"
                }
            },
            "provisioner": "csi.brewfs.io",
            "reclaimPolicy": "Retain",
            "volumeBindingMode": "Immediate",
            "allowVolumeExpansion": false,
            "parameters": {
                "cluster": "cluster", "workspace": "child-id",
                "source_workspace": "source-id", "volume_format": "workspace-v1",
                "config": "/etc/brewfs/config.yaml", "binary": "/usr/local/bin/brewfs",
                "mount_root": "/var/lib/brewfs-csi/mounts"
            }
        }))
        .unwrap()
    }

    #[test]
    fn fork_regression_204_schema_freezes_spec() {
        let crd = serde_json::to_value(BrewFSWorkspaceFork::crd()).unwrap();
        let schema = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"];
        assert!(schema["x-kubernetes-validations"]
            .as_array()
            .is_some_and(|rules| rules
                .iter()
                .any(|r| r["rule"] == "self.spec == oldSelf.spec")));
        assert_eq!(
            schema["properties"]["spec"]["required"],
            json!(["clusterRef", "sourceWorkspaceRef", "workspaceName"])
        );
    }

    #[test]
    fn fork_regression_204_schema_prevents_clearing_recorded_identity() {
        let crd = serde_json::to_value(BrewFSWorkspaceFork::crd()).unwrap();
        let schema = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"];
        for field in ["identity", "workspaceId", "snapshotId", "storageClassName"] {
            let expected = format!("!has(oldSelf.status) || !has(oldSelf.status.{field}) || (has(self.status) && has(self.status.{field}) && self.status.{field} == oldSelf.status.{field})");
            assert!(
                schema["x-kubernetes-validations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|rule| rule["rule"] == expected),
                "{field}"
            );
        }
        let identity = &schema["properties"]["status"]["properties"]["identity"];
        assert_eq!(identity["type"], "object");
        assert_eq!(identity["required"].as_array().unwrap().len(), 10);
        for property in identity["properties"].as_object().unwrap().values() {
            assert!(property["maxLength"].as_u64().is_some());
        }
    }

    #[test]
    fn fork_regression_204_cleanup_uses_recorded_identity_after_spec_edit() {
        let mut fork = fork();
        fork.status.as_mut().unwrap().workspace_id = Some("child-id".into());
        let original = persisted_identity(&fork).unwrap().clone();
        fork.spec.cluster_ref.name = "other-cluster".into();
        fork.spec.source_workspace_ref.name = "other-source".into();
        fork.spec.workspace_name = "other-child".into();
        assert!(validate_source(&fork, &source()).is_err());
        assert_eq!(persisted_identity(&fork).unwrap(), &original);
        let snapshot = desired_snapshot(&fork).unwrap();
        assert_eq!(snapshot.name_any(), original.snapshot_name);
        assert_eq!(snapshot.spec.workspace_ref.name, "parent");
        assert_eq!(snapshot.spec.cluster_ref.name, "cluster");
        let class = desired_storage_class(&fork).unwrap();
        assert_eq!(class.name_any(), original.storage_class_name);
        assert_eq!(class.parameters.as_ref().unwrap()["cluster"], "cluster");
        assert_eq!(
            class.parameters.as_ref().unwrap()["source_workspace"],
            "source-id"
        );
        fork.status.as_mut().unwrap().identity = None;
        assert!(persisted_identity(&fork).is_err());
    }

    #[test]
    fn fork_regression_204_source_recreation_cannot_redirect_fork() {
        let fork = fork();
        let mut source = source();
        validate_source(&fork, &source).unwrap();
        source.metadata.uid = Some("recreated-parent".into());
        assert!(validate_source(&fork, &source).is_err());
        source.metadata.uid = Some("source-uid".into());
        source.status.as_mut().unwrap().workspace_id = Some("different-backend".into());
        assert!(validate_source(&fork, &source).is_err());
    }

    #[test]
    fn fork_regression_204_pending_status_preserves_cleanup_identity() {
        let mut fork = fork();
        fork.status = Some(
            updated_status(
                &fork,
                "Ready",
                "ready",
                Some("child-id".into()),
                Some("storage-class".into()),
                Some("snapshot-id".into()),
            )
            .unwrap(),
        );
        let pending = updated_status(&fork, "Pending", "retry", None, None, None).unwrap();
        let ready = fork.status.as_ref().unwrap();
        assert_eq!(pending.identity, ready.identity);
        assert_eq!(pending.workspace_id, ready.workspace_id);
        assert_eq!(pending.snapshot_id, ready.snapshot_id);
        assert_eq!(pending.storage_class_name, ready.storage_class_name);
        assert!(updated_status(
            &fork,
            "Ready",
            "changed",
            Some("other-child".into()),
            None,
            None
        )
        .is_err());
        assert!(updated_status(
            &fork,
            "Ready",
            "changed",
            None,
            Some("other-class".into()),
            None
        )
        .is_err());
        assert!(updated_status(
            &fork,
            "Ready",
            "changed",
            None,
            None,
            Some("other-snapshot".into())
        )
        .is_err());
    }

    #[test]
    fn fork_regression_204_created_snapshot_has_controller_owner() {
        let fork = fork();
        let snapshot = desired_snapshot(&fork).unwrap();
        assert_eq!(
            snapshot.owner_references(),
            &[fork.controller_owner_ref(&()).unwrap()]
        );
        validate_snapshot(&fork, &snapshot, "source-id").unwrap();
    }

    #[test]
    fn fork_regression_204_snapshot_rejects_foreign_owner_and_source() {
        let fork = fork();
        let expected = snapshot(&fork);
        validate_snapshot(&fork, &expected, "source-id").unwrap();
        let mut foreign = expected.clone();
        foreign.metadata.owner_references = None;
        assert!(validate_snapshot(&fork, &foreign, "source-id").is_err());
        foreign = expected.clone();
        foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "other-fork".into();
        assert!(validate_snapshot(&fork, &foreign, "source-id").is_err());
        foreign = expected;
        foreign.status.as_mut().unwrap().source_workspace_id = Some("other-source".into());
        assert!(validate_snapshot(&fork, &foreign, "source-id").is_err());
    }

    #[test]
    fn fork_regression_204_child_rejects_foreign_identity() {
        let fork = fork();
        let expected = child(&fork);
        validate_child(&fork, "fork-snapshot", &expected).unwrap();
        for field in [
            "owner",
            "owner-uid",
            "owner-kind",
            "cluster",
            "source",
            "source-kind",
            "owner-id",
        ] {
            let mut foreign = expected.clone();
            match field {
                "owner" => foreign.metadata.owner_references = None,
                "owner-uid" => {
                    foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "foreign".into()
                }
                "owner-kind" => {
                    foreign.metadata.owner_references.as_mut().unwrap()[0].kind = "OtherKind".into()
                }
                "cluster" => foreign.spec.cluster_ref.name = "other".into(),
                "source" => foreign.spec.source.name = Some("other-snapshot".into()),
                "source-kind" => foreign.spec.source.kind = WorkspaceSourceKind::ClusterRoot,
                "owner-id" => foreign.spec.owner_id = Some("other-owner".into()),
                _ => unreachable!(),
            }
            assert!(
                validate_child(&fork, "fork-snapshot", &foreign).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn fork_regression_204_recorded_backend_ids_cannot_be_replaced() {
        let mut fork = fork();
        fork.status.as_mut().unwrap().snapshot_id = Some("recorded-snapshot".into());
        fork.status.as_mut().unwrap().workspace_id = Some("recorded-child".into());
        let mut snapshot = snapshot(&fork);
        assert!(validate_snapshot(&fork, &snapshot, "source-id").is_err());
        snapshot.status.as_mut().unwrap().snapshot_id = Some("recorded-snapshot".into());
        validate_snapshot(&fork, &snapshot, "source-id").unwrap();
        snapshot.status = None;
        assert!(validate_snapshot(&fork, &snapshot, "source-id").is_err());
        let mut child = child(&fork);
        assert!(validate_child(&fork, "fork-snapshot", &child).is_err());
        child.status = Some(BrewFSWorkspaceStatus {
            workspace_id: Some("recorded-child".into()),
            ..Default::default()
        });
        validate_child(&fork, "fork-snapshot", &child).unwrap();
        child.status.as_mut().unwrap().workspace_id = Some("replaced-child".into());
        assert!(validate_child(&fork, "fork-snapshot", &child).is_err());
    }

    #[test]
    fn fork_regression_205_storage_class_requires_owner_and_all_parameters() {
        let desired = storage_class();
        validate_storage_class(&desired, &desired).unwrap();
        for key in ["storage.brewfs.io/fork-uid", "storage.brewfs.io/fork-id"] {
            let mut foreign = desired.clone();
            foreign
                .metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(key.into(), "other".into());
            assert!(validate_storage_class(&foreign, &desired).is_err(), "{key}");
        }
        for key in desired.parameters.as_ref().unwrap().keys() {
            let mut foreign = desired.clone();
            foreign
                .parameters
                .as_mut()
                .unwrap()
                .insert(key.clone(), "other".into());
            assert!(validate_storage_class(&foreign, &desired).is_err(), "{key}");
        }
        let mut foreign = desired.clone();
        foreign
            .parameters
            .as_mut()
            .unwrap()
            .insert("extra".into(), "other".into());
        assert!(validate_storage_class(&foreign, &desired).is_err());
        foreign.metadata.annotations = Some(BTreeMap::new());
        assert!(validate_storage_class(&foreign, &desired).is_err());
    }

    #[test]
    fn fork_regression_205_delete_is_bound_to_observed_uid() {
        let mut existing = storage_class();
        let wire = serde_json::to_value(delete_params(&existing).unwrap()).unwrap();
        assert_eq!(wire["preconditions"]["uid"], "storage-uid");
        assert_eq!(wire["preconditions"]["resourceVersion"], "42");
        existing.metadata.uid = None;
        assert!(delete_params(&existing).is_err());
        existing.metadata.uid = Some("storage-uid".into());
        existing.metadata.resource_version = None;
        assert!(delete_params(&existing).is_err());
    }

    #[test]
    fn fork_regression_205_storage_class_rejects_policy_changes() {
        let desired = storage_class();
        for field in [
            "provisioner",
            "reclaim",
            "binding",
            "expansion",
            "mount-options",
            "owner",
        ] {
            let mut foreign = desired.clone();
            match field {
                "provisioner" => foreign.provisioner = "other.csi.io".into(),
                "reclaim" => foreign.reclaim_policy = Some("Delete".into()),
                "binding" => foreign.volume_binding_mode = Some("WaitForFirstConsumer".into()),
                "expansion" => foreign.allow_volume_expansion = Some(true),
                "mount-options" => foreign.mount_options = Some(vec!["ro".into()]),
                "owner" => {
                    foreign.metadata.owner_references =
                        Some(vec![fork().controller_owner_ref(&()).unwrap()])
                }
                _ => unreachable!(),
            }
            assert!(
                validate_storage_class(&foreign, &desired).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn fork_regression_205_cluster_scoped_class_has_explicit_fork_identity() {
        let mut fork = fork();
        assert!(desired_storage_class(&fork).is_err());
        fork.status.as_mut().unwrap().workspace_id = Some("child-id".into());
        let class = desired_storage_class(&fork).unwrap();
        assert!(class.owner_references().is_empty());
        assert_eq!(class.annotations()[FORK_UID], "fork-uid");
        assert_eq!(class.annotations()[FORK_ID], "default/fork");
        validate_storage_class(&class, &class).unwrap();
        let mut foreign = class.clone();
        foreign.metadata.annotations = None;
        assert!(validate_storage_class(&foreign, &class).is_err());
        assert!(validate_storage_class(&foreign, &foreign).is_err());
    }

    #[test]
    fn fork_regression_206_adversarial_names_do_not_collide() {
        let a = format!("{}x", "a".repeat(48));
        let b = format!("{}y", "a".repeat(48));
        let first = storage_class_name("default", &a, "fork-uid/source-id");
        let second = storage_class_name("default", &b, "fork-uid/source-id");
        assert_ne!(first, second);
        assert_eq!(
            first,
            storage_class_name("default", &a, "fork-uid/source-id")
        );
        assert_ne!(
            first,
            storage_class_name("default", &a, "new-fork/source-id")
        );
        for name in [first, second] {
            assert!(name.len() <= 63);
            assert!(name.starts_with("brewfs-default-"));
            assert!(name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
            assert!(!name.ends_with('-'));
        }
    }

    #[test]
    fn fork_regression_206_hash_covers_full_source_identity() {
        let fork = fork();
        let source = source();
        let original = new_identity(&fork, &source).unwrap().storage_class_name;
        for field in [
            "namespace",
            "fork-name",
            "fork-uid",
            "cluster",
            "source-name",
            "source-uid",
            "source-id",
        ] {
            let mut fork = fork.clone();
            let mut source = source.clone();
            match field {
                "namespace" => fork.metadata.namespace = Some("another".into()),
                "fork-name" => fork.metadata.name = Some("another".into()),
                "fork-uid" => fork.metadata.uid = Some("another".into()),
                "cluster" => fork.spec.cluster_ref.name = "another".into(),
                "source-name" => fork.spec.source_workspace_ref.name = "another".into(),
                "source-uid" => source.metadata.uid = Some("another".into()),
                "source-id" => {
                    source.status.as_mut().unwrap().workspace_id = Some("another".into())
                }
                _ => unreachable!(),
            }
            assert_ne!(
                original,
                new_identity(&fork, &source).unwrap().storage_class_name,
                "{field}"
            );
        }
        assert_ne!(
            storage_class_name("a-b", "c", "id"),
            storage_class_name("a", "b-c", "id")
        );
    }
}

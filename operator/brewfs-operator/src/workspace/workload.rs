use std::collections::BTreeMap;

use anyhow::{anyhow, Context as _};
use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMap, Container, ContainerPort, EmptyDirVolumeSource, EnvVar, EnvVarSource,
    ExecAction, HostPathVolumeSource, PersistentVolumeClaim, PersistentVolumeClaimSpec,
    PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec, Probe, Secret, SecretKeySelector,
    SecurityContext, Service, ServicePort, ServiceSpec, Volume, VolumeMount,
    VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::{Resource, ResourceExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;

use crate::crd::BrewFSCluster;
use crate::reconciler::{consumer_extra_volumes, render_consumer_container};

use super::admin::{catalog_namespace, revision_to_status, WorkspaceView};
use super::crd::{
    BrewFSWorkspace, BrewFSWorkspaceMount, BrewFSWorkspaceMountStatus, BrewFSWorkspaceStatus,
    WorkspaceCacheMode, WorkspaceClusterSpec,
};

const WORKSPACE_VOLUME: &str = "workspace-mount";
const CACHE_VOLUME: &str = "workspace-cache";
const CONFIG_VOLUME: &str = "workspace-config";
const FUSE_VOLUME: &str = "fuse-device";
const CATALOG_PASSWORD_KEY: &str = "password";

pub async fn reconcile_workspace_catalog(
    client: &kube::Client,
    namespace: &str,
    cluster: &BrewFSCluster,
    spec: &WorkspaceClusterSpec,
) -> anyhow::Result<()> {
    if spec.catalog_backend != super::crd::WorkspaceCatalogBackend::Redis {
        return Ok(());
    }
    let owner = cluster
        .controller_owner_ref(&())
        .ok_or_else(|| anyhow!("BrewFSCluster has no owner reference identity"))?;
    let name = workspace_catalog_name(&cluster.name_any());
    let secret_name = workspace_catalog_secret_name(&cluster.name_any());
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    if secrets.get_opt(&secret_name).await?.is_none() {
        let secret = Secret {
            metadata: owned_meta(
                &secret_name,
                &owner,
                workspace_catalog_labels(&cluster.name_any()),
            ),
            string_data: Some(BTreeMap::from([(
                CATALOG_PASSWORD_KEY.into(),
                uuid::Uuid::new_v4().simple().to_string(),
            )])),
            type_: Some("Opaque".into()),
            ..Secret::default()
        };
        match secrets.create(&PostParams::default(), &secret).await {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 409 => {}
            Err(error) => return Err(error.into()),
        }
    }

    let labels = workspace_catalog_labels(&cluster.name_any());
    let data_claim_name = format!("{name}-data");
    let data_claim = PersistentVolumeClaim {
        metadata: owned_meta(&data_claim_name, &owner, labels.clone()),
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".into()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".into(),
                    Quantity(spec.catalog_storage_size.clone()),
                )])),
                ..VolumeResourceRequirements::default()
            }),
            ..PersistentVolumeClaimSpec::default()
        }),
        ..PersistentVolumeClaim::default()
    };
    apply(
        &Api::namespaced(client.clone(), namespace),
        &data_claim_name,
        &data_claim,
    )
    .await?;
    let service = Service {
        metadata: owned_meta(&name, &owner, labels.clone()),
        spec: Some(ServiceSpec {
            selector: Some(labels.clone()),
            ports: Some(vec![ServicePort {
                name: Some("redis".into()),
                port: cluster.spec.redis.port,
                target_port: Some(
                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                        cluster.spec.redis.port,
                    ),
                ),
                ..ServicePort::default()
            }]),
            ..ServiceSpec::default()
        }),
        ..Service::default()
    };
    apply(&Api::namespaced(client.clone(), namespace), &name, &service).await?;

    let password_env = EnvVar {
        name: "REDIS_PASSWORD".into(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret_name,
                key: CATALOG_PASSWORD_KEY.into(),
                ..SecretKeySelector::default()
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    };
    let stateful_set = StatefulSet {
        metadata: owned_meta(&name, &owner, labels.clone()),
        spec: Some(StatefulSetSpec {
            service_name: name.clone(),
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..LabelSelector::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    containers: vec![Container {
                        name: "redis".into(),
                        image: Some(cluster.spec.redis.image.clone()),
                        command: Some(vec!["/bin/sh".into(), "-ec".into()]),
                        args: Some(vec!["exec redis-server --dir /data --appendonly yes --appendfsync everysec --requirepass \"$REDIS_PASSWORD\"".into()]),
                        env: Some(vec![password_env]),
                        ports: Some(vec![ContainerPort {
                            name: Some("redis".into()),
                            container_port: cluster.spec.redis.port,
                            ..ContainerPort::default()
                        }]),
                        readiness_probe: Some(Probe {
                            exec: Some(ExecAction {
                                command: Some(vec![
                                    "/bin/sh".into(),
                                    "-ec".into(),
                                    "redis-cli -a \"$REDIS_PASSWORD\" ping | grep -qx PONG".into(),
                                ]),
                            }),
                            period_seconds: Some(3),
                            ..Probe::default()
                        }),
                        volume_mounts: Some(vec![VolumeMount {
                            name: "catalog-data".into(),
                            mount_path: "/data".into(),
                            ..VolumeMount::default()
                        }]),
                        ..Container::default()
                    }],
                    volumes: Some(vec![Volume {
                        name: "catalog-data".into(),
                        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                            claim_name: data_claim_name,
                            read_only: Some(false),
                        }),
                        ..Volume::default()
                    }]),
                    ..PodSpec::default()
                }),
            },
            ..StatefulSetSpec::default()
        }),
        ..StatefulSet::default()
    };
    apply(
        &Api::namespaced(client.clone(), namespace),
        &name,
        &stateful_set,
    )
    .await
}

pub async fn load_workspace_catalog_password(
    client: &kube::Client,
    namespace: &str,
    cluster: &BrewFSCluster,
    spec: &WorkspaceClusterSpec,
) -> anyhow::Result<Option<String>> {
    if spec.catalog_backend != super::crd::WorkspaceCatalogBackend::Redis {
        return Ok(None);
    }
    let secret_name = workspace_catalog_secret_name(&cluster.name_any());
    let secret = Api::<Secret>::namespaced(client.clone(), namespace)
        .get(&secret_name)
        .await
        .with_context(|| format!("load workspace catalog Secret {secret_name}"))?;
    let bytes = secret
        .data
        .as_ref()
        .and_then(|data| data.get(CATALOG_PASSWORD_KEY))
        .ok_or_else(|| anyhow!("workspace catalog Secret has no password"))?;
    Ok(Some(
        String::from_utf8(bytes.0.clone()).context("workspace catalog password is not UTF-8")?,
    ))
}

pub async fn reconcile_workspace_mount_workload(
    client: &kube::Client,
    namespace: &str,
    mount: &BrewFSWorkspaceMount,
    workspace: &BrewFSWorkspace,
    cluster: &BrewFSCluster,
    cluster_spec: &WorkspaceClusterSpec,
) -> anyhow::Result<()> {
    let owner = mount
        .controller_owner_ref(&())
        .ok_or_else(|| anyhow!("BrewFSWorkspaceMount has no owner reference identity"))?;
    let workspace_status = workspace
        .status
        .as_ref()
        .ok_or_else(|| anyhow!("workspace status is missing"))?;
    let config_name = format!("{}-workspace-config", mount.name_any());
    let cache_name = format!("{}-workspace-cache", mount.name_any());
    let workload_name = format!("{}-workspace", mount.name_any());
    let labels = workload_labels(&mount.name_any());

    apply_workspace_config(
        client,
        namespace,
        &config_name,
        mount,
        workspace_status,
        cluster,
        cluster_spec,
        &owner,
    )
    .await?;
    if mount.spec.cache.mode == WorkspaceCacheMode::WriteBack {
        apply_cache_pvc(client, namespace, &cache_name, mount, &owner).await?;
    }
    apply_headless_service(client, namespace, &workload_name, labels.clone(), &owner).await?;
    let desired = build_stateful_set(
        &workload_name,
        &config_name,
        &cache_name,
        labels,
        mount,
        cluster,
        &owner,
    );
    let sets: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);
    apply(&sets, &workload_name, &desired).await?;
    let observed = sets.get(&workload_name).await?;
    let ready = observed
        .status
        .as_ref()
        .and_then(|status| status.ready_replicas)
        .unwrap_or_default()
        >= 1;
    patch_mount_status(
        &Api::namespaced(client.clone(), namespace),
        mount,
        if ready { "Mounted" } else { "Starting" },
        if ready {
            "workspace mount and agent pod are ready"
        } else {
            "waiting for the workspace FUSE sidecar readiness gate"
        },
        Some(workspace_status),
    )
    .await
}

pub async fn delete_workspace_mount_workload(
    client: &kube::Client,
    namespace: &str,
    mount: &BrewFSWorkspaceMount,
) -> anyhow::Result<()> {
    let workload_name = format!("{}-workspace", mount.name_any());
    let sets: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);
    if let Some(mut set) = sets.get_opt(&workload_name).await? {
        if set.spec.as_ref().and_then(|spec| spec.replicas) != Some(0) {
            set.spec.as_mut().expect("StatefulSet has spec").replicas = Some(0);
            sets.patch(
                &workload_name,
                &PatchParams::apply("brewfs-workspace-operator").force(),
                &Patch::Apply(&set),
            )
            .await?;
            return Err(anyhow!(
                "waiting for workspace mount pod to terminate cleanly"
            ));
        }
        if set
            .status
            .as_ref()
            .and_then(|status| status.current_replicas)
            .unwrap_or_default()
            > 0
        {
            return Err(anyhow!(
                "waiting for workspace mount pod to terminate cleanly"
            ));
        }
        sets.delete(&workload_name, &DeleteParams::default())
            .await?;
        return Err(anyhow!("waiting for workspace StatefulSet deletion"));
    }

    for name in [
        format!("{}-workspace-config", mount.name_any()),
        workload_name.clone(),
    ] {
        let configs: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
        if configs.get_opt(&name).await?.is_some() {
            configs.delete(&name, &DeleteParams::default()).await?;
        }
        let services: Api<Service> = Api::namespaced(client.clone(), namespace);
        if services.get_opt(&name).await?.is_some() {
            services.delete(&name, &DeleteParams::default()).await?;
        }
    }
    if mount.spec.cache.reclaim_policy == super::crd::WorkspaceCacheReclaimPolicy::Delete {
        let pvcs: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), namespace);
        let cache_name = format!("{}-workspace-cache", mount.name_any());
        if pvcs.get_opt(&cache_name).await?.is_some() {
            pvcs.delete(&cache_name, &DeleteParams::default()).await?;
        }
    }
    Ok(())
}

pub async fn patch_mount_status(
    api: &Api<BrewFSWorkspaceMount>,
    mount: &BrewFSWorkspaceMount,
    phase: &str,
    message: &str,
    workspace: Option<&BrewFSWorkspaceStatus>,
) -> anyhow::Result<()> {
    let status = BrewFSWorkspaceMountStatus {
        observed_generation: mount.metadata.generation,
        phase: phase.into(),
        message: message.into(),
        pod_name: Some(format!("{}-workspace-0", mount.name_any())),
        pod_uid: None,
        workspace_id: workspace.and_then(|status| status.workspace_id.clone()),
        lease_id: None,
        holder_generation: None,
        mounted_head_layer_id: workspace.and_then(|status| status.head_layer_id.clone()),
        mounted_head_epoch: workspace.and_then(|status| status.head_epoch),
        mounted_base_revision: workspace.and_then(|status| status.current_base_revision.clone()),
        conditions: Vec::new(),
    };
    if mount.status.as_ref() == Some(&status) {
        return Ok(());
    }
    api.patch_status(
        &mount.name_any(),
        &PatchParams::apply("brewfs-workspace-operator").force(),
        &Patch::Apply(json!({
            "apiVersion": "storage.brewfs.io/v1alpha1",
            "kind": "BrewFSWorkspaceMount",
            "status": status,
        })),
    )
    .await?;
    Ok(())
}

pub async fn patch_mount_backend_status(
    api: &Api<BrewFSWorkspaceMount>,
    mount: &BrewFSWorkspaceMount,
    backend: &WorkspaceView,
) -> anyhow::Result<()> {
    let active = backend
        .leases
        .iter()
        .filter(|lease| lease.state == brewfs::workspace_overlay::model::LeaseState::Active)
        .max_by_key(|lease| lease.created_at_ns);
    let status = BrewFSWorkspaceMountStatus {
        observed_generation: mount.metadata.generation,
        phase: if active.is_some() {
            "Mounted"
        } else {
            "Starting"
        }
        .into(),
        message: if active.is_some() {
            "workspace mount lease is active"
        } else {
            "waiting for the FUSE sidecar to acquire its writable lease"
        }
        .into(),
        pod_name: Some(format!("{}-workspace-0", mount.name_any())),
        pod_uid: None,
        workspace_id: Some(backend.record.workspace_id.to_string()),
        lease_id: active.map(|lease| lease.lease_id.to_string()),
        holder_generation: active.map(|lease| lease.holder_generation),
        mounted_head_layer_id: Some(backend.record.head_layer_id.to_string()),
        mounted_head_epoch: Some(backend.record.head_epoch),
        mounted_base_revision: Some(revision_to_status(&backend.base_revision)),
        conditions: Vec::new(),
    };
    if mount.status.as_ref() == Some(&status) {
        return Ok(());
    }
    api.patch_status(
        &mount.name_any(),
        &PatchParams::apply("brewfs-workspace-operator").force(),
        &Patch::Apply(json!({
            "apiVersion": "storage.brewfs.io/v1alpha1",
            "kind": "BrewFSWorkspaceMount",
            "status": status,
        })),
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_workspace_config(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    mount: &BrewFSWorkspaceMount,
    workspace: &BrewFSWorkspaceStatus,
    cluster: &BrewFSCluster,
    cluster_spec: &WorkspaceClusterSpec,
    owner: &OwnerReference,
) -> anyhow::Result<()> {
    let workspace_id = workspace
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow!("workspace status has no workspaceId"))?;
    let meta = match cluster_spec.catalog_backend {
        super::crd::WorkspaceCatalogBackend::Redis => format!(
            "meta:\n  backend: redis\n  redis:\n    url: \"redis://{}-workspace-redis:{}\"\n",
            cluster.name_any(),
            cluster.spec.redis.port
        ),
        super::crd::WorkspaceCatalogBackend::TiKv => {
            let endpoints = cluster_spec
                .tikv_pd_endpoints
                .iter()
                .map(|endpoint| format!("      - {endpoint}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "meta:\n  backend: tikv\n  tikv:\n    pd_endpoints:\n{endpoints}\n    namespace: {}\n",
                catalog_namespace(&cluster.name_any(), namespace, cluster_spec)
            )
        }
    };
    let writeback_mode = match mount.spec.cache.mode {
        WorkspaceCacheMode::WriteThrough => "upload_before_commit",
        WorkspaceCacheMode::WriteBack => "commit_before_upload",
    };
    let config = format!(
        "mount_point: {mount_path}\nvolume_format: workspace-v1\nworkspace: {workspace_id}\nworkspace_namespace: {catalog_namespace}\ndata:\n  backend: s3\n  s3:\n    bucket: {bucket}\n    region: {region}\n    part_size: {part_size}\n    max_concurrency: {max_concurrency}\n    force_path_style: {force_path_style}\n    endpoint: http://{cluster_name}-rustfs:{rustfs_port}\n{meta}layout:\n  chunk_size: {chunk_size}\n  block_size: {block_size}\ncache:\n  cache_root: /var/lib/brewfs/cache\n  writeback_mode: {writeback_mode}\n  writeback_persist_sync: true\n  writeback_require_stage_before_commit: true\n",
        mount_path = mount.spec.mount_path,
        workspace_id = workspace_id,
        catalog_namespace = catalog_namespace(&cluster.name_any(), namespace, cluster_spec),
        bucket = cluster.spec.rustfs.bucket,
        region = cluster.spec.rustfs.region,
        part_size = cluster.spec.mount_config.part_size,
        max_concurrency = cluster.spec.mount_config.max_concurrency,
        force_path_style = cluster.spec.mount_config.force_path_style,
        cluster_name = cluster.name_any(),
        rustfs_port = cluster.spec.rustfs.port,
        meta = meta,
        chunk_size = cluster.spec.mount_config.chunk_size,
        block_size = cluster.spec.mount_config.block_size,
        writeback_mode = writeback_mode,
    );
    let desired = ConfigMap {
        metadata: owned_meta(name, owner, workload_labels(&mount.name_any())),
        data: Some(BTreeMap::from([("config.yaml".into(), config)])),
        ..ConfigMap::default()
    };
    apply(&Api::namespaced(client.clone(), namespace), name, &desired).await
}

async fn apply_cache_pvc(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    mount: &BrewFSWorkspaceMount,
    owner: &OwnerReference,
) -> anyhow::Result<()> {
    let mut metadata = owned_meta(name, owner, workload_labels(&mount.name_any()));
    if mount.spec.cache.reclaim_policy == super::crd::WorkspaceCacheReclaimPolicy::Retain {
        metadata.owner_references = None;
    }
    let desired = PersistentVolumeClaim {
        metadata,
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".into()]),
            storage_class_name: mount.spec.cache.storage_class_name.clone(),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".into(),
                    Quantity(mount.spec.cache.size.clone()),
                )])),
                ..VolumeResourceRequirements::default()
            }),
            ..PersistentVolumeClaimSpec::default()
        }),
        ..PersistentVolumeClaim::default()
    };
    apply(&Api::namespaced(client.clone(), namespace), name, &desired).await
}

async fn apply_headless_service(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    labels: BTreeMap<String, String>,
    owner: &OwnerReference,
) -> anyhow::Result<()> {
    let desired = Service {
        metadata: owned_meta(name, owner, labels.clone()),
        spec: Some(ServiceSpec {
            cluster_ip: Some("None".into()),
            publish_not_ready_addresses: Some(true),
            selector: Some(labels),
            ..ServiceSpec::default()
        }),
        ..Service::default()
    };
    apply(&Api::namespaced(client.clone(), namespace), name, &desired).await
}

fn build_stateful_set(
    name: &str,
    config_name: &str,
    cache_name: &str,
    labels: BTreeMap<String, String>,
    mount: &BrewFSWorkspaceMount,
    cluster: &BrewFSCluster,
    owner: &OwnerReference,
) -> StatefulSet {
    let mut pod_labels = mount
        .spec
        .agent
        .as_ref()
        .map(|agent| agent.labels.clone())
        .unwrap_or_default();
    pod_labels.extend(labels.clone());
    let mut volumes = vec![
        Volume {
            name: WORKSPACE_VOLUME.into(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Volume::default()
        },
        Volume {
            name: CONFIG_VOLUME.into(),
            config_map: Some(k8s_openapi::api::core::v1::ConfigMapVolumeSource {
                name: config_name.into(),
                ..Default::default()
            }),
            ..Volume::default()
        },
        Volume {
            name: FUSE_VOLUME.into(),
            host_path: Some(HostPathVolumeSource {
                path: "/dev/fuse".into(),
                type_: Some("CharDevice".into()),
            }),
            ..Volume::default()
        },
    ];
    if mount.spec.cache.mode == WorkspaceCacheMode::WriteBack {
        volumes.push(Volume {
            name: CACHE_VOLUME.into(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: cache_name.into(),
                read_only: Some(false),
            }),
            ..Volume::default()
        });
    } else {
        volumes.push(Volume {
            name: CACHE_VOLUME.into(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Volume::default()
        });
    }
    if let Some(agent) = &mount.spec.agent {
        volumes.extend(consumer_extra_volumes(&agent.volumes));
    }

    let catalog_override = match cluster
        .spec
        .workspace
        .as_ref()
        .map(|spec| spec.catalog_backend)
    {
        Some(super::crd::WorkspaceCatalogBackend::Redis) => format!(
            " --meta-url \"redis://:${{REDIS_PASSWORD}}@{}-workspace-redis:{}/\"",
            cluster.name_any(),
            cluster.spec.redis.port
        ),
        _ => String::new(),
    };
    let mut sidecar_env = rustfs_env(cluster);
    if cluster
        .spec
        .workspace
        .as_ref()
        .is_some_and(|spec| spec.catalog_backend == super::crd::WorkspaceCatalogBackend::Redis)
    {
        sidecar_env.push(EnvVar {
            name: "REDIS_PASSWORD".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: workspace_catalog_secret_name(&cluster.name_any()),
                    key: CATALOG_PASSWORD_KEY.into(),
                    ..SecretKeySelector::default()
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        });
    }
    let mount_sidecar = Container {
        name: "brewfs-mount".into(),
        image: Some(mount.spec.image.clone()),
        image_pull_policy: Some(mount.spec.image_pull_policy.clone()),
        command: Some(vec!["/bin/sh".into(), "-ec".into()]),
        args: Some(vec![format!(
            "mkdir -p {path} /var/lib/brewfs/cache && exec /usr/local/bin/brewfs mount --privileged --workspace-operator-managed --config /run/brewfs/config.yaml{catalog_override} {path}",
            path = mount.spec.mount_path,
        )]),
        env: Some(sidecar_env),
        restart_policy: Some("Always".into()),
        security_context: Some(SecurityContext {
            privileged: Some(true),
            allow_privilege_escalation: Some(true),
            capabilities: Some(Capabilities {
                add: Some(vec!["SYS_ADMIN".into()]),
                ..Capabilities::default()
            }),
            ..SecurityContext::default()
        }),
        readiness_probe: Some(Probe {
            exec: Some(ExecAction {
                command: Some(vec![
                    "/bin/sh".into(),
                    "-ec".into(),
                    format!("grep -qs ' {} ' /proc/mounts", mount.spec.mount_path),
                ]),
            }),
            period_seconds: Some(2),
            failure_threshold: Some(15),
            ..Probe::default()
        }),
        volume_mounts: Some(vec![
            VolumeMount {
                name: WORKSPACE_VOLUME.into(),
                mount_path: mount.spec.mount_path.clone(),
                mount_propagation: Some("Bidirectional".into()),
                ..VolumeMount::default()
            },
            VolumeMount {
                name: CACHE_VOLUME.into(),
                mount_path: "/var/lib/brewfs/cache".into(),
                ..VolumeMount::default()
            },
            VolumeMount {
                name: CONFIG_VOLUME.into(),
                mount_path: "/run/brewfs/config.yaml".into(),
                sub_path: Some("config.yaml".into()),
                read_only: Some(true),
                ..VolumeMount::default()
            },
            VolumeMount {
                name: FUSE_VOLUME.into(),
                mount_path: "/dev/fuse".into(),
                ..VolumeMount::default()
            },
        ]),
        ..Container::default()
    };

    let mut init_containers = vec![mount_sidecar];
    init_containers.push(Container {
        name: "wait-for-brewfs".into(),
        image: Some("busybox:1.36".into()),
        command: Some(vec!["/bin/sh".into(), "-ec".into()]),
        args: Some(vec![format!(
            "until grep -qs ' {} ' /proc/mounts; do sleep 1; done",
            mount.spec.mount_path
        )]),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.into(),
            mount_path: mount.spec.mount_path.clone(),
            mount_propagation: Some("HostToContainer".into()),
            ..VolumeMount::default()
        }]),
        ..Container::default()
    });
    if let Some(agent) = &mount.spec.agent {
        init_containers.extend(agent.init_containers.iter().map(|container| {
            let mut rendered =
                render_consumer_container(container, &mount.spec.mount_path, WORKSPACE_VOLUME);
            harden_agent_container(&mut rendered);
            rendered
        }));
    }
    let containers = mount
        .spec
        .agent
        .as_ref()
        .map(|agent| {
            agent
                .containers
                .iter()
                .map(|container| {
                    let mut rendered = render_consumer_container(
                        container,
                        &mount.spec.mount_path,
                        WORKSPACE_VOLUME,
                    );
                    harden_agent_container(&mut rendered);
                    rendered
                })
                .collect::<Vec<_>>()
        })
        .filter(|containers| !containers.is_empty())
        .unwrap_or_else(|| {
            vec![Container {
                name: "workspace-holder".into(),
                image: Some("busybox:1.36".into()),
                command: Some(vec!["/bin/sh".into(), "-ec".into()]),
                args: Some(vec!["sleep infinity".into()]),
                security_context: Some(unprivileged_security_context()),
                volume_mounts: Some(vec![VolumeMount {
                    name: WORKSPACE_VOLUME.into(),
                    mount_path: mount.spec.mount_path.clone(),
                    mount_propagation: Some("HostToContainer".into()),
                    ..VolumeMount::default()
                }]),
                ..Container::default()
            }]
        });

    StatefulSet {
        metadata: owned_meta(name, owner, labels.clone()),
        spec: Some(StatefulSetSpec {
            service_name: name.into(),
            replicas: Some(1),
            pod_management_policy: Some("OrderedReady".into()),
            update_strategy: Some(StatefulSetUpdateStrategy {
                type_: Some("OnDelete".into()),
                ..StatefulSetUpdateStrategy::default()
            }),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..LabelSelector::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    annotations: mount
                        .spec
                        .agent
                        .as_ref()
                        .map(|agent| agent.annotations.clone()),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    share_process_namespace: Some(false),
                    service_account_name: mount.spec.service_account_name.clone(),
                    node_selector: (!mount.spec.node_selector.is_empty())
                        .then(|| mount.spec.node_selector.clone()),
                    termination_grace_period_seconds: Some(i64::from(
                        mount.spec.termination_grace_period_seconds,
                    )),
                    init_containers: Some(init_containers),
                    containers,
                    volumes: Some(volumes),
                    ..PodSpec::default()
                }),
            },
            ..StatefulSetSpec::default()
        }),
        ..StatefulSet::default()
    }
}

fn rustfs_env(cluster: &BrewFSCluster) -> Vec<EnvVar> {
    let secret_name = format!("{}-rustfs-credentials", cluster.name_any());
    [
        ("AWS_ACCESS_KEY_ID", "accessKey"),
        ("AWS_SECRET_ACCESS_KEY", "secretKey"),
    ]
    .into_iter()
    .map(|(name, key)| EnvVar {
        name: name.into(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret_name.clone(),
                key: key.into(),
                ..SecretKeySelector::default()
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    })
    .chain(std::iter::once(EnvVar {
        name: "AWS_DEFAULT_REGION".into(),
        value: Some(cluster.spec.rustfs.region.clone()),
        ..EnvVar::default()
    }))
    .collect()
}

fn harden_agent_container(container: &mut Container) {
    container.security_context = Some(unprivileged_security_context());
}

fn unprivileged_security_context() -> SecurityContext {
    SecurityContext {
        privileged: Some(false),
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".into()]),
            ..Capabilities::default()
        }),
        ..SecurityContext::default()
    }
}

fn workload_labels(instance: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".into(), "brewfs-workspace".into()),
        ("app.kubernetes.io/instance".into(), instance.into()),
        (
            "app.kubernetes.io/managed-by".into(),
            "brewfs-operator".into(),
        ),
    ])
}

fn workspace_catalog_name(cluster_name: &str) -> String {
    format!("{cluster_name}-workspace-redis")
}

fn workspace_catalog_secret_name(cluster_name: &str) -> String {
    format!("{cluster_name}-workspace-catalog")
}

fn workspace_catalog_labels(cluster_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "app.kubernetes.io/name".into(),
            "brewfs-workspace-redis".into(),
        ),
        ("app.kubernetes.io/instance".into(), cluster_name.into()),
        (
            "app.kubernetes.io/managed-by".into(),
            "brewfs-operator".into(),
        ),
    ])
}

fn owned_meta(name: &str, owner: &OwnerReference, labels: BTreeMap<String, String>) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.into()),
        labels: Some(labels),
        owner_references: Some(vec![owner.clone()]),
        ..ObjectMeta::default()
    }
}

async fn apply<K>(api: &Api<K>, name: &str, desired: &K) -> anyhow::Result<()>
where
    K: Clone + Serialize + DeserializeOwned + Resource + Send + Sync + std::fmt::Debug,
    K::DynamicType: Default,
{
    api.patch(
        name,
        &PatchParams::apply("brewfs-workspace-operator").force(),
        &Patch::Apply(desired),
    )
    .await
    .with_context(|| format!("apply workspace resource {name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{BrewFSClusterSpec, MountConfigSpec, RedisSpec, RustFsSpec};
    use crate::workspace::crd::{BrewFSWorkspaceMountSpec, NamespacedNameRef, WorkspaceCacheSpec};

    #[test]
    fn generated_agent_has_no_backend_secret_or_privilege() {
        let mount = BrewFSWorkspaceMount::new(
            "agent",
            BrewFSWorkspaceMountSpec {
                workspace_ref: NamespacedNameRef { name: "ws".into() },
                mount_path: "/workspace".into(),
                image: "brewfs:test".into(),
                image_pull_policy: "IfNotPresent".into(),
                cache: WorkspaceCacheSpec::default(),
                agent: None,
                termination_grace_period_seconds: 90,
                service_account_name: None,
                node_selector: BTreeMap::new(),
            },
        );
        let cluster = BrewFSCluster {
            metadata: ObjectMeta {
                name: Some("demo".into()),
                ..ObjectMeta::default()
            },
            spec: BrewFSClusterSpec {
                redis: RedisSpec::default(),
                rustfs: RustFsSpec::default(),
                mount_config: MountConfigSpec::default(),
                workspace: Some(WorkspaceClusterSpec {
                    enabled: true,
                    ..WorkspaceClusterSpec::default()
                }),
            },
            status: None,
        };
        let owner = OwnerReference {
            api_version: "storage.brewfs.io/v1alpha1".into(),
            kind: "BrewFSWorkspaceMount".into(),
            name: "agent".into(),
            uid: "uid".into(),
            ..OwnerReference::default()
        };
        let set = build_stateful_set(
            "agent-workspace",
            "agent-config",
            "agent-cache",
            workload_labels("agent"),
            &mount,
            &cluster,
            &owner,
        );
        let pod = set.spec.unwrap().template.spec.unwrap();
        assert_eq!(pod.automount_service_account_token, Some(false));
        let agent = &pod.containers[0];
        assert_eq!(
            agent
                .security_context
                .as_ref()
                .and_then(|security| security.privileged),
            Some(false)
        );
        assert!(agent.env.is_none());
        let sidecar = &pod.init_containers.unwrap()[0];
        assert_eq!(sidecar.restart_policy.as_deref(), Some("Always"));
        assert!(sidecar
            .env
            .as_ref()
            .is_some_and(|env| { env.iter().any(|value| value.name == "AWS_ACCESS_KEY_ID") }));
        assert!(sidecar.env.as_ref().is_some_and(|env| {
            env.iter().any(|value| {
                value.name == "REDIS_PASSWORD"
                    && value
                        .value_from
                        .as_ref()
                        .and_then(|source| source.secret_key_ref.as_ref())
                        .is_some_and(|secret| secret.name == "demo-workspace-catalog")
            })
        }));
        let command = sidecar.args.as_ref().unwrap().join(" ");
        assert!(command.contains("--workspace-operator-managed"));
        assert!(command.contains("demo-workspace-redis"));
    }
}

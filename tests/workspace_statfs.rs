#![cfg(feature = "workspace-overlay")]

use std::sync::Arc;

use brewfs::meta::MetaLayer;
use brewfs::meta::store::DEFAULT_STATFS_AVAILABLE_INODES;
use brewfs::workspace_overlay::catalog::{AcquireLease, CreateVolumeRoot, WorkspaceStore};
use brewfs::workspace_overlay::ids::{LayerId, LeaseId, WorkspaceId};
use brewfs::workspace_overlay::meta_layer::WorkspaceMetaLayer;
use brewfs::workspace_overlay::model::{ViewContext, WORKSPACE_SCHEMA_VERSION};
use brewfs::workspace_overlay::stores::database::SqliteWorkspaceStore;
use uuid::Uuid;

#[tokio::test]
async fn statfs_reports_synthetic_one_pib_without_usage_accounting() {
    // Metadata only: no object store is needed for capacity reporting.
    let store = Arc::new(
        SqliteWorkspaceStore::connect("sqlite::memory:")
            .await
            .unwrap(),
    );
    store.initialize_workspace_schema().await.unwrap();
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(100));
    let workspace = store
        .create_volume_root(CreateVolumeRoot {
            volume_format: "workspace-v1".into(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            volume_id: Uuid::from_u128(101),
            workspace_id,
            root_layer_id: LayerId::from_uuid(Uuid::from_u128(102)),
            writable_layer_id: LayerId::from_uuid(Uuid::from_u128(103)),
            owner_id: Some("statfs-test".into()),
        })
        .await
        .unwrap();
    let lease = store
        .acquire_lease(AcquireLease {
            workspace_id,
            lease_id: LeaseId::from_uuid(Uuid::from_u128(104)),
            holder_generation: 1,
            ttl_ns: 60_000_000_000,
        })
        .await
        .unwrap();
    let meta = WorkspaceMetaLayer::new(
        store,
        ViewContext {
            workspace_id,
            head_layer_id: workspace.head_layer_id,
            head_epoch: workspace.head_epoch,
            lease_id: lease.lease_id,
            holder_generation: lease.holder_generation,
        },
    );
    for populated in [false, true] {
        if populated {
            meta.create_file(meta.root_ino(), "statfs-file".into())
                .await
                .unwrap();
        }
        let snapshot = meta.stat_fs().await.unwrap();
        assert_eq!(snapshot.total_space, 1_u64 << 50);
        assert_eq!(snapshot.available_space, 1_u64 << 50);
        // Usage is deliberately unknown, even for a populated workspace.
        assert_eq!(snapshot.used_inodes, 0);
        assert_eq!(snapshot.available_inodes, DEFAULT_STATFS_AVAILABLE_INODES);
    }
}

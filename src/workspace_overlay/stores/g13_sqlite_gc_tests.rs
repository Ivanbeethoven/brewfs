use super::*;
use crate::workspace_overlay::catalog::{
    AdvanceSeal, DeleteLayerMetadata, MarkDeleting, ReleaseLease,
};
use crate::workspace_overlay::error::WorkspaceError;
use crate::workspace_overlay::ids::JournalId;
use crate::workspace_overlay::model::{LeaseState, SealPhase, SnapshotLease, WorkspaceState};
use crate::workspace_overlay::stores::g13_gc_contract_tests::{
    BOUNDARIES, assert_delete, assert_mark,
};

async fn fixture(
    reaped: bool,
    released: bool,
) -> (
    tempfile::TempDir,
    Arc<SqliteWorkspaceStore>,
    Arc<SqliteWorkspaceStore>,
    SnapshotLease,
) {
    let temp = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", temp.path().join("gc.db").display());
    let writer = Arc::new(SqliteWorkspaceStore::connect(&url).await.unwrap());
    writer.initialize_workspace_schema().await.unwrap();
    let peer = Arc::new(SqliteWorkspaceStore::connect(&url).await.unwrap());
    let workspace = writer.create_volume_root(create_request()).await.unwrap();
    // SQLite's production lease clock is real wall time. A one-nanosecond lease
    // is expired by the next catalog call; only GC's explicit now input is swept.
    // No SQL row/state injection, fake GC result, sleep, or lease-clock claim.
    let lease = writer
        .acquire_lease(AcquireLease {
            workspace_id: workspace.workspace_id,
            lease_id: LeaseId::from_uuid(id(91_005)),
            holder_generation: 1,
            ttl_ns: 1,
        })
        .await
        .unwrap();
    if released {
        writer
            .release_lease(ReleaseLease {
                lease_id: lease.lease_id,
                holder_generation: lease.holder_generation,
            })
            .await
            .unwrap();
    }
    writer
        .mark_workspace_deleting(MarkDeleting {
            workspace_id: workspace.workspace_id,
            force_fence_lease: false,
        })
        .await
        .unwrap();
    assert_eq!(
        peer.load_workspace(workspace.workspace_id)
            .await
            .unwrap()
            .state,
        WorkspaceState::Deleting
    );
    if reaped {
        assert_eq!(peer.reap_expired_leases().await.unwrap(), 1);
    }
    let mut actual = peer.list_leases(workspace.workspace_id).await.unwrap();
    assert_eq!(actual.len(), 1);
    let actual = actual.pop().unwrap();
    assert_eq!(actual.expires_at_ns, lease.expires_at_ns);
    assert_eq!(
        actual.state,
        if released {
            LeaseState::Released
        } else if reaped {
            LeaseState::Expired
        } else {
            LeaseState::Active
        }
    );
    (temp, writer, peer, actual)
}

#[tokio::test]
async fn g13a_sqlite_mark_before_and_after_reap_has_identical_grace_boundaries() {
    let (_temp, _writer, peer, lease) = fixture(false, false).await;
    for elapsed in BOUNDARIES {
        assert_mark(peer.as_ref(), &lease, elapsed).await;
    }
    assert_eq!(peer.reap_expired_leases().await.unwrap(), 1);
    let actual = peer.list_leases(lease.workspace_id).await.unwrap();
    assert_eq!(actual.len(), 1);
    let reaped = &actual[0];
    assert_eq!(reaped.state, LeaseState::Expired);
    assert_eq!(reaped.lease_id, lease.lease_id);
    assert_eq!(reaped.base_revision, lease.base_revision);
    assert_eq!(reaped.expires_at_ns, lease.expires_at_ns);
    for elapsed in BOUNDARIES {
        assert_mark(peer.as_ref(), reaped, elapsed).await;
    }
}

#[tokio::test]
async fn g13a_sqlite_delete_revalidation_preserves_expired_base_until_grace() {
    for reaped in [false, true] {
        for elapsed in BOUNDARIES {
            let (_temp, writer, peer, lease) = fixture(reaped, false).await;
            assert_delete(writer.as_ref(), peer.as_ref(), &lease, elapsed).await;
        }
    }
}

#[tokio::test]
async fn g13d_sqlite_packed_binding_history_keeps_layer_roots_after_workspace_delete() {
    let (_temp, _client, _snapshot, proof, _payload) =
        crate::workspace_overlay::stores::binding_tests::packed().await;
    let store = SqliteWorkspaceStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let request = crate::workspace_overlay::stores::binding_tests::request(&store, proof).await;
    let record = store
        .install_packed_lower_binding(request.clone())
        .await
        .unwrap();
    store
        .release_lease(ReleaseLease {
            lease_id: request.guard.lease_id,
            holder_generation: request.guard.holder_generation,
        })
        .await
        .unwrap();
    store
        .mark_workspace_deleting(MarkDeleting {
            workspace_id: request.guard.workspace_id,
            force_fence_lease: false,
        })
        .await
        .unwrap();

    let snapshot = store.gc_snapshot(i64::MAX / 2, 0).await.unwrap();
    assert!(
        snapshot
            .root_layers
            .contains(&record.base_revision.layer_id),
        "PWB3 base layer must remain a GC root while binding history is retained"
    );
    assert!(
        snapshot.root_layers.contains(&record.head_layer_id),
        "PWB3 writable head must remain a GC root while binding history is retained"
    );

    // The destructive recheck must carry the same PWB3 roots as
    // gc_snapshot. A caller with a stale candidate list must not be able to
    // transition either binding target to Deleting.
    for layer_id in [record.base_revision.layer_id, record.head_layer_id] {
        let error = store
            .delete_layer_metadata(DeleteLayerMetadata {
                layer_ids: vec![layer_id],
                now_ns: i64::MAX / 2,
                lease_grace_ns: 0,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, WorkspaceError::Busy),
            "PWB3 binding root {layer_id} must reject destructive revalidation: {error:?}"
        );
        assert_eq!(
            store.load_layer(layer_id).await.unwrap().state,
            if layer_id == record.head_layer_id {
                crate::workspace_overlay::model::LayerState::Deleting
            } else {
                crate::workspace_overlay::model::LayerState::Sealed
            }
        );
    }
}

#[tokio::test]
async fn g13a_sqlite_released_lease_is_not_an_expired_recovery_root() {
    let (_temp, writer, peer, lease) = fixture(false, true).await;
    let now = lease.expires_at_ns - 1;
    assert!(
        !peer
            .gc_snapshot(now, 200)
            .await
            .unwrap()
            .root_layers
            .contains(&lease.base_revision.layer_id)
    );
    writer
        .delete_layer_metadata(DeleteLayerMetadata {
            layer_ids: vec![lease.base_revision.layer_id],
            now_ns: now,
            lease_grace_ns: 200,
        })
        .await
        .unwrap();
    assert_eq!(
        peer.load_layer(lease.base_revision.layer_id)
            .await
            .unwrap()
            .state,
        LayerState::Deleting
    );
}

#[tokio::test]
async fn g13d_sqlite_finalize_rejects_bound_deleting_head_and_keeps_batch_atomic() {
    let (_objects, _client, _snapshot, proof, _payload) =
        crate::workspace_overlay::stores::binding_tests::packed().await;
    let store = SqliteWorkspaceStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let request = crate::workspace_overlay::stores::binding_tests::request(&store, proof).await;
    let record = store
        .install_packed_lower_binding(request.clone())
        .await
        .unwrap();
    store
        .mark_workspace_deleting(MarkDeleting {
            workspace_id: record.workspace_id,
            force_fence_lease: true,
        })
        .await
        .unwrap();
    let before = store.load_layer(record.head_layer_id).await.unwrap();
    assert_eq!(before.state, LayerState::Deleting);

    let orphan = LayerId::new();
    store
        .record_orphan_slice(RecordOrphanSlice {
            orphan_layer_id: orphan,
            slice_id: 103_001,
            slice_end: 6,
        })
        .await
        .unwrap();
    // This direct finalization bypasses the collector's earlier candidate
    // recheck. A Deleting head is still protected by its durable PWB3 history,
    // and rejecting one member must preserve the entire candidate batch.
    let error = store
        .finalize_layer_metadata_deletion(vec![orphan, record.head_layer_id])
        .await
        .unwrap_err();
    assert!(matches!(error, WorkspaceError::Busy));
    assert_eq!(
        store.load_layer(record.head_layer_id).await.unwrap(),
        before
    );
    assert_eq!(
        store.load_layer(orphan).await.unwrap().state,
        LayerState::Deleting
    );

    store
        .finalize_layer_metadata_deletion(vec![orphan])
        .await
        .unwrap();
    assert!(matches!(
        store.load_layer(orphan).await,
        Err(WorkspaceError::LayerNotFound(found)) if found == orphan,
    ));
    assert!(
        !store
            .gc_snapshot(i64::MAX / 2, 0)
            .await
            .unwrap()
            .slice_references
            .iter()
            .any(|reference| reference.layer_id == orphan)
    );
    assert_eq!(
        store
            .load_packed_binding_version(record.workspace_id, 1)
            .await
            .unwrap(),
        Some(record),
    );
}

#[tokio::test]
async fn g13d_sqlite_finalize_rechecks_packed_history_parent_closure() {
    let (_objects, _client, _snapshot, proof, _payload) =
        crate::workspace_overlay::stores::binding_tests::packed().await;
    let store = SqliteWorkspaceStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let request = crate::workspace_overlay::stores::binding_tests::request(&store, proof).await;
    let initial = store
        .install_packed_lower_binding(request.clone())
        .await
        .unwrap();
    let mut guard = HeadGuard {
        expected_head_epoch: initial.head_epoch,
        ..request.guard.clone()
    };
    let mut sealed = Vec::new();
    // Retain an actual native parent chain. The public seal primitives are
    // used directly here because WorkspaceLifecycle::seal flattens the chain.
    for _ in 0..3 {
        let journal = JournalId::new();
        store
            .begin_seal(BeginSeal {
                guard: guard.clone(),
                journal_id: journal,
                new_head_layer_id: LayerId::new(),
            })
            .await
            .unwrap();
        for (expected_phase, next_phase, pending_bytes) in [
            (SealPhase::Prepare, SealPhase::Quiesced, None),
            (SealPhase::Quiesced, SealPhase::DataDrained, Some(0)),
        ] {
            store
                .advance_seal(AdvanceSeal {
                    journal_id: journal,
                    expected_phase,
                    next_phase,
                    pending_bytes,
                    last_error: None,
                })
                .await
                .unwrap();
        }
        store.hash_seal(journal).await.unwrap();
        let result = store.commit_seal(journal).await.unwrap();
        guard.expected_head_layer_id = result.new_head_layer_id;
        guard.expected_head_epoch = result.head_epoch;
        sealed.push(result.revision);
    }
    store
        .mark_workspace_deleting(MarkDeleting {
            workspace_id: initial.workspace_id,
            force_fence_lease: true,
        })
        .await
        .unwrap();
    let ancestor = sealed[1].layer_id;
    // Stage a candidate while only the original binding history exists.
    store
        .delete_layer_metadata(DeleteLayerMetadata {
            layer_ids: vec![ancestor],
            now_ns: i64::MAX / 2,
            lease_grace_ns: 0,
        })
        .await
        .unwrap();
    let before = store.load_layer(ancestor).await.unwrap();
    assert_eq!(before.state, LayerState::Deleting);

    // Persist a checksummed later-history fixture over that real native chain.
    // This isolates history-root finalization; it makes no assertion about
    // publication or seal recovery for a deleted workspace.
    let mut history = initial.clone();
    history.binding.binding_version = 2;
    history.base_revision = sealed[2].clone();
    history.binding.base_layer_id = history.base_revision.layer_id;
    history.head_layer_id = guard.expected_head_layer_id;
    history.head_epoch = guard.expected_head_epoch;
    store
        .rewrite_packed_binding_for_test(history.workspace_id, &history, false)
        .await
        .unwrap();
    let snapshot = store.gc_snapshot(i64::MAX / 2, 0).await.unwrap();
    assert!(
        snapshot
            .root_layers
            .contains(&history.base_revision.layer_id)
    );
    assert!(!snapshot.root_layers.contains(&ancestor));
    assert_eq!(
        store
            .load_layer(history.base_revision.layer_id)
            .await
            .unwrap()
            .parent_layer_id,
        Some(ancestor),
    );
    let error = store
        .finalize_layer_metadata_deletion(vec![ancestor])
        .await
        .unwrap_err();
    assert!(matches!(error, WorkspaceError::Busy));
    assert_eq!(store.load_layer(ancestor).await.unwrap(), before);
    assert_eq!(
        store
            .load_packed_binding_version(history.workspace_id, 2)
            .await
            .unwrap(),
        Some(history),
    );
}

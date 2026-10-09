//! The real catalog/final CAS protocol on the existing atomic test substrate.
use super::super::native_slice_deletion::{EXTENT_GENERATION_KEY, slice_deletion_key};
use super::super::*;
use super::tests::JournalMemoryBackend;
use crate::workspace_overlay::packed_v3::wire005::{V3BudgetPool, V3MountBudget};
use std::sync::atomic::Ordering;
use uuid::Uuid;

fn layer(value: u128) -> LayerId {
    LayerId::from_uuid(Uuid::from_u128(value))
}

fn extent(layer: LayerId, ino: i64, sid: u64) -> DataExtentDelta {
    DataExtentDelta::data(layer, ino, 0, 0, 6, sid, 0, 1)
}

async fn catalog() -> (
    Arc<JournalMemoryBackend>,
    KvWorkspaceStore<JournalMemoryBackend>,
    Arc<V3MountBudget>,
) {
    let backend = Arc::new(JournalMemoryBackend::at_time(100));
    let budget = V3MountBudget::defaults();
    let store =
        KvWorkspaceStore::from_arc(backend.clone()).with_packed_reader_pin_budget(budget.clone());
    store.initialize_workspace_schema().await.unwrap();
    store
        .create_volume_root(CreateVolumeRoot {
            volume_format: VOLUME_FORMAT.into(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            volume_id: Uuid::from_u128(31),
            workspace_id: WorkspaceId::from_uuid(Uuid::from_u128(32)),
            root_layer_id: layer(33),
            writable_layer_id: layer(34),
            owner_id: None,
        })
        .await
        .unwrap();
    (backend, store, budget)
}

async fn orphan(
    store: &KvWorkspaceStore<JournalMemoryBackend>,
    target: LayerId,
    sid: u64,
) -> Result<(), WorkspaceError> {
    store
        .record_orphan_slice(RecordOrphanSlice {
            orphan_layer_id: target,
            slice_id: sid,
            slice_end: 6,
        })
        .await
}

async fn epoch(backend: &JournalMemoryBackend) -> u64 {
    backend
        .get(EXTENT_GENERATION_KEY)
        .await
        .unwrap()
        .map(|raw| decode(&raw).unwrap())
        .unwrap_or(0)
}

#[tokio::test]
async fn native_slice_reservation_wins_and_atomically_invalidates_prepared_birth() {
    let (backend, store, _) = catalog().await;
    orphan(&store, layer(41), 141).await.unwrap();
    let live = extent(layer(34), 9, 141);
    let mut checks = Vec::new();
    let mut writes = vec![put(extent_key(&live), &live).unwrap()];
    let _owner = store
        .prepare_native_extent_cas(&mut checks, &mut writes)
        .await
        .unwrap();
    store
        .reserve_gc_slice_deletion(141, 6, &[layer(41)])
        .await
        .unwrap();
    assert!(!backend.compare_and_swap(&checks, &writes).await.unwrap());
    assert!(backend.get(&extent_key(&live)).await.unwrap().is_none());
    assert!(matches!(
        orphan(&store, layer(42), 141).await,
        Err(WorkspaceError::Busy)
    ));
    assert!(matches!(
        store.load_layer(layer(42)).await,
        Err(WorkspaceError::LayerNotFound(_))
    ));
}

#[tokio::test]
async fn native_slice_existing_live_reference_blocks_reservation() {
    let (backend, store, _) = catalog().await;
    orphan(&store, layer(51), 151).await.unwrap();
    let live = extent(layer(34), 9, 151);
    let mut checks = Vec::new();
    let mut writes = vec![put(extent_key(&live), &live).unwrap()];
    let _owner = store
        .prepare_native_extent_cas(&mut checks, &mut writes)
        .await
        .unwrap();
    assert!(backend.compare_and_swap(&checks, &writes).await.unwrap());
    assert!(matches!(
        store.reserve_gc_slice_deletion(151, 6, &[layer(51)]).await,
        Err(WorkspaceError::Busy)
    ));
    assert!(
        backend
            .get(&slice_deletion_key(151))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn native_slice_unselected_unreachable_reference_is_still_protected() {
    let (backend, store, _) = catalog().await;
    orphan(&store, layer(61), 161).await.unwrap();
    orphan(&store, layer(62), 161).await.unwrap();
    assert!(matches!(
        store.reserve_gc_slice_deletion(161, 6, &[layer(61)]).await,
        Err(WorkspaceError::Busy)
    ));
    assert!(
        backend
            .get(&slice_deletion_key(161))
            .await
            .unwrap()
            .is_none()
    );
    store
        .reserve_gc_slice_deletion(161, 6, &[layer(61), layer(62)])
        .await
        .unwrap();
}

#[tokio::test]
async fn native_slice_multi_sid_packet_rejects_every_write_when_one_sid_reserved() {
    let (backend, store, budget) = catalog().await;
    orphan(&store, layer(71), 171).await.unwrap();
    store
        .reserve_gc_slice_deletion(171, 6, &[layer(71)])
        .await
        .unwrap();
    let before = backend.rows.lock().await.clone();
    let used = budget.state().used;
    let first = extent(layer(34), 8, 172);
    let second = extent(layer(34), 9, 171);
    let mut checks = Vec::new();
    let mut writes = vec![
        put(extent_key(&first), &first).unwrap(),
        put(extent_key(&second), &second).unwrap(),
        KvWrite::Put {
            key: b"fixture-unrelated-write".to_vec(),
            value: vec![1],
        },
    ];
    assert!(matches!(
        store
            .prepare_native_extent_cas(&mut checks, &mut writes)
            .await,
        Err(WorkspaceError::Busy)
    ));
    assert_eq!(*backend.rows.lock().await, before);
    assert_eq!(budget.state().used, used);
}

#[tokio::test]
async fn native_slice_hole_and_delete_advance_the_same_extent_epoch() {
    let (backend, store, _) = catalog().await;
    assert_eq!(epoch(&backend).await, 0);
    let hole = DataExtentDelta::hole(layer(34), 9, 0, 0, 6, 1);
    let key = extent_key(&hole);
    let mut checks = Vec::new();
    let mut writes = vec![put(key.clone(), &hole).unwrap()];
    let _owner = store
        .prepare_native_extent_cas(&mut checks, &mut writes)
        .await
        .unwrap();
    assert!(backend.compare_and_swap(&checks, &writes).await.unwrap());
    assert_eq!(epoch(&backend).await, 1);
    let stale = checks;
    let mut checks = Vec::new();
    let mut writes = vec![KvWrite::Delete { key }];
    let _delete_owner = store
        .prepare_native_extent_cas(&mut checks, &mut writes)
        .await
        .unwrap();
    assert!(backend.compare_and_swap(&checks, &writes).await.unwrap());
    assert_eq!(epoch(&backend).await, 2);
    assert!(!backend.compare_and_swap(&stale, &[]).await.unwrap());
}

#[tokio::test]
async fn native_slice_duplicate_primary_key_fails_before_cas_and_refunds_owner() {
    let (backend, store, budget) = catalog().await;
    let row = extent(layer(34), 9, 181);
    let write = put(extent_key(&row), &row).unwrap();
    let before = backend.rows.lock().await.clone();
    let used = budget.state().used;
    assert!(matches!(
        store
            .prepare_native_extent_cas(&mut Vec::new(), &mut vec![write.clone(), write])
            .await,
        Err(WorkspaceError::CorruptMetadata(_))
    ));
    assert_eq!(*backend.rows.lock().await, before);
    assert_eq!(budget.state().used, used);
}

#[tokio::test]
async fn native_slice_committed_unknown_reservation_restores_from_exact_same_identity() {
    let (backend, store, budget) = catalog().await;
    orphan(&store, layer(91), 191).await.unwrap();
    backend.lose_reply.store(true, Ordering::SeqCst);
    assert!(matches!(
        store.reserve_gc_slice_deletion(191, 6, &[layer(91)]).await,
        Err(WorkspaceError::Backend(_))
    ));
    let key = slice_deletion_key(191);
    let original = backend.get(&key).await.unwrap().unwrap();
    let reopened =
        KvWorkspaceStore::from_arc(backend.clone()).with_packed_reader_pin_budget(budget);
    reopened
        .reserve_gc_slice_deletion(191, 6, &[layer(91)])
        .await
        .unwrap();
    assert_eq!(backend.get(&key).await.unwrap().unwrap(), original);
    assert!(matches!(
        orphan(&reopened, layer(92), 191).await,
        Err(WorkspaceError::Busy)
    ));
}

#[tokio::test]
async fn native_slice_reservation_tail_only_grows_and_other_target_never_reuses_it() {
    let (backend, store, _) = catalog().await;
    orphan(&store, layer(101), 201).await.unwrap();
    store
        .reserve_gc_slice_deletion(201, 9, &[layer(101)])
        .await
        .unwrap();
    let initial = backend
        .get(&slice_deletion_key(201))
        .await
        .unwrap()
        .unwrap();
    store
        .reserve_gc_slice_deletion(201, 6, &[layer(101)])
        .await
        .unwrap();
    assert_eq!(
        backend
            .get(&slice_deletion_key(201))
            .await
            .unwrap()
            .unwrap(),
        initial
    );
    store
        .reserve_gc_slice_deletion(201, 13, &[layer(101)])
        .await
        .unwrap();
    assert_ne!(
        backend
            .get(&slice_deletion_key(201))
            .await
            .unwrap()
            .unwrap(),
        initial
    );
    assert!(matches!(
        store
            .reserve_gc_slice_deletion(201, 13, &[layer(102)])
            .await,
        Err(WorkspaceError::Busy)
    ));
}

#[tokio::test]
async fn native_slice_finalization_keeps_permanent_fence_and_advances_extent_epoch() {
    let (backend, store, _) = catalog().await;
    orphan(&store, layer(111), 211).await.unwrap();
    store
        .reserve_gc_slice_deletion(211, 6, &[layer(111)])
        .await
        .unwrap();
    let before = epoch(&backend).await;
    store
        .finalize_layer_metadata_deletion(vec![layer(111)])
        .await
        .unwrap();
    assert!(epoch(&backend).await > before);
    assert!(
        backend
            .get(&slice_deletion_key(211))
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        orphan(&store, layer(112), 211).await,
        Err(WorkspaceError::Busy)
    ));
}

#[tokio::test]
async fn native_slice_no_header_cannot_authorize_a_reservation() {
    let backend = Arc::new(JournalMemoryBackend::at_time(100));
    let store = KvWorkspaceStore::from_arc(backend.clone())
        .with_packed_reader_pin_budget(V3MountBudget::defaults());
    store.initialize_workspace_schema().await.unwrap();
    orphan(&store, layer(121), 221).await.unwrap();
    assert!(matches!(
        store.reserve_gc_slice_deletion(221, 6, &[layer(121)]).await,
        Err(WorkspaceError::Fenced)
    ));
    assert!(
        backend
            .get(&slice_deletion_key(221))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn native_slice_mounted_budget_identity_and_rejection_are_preserved() {
    let (backend, store, budget) = catalog().await;
    assert!(Arc::ptr_eq(store.native_auxiliary_budget(), &budget));
    let raw = KvWorkspaceStore::from_arc(backend);
    assert!(Arc::ptr_eq(
        raw.native_auxiliary_budget(),
        raw.native_auxiliary_budget()
    ));
    assert!(!Arc::ptr_eq(raw.native_auxiliary_budget(), &budget));
    let before = budget.state().used[V3BudgetPool::Metadata as usize];
    budget.close();
    let row = extent(layer(34), 9, 231);
    assert!(matches!(
        store
            .prepare_native_extent_cas(
                &mut Vec::new(),
                &mut vec![put(extent_key(&row), &row).unwrap()]
            )
            .await,
        Err(WorkspaceError::InvalidReadPlan(_))
    ));
    assert_eq!(budget.state().used[V3BudgetPool::Metadata as usize], before);
}

#[tokio::test]
async fn native_slice_orphan_unknown_reply_commits_one_complete_packet_without_replay() {
    let (backend, store, _) = catalog().await;
    let before = epoch(&backend).await;
    backend.lose_reply.store(true, Ordering::SeqCst);
    assert!(matches!(
        orphan(&store, layer(131), 241).await,
        Err(WorkspaceError::Backend(_))
    ));
    assert!(store.load_layer(layer(131)).await.is_ok());
    assert!(
        backend
            .get(&extent_key(&extent(layer(131), 1, 241)))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(epoch(&backend).await, before + 1);
}

#[tokio::test]
async fn native_slice_public_compaction_rejects_reserved_sid_and_preserves_topology() {
    let (backend, store, budget) = catalog().await;
    orphan(&store, layer(141), 251).await.unwrap();
    store
        .reserve_gc_slice_deletion(251, 6, &[layer(141)])
        .await
        .unwrap();
    let workspace = store
        .load_workspace(WorkspaceId::from_uuid(Uuid::from_u128(32)))
        .await
        .unwrap();
    let before = backend.rows.lock().await.clone();
    let used = budget.state().used;
    let result = store
        .install_compaction(InstallCompaction {
            workspace_id: workspace.workspace_id,
            expected_head_layer_id: workspace.head_layer_id,
            expected_head_epoch: workspace.head_epoch,
            expected_parent_layer_id: layer(33),
            compacted_layer_id: layer(142),
            replacement_head_layer_id: layer(143),
            delta: crate::workspace_overlay::digest::CanonicalLayerDelta {
                extents: vec![extent(layer(142), 9, 251)],
                ..Default::default()
            },
        })
        .await;
    assert!(matches!(result, Err(WorkspaceError::Busy)));
    assert_eq!(*backend.rows.lock().await, before);
    assert_eq!(budget.state().used, used);
}

#[tokio::test]
async fn native_slice_public_packed_mutation_rejects_reserved_sid_and_preserves_sequences() {
    let backend = Arc::new(JournalMemoryBackend::at_time(100));
    let budget = V3MountBudget::defaults();
    let store =
        KvWorkspaceStore::from_arc(backend.clone()).with_packed_reader_pin_budget(budget.clone());
    let (_directory, _client, _snapshot, lower, _payload) =
        crate::workspace_overlay::stores::binding_tests::packed().await;
    let install = crate::workspace_overlay::stores::binding_tests::request(&store, lower).await;
    let record = store
        .install_packed_lower_binding(install.clone())
        .await
        .unwrap();
    let guard = HeadGuard {
        expected_head_epoch: record.head_epoch,
        ..install.guard
    };
    orphan(&store, layer(151), 261).await.unwrap();
    store
        .reserve_gc_slice_deletion(261, 6, &[layer(151)])
        .await
        .unwrap();
    let layers = store
        .load_layer_chain(guard.expected_head_layer_id)
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let mut request = VersionedMutation::empty(guard.clone(), layers, 64);
    request.inodes.push(InodeDelta {
        layer_id: guard.expected_head_layer_id,
        ino: 400,
        state: InodeState::Present,
        kind: 0,
        size: 6,
        mode: 0o100644,
        uid: 1,
        gid: 2,
        rdev: 0,
        nlink: 1,
        atime_ns: 0,
        mtime_ns: 0,
        ctime_ns: 0,
        symlink_target: None,
        parent_hint: Some(1),
        data_version: 1,
        sequence: 1,
    });
    request
        .extents
        .push(extent(guard.expected_head_layer_id, 400, 261));
    let before = backend.rows.lock().await.clone();
    let used = budget.state().used;
    assert!(matches!(
        store
            .apply_packed_versioned_mutation(request, record.binding)
            .await,
        Err(WorkspaceError::Busy)
    ));
    assert_eq!(*backend.rows.lock().await, before);
    assert_eq!(budget.state().used, used);
}

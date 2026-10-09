//! Public store + real authenticated PM11 adapter tests. The binding authority
//! is deterministic in-memory test wiring, not persisted packed publication.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;

use crate::cadapter::client::{ObjectBackend, ObjectByteStream, ObjectClient};
use crate::cadapter::localfs::LocalFsBackend;
use crate::chunk::read_plan::{
    ReadPlanError, ReadSource, ReadViewChanged, WorkspaceReadPlanProvider, execute_unified_into,
};
use crate::chunk::{BlockKey, BlockStore, ChunkLayout};
use crate::meta::MetaLayer;
use crate::meta::store::MetaError;
use crate::workspace_overlay::catalog::{
    HeadGuard, PackedLowerBinding, VersionedMutation, WorkspaceStore,
};
use crate::workspace_overlay::error::WorkspaceError;
use crate::workspace_overlay::meta_layer::{WorkspaceMetaLayer, WorkspacePackedBindingAuthority};
use crate::workspace_overlay::model::{
    AclDelta, DataExtentDelta, DentryDelta, InodeDelta, InodeState, ValueOp, XattrDelta,
};
use crate::workspace_overlay::packed_v3::wire005::{
    AuthenticatedV3Snapshot, V3ProducerOptions, V3SnapshotProducer,
};
use crate::workspace_overlay::packed_v3::{
    AccessProfile, GroupMeta, GroupMetaEntry, GroupMetaExtent, PackedFrameInput, PackedGroupInput,
    PackedV3ReadonlyMeta, SizeClass, SizeClassTable,
};
use crate::workspace_overlay::stores::database::SqliteWorkspaceStore;

#[path = "packed_lower_tests/default_reader_attach_contracts.rs"]
mod default_reader_attach_contracts;

#[derive(Clone)]
struct CountedBackend {
    inner: LocalFsBackend,
    gets: Arc<AtomicU64>,
    fail_reads: Arc<AtomicBool>,
}

#[async_trait]
impl ObjectBackend for CountedBackend {
    async fn put_object(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.inner.put_object(key, bytes).await
    }
    async fn put_object_create_only(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.inner.put_object_create_only(key, bytes).await
    }
    async fn get_object(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            !self.fail_reads.load(Ordering::SeqCst),
            "injected missing/corrupt lower object"
        );
        self.inner.get_object(key).await
    }
    async fn get_object_range(
        &self,
        key: &str,
        offset: u64,
        bytes: &mut [u8],
    ) -> anyhow::Result<usize> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            !self.fail_reads.load(Ordering::SeqCst),
            "injected missing/corrupt lower object"
        );
        self.inner.get_object_range(key, offset, bytes).await
    }
    async fn get_object_range_stream(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> anyhow::Result<ObjectByteStream> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            !self.fail_reads.load(Ordering::SeqCst),
            "injected missing/corrupt lower object"
        );
        self.inner.get_object_range_stream(key, offset, len).await
    }
    async fn get_etag(&self, key: &str) -> anyhow::Result<String> {
        self.inner.get_etag(key).await
    }
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        self.inner.delete_object(key).await
    }
}

struct UpperStore {
    reads: AtomicU64,
}

#[async_trait]
impl BlockStore for UpperStore {
    async fn write_fresh_range(
        &self,
        _key: BlockKey,
        _offset: u64,
        _bytes: &[u8],
    ) -> anyhow::Result<u64> {
        anyhow::bail!("test fixture only reads preinstalled native bytes")
    }
    async fn read_range(&self, key: BlockKey, offset: u64, bytes: &mut [u8]) -> anyhow::Result<()> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            key.0 == 77 && offset + bytes.len() as u64 <= 4096,
            "unknown/short upper block"
        );
        bytes.fill(0xa7);
        Ok(())
    }
    async fn delete_range(&self, _key: BlockKey, _count: u64) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FixedAuthority {
    binding: PackedLowerBinding,
    valid: AtomicBool,
}

#[async_trait]
impl WorkspacePackedBindingAuthority for FixedAuthority {
    async fn validate(
        &self,
        _guard: &HeadGuard,
        expected: &PackedLowerBinding,
    ) -> Result<(), WorkspaceError> {
        if !self.valid.load(Ordering::SeqCst) || expected != &self.binding {
            return Err(WorkspaceError::Fenced);
        }
        Ok(())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    meta: WorkspaceMetaLayer<SqliteWorkspaceStore>,
    lower: Arc<PackedV3ReadonlyMeta<CountedBackend>>,
    upper: Arc<UpperStore>,
    gets: Arc<AtomicU64>,
    authority: Arc<FixedAuthority>,
    payload: Vec<u8>,
    fail_reads: Arc<AtomicBool>,
}

impl Fixture {
    async fn mutation(&self) -> VersionedMutation {
        let view = self.meta.view_context().await;
        let layers = self
            .meta
            .store()
            .load_layer_chain(view.head_layer_id)
            .await
            .unwrap();
        VersionedMutation::empty(
            HeadGuard {
                workspace_id: view.workspace_id,
                expected_head_layer_id: view.head_layer_id,
                expected_head_epoch: view.head_epoch,
                lease_id: view.lease_id,
                holder_generation: view.holder_generation,
            },
            layers.try_into().unwrap(),
            4096,
        )
    }

    async fn copy_up(&self, extents: Vec<DataExtentDelta>) {
        let mut request = self.mutation().await;
        // Public metadata-only copy-up; it deliberately creates no full-file
        // extent so lower bytes remain visible in every absent interval.
        request.inodes.push(InodeDelta {
            layer_id: request.guard.expected_head_layer_id,
            ino: 400,
            state: InodeState::Present,
            kind: 0,
            size: self.payload.len() as u64,
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
            sequence: 0,
        });
        request.extents = extents;
        self.meta
            .store()
            .apply_versioned_mutation(request)
            .await
            .unwrap();
    }

    async fn head(&self) -> crate::workspace_overlay::ids::LayerId {
        self.meta.view_context().await.head_layer_id
    }

    async fn set_native_file_size(&self, size: u64) {
        // This adapter-only fixture has no persisted packed writer authority.
        // Change its native view through the actual conditional public store;
        // packed VFS truncate belongs to the mounted KV permission fixtures.
        let mut request = self.mutation().await;
        let rows = self
            .meta
            .store()
            .get_inode_deltas(crate::workspace_overlay::catalog::InodeQuery {
                layer_ids: request
                    .expected_layers
                    .iter()
                    .map(|layer| layer.layer_id)
                    .collect(),
                ino: 400,
            })
            .await
            .unwrap();
        let mut inode =
            crate::workspace_overlay::resolver::resolve_inode(&request.expected_layers, &rows, 400)
                .unwrap()
                .unwrap()
                .inode;
        inode.size = size;
        inode.data_version = inode.data_version.checked_add(1).unwrap();
        request.inodes.push(inode);
        self.meta
            .store()
            .apply_versioned_mutation(request)
            .await
            .unwrap();
    }
}

async fn fixture() -> Fixture {
    let native = super::test_meta().await;
    let temp = tempfile::tempdir().unwrap();
    let gets = Arc::new(AtomicU64::new(0));
    let fail_reads = Arc::new(AtomicBool::new(false));
    let backend = CountedBackend {
        inner: LocalFsBackend::new(temp.path().join("objects")),
        gets: gets.clone(),
        fail_reads: fail_reads.clone(),
    };
    let client = ObjectClient::new(backend);
    let payload: Vec<_> = (0..8192).map(|i| (i % 251 + 1) as u8).collect();
    let entry = GroupMetaEntry {
        name: b"file".to_vec(),
        inode: 400,
        kind: 1,
        mode: 0o100644,
        uid: 1,
        gid: 2,
        rdev: 0,
        nlink: 1,
        atime_ns: 0,
        mtime_ns: 0,
        ctime_ns: 0,
        size: payload.len() as u64,
        flags: 0,
        inline_data: Arc::from([]),
        extents: vec![GroupMetaExtent {
            file_offset: 0,
            logical_len: payload.len() as u32,
            frame_ordinal: 0,
            raw_offset: 0,
            raw_len: payload.len() as u32,
        }],
    };
    let group = PackedGroupInput {
        group_id: 1,
        parent_dir_key: [7; 32],
        metadata: GroupMeta::new(vec![entry]).unwrap().encode().unwrap(),
        frame_ordinals: vec![0],
        entry_count: 1,
        file_count: 1,
        layout_profile: AccessProfile::RandomSmallFile,
    };
    let mut producer = V3SnapshotProducer::new(
        client.clone(),
        temp.path(),
        "g10b".into(),
        V3ProducerOptions {
            snapshot_id: [9; 32],
            root_dir_key: [7; 32],
            root_inode: 1,
            profile: AccessProfile::RandomSmallFile,
            size_classes: SizeClassTable::default(),
            build_policy: Default::default(),
            metadata_codec: crate::workspace_overlay::packed_v3::PackedCodec::Raw,
            data_codec: crate::workspace_overlay::packed_v3::PackedCodec::Raw,
        },
    )
    .await
    .unwrap();
    producer
        .add_container(
            1,
            &[group],
            &[PackedFrameInput {
                raw: payload.clone(),
                size_class: SizeClass::Tiny,
                codec: 0,
                first_file_slot: 0,
                last_file_slot: 0,
            }],
            &[1],
        )
        .await
        .unwrap();
    producer
        .add_cold_attributes(
            &crate::workspace_overlay::packed_v3::wire005::V3ColdAttributes {
                inode: 400,
                symlink_target: None,
                xattrs: vec![crate::workspace_overlay::packed_v3::wire005::V3Xattr {
                    name: b"user.masked".to_vec(),
                    value: b"nonzero lower xattr".to_vec(),
                }],
                acl: vec![crate::meta::store::AclRule {
                    acl_type: 1,
                    qualifier: 2,
                    permissions: 7,
                }],
            },
        )
        .await
        .unwrap();
    let reference = producer.finish().await.unwrap();
    let snapshot = AuthenticatedV3Snapshot::open(&client, &reference)
        .await
        .unwrap();
    let lower = Arc::new(PackedV3ReadonlyMeta::from_v3(client, snapshot, 4096, 0));
    let view = native.view_context().await;
    let layers = native
        .store()
        .load_layer_chain(view.head_layer_id)
        .await
        .unwrap();
    let binding = PackedLowerBinding {
        binding_version: 1,
        base_layer_id: layers[1].layer_id,
        manifest: reference,
    };
    let authority = Arc::new(FixedAuthority {
        binding: binding.clone(),
        valid: AtomicBool::new(true),
    });
    let upper = Arc::new(UpperStore {
        reads: AtomicU64::new(0),
    });
    let meta = WorkspaceMetaLayer::with_chunk_size(native.store().clone(), view, 4096)
        .with_packed_v3_lower(
            binding,
            lower.clone(),
            authority.clone(),
            upper.clone(),
            ChunkLayout {
                chunk_size: 4096,
                block_size: 4096,
            },
        )
        .unwrap();
    meta.initialize().await.unwrap();
    Fixture {
        _temp: temp,
        meta,
        lower,
        upper,
        gets,
        authority,
        payload,
        fail_reads,
    }
}

#[tokio::test]
async fn public_catalog_binding_attach_requires_persisted_binding() {
    let f = fixture().await;
    let view = f.meta.view_context().await;
    let candidate = WorkspaceMetaLayer::with_chunk_size(f.meta.store().clone(), view, 4096)
        .with_packed_v3_lower_from_store(
            f.lower.clone(),
            f.upper.clone(),
            ChunkLayout {
                chunk_size: 4096,
                block_size: 4096,
            },
        )
        .await;
    assert!(matches!(
        candidate,
        Err(MetaError::Io(error)) if error.raw_os_error() == Some(libc::ESTALE)
    ));
}

#[tokio::test]
async fn public_prepared_full_upper_data_and_hole_issue_zero_lower_gets() {
    let f = fixture().await;
    let head = f.head().await;
    f.copy_up(vec![
        DataExtentDelta::data(head, 400, 0, 0, 2048, 77, 13, 0),
        DataExtentDelta::hole(head, 400, 0, 2048, 2048, 0),
    ])
    .await;
    let before = f.gets.load(Ordering::SeqCst);
    let prepared = f
        .meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    let mut out = vec![0x55; 4096];
    execute_unified_into(prepared.fetcher.as_ref(), 0, &prepared.plan, &mut out)
        .await
        .unwrap();
    assert_eq!(&out[..2048], &[0xa7; 2048]);
    assert_eq!(&out[2048..], &[0; 2048]);
    assert_eq!(
        f.gets.load(Ordering::SeqCst),
        before,
        "all runtime lower metadata/payload GETs must be zero"
    );
    assert!(f.upper.reads.load(Ordering::SeqCst) > 0);
    assert!(matches!(
        f.meta.read_plan(400, 0, 0, 4096).await,
        Err(MetaError::NotSupported(_))
    ));
}

#[tokio::test]
async fn public_prepared_partial_upper_reads_exact_nonzero_lower_gaps() {
    let f = fixture().await;
    let head = f.head().await;
    f.copy_up(vec![
        DataExtentDelta::data(head, 400, 0, 19, 5, 77, 42, 0),
        DataExtentDelta::hole(head, 400, 0, 27, 4, 0),
    ])
    .await;
    let prepared = f
        .meta
        .prepare_unified_read(400, 0, 11, 30)
        .await
        .unwrap()
        .unwrap();
    let packed_ranges: Vec<_> = prepared
        .plan
        .segments
        .iter()
        .filter(|s| {
            matches!(
                s.source,
                ReadSource::PackedFrame { .. } | ReadSource::PackedInline { .. }
            )
        })
        .map(|s| (s.logical_offset, s.length))
        .collect();
    assert_eq!(packed_ranges, vec![(11, 8), (24, 3), (31, 10)]);
    let mut out = vec![0; 30];
    execute_unified_into(prepared.fetcher.as_ref(), 11, &prepared.plan, &mut out)
        .await
        .unwrap();
    let mut expected = f.payload[11..41].to_vec();
    expected[8..13].fill(0xa7);
    expected[16..20].fill(0);
    assert_eq!(out, expected);
    assert!(f.gets.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn public_copyup_truncate_extend_masks_lower_across_chunks() {
    let f = fixture().await;
    // Keep the actual authenticated objects and GET counter, but attach this
    // mutation contract to a real KV catalog with persisted writer authority.
    let store = Arc::new(
        crate::workspace_overlay::stores::kv_store::packed_adapter_test_store(
            f.lower.mount_budget(),
        ),
    );
    let client = ObjectClient::new(CountedBackend {
        inner: LocalFsBackend::new(f._temp.path().join("objects")),
        gets: f.gets.clone(),
        fail_reads: f.fail_reads.clone(),
    });
    let snapshot = AuthenticatedV3Snapshot::open(&client, &f.authority.binding.manifest)
        .await
        .unwrap();
    let reader = crate::workspace_overlay::packed_v3::wire005::V3IndexReader::new(client, 0);
    let proof = crate::workspace_overlay::publish::binding::VerifiedPackedLower::from_authenticated_snapshot(
        &snapshot,
        &reader,
    )
    .await
    .unwrap();
    let install =
        crate::workspace_overlay::stores::binding_tests::request(store.as_ref(), proof).await;
    let record = store
        .install_packed_lower_binding(install.clone())
        .await
        .unwrap();
    let view = crate::workspace_overlay::model::ViewContext {
        workspace_id: install.guard.workspace_id,
        head_layer_id: install.guard.expected_head_layer_id,
        head_epoch: record.head_epoch,
        lease_id: install.guard.lease_id,
        holder_generation: install.guard.holder_generation,
    };
    let meta = WorkspaceMetaLayer::with_chunk_size(store, view, 4096)
        .with_packed_v3_lower_from_store(
            f.lower.clone(),
            f.upper.clone(),
            ChunkLayout {
                chunk_size: 4096,
                block_size: 4096,
            },
        )
        .await
        .unwrap();
    meta.initialize().await.unwrap();
    assert!(meta.store().supports_packed_permissions());
    // The first real truncate must copy up the immutable inode itself.
    assert!(
        meta.store()
            .get_inode_deltas(crate::workspace_overlay::catalog::InodeQuery {
                layer_ids: vec![install.guard.expected_head_layer_id],
                ino: 400,
            })
            .await
            .unwrap()
            .is_empty()
    );
    meta.truncate(400, 2048, 4096).await.unwrap();
    assert_eq!(meta.stat(400).await.unwrap().unwrap().size, 2048);
    meta.truncate(400, 8192, 4096).await.unwrap();
    assert_eq!(meta.stat(400).await.unwrap().unwrap().size, 8192);
    let before = f.gets.load(Ordering::SeqCst);
    let hidden = meta
        .prepare_unified_read(400, 1, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    let mut out = vec![0x77; 4096];
    execute_unified_into(hidden.fetcher.as_ref(), 0, &hidden.plan, &mut out)
        .await
        .unwrap();
    assert_eq!(out, vec![0; 4096]);
    assert_eq!(f.gets.load(Ordering::SeqCst), before);
    let first = meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    execute_unified_into(first.fetcher.as_ref(), 0, &first.plan, &mut out)
        .await
        .unwrap();
    assert_eq!(&out[..2048], &f.payload[..2048]);
    assert_eq!(&out[2048..], &[0; 2048]);
    drop(hidden);
    drop(first);
    meta.packed_reader_session()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn public_prepared_rejects_same_epoch_mutation_and_binding_fence_before_io() {
    let f = fixture().await;
    let head = f.head().await;
    f.copy_up(vec![DataExtentDelta::hole(head, 400, 0, 0, 4096, 0)])
        .await;
    let old = f
        .meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    let epoch = f.meta.view_context().await.head_epoch;
    f.set_native_file_size(4096).await;
    assert_eq!(f.meta.view_context().await.head_epoch, epoch);
    let before = f.gets.load(Ordering::SeqCst);
    let mut out = vec![0x55; 4096];
    let err = execute_unified_into(old.fetcher.as_ref(), 0, &old.plan, &mut out)
        .await
        .unwrap_err();
    assert!(matches!(err, ReadPlanError::StaleView(ReadViewChanged)));
    assert_eq!(out, vec![0x55; 4096]);
    let current = f
        .meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        old.plan.generation, current.plan.generation,
        "same-head-epoch upper mutation must advance the visible generation"
    );
    let mut stale_plan = old.plan.clone();
    stale_plan.generation.workspace_mutation_sequence = stale_plan
        .generation
        .workspace_mutation_sequence
        .saturating_add(1);
    let err = execute_unified_into(old.fetcher.as_ref(), 0, &stale_plan, &mut out)
        .await
        .unwrap_err();
    assert!(matches!(err, ReadPlanError::StaleView(ReadViewChanged)));
    f.authority.valid.store(false, Ordering::SeqCst);
    let err = execute_unified_into(current.fetcher.as_ref(), 0, &current.plan, &mut out)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ReadPlanError::Backend(ref cause) if matches!(cause.downcast_ref::<WorkspaceError>(), Some(WorkspaceError::Fenced)))
    );
    assert_eq!(f.gets.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn public_metadata_absence_falls_through_and_masks_are_terminal() {
    let f = fixture().await;
    assert_eq!(f.meta.lookup(1, "file").await.unwrap(), Some(400));
    assert_eq!(f.meta.stat(400).await.unwrap().unwrap().size, 8192);
    assert_eq!(
        f.meta.get_xattr(400, "user.masked").await.unwrap(),
        Some(b"nonzero lower xattr".to_vec())
    );
    assert_eq!(
        f.meta
            .get_acl(400, 1, 2)
            .await
            .unwrap()
            .unwrap()
            .permissions,
        7
    );
    f.copy_up(Vec::new()).await;
    let mut request = f.mutation().await;
    let head = request.guard.expected_head_layer_id;
    request.inodes.push(InodeDelta {
        layer_id: head,
        ino: 400,
        state: InodeState::Present,
        kind: 0,
        size: f.payload.len() as u64,
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
        sequence: 0,
    });
    request
        .dentries
        .push(DentryDelta::whiteout(head, 1, b"file".to_vec(), 0));
    request.xattrs.push(XattrDelta {
        layer_id: head,
        ino: 400,
        name: b"user.masked".to_vec(),
        op: ValueOp::Whiteout,
        value: None,
        sequence: 0,
    });
    request.acls.push(AclDelta {
        layer_id: head,
        ino: 400,
        acl_type: 1,
        acl_id: 2,
        op: ValueOp::Whiteout,
        value: None,
        sequence: 0,
    });
    f.meta
        .store()
        .apply_versioned_mutation(request)
        .await
        .unwrap();
    let before = f.gets.load(Ordering::SeqCst);
    assert_eq!(f.meta.lookup(1, "file").await.unwrap(), None);
    assert_eq!(f.meta.get_xattr(400, "user.masked").await.unwrap(), None);
    assert_eq!(f.meta.get_acl(400, 1, 2).await.unwrap(), None);
    assert_eq!(f.gets.load(Ordering::SeqCst), before);
    let mut request = f.mutation().await;
    let rows = f
        .meta
        .store()
        .get_inode_deltas(crate::workspace_overlay::catalog::InodeQuery {
            layer_ids: request
                .expected_layers
                .iter()
                .map(|layer| layer.layer_id)
                .collect(),
            ino: 400,
        })
        .await
        .unwrap();
    let mut inode =
        crate::workspace_overlay::resolver::resolve_inode(&request.expected_layers, &rows, 400)
            .unwrap()
            .unwrap()
            .inode;
    inode.state = InodeState::Deleted;
    request.inodes.push(inode);
    f.meta
        .store()
        .apply_versioned_mutation(request)
        .await
        .unwrap();
    assert!(f.meta.stat(400).await.unwrap().is_none());
    assert!(matches!(
        f.meta.prepare_unified_read(400, 0, 0, 1).await,
        Err(MetaError::NotFound(400))
    ));
    assert_eq!(f.gets.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn public_prepared_authenticated_lower_eof_zeros_extension_but_corruption_fails() {
    let f = fixture().await;
    f.copy_up(Vec::new()).await;
    f.set_native_file_size(8200).await;
    let extension = f
        .meta
        .prepare_unified_read(400, 2, 0, 8)
        .await
        .unwrap()
        .unwrap();
    let mut out = [0x55; 8];
    execute_unified_into(extension.fetcher.as_ref(), 0, &extension.plan, &mut out)
        .await
        .unwrap();
    assert_eq!(out, [0; 8]);
    f.fail_reads.store(true, Ordering::SeqCst);
    match f.meta.prepare_unified_read(400, 0, 0, 8).await {
        Err(_) => {}
        Ok(Some(prepared)) => {
            assert!(
                execute_unified_into(prepared.fetcher.as_ref(), 0, &prepared.plan, &mut out)
                    .await
                    .is_err()
            );
        }
        Ok(None) => panic!("packed capability silently returned no plan"),
    }
}

#[tokio::test]
async fn public_prepared_lease_fencing_does_not_reacquire_or_fetch() {
    let f = fixture().await;
    let head = f.head().await;
    f.copy_up(vec![DataExtentDelta::hole(head, 400, 0, 0, 4096, 0)])
        .await;
    let prepared = f
        .meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    let view = f.meta.view_context().await;
    f.meta
        .store()
        .release_lease(crate::workspace_overlay::catalog::ReleaseLease {
            lease_id: view.lease_id,
            holder_generation: view.holder_generation,
        })
        .await
        .unwrap();
    let before = f.gets.load(Ordering::SeqCst);
    let mut out = vec![0x55; 4096];
    let err = execute_unified_into(prepared.fetcher.as_ref(), 0, &prepared.plan, &mut out)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ReadPlanError::Backend(ref cause) if matches!(cause.downcast_ref::<WorkspaceError>(), Some(WorkspaceError::Fenced)))
    );
    assert_eq!(out, vec![0x55; 4096]);
    assert_eq!(f.gets.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn public_request_fence_reads_fresh_effective_eof_and_releases_owners() {
    let f = fixture().await;
    assert!(f.meta.requires_unified_read_request_fence());
    let lower_request = f
        .meta
        .begin_unified_read_request(400)
        .await
        .unwrap()
        .expect("mutable packed reads must retain a whole-request fence");
    assert_eq!(lower_request.file_size(), f.payload.len() as u64);
    lower_request.ensure_current().await.unwrap();
    drop(lower_request);

    f.copy_up(Vec::new()).await;
    assert_eq!(f.meta.stat(400).await.unwrap().unwrap().size, 8192);
    let lower = f.meta.packed_lower().unwrap();
    let budget_before = lower.budget.state().used;
    let gets_before = f.gets.load(Ordering::SeqCst);
    for size in [8200, 4097, 0] {
        // Capture after a mutation through the public API. A previously cached
        // inode or readonly lower EOF must not suppress extension or shrink.
        f.set_native_file_size(size).await;
        let request = f
            .meta
            .begin_unified_read_request(400)
            .await
            .unwrap()
            .expect("mutable packed reads must retain a whole-request fence");
        assert_eq!(request.file_size(), size);
        request.ensure_current().await.unwrap();
        assert!(
            lower
                .budget
                .state()
                .used
                .iter()
                .zip(budget_before)
                .any(|(used, baseline)| *used > baseline),
            "the request's retained metadata/fence owners must be admitted"
        );
        drop(request);
        assert_eq!(lower.budget.state().used, budget_before);
    }
    assert_eq!(f.gets.load(Ordering::SeqCst), gets_before);
}

#[tokio::test]
async fn public_request_fence_rejects_same_epoch_sequence_between_chunks() {
    let f = fixture().await;
    let head = f.head().await;
    f.copy_up(vec![DataExtentDelta::hole(head, 400, 0, 0, 4096, 0)])
        .await;
    let request = f
        .meta
        .begin_unified_read_request(400)
        .await
        .unwrap()
        .expect("mutable packed reads must retain a whole-request fence");
    let first = f
        .meta
        .prepare_unified_read(400, 0, 0, 4096)
        .await
        .unwrap()
        .unwrap();
    let mut bytes = vec![0x55; 4096];
    execute_unified_into(first.fetcher.as_ref(), 0, &first.plan, &mut bytes)
        .await
        .unwrap();
    assert_eq!(bytes, vec![0; 4096]);
    let epoch = f.meta.view_context().await.head_epoch;
    f.set_native_file_size(8200).await;
    assert_eq!(f.meta.view_context().await.head_epoch, epoch);
    // A second chunk can independently prepare under the new sequence. The
    // enclosing request must reject the combination of these two views.
    let second = f
        .meta
        .prepare_unified_read(400, 2, 0, 8)
        .await
        .unwrap()
        .unwrap();
    execute_unified_into(second.fetcher.as_ref(), 0, &second.plan, &mut bytes[..8])
        .await
        .unwrap();
    let error = request.ensure_current().await.unwrap_err();
    assert!(error.downcast_ref::<ReadViewChanged>().is_some());
    assert!(error.downcast_ref::<WorkspaceError>().is_none());
    let fresh = f
        .meta
        .begin_unified_read_request(400)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.file_size(), 8200);
    fresh.ensure_current().await.unwrap();
}

#[tokio::test]
async fn public_request_fence_keeps_binding_and_lease_fencing_fatal() {
    for revoke_binding in [false, true] {
        let f = fixture().await;
        f.copy_up(Vec::new()).await;
        let request = f
            .meta
            .begin_unified_read_request(400)
            .await
            .unwrap()
            .expect("mutable packed reads must retain a whole-request fence");
        if revoke_binding {
            f.authority.valid.store(false, Ordering::SeqCst);
        } else {
            let view = f.meta.view_context().await;
            f.meta
                .store()
                .release_lease(crate::workspace_overlay::catalog::ReleaseLease {
                    lease_id: view.lease_id,
                    holder_generation: view.holder_generation,
                })
                .await
                .unwrap();
        }
        let gets_before = f.gets.load(Ordering::SeqCst);
        let error = request.ensure_current().await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::Fenced)
        ));
        assert!(error.downcast_ref::<ReadViewChanged>().is_none());
        let next = f.meta.begin_unified_read_request(400).await;
        let error = match next {
            Err(error) => error,
            Ok(_) => panic!("lost ownership must reject a new read request"),
        };
        assert!(error.downcast_ref::<ReadViewChanged>().is_none());
        assert_eq!(f.gets.load(Ordering::SeqCst), gets_before);
    }
}

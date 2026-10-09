//! Real original-ID native capture -> producer -> compare -> two native CAS.
//! No injected source proof, no raw digest builder and no topology publication.

#[path = "packed_merged_permission_tests.rs"]
mod packed_merged_permission_tests;

#[path = "packed_raw_bytes_tests.rs"]
mod packed_raw_bytes_tests;

#[path = "native_payload_budget_tests.rs"]
mod native_payload_budget_tests;

#[path = "packed_enumeration_tests.rs"]
mod packed_enumeration_tests;

use super::*;
use crate::cadapter::client::ObjectClient;
use crate::cadapter::localfs::LocalFsBackend;
use crate::chunk::ChunkLayout;
use crate::chunk::read_plan::{ReadSource, WorkspaceReadPlanProvider, execute_unified_into};
use crate::chunk::store::InMemoryBlockStore;
use crate::meta::layer::MetaLayer;
use crate::meta::store::AclRule;
use crate::vfs::config::VFSConfig;
use crate::vfs::fs::VFS;
use crate::workspace_overlay::catalog::VersionedMutation;
use crate::workspace_overlay::digest::{delta_digest, root_hash};
use crate::workspace_overlay::ids::JournalId;
use crate::workspace_overlay::meta_layer::{
    PinnedCatalogPackedBindingAuthority, WorkspaceMetaLayer,
};
use crate::workspace_overlay::model::{
    DentryDelta, DentryOp, InodeState, LayerState, SealPhase, ViewContext,
};
use crate::workspace_overlay::packed_reader_lifecycle::{
    PackedReaderLeaseOptions, PackedReaderSession,
};
use crate::workspace_overlay::packed_v3::PackedV3ReadonlyMeta;
use crate::workspace_overlay::packed_v3::wire005::{
    AuthenticatedV3Snapshot, FrozenNativeArtifact, NativeCaptureLimits, V3ProducerOptions,
};
use crate::workspace_overlay::packed_v3::{AccessProfile, PackedCodec, SizeClassTable};
use crate::workspace_overlay::stores::kv_store::packed_native_freeze::{
    FrozenNativeDeltaHash, NativeDeltaHashLimits,
};
use tokio_util::sync::CancellationToken;

type NativeMeta = WorkspaceMetaLayer<KvWorkspaceStore<PinMemoryBackend>>;
type NativeVfs = VFS<InMemoryBlockStore, NativeMeta>;

const CHUNK: u64 = 4096;
const OVERWRITE: &[u8] = b"upper-overwrite";
const TAIL: &[u8] = b"upper-tail";
const RAW_ALIAS: &[u8] = b"raw-\xff-alias";

struct Fixture {
    directory: tempfile::TempDir,
    client: ObjectClient<LocalFsBackend>,
    store: Arc<KvWorkspaceStore<PinMemoryBackend>>,
    backend: PinMemoryBackend,
    reader: Arc<dyn PackedReaderSession>,
    budget: Arc<V3MountBudget>,
    meta: Arc<NativeMeta>,
    vfs: NativeVfs,
    guard: HeadGuard,
    payload: Vec<u8>,
}

async fn fixture(wrong_vfs_upper: bool) -> Fixture {
    fixture_from_packed(wrong_vfs_upper, packed().await).await
}

async fn fixture_from_packed(
    wrong_vfs_upper: bool,
    input: (
        tempfile::TempDir,
        ObjectClient<LocalFsBackend>,
        AuthenticatedV3Snapshot,
        crate::workspace_overlay::publish::binding::VerifiedPackedLower,
        Vec<u8>,
    ),
) -> Fixture {
    let (directory, client, snapshot, lower_proof, payload) = input;
    let backend = PinMemoryBackend::default();
    backend.now.store(1_000_000_000, Ordering::SeqCst);
    let budget = V3MountBudget::defaults();
    let store = Arc::new(
        KvWorkspaceStore::new(backend.clone()).with_packed_reader_pin_budget(budget.clone()),
    );
    let initial = request(store.as_ref(), lower_proof).await;
    let binding = store
        .install_packed_lower_binding(initial.clone())
        .await
        .unwrap();
    let guard = HeadGuard {
        expected_head_epoch: binding.head_epoch,
        ..initial.guard
    };
    let reader = store
        .clone()
        .open_packed_reader_session(
            guard.clone(),
            budget.clone(),
            PackedReaderLeaseOptions::default(),
        )
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&reader.mount_budget(), &budget));
    let lower = Arc::new(
        PackedV3ReadonlyMeta::from_v3_budget(client.clone(), snapshot, CHUNK, 0, budget.clone())
            .unwrap(),
    );
    let upper = Arc::new(InMemoryBlockStore::new());
    let layout = ChunkLayout {
        chunk_size: CHUNK,
        block_size: u32::try_from(CHUNK).unwrap(),
    };
    let authority = Arc::new(PinnedCatalogPackedBindingAuthority {
        store: store.clone(),
        reader: reader.clone(),
    });
    let meta = Arc::new(
        WorkspaceMetaLayer::with_chunk_size(
            store.clone(),
            ViewContext {
                workspace_id: guard.workspace_id,
                head_layer_id: guard.expected_head_layer_id,
                head_epoch: guard.expected_head_epoch,
                lease_id: guard.lease_id,
                holder_generation: guard.holder_generation,
            },
            CHUNK,
        )
        .with_packed_v3_lower(binding.binding, lower, authority, upper.clone(), layout)
        .unwrap(),
    );
    meta.initialize().await.unwrap();
    let vfs_upper = if wrong_vfs_upper {
        Arc::new(InMemoryBlockStore::new())
    } else {
        upper
    };
    let provider: Arc<dyn WorkspaceReadPlanProvider> = meta.clone();
    let vfs = VFS::from_readonly_components_with_provider(
        VFSConfig::new(layout),
        vfs_upper,
        meta.clone(),
        provider,
    )
    .unwrap();
    Fixture {
        directory,
        client,
        store,
        backend,
        reader,
        budget,
        meta,
        vfs,
        guard,
        payload,
    }
}

fn limits() -> NativeCaptureLimits {
    NativeCaptureLimits {
        max_inodes: 100,
        max_names: 100,
        max_spans: 1000,
        max_logical_bytes: 1 << 20,
        max_data_bytes: 1 << 20,
        max_payload_disk_bytes: 8 << 20,
        max_sqlite_disk_bytes: 8 << 20,
        max_producer_spool_disk_bytes: 8 << 20,
        sqlite_cache_bytes: 64 << 10,
        max_sql_vm_steps: 1_000_000,
    }
}

fn options() -> V3ProducerOptions {
    V3ProducerOptions {
        snapshot_id: [0x42; 32],
        root_dir_key: [0x73; 32],
        root_inode: 1,
        profile: AccessProfile::RandomSmallFile,
        size_classes: SizeClassTable::default(),
        build_policy: Default::default(),
        metadata_codec: PackedCodec::Raw,
        data_codec: PackedCodec::Raw,
    }
}

async fn add_raw_alias(f: &Fixture) {
    let expected_layers = f
        .store
        .load_layer_chain(f.guard.expected_head_layer_id)
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let mut inode = f
        .store
        .load_layer_delta(f.guard.expected_head_layer_id)
        .await
        .unwrap()
        .inodes
        .into_iter()
        .find(|inode| inode.ino == 400)
        .expect("real lower copy-up inode");
    inode.nlink += 1;
    inode.sequence = 0;
    let native_kind = inode.kind;
    let mut mutation = VersionedMutation::empty(f.guard.clone(), expected_layers, CHUNK);
    mutation.inodes.push(inode);
    mutation.dentries.push(DentryDelta::put(
        f.guard.expected_head_layer_id,
        1,
        RAW_ALIAS.to_vec(),
        400,
        native_kind,
        0,
    ));
    f.store.apply_versioned_mutation(mutation).await.unwrap();
}

async fn candidate_bytes_and_holes(
    candidate: &PackedV3ReadonlyMeta<LocalFsBackend>,
) -> (Vec<u8>, Vec<(u64, u64)>) {
    let mut result = vec![0x55; 8192];
    let mut holes = Vec::new();
    for chunk in 0..2 {
        let prepared = candidate
            .prepare_unified_read(400, chunk, 0, CHUNK)
            .await
            .unwrap()
            .unwrap();
        for segment in &prepared.plan.segments {
            if matches!(&segment.source, ReadSource::Hole) {
                let begin = chunk * CHUNK + segment.logical_offset;
                holes.push((begin, begin + segment.length));
            }
        }
        execute_unified_into(
            prepared.fetcher.as_ref(),
            0,
            &prepared.plan,
            &mut result[(chunk * CHUNK) as usize..((chunk + 1) * CHUNK) as usize],
        )
        .await
        .unwrap();
    }
    (result, holes)
}

#[tokio::test]
async fn actual_native_effective_roundtrip_preserves_ids_hardlinks_holes_cold_and_native_hash() {
    let f = fixture(false).await;
    assert_eq!(f.vfs.stat("/nonzero").await.unwrap().ino, 400);
    let alias = f.vfs.link("/nonzero", "/alias").await.unwrap();
    assert_eq!((alias.ino, alias.nlink), (400, 2));
    let fh = f
        .vfs
        .open(
            400,
            f.vfs.stat("/nonzero").await.unwrap(),
            true,
            true,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        f.vfs.write(fh, 128, OVERWRITE).await.unwrap(),
        OVERWRITE.len()
    );
    f.vfs.flush(fh).await.unwrap();
    f.vfs
        .workspace_hole_fallocate_from_fuse(fh, 400, 512, 257, true)
        .await
        .unwrap();
    f.vfs.truncate_inode(400, 8192).await.unwrap();
    assert_eq!(f.vfs.write(fh, 4352, TAIL).await.unwrap(), TAIL.len());
    f.vfs.flush(fh).await.unwrap();
    f.vfs.close(fh).await.unwrap();
    let (symlink, _) = f.vfs.create_symlink("/symlink", "/nonzero").await.unwrap();
    assert!(symlink > 400);
    let xattr: Vec<u8> = (0..65536).map(|index| (index % 251) as u8).collect();
    f.vfs
        .set_xattr_bytes_ino(400, b"user.boundary", &xattr, 0)
        .await
        .unwrap();
    let acl = AclRule {
        acl_type: 1,
        qualifier: 42,
        permissions: 5,
    };
    f.vfs.set_acl_ino(400, acl.clone()).await.unwrap();
    let deleted = f.vfs.create_file("/deleted-native").await.unwrap();
    f.vfs.unlink("/deleted-native").await.unwrap();
    add_raw_alias(&f).await;

    let mut expected = vec![0; 8192];
    expected[..CHUNK as usize].copy_from_slice(&f.payload);
    expected[128..128 + OVERWRITE.len()].copy_from_slice(OVERWRITE);
    expected[512..769].fill(0);
    expected[4352..4352 + TAIL.len()].copy_from_slice(TAIL);
    // Freeze real local admission/metadata/upload work before native Q.
    let local = f.vfs.quiesce_packed_vfs().await.unwrap();
    let expected_layers = f
        .store
        .load_layer_chain(f.guard.expected_head_layer_id)
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let native_delta = f
        .store
        .load_layer_delta(f.guard.expected_head_layer_id)
        .await
        .unwrap();
    assert!(
        native_delta
            .inodes
            .iter()
            .any(|row| row.ino == deleted && row.state == InodeState::Deleted)
    );
    assert!(
        native_delta
            .dentries
            .iter()
            .any(|row| row.name == b"deleted-native" && row.op == DentryOp::Whiteout)
    );
    assert!(
        native_delta.acls.iter().any(
            |row| row.ino == 400 && row.value.as_deref() == Some(5u32.to_be_bytes().as_slice())
        )
    );
    let expected_delta = delta_digest(&native_delta).unwrap();
    let parent = f
        .store
        .load_layer(f.guard.expected_head_layer_id)
        .await
        .unwrap()
        .parent_layer_id
        .unwrap();
    let expected_root = root_hash(
        f.store.load_layer(parent).await.unwrap().root_hash.unwrap(),
        expected_delta,
    );
    let journal_id = JournalId::new();
    let planned_head = LayerId::new();
    let native = Arc::new(
        f.store
            .clone()
            .begin_packed_native_quiesce(
                f.guard.clone(),
                expected_layers,
                journal_id,
                planned_head,
                f.budget.clone(),
            )
            .await
            .unwrap(),
    );
    let immutable_receipt = native.canonical_receipt_bytes().to_vec();
    let source = FrozenNativeArtifact::capture(
        native.clone(),
        local,
        f.directory.path().to_path_buf(),
        limits(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (source, manifest) = source
        .produce_candidate(
            f.client.clone(),
            f.directory.path().to_path_buf(),
            "native-effective-test/staged".into(),
            uuid::Uuid::new_v4(),
            options(),
        )
        .await
        .unwrap();
    assert!(
        manifest.key.contains("/native-incarnation-"),
        "producer requires fresh physical incarnation"
    );
    let snapshot = AuthenticatedV3Snapshot::open(&f.client, &manifest)
        .await
        .unwrap();
    let candidate = Arc::new(
        PackedV3ReadonlyMeta::from_v3_budget(
            f.client.clone(),
            snapshot,
            CHUNK,
            0,
            f.budget.clone(),
        )
        .unwrap(),
    );
    assert_eq!(candidate.lookup(1, "nonzero").await.unwrap(), Some(400));
    assert_eq!(candidate.lookup(1, "alias").await.unwrap(), Some(400));
    assert_eq!(
        candidate
            .lookup_with_attr_bytes(1, RAW_ALIAS)
            .await
            .unwrap()
            .unwrap()
            .0,
        400
    );
    assert_eq!(candidate.stat_fresh(400).await.unwrap().unwrap().nlink, 3);
    assert_eq!(candidate.lookup(1, "symlink").await.unwrap(), Some(symlink));
    assert_eq!(
        candidate.read_symlink_bytes(symlink).await.unwrap(),
        b"/nonzero"
    );
    assert_eq!(
        candidate
            .get_xattr_bytes(400, b"user.boundary")
            .await
            .unwrap(),
        Some(xattr)
    );
    assert_eq!(
        candidate
            .get_acl(400, acl.acl_type, acl.qualifier)
            .await
            .unwrap(),
        Some(acl)
    );
    assert!(candidate.stat_fresh(deleted).await.unwrap().is_none());
    let (actual, holes) = candidate_bytes_and_holes(candidate.as_ref()).await;
    assert_eq!(actual, expected);
    for (begin, end) in [(512, 769), (4096, 4352), (4352 + TAIL.len() as u64, 8192)] {
        assert!(
            holes
                .iter()
                .any(|(hole_begin, hole_end)| *hole_begin <= begin && *hole_end >= end),
            "required native Hole [{begin},{end}) absent: {holes:?}"
        );
    }
    let verified = source.compare_candidate(candidate.clone()).await.unwrap();
    let source_digest = verified.source_digest();
    assert_eq!(
        (
            verified.counts().inodes,
            verified.counts().names,
            verified.counts().logical_bytes
        ),
        (3, 4, 8192)
    );
    assert_eq!(
        verified.counts().data_bytes,
        CHUNK - 257 + TAIL.len() as u64
    );
    assert_eq!(verified.highest_inode(), symlink);
    let native_hash = Arc::new(
        FrozenNativeDeltaHash::capture(
            native.clone(),
            f.reader.clone(),
            NativeDeltaHashLimits {
                max_native_delta_rows: 1000,
                max_canonical_bytes: 1 << 20,
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        (native_hash.delta_digest(), native_hash.root_hash()),
        (expected_delta, expected_root)
    );

    // Hold the real DD CAS response after commit and inspect the actual native
    // catalog between the two production transitions, rather than phase flags.
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *f.backend.reply_barrier.lock().await = Some((entered.clone(), resume.clone()));
    let entered_wait = entered.notified();
    let hash_for_driver = native_hash.clone();
    let driver = tokio::spawn(async move { verified.promote_native_hashed(hash_for_driver).await });
    tokio::time::timeout(Duration::from_secs(3), entered_wait)
        .await
        .unwrap();
    let drained = f.store.load_seal_journal(journal_id).await.unwrap();
    assert_eq!(
        (
            drained.phase,
            drained.pending_bytes,
            drained.delta_digest,
            drained.root_hash
        ),
        (SealPhase::DataDrained, 0, None, None)
    );
    assert_eq!(
        f.store
            .load_workspace(f.guard.workspace_id)
            .await
            .unwrap()
            .head_layer_id,
        f.guard.expected_head_layer_id
    );
    resume.notify_one();
    let promoted = tokio::time::timeout(Duration::from_secs(10), driver)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let hashed = match promoted {
        Ok(hashed) => hashed,
        Err(failure) => panic!("actual source promotion failed: {}", failure.error),
    };
    hashed.validate().await.unwrap();
    assert_eq!(hashed.source_digest(), source_digest);
    assert_eq!(
        hashed.phase_authority().canonical_receipt_bytes(),
        immutable_receipt
    );
    assert_eq!(native.canonical_receipt_bytes(), immutable_receipt);
    let journal = f.store.load_seal_journal(journal_id).await.unwrap();
    assert_eq!(
        (journal.phase, journal.delta_digest, journal.root_hash),
        (SealPhase::Hashed, Some(expected_delta), Some(expected_root))
    );
    assert_eq!(hashed.phase_authority().journal(), &journal);
    // Native hashing did not perform the separately required carrier/PWB CAS.
    assert_eq!(
        f.store
            .load_workspace(f.guard.workspace_id)
            .await
            .unwrap()
            .head_layer_id,
        f.guard.expected_head_layer_id
    );
    assert_eq!(
        f.store
            .load_layer(f.guard.expected_head_layer_id)
            .await
            .unwrap()
            .state,
        LayerState::Sealing
    );
    assert!(f.store.load_layer(planned_head).await.is_err());
    drop(hashed);
    drop(native_hash);
    drop(native);
    drop(candidate);
    f.meta.shutdown_session().await.unwrap();
}

#[tokio::test]
async fn actual_native_capture_rejects_other_vfs_upper_without_catalog_write() {
    let f = fixture(true).await;
    let local = f.vfs.quiesce_packed_vfs().await.unwrap();
    let expected_layers = f
        .store
        .load_layer_chain(f.guard.expected_head_layer_id)
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let journal_id = JournalId::new();
    let native = Arc::new(
        f.store
            .clone()
            .begin_packed_native_quiesce(
                f.guard.clone(),
                expected_layers,
                journal_id,
                LayerId::new(),
                f.budget.clone(),
            )
            .await
            .unwrap(),
    );
    let before = f.backend.rows.lock().await.clone();
    let result = FrozenNativeArtifact::capture(
        native.clone(),
        local,
        f.directory.path().to_path_buf(),
        limits(),
        CancellationToken::new(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("other upper Arc accepted as frozen source"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("identity mismatch"), "{error}");
    assert_eq!(*f.backend.rows.lock().await, before);
    assert_eq!(
        f.store.load_seal_journal(journal_id).await.unwrap().phase,
        SealPhase::Quiesced
    );
    native.validate().await.unwrap();
    drop(native);
    f.meta.shutdown_session().await.unwrap();
}

#[tokio::test]
async fn packed_workspace_filesystem_57_byte_write_and_readback_complete_within_deadline() {
    use asyncfuse::raw::{Filesystem, Request};
    use std::ffi::OsStr;
    use std::time::Duration;
    use tokio::time::timeout;

    const WAIT: Duration = Duration::from_secs(5);
    let f = timeout(WAIT, fixture(false))
        .await
        .expect("packed fixture setup exceeded five seconds");
    let request = |unique| Request {
        unique,
        pid: std::process::id(),
        ..Request::default()
    };
    let created = timeout(
        WAIT,
        Filesystem::create(
            &f.vfs,
            request(1),
            1,
            OsStr::new("write-diagnostic"),
            0o644,
            libc::O_WRONLY as u32,
        ),
    )
    .await
    .expect("actual Filesystem::create exceeded five seconds")
    .unwrap();
    let ino = created.attr.ino;
    assert_ne!(created.fh, 0);
    let payload = [0x5a; 57];
    let written = timeout(
        WAIT,
        Filesystem::write(
            &f.vfs,
            request(2),
            ino,
            created.fh,
            0,
            &payload,
            0,
            libc::O_WRONLY as u32,
        ),
    )
    .await
    .expect("actual Filesystem::write exceeded five seconds")
    .unwrap();
    assert_eq!(written.written, 57);
    timeout(
        WAIT,
        Filesystem::fsync(&f.vfs, request(3), ino, created.fh, false),
    )
    .await
    .expect("actual Filesystem::fsync exceeded five seconds")
    .unwrap();
    timeout(
        WAIT,
        Filesystem::release(
            &f.vfs,
            request(4),
            ino,
            created.fh,
            libc::O_WRONLY as u32,
            0,
            false,
        ),
    )
    .await
    .expect("actual writer release exceeded five seconds")
    .unwrap();
    let opened = timeout(
        WAIT,
        Filesystem::open(&f.vfs, request(5), ino, libc::O_RDONLY as u32),
    )
    .await
    .expect("actual read open exceeded five seconds")
    .unwrap();
    let read = timeout(
        WAIT,
        Filesystem::read(&f.vfs, request(6), ino, opened.fh, 0, 57),
    )
    .await
    .expect("actual Filesystem::read exceeded five seconds")
    .unwrap();
    assert_eq!(read.data.as_ref(), payload.as_slice());
    drop(read);
    timeout(
        WAIT,
        Filesystem::release(
            &f.vfs,
            request(7),
            ino,
            opened.fh,
            libc::O_RDONLY as u32,
            0,
            false,
        ),
    )
    .await
    .expect("actual reader release exceeded five seconds")
    .unwrap();
    assert_eq!(f.vfs.open_file_handle_count(), 0);
    timeout(WAIT, f.reader.shutdown())
        .await
        .expect("packed reader shutdown exceeded five seconds")
        .unwrap();
}

use super::*;
use crate::workspace_overlay::entity::{
    backend::{CommitOutcome, MemoryEntityBackend},
    codec::encode,
    model::{ChunkSource, EntityInode, EntityKind, LowerBinding},
};

struct Fixture {
    volume_id: Uuid,
    backend: Arc<MemoryEntityBackend>,
    lifecycle: Arc<Lifecycle<MemoryEntityBackend>>,
    ownership: Ownership<MemoryEntityBackend>,
    keys: EntityKeys,
    session: WriterSession,
}

impl Fixture {
    async fn new() -> Self {
        let backend = Arc::new(MemoryEntityBackend::new(10));
        let volume_id = Uuid::new_v4();
        let keys = EntityKeys::new(volume_id, Uuid::new_v4());
        let lifecycle = Arc::new(Lifecycle::new(backend.clone(), keys.clone()));
        lifecycle
            .provision(&LowerBinding {
                manifest_key: b"reference-lower".to_vec(),
                manifest_digest: [3; 32],
                root_ino: 1,
                max_ino: 2,
            })
            .await
            .unwrap();
        let session = lifecycle.acquire(Uuid::new_v4(), 1000).await.unwrap();
        let ownership = Ownership::new(backend.clone(), keys.clone());
        Self {
            volume_id,
            backend,
            lifecycle,
            ownership,
            keys,
            session,
        }
    }

    async fn upload(&self) -> u64 {
        let token = self
            .ownership
            .allocate_upload(&self.session, 16)
            .await
            .unwrap();
        self.ownership
            .uploaded(&self.session, &token)
            .await
            .unwrap();
        self.ownership
            .uploaded(&self.session, &token)
            .await
            .unwrap();
        token.slice_id()
    }

    async fn row(&self, id: u64) -> SliceOwnership {
        let snapshot = self.backend.read(&[self.keys.slice(id)]).await.unwrap();
        decode(snapshot.rows[0].as_deref().unwrap()).unwrap()
    }

    async fn replacement(
        &self,
        ino: i64,
        index: u64,
        old: &ChunkMap,
        new: &ChunkMap,
    ) -> EntityPlan {
        let mut plan =
            plan_chunk_membership(self.backend.as_ref(), &self.keys, ino, index, old, new)
                .await
                .unwrap();
        let key = self.keys.chunk(ino, index);
        let snapshot = self.backend.read(std::slice::from_ref(&key)).await.unwrap();
        plan.checks.push(RowCheck {
            key: key.clone(),
            expected: snapshot.rows[0].clone(),
        });
        plan.writes.push(put(key, new).unwrap());
        plan
    }

    async fn publish(&self, ino: i64, index: u64, old: &ChunkMap, new: &ChunkMap) {
        let plan = self.replacement(ino, index, old, new).await;
        let authority = self.lifecycle.authority(&self.session).await.unwrap();
        assert_eq!(
            self.backend.commit(&authority, &plan).await.unwrap(),
            CommitOutcome::Committed
        );
    }
}

fn map(id: u64) -> ChunkMap {
    ChunkMap::holes(16)
        .unwrap()
        .replace(
            0,
            2,
            ChunkSource::Data {
                slice_id: id,
                slice_offset: 0,
            },
        )
        .unwrap()
        .replace(
            4,
            2,
            ChunkSource::Data {
                slice_id: id,
                slice_offset: 4,
            },
        )
        .unwrap()
}

#[tokio::test]
async fn memberships_count_distinct_chunks_and_replay_cannot_double_release() {
    let fixture = Fixture::new().await;
    let id = fixture.upload().await;
    let holes = ChunkMap::holes(16).unwrap();
    let data = map(id);
    fixture.publish(3, 0, &holes, &data).await;
    assert_eq!(
        fixture.row(id).await.current_refs,
        1,
        "two spans in one chunk count once"
    );
    assert!(!fixture.row(id).await.staging_hold);
    fixture.publish(3, 1, &holes, &data).await;
    assert_eq!(fixture.row(id).await.current_refs, 2);
    let plan = fixture.replacement(3, 0, &data, &holes).await;
    let authority = fixture.lifecycle.authority(&fixture.session).await.unwrap();
    assert_eq!(
        fixture.backend.commit(&authority, &plan).await.unwrap(),
        CommitOutcome::Committed
    );
    assert_eq!(
        fixture.backend.commit(&authority, &plan).await.unwrap(),
        CommitOutcome::Conflict
    );
    let row = fixture.row(id).await;
    assert_eq!(row.current_refs, 1);
    assert!(row.retirement_hold);
    fixture.publish(3, 1, &data, &holes).await;
    let row = fixture.row(id).await;
    assert_eq!(row.current_refs, 0);
    assert!(
        row.retirement_hold,
        "zero references cannot reclaim an old read plan"
    );
}

#[tokio::test]
async fn pending_and_unpublished_uploads_block_reclamation_and_upload_token_drains() {
    let fixture = Fixture::new().await;
    let token = fixture
        .ownership
        .allocate_upload(&fixture.session, 16)
        .await
        .unwrap();
    let id = token.slice_id();
    let holes = ChunkMap::holes(16).unwrap();
    assert!(matches!(
        plan_chunk_membership(
            fixture.backend.as_ref(),
            &fixture.keys,
            3,
            0,
            &holes,
            &map(id)
        )
        .await,
        Err(EntityError::Busy)
    ));
    let cloned_token = token.clone();
    let lifecycle = fixture.lifecycle.clone();
    let session = fixture.session.clone();
    let task = tokio::spawn(async move { lifecycle.enter_maintenance(&session).await });
    for _ in 0..100 {
        if fixture.session.is_closed() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(fixture.session.is_closed());
    assert!(!task.is_finished());
    drop(token);
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "cloned upload token retains its obligation"
    );
    drop(cloned_token);
    let maintenance = task.await.unwrap().unwrap();
    assert!(matches!(
        fixture.ownership.reserve_delete(&maintenance, id).await,
        Err(EntityError::Busy)
    ));
    assert!(matches!(
        fixture.lifecycle.discard_empty(&maintenance).await,
        Err(EntityError::Busy)
    ));
    fixture
        .ownership
        .abandon_upload(&maintenance, id)
        .await
        .unwrap();
    let deletion = fixture
        .ownership
        .reserve_delete(&maintenance, id)
        .await
        .unwrap();
    fixture
        .ownership
        .complete_delete(&maintenance, &deletion)
        .await
        .unwrap();
    fixture
        .ownership
        .complete_delete(&maintenance, &deletion)
        .await
        .unwrap();
    assert!(matches!(
        fixture.row(id).await.state,
        SliceState::Deleted { .. }
    ));
    fixture.lifecycle.discard_empty(&maintenance).await.unwrap();
}

#[tokio::test]
async fn uploaded_but_unpublished_object_keeps_staging_owner() {
    let fixture = Fixture::new().await;
    let id = fixture.upload().await;
    let maintenance = fixture
        .lifecycle
        .enter_maintenance(&fixture.session)
        .await
        .unwrap();
    assert!(matches!(
        fixture.ownership.reserve_delete(&maintenance, id).await,
        Err(EntityError::Busy)
    ));
    assert!(fixture.row(id).await.staging_hold);
}

#[tokio::test]
async fn retired_object_waits_for_reader_then_deletion_excludes_stale_publication() {
    let fixture = Fixture::new().await;
    let id = fixture.upload().await;
    let holes = ChunkMap::holes(16).unwrap();
    let data = map(id);
    fixture.publish(3, 0, &holes, &data).await;
    let old_read_plan = fixture.session.admit().unwrap();
    fixture.publish(3, 0, &data, &holes).await;
    let stale_plan = fixture.replacement(3, 0, &holes, &data).await;
    let stale_authority = fixture.lifecycle.authority(&fixture.session).await.unwrap();
    let lifecycle = fixture.lifecycle.clone();
    let session = fixture.session.clone();
    let task = tokio::spawn(async move { lifecycle.enter_maintenance(&session).await });
    for _ in 0..100 {
        if fixture.session.is_closed() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(!task.is_finished());
    assert!(fixture.row(id).await.retirement_hold);
    drop(old_read_plan);
    let maintenance = task.await.unwrap().unwrap();
    let deletion = fixture
        .ownership
        .reserve_delete(&maintenance, id)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .backend
            .commit(&stale_authority, &stale_plan)
            .await
            .unwrap(),
        CommitOutcome::Conflict
    );
    assert!(matches!(
        plan_chunk_membership(fixture.backend.as_ref(), &fixture.keys, 3, 0, &holes, &data).await,
        Err(EntityError::Busy)
    ));
    fixture
        .ownership
        .complete_delete(&maintenance, &deletion)
        .await
        .unwrap();
    assert!(matches!(
        fixture.row(id).await.state,
        SliceState::Deleted { .. }
    ));
}

#[tokio::test]
async fn cross_namespace_missing_membership_and_underflow_fail_without_map_changes() {
    let fixture = Fixture::new().await;
    let id = fixture.upload().await;
    let holes = ChunkMap::holes(16).unwrap();
    let data = map(id);
    let foreign_keys = EntityKeys::new(fixture.volume_id, Uuid::new_v4());
    assert!(
        plan_chunk_membership(fixture.backend.as_ref(), &foreign_keys, 3, 0, &holes, &data)
            .await
            .is_err()
    );
    // Pretending an unpublished slice already has a membership must fail closed.
    assert!(matches!(
        plan_chunk_membership(fixture.backend.as_ref(), &fixture.keys, 3, 0, &data, &holes).await,
        Err(EntityError::Corrupt(_))
    ));
    fixture.publish(3, 0, &holes, &data).await;
    let mut corrupt = fixture.row(id).await;
    corrupt.current_refs = 0;
    let authority = fixture.lifecycle.authority(&fixture.session).await.unwrap();
    fixture
        .backend
        .commit(
            &authority,
            &EntityPlan {
                checks: Vec::new(),
                writes: vec![put(fixture.keys.slice(id), &corrupt).unwrap()],
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        plan_chunk_membership(fixture.backend.as_ref(), &fixture.keys, 3, 0, &data, &holes).await,
        Err(EntityError::Corrupt(_))
    ));
    assert_eq!(
        fixture
            .backend
            .read(&[fixture.keys.chunk(3, 0)])
            .await
            .unwrap()
            .rows[0],
        Some(encode(&data).unwrap())
    );
}

#[tokio::test]
async fn orphan_cleanup_advances_with_memberships_and_is_idempotent() {
    let fixture = Fixture::new().await;
    let id = fixture.upload().await;
    let holes = ChunkMap::holes(16).unwrap();
    let data = map(id);
    fixture.publish(3, 0, &holes, &data).await;
    fixture.publish(3, 1, &holes, &data).await;
    let inode = EntityInode {
        ino: 3,
        kind: EntityKind::File,
        size: 32,
        mode: 0o600,
        uid: 1,
        gid: 1,
        rdev: 0,
        nlink: 0,
        atime_ns: 0,
        mtime_ns: 0,
        ctime_ns: 0,
        symlink_target: None,
        parent_hint: Some(1),
        lower_visible_size: 0,
        data_version: 1,
    };
    let mut orphan_plan = plan_orphan(&fixture.keys, &fixture.session, 3, None).unwrap();
    orphan_plan
        .writes
        .push(put(fixture.keys.inode(3), &UpperInode::Present(inode)).unwrap());
    let authority = fixture.lifecycle.authority(&fixture.session).await.unwrap();
    assert_eq!(
        fixture
            .backend
            .commit(&authority, &orphan_plan)
            .await
            .unwrap(),
        CommitOutcome::Committed
    );
    assert_eq!(
        fixture
            .backend
            .commit(&authority, &orphan_plan)
            .await
            .unwrap(),
        CommitOutcome::Conflict
    );
    let current = fixture
        .backend
        .read(&[fixture.keys.orphan(3)])
        .await
        .unwrap()
        .rows
        .remove(0);
    assert!(
        plan_orphan(&fixture.keys, &fixture.session, 3, current)
            .unwrap()
            .writes
            .is_empty()
    );
    let open_handle = fixture.session.admit().unwrap();
    let lifecycle = fixture.lifecycle.clone();
    let session = fixture.session.clone();
    let task = tokio::spawn(async move { lifecycle.enter_maintenance(&session).await });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(fixture.row(id).await.current_refs, 2);
    drop(open_handle);
    let maintenance = task.await.unwrap().unwrap();
    assert!(
        !fixture
            .ownership
            .recover_orphan_step(&maintenance, 3)
            .await
            .unwrap()
    );
    assert_eq!(fixture.row(id).await.current_refs, 1);
    let cursor: OrphanRecord = decode(
        fixture
            .backend
            .read(&[fixture.keys.orphan(3)])
            .await
            .unwrap()
            .rows[0]
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cursor.cleanup_chunk, 1);
    assert!(
        !fixture
            .ownership
            .recover_orphan_step(&maintenance, 3)
            .await
            .unwrap()
    );
    assert_eq!(fixture.row(id).await.current_refs, 0);
    assert!(fixture.row(id).await.retirement_hold);
    assert!(
        fixture
            .ownership
            .recover_orphan_step(&maintenance, 3)
            .await
            .unwrap()
    );
    assert!(
        fixture
            .ownership
            .recover_orphan_step(&maintenance, 3)
            .await
            .unwrap()
    );
    assert_eq!(
        fixture
            .backend
            .read(&[fixture.keys.inode(3)])
            .await
            .unwrap()
            .rows[0],
        Some(encode(&UpperInode::Deleted).unwrap())
    );
    let reservation = fixture
        .ownership
        .reserve_delete(&maintenance, id)
        .await
        .unwrap();
    fixture
        .ownership
        .complete_delete(&maintenance, &reservation)
        .await
        .unwrap();
}

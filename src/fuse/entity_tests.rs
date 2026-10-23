use super::*;
use crate::cadapter::localfs::LocalFsBackend;
use crate::workspace_overlay::entity::{
    keys::EntityKeys,
    lifecycle::Lifecycle,
    memory::MemoryEntityBackend,
    resolver::{ImmutableLower, test_support::MemoryLower},
};
use futures_util::TryStreamExt;
use uuid::Uuid;

fn owner() -> Credentials {
    Credentials {
        uid: 1000,
        gid: 1000,
        groups: vec![1000],
        umask: 0,
    }
}

fn request() -> Request {
    Request {
        uid: 1000,
        gid: 1000,
        ..Default::default()
    }
}

async fn fixture() -> (
    tempfile::TempDir,
    EntityFuse<MemoryEntityBackend, MemoryLower, LocalFsBackend>,
    u64,
    Arc<MemoryEntityBackend>,
) {
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(MemoryEntityBackend::new(1000));
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let mut lower = MemoryLower::empty();
    lower.inodes.get_mut(&1).unwrap().mode = 0o777;
    let lower = Arc::new(lower);
    let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
    lifecycle.provision(lower.binding()).await.unwrap();
    let session = lifecycle.acquire(Uuid::new_v4(), 1_000_000).await.unwrap();
    let namespace = EntityNamespace::new(backend.clone(), lower, keys, session);
    let data = EntityData::new(
        namespace.clone(),
        Arc::new(LocalFsBackend::new(temp.path())),
        4096,
    )
    .await
    .unwrap();
    let directory = namespace
        .create(
            CreateRequest {
                parent: 1,
                name: b"directory".to_vec(),
                kind: EntityKind::Directory,
                mode: 0o700,
                rdev: 0,
                symlink_target: None,
            },
            owner(),
        )
        .await
        .unwrap();
    namespace
        .create(
            CreateRequest {
                parent: directory.ino,
                name: b"child".to_vec(),
                kind: EntityKind::File,
                mode: 0o600,
                rdev: 0,
                symlink_target: None,
            },
            owner(),
        )
        .await
        .unwrap();
    let fuse = EntityFuse::new(namespace, data, 1).unwrap();
    let node = fuse.node(directory.ino);
    (temp, fuse, node, backend)
}

async fn check_both_directory_streams(
    fuse: &EntityFuse<MemoryEntityBackend, MemoryLower, LocalFsBackend>,
    node: u64,
    fh: u64,
) {
    let names: Vec<_> = fuse
        .readdir(request(), node, fh, 0)
        .await
        .unwrap()
        .entries
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, [".", "..", "child"].map(OsString::from));
    let names: Vec<_> = fuse
        .readdirplus(request(), node, fh, 0, 0)
        .await
        .unwrap()
        .entries
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, [".", "..", "child"].map(OsString::from));
}

#[tokio::test]
async fn readable_directory_without_search_permission_can_be_enumerated() {
    let (_temp, fuse, node, _backend) = fixture().await;
    let ino = fuse.ino(node).unwrap();
    fuse.state
        .namespace
        .chmod(ino, 0o400, owner())
        .await
        .unwrap();
    let opened = fuse
        .opendir(request(), node, libc::O_RDONLY as u32)
        .await
        .unwrap();
    check_both_directory_streams(&fuse, node, opened.fh).await;
    assert!(matches!(
        fuse.state.namespace.lookup(ino, b"child", &owner()).await,
        Err(EntityError::Permission)
    ));
}

#[tokio::test]
async fn directory_handle_retains_read_grant_after_chmod() {
    let (_temp, fuse, node, _backend) = fixture().await;
    let opened = fuse
        .opendir(request(), node, libc::O_RDONLY as u32)
        .await
        .unwrap();
    fuse.state
        .namespace
        .chmod(fuse.ino(node).unwrap(), 0, owner())
        .await
        .unwrap();
    check_both_directory_streams(&fuse, node, opened.fh).await;
    assert!(
        fuse.opendir(request(), node, libc::O_RDONLY as u32)
            .await
            .is_err()
    );
    assert!(
        fuse.readdir(request(), node, opened.fh + 1, 0)
            .await
            .is_err()
    );
    fuse.releasedir(request(), node, opened.fh, 0)
        .await
        .unwrap();
    assert!(fuse.readdir(request(), node, opened.fh, 0).await.is_err());
    assert!(
        fuse.readdirplus(request(), node, opened.fh, 0, 0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn directory_handle_still_requires_live_writer_authority() {
    let (_temp, fuse, node, backend) = fixture().await;
    let opened = fuse
        .opendir(request(), node, libc::O_RDONLY as u32)
        .await
        .unwrap();
    backend.set_time(2_000_000).unwrap();
    assert_eq!(
        fuse.readdir(request(), node, opened.fh, 0).await.err(),
        Some(libc::ESTALE.into())
    );
    assert_eq!(
        fuse.readdirplus(request(), node, opened.fh, 0, 0)
            .await
            .err(),
        Some(libc::ESTALE.into())
    );
}

#[tokio::test]
async fn opening_directory_still_requires_ancestor_search_permission() {
    let (_temp, fuse, node, _backend) = fixture().await;
    let root = Credentials {
        uid: 0,
        gid: 0,
        groups: vec![0],
        umask: 0,
    };
    fuse.state.namespace.chmod(1, 0o744, root).await.unwrap();
    assert_eq!(
        fuse.opendir(request(), node, libc::O_RDONLY as u32)
            .await
            .err(),
        Some(libc::EACCES.into())
    );
}

use super::*;
use crate::workspace_overlay::entity::{
    memory::MemoryEntityBackend, redis::RedisEntityBackend, resolver::test_support::MemoryLower,
};
use uuid::Uuid;

fn credentials(uid: u32) -> Credentials {
    Credentials {
        uid,
        gid: uid,
        groups: vec![uid],
        umask: 0o022,
    }
}

fn request(name: &[u8], mode: u32) -> CreateRequest {
    CreateRequest {
        parent: 1,
        name: name.to_vec(),
        kind: EntityKind::File,
        mode,
        rdev: 0,
        symlink_target: None,
    }
}

async fn exercise<B: EntityBackend + 'static>(backend: Arc<B>, volume: Uuid) {
    let mut lower = MemoryLower::empty();
    lower.add_file(1, b"lower", 2, 8);
    let lower = Arc::new(lower);
    let keys = EntityKeys::new(volume, Uuid::new_v4());
    let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
    lifecycle.provision(lower.binding()).await.unwrap();
    let session = lifecycle
        .acquire(Uuid::new_v4(), 300_000_000_000)
        .await
        .unwrap();
    let ns = EntityNamespace::new(backend, lower, keys, session);
    let root = credentials(0);
    let user = credentials(101);
    ns.chmod(1, 0o777, root.clone()).await.unwrap();
    let (inode, handle) = ns
        .create_open(request(b"private", 0), user.clone(), 6)
        .await
        .unwrap();
    assert_eq!(inode.mode, 0);
    assert_eq!(ns.validate_handle(&handle, 6), Ok(inode.ino));
    assert!(matches!(
        ns.open_inode(inode.ino, 4, &user).await,
        Err(EntityError::Permission)
    ));
    assert_eq!(ns.getattr(inode.ino, &user).await.unwrap().ino, inode.ino);
    assert!(matches!(
        ns.create_open(request(b"private", 0), user.clone(), 6)
            .await,
        Err(EntityError::Exists)
    ));
    ns.unlink(1, b"lower".to_vec(), false, root.clone())
        .await
        .unwrap();
    let entries = ns.readdir(1, &user).await.unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|(name, _)| name.as_slice())
            .collect::<Vec<_>>(),
        vec![b"private".as_slice()]
    );
    ns.unlink(1, b"private".to_vec(), false, user.clone())
        .await
        .unwrap();
    assert_eq!(ns.validate_handle(&handle, 6), Ok(inode.ino));
    assert_eq!(ns.getattr(inode.ino, &user).await.unwrap().nlink, 0);
    assert!(matches!(
        ns.open_inode(inode.ino, 4, &root).await,
        Err(EntityError::NotFound)
    ));
    ns.close(handle).unwrap();
    assert!(ns.readdir(1, &root).await.unwrap().is_empty());

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let ns = ns.clone();
        let user = user.clone();
        tasks.push(tokio::spawn(async move {
            ns.create_open(request(b"exclusive", 0o600), user, 6).await
        }));
    }
    let mut winners = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok((inode, handle)) => {
                winners += 1;
                assert_eq!(ns.validate_handle(&handle, 6), Ok(inode.ino));
                ns.close(handle).unwrap();
            }
            Err(error) => assert_eq!(error, EntityError::Exists),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(ns.readdir(1, &root).await.unwrap().len(), 1);
}

#[tokio::test]
async fn memory_entity_mount_namespace_operations() {
    exercise(Arc::new(MemoryEntityBackend::new(1)), Uuid::new_v4()).await;
}

#[tokio::test]
#[ignore = "requires BREWFS_TEST_REDIS_URL pointing to an isolated Redis"]
async fn redis_entity_mount_namespace_operations() {
    let volume = Uuid::new_v4();
    let url = std::env::var("BREWFS_TEST_REDIS_URL").expect("BREWFS_TEST_REDIS_URL");
    exercise(
        Arc::new(RedisEntityBackend::connect(&url, volume).await.unwrap()),
        volume,
    )
    .await;
}

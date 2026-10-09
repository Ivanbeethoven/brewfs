//! Actual adapter OPEN/RELEASE and mount budget, not an allocator event probe.
use super::*;

#[tokio::test]
async fn g07_existing_api_roots_exhaustion_precedes_client_open_and_recovers() {
    let mut limits = V3BudgetLimits::default();
    limits.bytes[V3BudgetPool::Control as usize] = 32 << 10;
    limits.bytes[V3BudgetPool::Metadata as usize] = 64 << 20;
    let budget = V3MountBudget::new(limits).unwrap();
    budget.validate_frame_capability(8 << 20).unwrap();
    let f = fixture_with_budget(budget).await;
    assert!(f.fs.meta_layer().install_v3_stats(f.fs.stats()));
    let idle = f.budget.state().used;
    let remaining = f.budget.capacity(V3BudgetPool::Roots) - idle[V3BudgetPool::Roots as usize];
    assert!(remaining > 0);
    let held = f.budget.admit(&[(V3BudgetPool::Roots, remaining)]).unwrap();
    let full = f.budget.state().used;
    assert_eq!(
        full[V3BudgetPool::Roots as usize],
        f.budget.capacity(V3BudgetPool::Roots)
    );
    for (ino, directory, unique) in [
        (f.ino, false, 1901),
        (1, true, 1902),
        (STATS_INODE, false, 1903),
    ] {
        let request_owner = Filesystem::reserve_request_memory(&f.fs, 8)
            .unwrap()
            .unwrap();
        let opened = if directory {
            Filesystem::opendir(&f.fs, request(unique), ino, libc::O_RDONLY as u32).await
        } else {
            Filesystem::open(&f.fs, request(unique), ino, libc::O_RDONLY as u32).await
        };
        drop(request_owner);
        assert_eq!(opened.err(), Some(Errno::from(libc::ENOMEM)));
        assert_eq!(f.fs.open_file_handle_count(), 0);
        assert_eq!(f.budget.state().used, full);
    }
    // The source must separately show preadmission before client Box::new.
    // The assertions above measure API side effects and budget conservation;
    // they cannot observe a speculative allocation followed by deallocation.
    drop(held);
    assert_eq!(f.budget.state().used, idle);
    for (ino, directory, unique) in [
        (f.ino, false, 1911),
        (1, true, 1912),
        (STATS_INODE, false, 1913),
    ] {
        let request_owner = Filesystem::reserve_request_memory(&f.fs, 8)
            .unwrap()
            .unwrap();
        let opened = if directory {
            Filesystem::opendir(&f.fs, request(unique), ino, libc::O_RDONLY as u32)
                .await
                .unwrap()
        } else {
            Filesystem::open(&f.fs, request(unique), ino, libc::O_RDONLY as u32)
                .await
                .unwrap()
        };
        drop(request_owner);
        assert_ne!(opened.fh, 0);
        assert!(
            f.budget.state().used[V3BudgetPool::Roots as usize]
                > idle[V3BudgetPool::Roots as usize]
        );
        let release_owner = Filesystem::reserve_request_memory(&f.fs, 24)
            .unwrap()
            .unwrap();
        if directory {
            Filesystem::releasedir(&f.fs, request(unique + 100), ino, opened.fh, 0)
                .await
                .unwrap();
        } else {
            Filesystem::release(&f.fs, request(unique + 100), ino, opened.fh, 0, 0, false)
                .await
                .unwrap();
        }
        drop(release_owner);
        assert_eq!(f.fs.open_file_handle_count(), 0);
        assert!(f.fs.virtual_stats_snapshot(opened.fh).is_none());
        assert_eq!(
            f.budget.state().used[V3BudgetPool::Roots as usize],
            idle[V3BudgetPool::Roots as usize]
        );
        assert_eq!(
            f.budget.state().used[V3BudgetPool::Control as usize],
            idle[V3BudgetPool::Control as usize]
        );
    }
    assert!(f.budget.state().peak[V3BudgetPool::Control as usize] <= 32768);
    Filesystem::prepare_unmount(&f.fs).await.unwrap();
}

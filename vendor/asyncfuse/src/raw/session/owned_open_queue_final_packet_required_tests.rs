//! Regression for the actual final OPEN packet's complete Roots lifetime.
//! The same strict oracle rejects an implementation that charges only its Box.
use super::*;

#[tokio::test]
async fn stored_open_roots_survive_actual_box_retirement_until_actual_final_packet_drop() {
    let fs = Arc::new(ProbeFs::default());
    let mut session = Session::new(MountOptions::default()).with_workers(1, 1);
    let mut receiver = session.response_receivers[0].take().unwrap();
    session.ensure_workers(fs.clone()).unwrap();
    let baseline = fs.roots.load(Ordering::Acquire);
    let actual_future_bytes = worker::readonly_ordinary_future_layout::<ProbeFs>().0;
    session
        .workers
        .as_ref()
        .unwrap()
        .submit(item(&session, &fs, fuse_opcode::FUSE_OPEN, 10));
    entered(&fs, 1).await;
    assert!(fs.roots.load(Ordering::Acquire) >= baseline + actual_future_bytes);
    fs.release_opens();
    let reply = packet(&mut receiver).await;
    assert_eq!(header(&reply).unique, 10);
    assert_eq!(header(&reply).error, 0);
    // Await actual successful worker cleanup to prove the Box and lane buffer
    // Drop completed, while the real final FuseData/Bytes remains held here.
    session.workers.as_mut().unwrap().shutdown().await;
    let with_held_actual_packet = fs.roots.load(Ordering::Acquire);
    let after_lane_baseline = baseline - OpenLanePlan::storage_bytes(1, 1).unwrap() as usize;
    assert_eq!(session.inflight.load(Ordering::Acquire), 1);
    drop(reply);
    assert_eq!(session.inflight.load(Ordering::Acquire), 0);
    // Fail only after real userspace cleanup: step01 lacks the final-packet
    // retirement controller/marker. Never label Box-only charge full GREEN.
    assert!(
        with_held_actual_packet >= after_lane_baseline + actual_future_bytes,
        "full Stored Roots lifetime OPEN: actual Box/lane gone, final packet still held"
    );
}

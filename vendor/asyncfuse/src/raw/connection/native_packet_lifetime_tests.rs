//! Test actual native WriteRequest/PendingIo/oneshot paths without a FUSE mount.
use super::*;
use crate::raw::session::owned_open_queue::tests::actual_packet_after_real_box_retirement;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn closed_actual_native_completion_receiver_reaps_final_packet_without_new_input() {
    let (packet, used, baseline, _session) = actual_packet_after_real_box_retirement().await;
    let held = used.load(Ordering::Acquire);
    assert!(held > baseline);
    let (tx, rx) = oneshot::channel();
    let pending = PendingIo::Write(InflightWrite {
        req: WriteRequest {
            data: packet.clone(),
            body_extend: None,
            reply: tx,
        },
        _iovecs: Box::new(
            [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 2],
        ),
    });
    drop(packet); // actor/caller gone, native's actual clone remains
    drop(rx); // actual completion send will return its owned payload
    assert_eq!(used.load(Ordering::Acquire), held);
    pending.complete(Err(io::Error::from_raw_os_error(libc::EIO)));
    assert_eq!(
        used.load(Ordering::Acquire),
        baseline,
        "failed oneshot payload Drop must make real progress"
    );
}

#[tokio::test]
async fn actual_native_completion_payload_keeps_packet_until_received_buffers_drop() {
    let (packet, used, baseline, _session) = actual_packet_after_real_box_retirement().await;
    let held = used.load(Ordering::Acquire);
    let (tx, rx) = oneshot::channel();
    let pending = PendingIo::Write(InflightWrite {
        req: WriteRequest {
            data: packet.clone(),
            body_extend: None,
            reply: tx,
        },
        _iovecs: Box::new(
            [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 2],
        ),
    });
    drop(packet);
    pending.complete(Ok(32));
    assert_eq!(
        used.load(Ordering::Acquire),
        held,
        "real oneshot payload still owns Bytes"
    );
    let (buffers, result) = rx.await.unwrap();
    assert_eq!(result.unwrap(), 32);
    assert_eq!(used.load(Ordering::Acquire), held);
    drop(buffers);
    assert_eq!(used.load(Ordering::Acquire), baseline);
}

// Public/native value layouts; opaque channel/task heap cells remain OPEN.
#[test]
fn actual_native_packet_queue_value_layout_receipt() {
    fn metric<T>(name: &str) {
        let layout = std::alloc::Layout::new::<T>();
        eprintln!(
            "BREWFS_ACTUAL_NATIVE_VALUE_LAYOUT name={name} bytes={} align={}",
            layout.size(),
            layout.align()
        );
    }
    type WriteCompletion = CompleteIoResult<(Bytes, Option<Bytes>), usize>;
    metric::<WriteRequest>("WriteRequest");
    metric::<ReadRequest>("ReadRequest");
    metric::<RingRequest>("RingRequest");
    metric::<InflightWrite>("InflightWrite");
    metric::<PendingIo>("PendingIo");
    metric::<[libc::iovec; 2]>("actual_iovec_array");
    metric::<(Bytes, Option<Bytes>)>("write_buffers_tuple");
    metric::<WriteCompletion>("write_completion_payload");
    metric::<oneshot::Sender<WriteCompletion>>("oneshot_sender_value_only");
    metric::<oneshot::Receiver<WriteCompletion>>("oneshot_receiver_value_only");
    metric::<mpsc::Sender<RingRequest>>("mpsc_sender_value_only");
    metric::<mpsc::Receiver<RingRequest>>("mpsc_receiver_value_only");
}

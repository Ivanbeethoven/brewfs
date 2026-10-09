//! Exercise the real Request -> generated client -> tonic Channel HTTP/2 bridge.
//! The peer uses a memory duplex stream; this fixture opens no socket.

use super::*;

use std::convert::Infallible;
use std::future::{ready, Future, Ready};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::future::poll_fn;
use futures::StreamExt;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::Notify;
use tonic::codegen::{http, Body, Bytes, Service};
use tonic::server::NamedService;
use tonic::transport::server::Connected;
use tonic::transport::{Endpoint, Server};

use crate::store::KvClient;

#[derive(Default)]
struct Receipts {
    timeouts: Mutex<Vec<Duration>>,
    versions: Mutex<Vec<u64>>,
    calls: AtomicUsize,
    inline_entered: AtomicUsize,
    inline_dropped: AtomicUsize,
    observed: Notify,
}

struct InlineReceipt(Arc<Receipts>);

impl Drop for InlineReceipt {
    fn drop(&mut self) {
        self.0.inline_dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct ObservedGet {
    get: kvrpcpb::GetRequest,
    receipts: Arc<Receipts>,
}

#[async_trait]
impl Request for ObservedGet {
    async fn dispatch(
        &self,
        client: &TikvClient<Channel>,
        timeout: Duration,
    ) -> Result<Box<dyn Any>> {
        self.receipts.inline_entered.fetch_add(1, Ordering::SeqCst);
        let _receipt = InlineReceipt(self.receipts.clone());
        // The receipt surrounds the original Get dispatch, including its
        // generated client future; this is not a substitute mock RPC.
        self.get.dispatch(client, timeout).await
    }

    fn label(&self) -> &'static str {
        self.get.label()
    }

    fn as_any(&self) -> &dyn Any {
        self.get.as_any()
    }

    fn set_leader(&mut self, leader: &RegionWithLeader) -> Result<()> {
        self.get.set_leader(leader)
    }

    fn set_api_version(&mut self, version: kvrpcpb::ApiVersion) {
        self.get.set_api_version(version);
    }
}

impl KvRequest for ObservedGet {
    type Response = kvrpcpb::GetResponse;
}

struct RealGetClient {
    client: TikvClient<Channel>,
    configured_timeout: Duration,
}

#[async_trait]
impl KvClient for RealGetClient {
    async fn dispatch(&self, request: &dyn Request) -> Result<Box<dyn Any>> {
        request
            .dispatch(&self.client, self.configured_timeout)
            .await
    }
}

fn real_plan(
    client: &TikvClient<Channel>,
    receipts: &Arc<Receipts>,
    configured_timeout: Duration,
    deadline: Instant,
) -> BoundedDeadlineDispatch<ObservedGet> {
    BoundedDeadlineDispatch {
        inner: Dispatch {
            request: ObservedGet {
                get: kvrpcpb::GetRequest {
                    key: b"bridge-test-key".to_vec(),
                    version: 123,
                    ..Default::default()
                },
                receipts: receipts.clone(),
            },
            kv_client: Some(Arc::new(RealGetClient {
                client: client.clone(),
                configured_timeout,
            })),
        },
        deadline,
    }
}

fn parse_timeout(value: &http::HeaderValue) -> Duration {
    let value = value.to_str().unwrap().as_bytes();
    assert!((2..=9).contains(&value.len()));
    assert!(value[..value.len() - 1].iter().all(u8::is_ascii_digit));
    let amount: u64 = std::str::from_utf8(&value[..value.len() - 1])
        .unwrap()
        .parse()
        .unwrap();
    assert!(amount > 0);
    match value[value.len() - 1] {
        b'H' => Duration::from_secs(amount * 3600),
        b'M' => Duration::from_secs(amount * 60),
        b'S' => Duration::from_secs(amount),
        b'm' => Duration::from_millis(amount),
        b'u' => Duration::from_micros(amount),
        b'n' => Duration::from_nanos(amount),
        _ => panic!("invalid grpc-timeout unit"),
    }
}

#[derive(Clone)]
struct WirePeer {
    receipts: Arc<Receipts>,
    hold_response: bool,
}

impl NamedService for WirePeer {
    const NAME: &'static str = "tikvpb.Tikv";
}

impl<B> Service<http::Request<B>> for WirePeer
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::fmt::Debug + Send,
{
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<std::result::Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let receipts = self.receipts.clone();
        let hold_response = self.hold_response;
        assert_eq!(request.uri().path(), "/tikvpb.Tikv/KvGet");
        let timeout = parse_timeout(request.headers().get("grpc-timeout").unwrap());
        Box::pin(async move {
            let mut body = Box::pin(request.into_body());
            let mut frame = Vec::new();
            while let Some(chunk) = poll_fn(|cx| body.as_mut().poll_data(cx)).await {
                frame.extend_from_slice(&chunk.unwrap());
                assert!(
                    frame.len() <= 256,
                    "test request exceeded its fixed envelope"
                );
            }
            assert!(frame.len() >= 5);
            assert_eq!(frame[0], 0);
            let length = u32::from_be_bytes(frame[1..5].try_into().unwrap()) as usize;
            assert_eq!(length, frame.len() - 5);
            let get = kvrpcpb::GetRequest::decode(&frame[5..]).unwrap();
            assert_eq!(get.key, b"bridge-test-key");
            receipts.timeouts.lock().unwrap().push(timeout);
            receipts.versions.lock().unwrap().push(get.version);
            receipts.calls.fetch_add(1, Ordering::SeqCst);
            receipts.observed.notify_one();
            if hold_response {
                return futures::future::pending().await;
            }
            let response = kvrpcpb::GetResponse {
                not_found: true,
                ..Default::default()
            };
            let payload = response.encode_to_vec();
            let mut frame = vec![0];
            frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            frame.extend(payload);
            let mut response = http::Response::new(FrameBody(Some(frame.into())).boxed_unsync());
            response
                .headers_mut()
                .insert("content-type", "application/grpc".parse().unwrap());
            Ok(response)
        })
    }
}

struct FrameBody(Option<Bytes>);

impl Body for FrameBody {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Bytes, Self::Error>>> {
        Poll::Ready(self.0.take().map(Ok))
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<std::result::Result<Option<http::HeaderMap>, Self::Error>> {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        Poll::Ready(Ok(Some(trailers)))
    }
}

struct PeerIo(DuplexStream);

impl Connected for PeerIo {
    type ConnectInfo = ();
    fn connect_info(&self) {}
}

impl AsyncRead for PeerIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PeerIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

struct OnceConnector(Option<DuplexStream>);

impl Service<http::Uri> for OnceConnector {
    type Response = DuplexStream;
    type Error = std::io::Error;
    type Future = Ready<std::io::Result<DuplexStream>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: http::Uri) -> Self::Future {
        ready(self.0.take().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "test connector has no second stream",
            )
        }))
    }
}

async fn with_wire_peer<F, Fut>(hold_response: bool, test: F)
where
    F: FnOnce(TikvClient<Channel>, Arc<Receipts>) -> Fut,
    Fut: Future<Output = ()>,
{
    let receipts = Arc::new(Receipts::default());
    let (client_io, server_io) = tokio::io::duplex(16 << 10);
    let incoming = futures::stream::once(async { Ok::<_, std::io::Error>(PeerIo(server_io)) })
        .chain(futures::stream::pending());
    let server = Server::builder()
        .add_service(WirePeer {
            receipts: receipts.clone(),
            hold_response,
        })
        .serve_with_incoming(incoming);
    let client = async {
        let channel = Endpoint::from_static("http://bridge-test.invalid")
            .connect_with_connector(OnceConnector(Some(client_io)))
            .await
            .unwrap();
        let client = TikvClient::new(channel).max_decoding_message_size(16 << 10);
        test(client, receipts).await;
    };
    // Poll fixture server and client inline. Only tonic's existing transport
    // driver internals create tasks; this adapter/fixture adds no explicit task.
    tokio::select! {
        _ = client => {},
        result = server => panic!("test peer ended early: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("bridge fixture did not finish"),
    }
}

#[tokio::test]
async fn real_http_get_timeout_decreases_with_the_same_absolute_deadline() {
    with_wire_peer(false, |client, receipts| async move {
        let deadline = Instant::now() + Duration::from_secs(2);
        let plan = real_plan(&client, &receipts, Duration::from_secs(5), deadline);
        assert!(plan.execute().await.unwrap().not_found);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(plan.execute().await.unwrap().not_found);
        let timeouts = receipts.timeouts.lock().unwrap();
        assert_eq!(timeouts.len(), 2);
        assert!(timeouts[0] <= Duration::from_secs(2));
        assert!(timeouts[1] < timeouts[0]);
        assert!(timeouts[0] - timeouts[1] >= Duration::from_millis(20));
        assert_eq!(*receipts.versions.lock().unwrap(), vec![123, 123]);
        assert_eq!(receipts.calls.load(Ordering::SeqCst), 2);
        assert_eq!(receipts.inline_entered.load(Ordering::SeqCst), 2);
        assert_eq!(receipts.inline_dropped.load(Ordering::SeqCst), 2);
    })
    .await;
}

#[tokio::test]
async fn real_http_expired_or_zero_timeout_sends_no_get() {
    with_wire_peer(false, |client, receipts| async move {
        for (configured, deadline) in [
            (Duration::from_secs(5), Instant::now()),
            (Duration::ZERO, Instant::now() + Duration::from_secs(2)),
        ] {
            let result = real_plan(&client, &receipts, configured, deadline)
                .execute()
                .await;
            assert!(matches!(result, Err(Error::StringError(_))));
        }
        // Exercise the Request adapter's own expiration check without the
        // Plan's earlier guard, using the real connected generated client.
        let request = DeadlineRequest {
            inner: ObservedGet {
                get: kvrpcpb::GetRequest::default(),
                receipts: receipts.clone(),
            },
            deadline: Instant::now(),
        };
        assert!(matches!(
            request.dispatch(&client, Duration::from_secs(5)).await,
            Err(Error::StringError(_))
        ));
        assert_eq!(receipts.calls.load(Ordering::SeqCst), 0);
        assert_eq!(receipts.inline_entered.load(Ordering::SeqCst), 0);
        assert_eq!(receipts.inline_dropped.load(Ordering::SeqCst), 0);
        assert!(receipts.timeouts.lock().unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn real_http_held_get_deadline_drops_inline_future_without_retry() {
    with_wire_peer(true, |client, receipts| async move {
        let plan = real_plan(
            &client,
            &receipts,
            Duration::from_secs(5),
            Instant::now() + Duration::from_millis(500),
        );
        let result = plan.execute().await;
        match result {
            Err(Error::StringError(message)) => {
                assert_eq!(message, "bounded read data-batch deadline exhausted")
            }
            Err(Error::GrpcAPI(status)) => assert!(matches!(
                status.code(),
                tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
            )),
            _ => panic!("held RPC did not fail at its deadline"),
        }
        assert_eq!(receipts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.inline_entered.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.inline_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(*receipts.versions.lock().unwrap(), vec![123]);
    })
    .await;
}

#[tokio::test]
async fn real_http_caller_drop_releases_held_get_inline_future() {
    with_wire_peer(true, |client, receipts| async move {
        let plan = real_plan(
            &client,
            &receipts,
            Duration::from_secs(5),
            Instant::now() + Duration::from_secs(2),
        );
        let mut future = Box::pin(plan.execute());
        tokio::select! {
            _ = receipts.observed.notified() => {},
            _ = &mut future => panic!("held Get unexpectedly completed"),
        }
        assert_eq!(receipts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.inline_entered.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.inline_dropped.load(Ordering::SeqCst), 0);
        drop(future);
        assert_eq!(receipts.inline_dropped.load(Ordering::SeqCst), 1);
        // Observe the immediate count after cancellation. Static review of the
        // bridge separately establishes that it has no spawn or retry loop.
        tokio::task::yield_now().await;
        assert_eq!(receipts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*receipts.versions.lock().unwrap(), vec![123]);
    })
    .await;
}

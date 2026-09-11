mod fs;
mod path;
mod props;

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderValue, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, Method, Request, Response, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use dav_server::DavHandler;
use dav_server::body::Body as DavBody;
use dav_server::memls::MemLs;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::chunk::store::BlockStore;
use crate::meta::MetaStore;
use crate::meta::client::MetaClient;
use crate::meta::config::{CacheTtl, CompactConfig, MetaClientConfig};
use crate::meta::layer::MetaLayer;
use crate::vfs::cache::config::CacheConfig as VfsCacheConfig;
use crate::vfs::fs::VFS;

use self::fs::BrewFsDavFs;

tokio::task_local! {
    static REQUEST_METHOD: Method;
    static REQUEST_FLAGS: Arc<RequestFlags>;
    static REQUEST_IF_MATCH: Option<String>;
}

#[derive(Default)]
struct RequestFlags {
    invalid_body_length: AtomicBool,
    directory_not_empty: AtomicBool,
    precondition_failed: AtomicBool,
}

pub(super) fn mark_precondition_failed() {
    let _ = REQUEST_FLAGS.try_with(|flags| {
        flags.precondition_failed.store(true, Ordering::Relaxed);
    });
}

pub(super) fn mark_directory_not_empty() {
    let _ = REQUEST_FLAGS.try_with(|flags| {
        flags.directory_not_empty.store(true, Ordering::Relaxed);
    });
}

pub(super) fn current_request_if_match() -> Option<String> {
    REQUEST_IF_MATCH
        .try_with(|value| value.clone())
        .ok()
        .flatten()
}

pub(super) fn current_request_is_lock() -> bool {
    REQUEST_METHOD
        .try_with(|method| method.as_str() == "LOCK")
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct TlsOptions {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WebDavGatewayOptions {
    pub listen_addr: SocketAddr,
    pub credentials: Option<(String, String)>,
    pub tls: Option<TlsOptions>,
    pub atomic_put: bool,
}

#[derive(Clone)]
struct AuthConfig {
    principal: Arc<str>,
    user_digest: [u8; 32],
    password_digest: [u8; 32],
}

impl AuthConfig {
    fn new(user: String, password: String) -> Self {
        Self {
            principal: Arc::from(user.as_str()),
            user_digest: digest(user.as_bytes()),
            password_digest: digest(password.as_bytes()),
        }
    }

    fn authenticate(&self, value: &str) -> bool {
        let Some((scheme, encoded)) = value.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("basic") || encoded.is_empty() {
            return false;
        }
        let Ok(decoded) = STANDARD.decode(encoded) else {
            return false;
        };
        let Some(separator) = decoded.iter().position(|byte| *byte == b':') else {
            return false;
        };
        digest_eq(&digest(&decoded[..separator]), &self.user_digest)
            & digest_eq(&digest(&decoded[separator + 1..]), &self.password_digest)
    }
}

#[derive(Clone)]
struct ServerState {
    handler: DavHandler,
    auth: Option<AuthConfig>,
    atomic_put: bool,
}

pub async fn serve<S>(
    store: S,
    meta: Arc<dyn MetaStore>,
    layout: crate::chunk::ChunkLayout,
    compact: CompactConfig,
    cache: VfsCacheConfig,
    meta_ttl: CacheTtl,
    opts: WebDavGatewayOptions,
) -> anyhow::Result<()>
where
    S: BlockStore + Send + Sync + 'static,
{
    let props_supported = meta.capabilities().xattr;
    let mut config = MetaClientConfig {
        ttl: meta_ttl,
        ..Default::default()
    };
    config.options.mount_point = Some("brewfs-gateway-webdav".to_string());
    let meta_client = MetaClient::with_options(
        meta,
        config.capacity.clone(),
        config.effective_ttl(),
        config.options,
    );
    if let Err(error) = meta_client.initialize().await {
        meta_client.shutdown_runtime().await;
        return Err(anyhow::Error::from(error));
    }
    if let Err(error) = meta_client.start_control_plane().await {
        meta_client.shutdown_runtime().await;
        return Err(anyhow::Error::from(error));
    }

    let vfs = match VFS::with_meta_layer_with_cache_config(
        layout,
        Arc::new(store),
        meta_client.clone(),
        compact,
        cache,
    ) {
        Ok(vfs) => vfs,
        Err(error) => {
            meta_client.shutdown_runtime().await;
            return Err(anyhow::anyhow!("create VFS: {error}"));
        }
    };

    let filesystem = BrewFsDavFs::new(vfs, opts.atomic_put, props_supported);
    if let Err(error) = filesystem.initialize().await {
        meta_client.shutdown_runtime().await;
        return Err(error);
    }
    let cleanup_fs = filesystem.clone();
    let cleanup_cancel = CancellationToken::new();
    let cleanup_task = tokio::spawn({
        let cleanup_cancel = cleanup_cancel.clone();
        async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = cleanup_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(error) = cleanup_fs.cleanup_stale_staging().await {
                            tracing::warn!(error = %error, "webdav staging cleanup failed");
                        }
                    }
                }
            }
        }
    });

    let handler = DavHandler::builder()
        .filesystem(Box::new(filesystem))
        .locksystem(MemLs::new())
        .build_handler();
    let auth = opts
        .credentials
        .map(|(user, password)| AuthConfig::new(user, password));
    if auth.is_none() {
        tracing::warn!("webdav gateway allows anonymous read/write access");
    } else if opts.tls.is_none() {
        tracing::warn!("webdav Basic authentication is exposed over plaintext HTTP");
    }

    let app = axum::Router::new()
        .fallback(webdav_request)
        .with_state(ServerState {
            handler,
            auth,
            atomic_put: opts.atomic_put,
        });

    let serve_result: anyhow::Result<()> = async {
        if let Some(tls) = opts.tls {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(tls.cert, tls.key)
                .await
                .map_err(|error| anyhow::anyhow!("load WebDAV TLS certificate/key: {error}"))?;
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            let shutdown_task = tokio::spawn(async move {
                wait_for_shutdown().await;
                shutdown_handle.graceful_shutdown(Some(Duration::from_secs(30)));
            });
            tracing::info!(listen = %opts.listen_addr, "brewfs webdav gateway listening with TLS");
            let result = axum_server::bind_rustls(opts.listen_addr, config)
                .handle(handle)
                .serve(app.into_make_service())
                .await;
            shutdown_task.abort();
            result?;
        } else {
            let listener = tokio::net::TcpListener::bind(opts.listen_addr).await?;
            tracing::info!(listen = %opts.listen_addr, "brewfs webdav gateway listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(wait_for_shutdown())
                .await?;
        }
        Ok(())
    }
    .await;

    cleanup_cancel.cancel();
    let _ = cleanup_task.await;
    meta_client.shutdown_runtime().await;
    serve_result
}

async fn wait_for_shutdown() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %error, "failed to listen for WebDAV shutdown signal");
    }
}

async fn webdav_request(
    State(state): State<ServerState>,
    request: Request<Body>,
) -> Response<Body> {
    let principal = match &state.auth {
        Some(auth) => {
            let Some(value) = request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
            else {
                return unauthorized();
            };
            if !auth.authenticate(value) {
                return unauthorized();
            }
            auth.principal.to_string()
        }
        None => "anonymous".to_string(),
    };
    let expected_size =
        if state.atomic_put && matches!(request.method(), &Method::PUT | &Method::PATCH) {
            request
                .headers()
                .get(CONTENT_LENGTH)
                .or_else(|| request.headers().get("x-expected-entity-length"))
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| {
                    request
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(content_range_length)
                })
        } else {
            None
        };
    let request_flags = Arc::new(RequestFlags::default());
    let if_match = request
        .headers()
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let request = if let Some(expected_size) = expected_size {
        request.map(|body| bounded_body(body, expected_size, request_flags.clone()))
    } else {
        request
    };
    let method = request.method().clone();
    // RFC 4918 §9.1 allows servers to refuse infinite-depth PROPFIND.
    // dav-server 0.11 gates this on the `x-litmus` header (a litmus test
    // suite escape hatch), so gate it here instead where it applies to
    // every client.
    if infinite_depth_requested(&method, request.headers()) {
        return infinite_depth_rejected();
    }
    let response = REQUEST_METHOD
        .scope(
            method,
            REQUEST_FLAGS.scope(
                request_flags.clone(),
                REQUEST_IF_MATCH.scope(
                    if_match,
                    state.handler.handle_guarded(request, principal, ()),
                ),
            ),
        )
        .await;
    let (parts, body): (_, DavBody) = response.into_parts();
    let mut response = Response::from_parts(parts, Body::new(body));
    if request_flags.invalid_body_length.load(Ordering::Relaxed) {
        *response.status_mut() = StatusCode::BAD_REQUEST;
    } else if request_flags.precondition_failed.load(Ordering::Relaxed) {
        *response.status_mut() = StatusCode::PRECONDITION_FAILED;
    } else if request_flags.directory_not_empty.load(Ordering::Relaxed)
        && response.status() == StatusCode::METHOD_NOT_ALLOWED
    {
        *response.status_mut() = StatusCode::CONFLICT;
    }
    response
}

fn content_range_length(value: &HeaderValue) -> Option<u64> {
    let value = value.to_str().ok()?;
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, _) = range.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    end.checked_sub(start)?.checked_add(1)
}

fn bounded_body(body: Body, expected_size: u64, invalid_body_length: Arc<RequestFlags>) -> Body {
    let seen = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let chunks = body.into_data_stream().map({
        let seen = seen.clone();
        let failed = failed.clone();
        let invalid_body_length = invalid_body_length.clone();
        move |chunk| {
            let chunk = chunk.map_err(|error| {
                failed.store(true, Ordering::Relaxed);
                io::Error::other(error)
            })?;
            let length = chunk.len() as u64;
            let previous = seen.fetch_add(length, Ordering::Relaxed);
            if previous
                .checked_add(length)
                .is_none_or(|total| total > expected_size)
            {
                failed.store(true, Ordering::Relaxed);
                invalid_body_length
                    .invalid_body_length
                    .store(true, Ordering::Relaxed);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request body exceeds declared length",
                ));
            }
            Ok(chunk)
        }
    });
    let eof = futures_util::stream::once(async move {
        if failed.load(Ordering::Relaxed) || seen.load(Ordering::Relaxed) == expected_size {
            None
        } else {
            invalid_body_length
                .invalid_body_length
                .store(true, Ordering::Relaxed);
            Some(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request body is shorter than declared length",
            )))
        }
    })
    .filter_map(std::future::ready);
    Body::from_stream(chunks.chain(eof))
}

fn infinite_depth_requested(method: &Method, headers: &HeaderMap) -> bool {
    matches!(method.as_str(), "PROPFIND" | "REPORT")
        && headers
            .get("depth")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("infinity"))
}

fn infinite_depth_rejected() -> Response<Body> {
    // 501 matches dav-server 0.11's own infinite-depth PROPFIND response
    // (`propfind-finite-depth` error element).
    Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .header(CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:error xmlns:D="DAV:"><D:propfind-finite-depth/></D:error>"#,
        ))
        .expect("valid infinite-depth response")
}

fn unauthorized() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            WWW_AUTHENTICATE,
            r#"Basic realm="BrewFS WebDAV", charset="UTF-8""#,
        )
        .body(Body::empty())
        .expect("valid unauthorized response")
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn digest_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_accepts_exact_credentials() {
        let auth = AuthConfig::new("alice".to_string(), "s:cret".to_string());
        let encoded = STANDARD.encode("alice:s:cret");
        assert!(auth.authenticate(&format!("Basic {encoded}")));
        assert!(!auth.authenticate(&format!("Basic {}", STANDARD.encode("alice:wrong"))));
        assert!(!auth.authenticate("Bearer token"));
    }

    #[tokio::test]
    async fn request_method_scope_identifies_lock() {
        assert!(!current_request_is_lock());
        assert!(
            REQUEST_METHOD
                .scope(Method::from_bytes(b"LOCK").unwrap(), async {
                    current_request_is_lock()
                },)
                .await
        );
        assert!(
            !REQUEST_METHOD
                .scope(Method::PUT, async { current_request_is_lock() })
                .await
        );
    }

    #[test]
    fn infinite_depth_gating_matches_only_propfind_and_report() {
        let depth = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert("depth", HeaderValue::from_static(value));
            headers
        };
        let propfind = Method::from_bytes(b"PROPFIND").unwrap();
        let report = Method::from_bytes(b"REPORT").unwrap();

        assert!(infinite_depth_requested(&propfind, &depth("infinity")));
        assert!(infinite_depth_requested(&report, &depth("infinity")));
        assert!(infinite_depth_requested(&propfind, &depth("INFINITY")));
        assert!(!infinite_depth_requested(&Method::GET, &depth("infinity")));
        assert!(!infinite_depth_requested(&Method::PUT, &depth("infinity")));
        assert!(!infinite_depth_requested(&propfind, &depth("1")));
        assert!(!infinite_depth_requested(&propfind, &depth("0")));
        assert!(!infinite_depth_requested(&propfind, &HeaderMap::new()));
    }

    #[test]
    fn infinite_depth_response_is_501_with_dav_error_body() {
        let response = infinite_depth_rejected();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/xml; charset=utf-8")),
        );
    }

    #[test]
    fn content_range_length_parses_byte_ranges() {
        assert_eq!(
            content_range_length(&HeaderValue::from_static("bytes 0-5/6")),
            Some(6)
        );
        assert_eq!(
            content_range_length(&HeaderValue::from_static("bytes 5-5/*")),
            Some(1)
        );
        assert_eq!(
            content_range_length(&HeaderValue::from_static("bytes 6-5/6")),
            None
        );
        assert_eq!(
            content_range_length(&HeaderValue::from_static("items 0-5/6")),
            None
        );
    }

    #[tokio::test]
    async fn bounded_body_accepts_exact_length() {
        let body = bounded_body(Body::from("hello"), 5, Arc::new(RequestFlags::default()));
        assert_eq!(axum::body::to_bytes(body, 6).await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn bounded_body_rejects_short_and_long_input() {
        assert!(
            axum::body::to_bytes(
                bounded_body(Body::from("short"), 6, Arc::new(RequestFlags::default())),
                7,
            )
            .await
            .is_err()
        );
        assert!(
            axum::body::to_bytes(
                bounded_body(Body::from("too long"), 7, Arc::new(RequestFlags::default())),
                9,
            )
            .await
            .is_err()
        );
    }
}

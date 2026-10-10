//! S3 adapter: simplified aws-sdk-s3 implementation with multipart upload, retries, and validation.

use crate::cadapter::client::{ObjectBackend, ObjectByteStream};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::timeout::TimeoutConfig;
use aws_sdk_s3::config::RequestChecksumCalculation;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::{ByteStream, SdkBody};
use aws_sdk_s3::{Client, config::Region};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use bytes::Bytes;
use futures_util::StreamExt;
use hyper::Body;
use md5;
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use tokio_util::io::ReaderStream;

/// S3 backend configuration options
#[derive(Debug, Clone)]
pub struct S3Config {
    /// S3 bucket name
    pub bucket: String,
    /// AWS region (optional, will use default if not specified)
    pub region: Option<String>,
    /// Part size for multipart uploads in bytes (default: 8MB)
    pub part_size: usize,
    /// Maximum concurrent multipart upload parts (default: 4)
    pub max_concurrency: usize,
    /// Maximum retry attempts for failed operations (default: 3)
    pub max_retries: u32,
    /// Base delay for exponential backoff in milliseconds (default: 100ms)
    pub retry_base_delay: u64,
    /// Enable MD5 checksums for uploads (default: false, matching JuiceFS behavior)
    pub enable_md5: bool,
    /// Custom endpoint URL (e.g. for MinIO or localstack)
    pub endpoint: Option<String>,
    /// Force path-style access (required for some S3-compatible services)
    pub force_path_style: bool,
    /// Disable SDK-level request checksums and SigV4 payload hashing.
    /// Safe for self-hosted S3 backends (RustFS/MinIO) over trusted networks.
    pub disable_payload_checksum: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: None,
            part_size: 16 * 1024 * 1024, // 16MB — larger parts reduce HTTP overhead
            max_concurrency: 32,         // Raise S3 parallelism to keep multi-job reads saturated
            max_retries: 1,
            retry_base_delay: 100,
            enable_md5: false,
            endpoint: None,
            force_path_style: false,
            disable_payload_checksum: true,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct S3Backend {
    client: Client,
    config: S3Config,
    mutation_replay_forbidden: bool,
}

#[derive(Clone, Copy)]
enum S3MutationReplay {
    Inherit,
    Forbidden,
}

#[cfg(test)]
mod gc_replay_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Exercise the complete GC constructor and physical SDK transport. A
    // wrapper call count cannot detect an SDK retry beneath delete_object.
    #[tokio::test]
    async fn gc_factory_never_replays_retryable_or_lost_delete_reply() {
        for lost_reply in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let attempts = Arc::new(AtomicUsize::new(0));
            let observed = attempts.clone();
            let (stop, mut stopping) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        _=&mut stopping=>break,
                        result=listener.accept()=>result.unwrap(),
                    };
                    let mut socket = accepted.0;
                    let mut request = Vec::new();
                    let mut byte = [0u8; 1];
                    while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).await.unwrap() == 0 {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    assert!(request.starts_with(b"DELETE "));
                    observed.fetch_add(1, Ordering::SeqCst);
                    if !lost_reply {
                        socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    }
                    // Dropping the stream models a dispatched DELETE whose
                    // result was lost, without guessing the object's state.
                }
            });
            let backend = S3Backend::with_gc_static_credentials(
                S3Config {
                    bucket: "gc-test".into(),
                    region: Some("us-east-1".into()),
                    endpoint: Some(format!("http://{address}")),
                    force_path_style: true,
                    max_retries: 7,
                    ..Default::default()
                },
                "test-admin".into(),
                "test-admin-secret".into(),
            )
            .await
            .unwrap();
            assert!(backend.forbids_mutation_replay());
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    backend.delete_object("candidate")
                )
                .await
                .unwrap()
                .is_err()
            );
            stop.send(()).unwrap();
            server.await.unwrap();
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "physical SDK retry must stay disabled"
            );
        }
    }
}

#[allow(dead_code)]
impl S3Backend {
    /// Create new S3 backend with default configuration
    pub async fn new(bucket: impl Into<String>) -> Result<Self> {
        let config = S3Config {
            bucket: bucket.into(),
            ..Default::default()
        };
        Self::with_config(config).await
    }

    /// Create new S3 backend with custom configuration
    pub async fn with_config(config: S3Config) -> Result<Self> {
        Self::with_optional_static_credentials(config, None, S3MutationReplay::Inherit).await
    }

    /// GC keeps uncertain physical mutations quarantined. SDK retries must
    /// not replay DELETE/PUT beneath that durable protocol, including when
    /// AWS_MAX_ATTEMPTS or a shared SDK config enables retries elsewhere.
    pub async fn with_gc_config(config: S3Config) -> Result<Self> {
        Self::with_optional_static_credentials(config, None, S3MutationReplay::Forbidden).await
    }

    pub async fn with_gc_static_credentials(
        config: S3Config,
        access_key: String,
        secret_key: String,
    ) -> Result<Self> {
        if access_key.is_empty() || secret_key.is_empty() {
            anyhow::bail!("S3 GC credentials are missing");
        }
        let credentials = aws_sdk_s3::config::Credentials::new(
            access_key,
            secret_key,
            None,
            None,
            "brewfs-operator-gc",
        );
        Self::with_optional_static_credentials(
            config,
            Some(credentials),
            S3MutationReplay::Forbidden,
        )
        .await
    }

    pub async fn with_static_credentials(
        config: S3Config,
        access_key: String,
        secret_key: String,
    ) -> Result<Self> {
        if access_key.is_empty() || secret_key.is_empty() {
            anyhow::bail!("S3 credentials are missing");
        }
        let credentials = aws_sdk_s3::config::Credentials::new(
            access_key,
            secret_key,
            None,
            None,
            "brewfs-operator",
        );
        Self::with_optional_static_credentials(config, Some(credentials), S3MutationReplay::Inherit)
            .await
    }

    async fn with_optional_static_credentials(
        mut config: S3Config,
        credentials: Option<aws_sdk_s3::config::Credentials>,
        replay: S3MutationReplay,
    ) -> Result<Self> {
        if matches!(replay, S3MutationReplay::Forbidden) {
            config.max_retries = 1;
        }
        if config.bucket.is_empty() {
            return Err(anyhow!("Bucket name cannot be empty"));
        }

        let mut aws_config_loader = aws_config::defaults(BehaviorVersion::latest());

        // Prevent indefinite hangs on stalled S3 connections.
        let timeout_config = TimeoutConfig::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .operation_timeout(Duration::from_secs(120))
            .build();
        aws_config_loader = aws_config_loader.timeout_config(timeout_config);

        if let Some(region) = &config.region {
            aws_config_loader = aws_config_loader.region(Region::new(region.clone()));
        }

        tracing::info!(
            endpoint = ?config.endpoint,
            region = ?config.region,
            bucket = %config.bucket,
            "s3 backend aws config load begin"
        );
        if let Some(credentials) = credentials {
            aws_config_loader = aws_config_loader.credentials_provider(credentials);
        }
        let aws_config = aws_config_loader.load().await;
        tracing::info!("s3 backend aws config load complete");

        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&aws_config);
        if matches!(replay, S3MutationReplay::Forbidden) {
            s3_config_builder = s3_config_builder.retry_config(
                aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1),
            );
        }

        if let Some(endpoint) = &config.endpoint {
            s3_config_builder = s3_config_builder.endpoint_url(endpoint);
        }

        if config.force_path_style {
            s3_config_builder = s3_config_builder.force_path_style(true);
        }

        if config.disable_payload_checksum {
            // Skip payload checksum (SigV4 SHA-256 of request body) to send
            // UNSIGNED-PAYLOAD. This matches JuiceFS behavior and avoids wasting
            // ~20% CPU on cryptographic hashing for non-AWS S3 backends (MinIO, RustFS, etc.).
            s3_config_builder = s3_config_builder
                .request_checksum_calculation(RequestChecksumCalculation::WhenRequired);
            s3_config_builder = s3_config_builder.response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            );
        }

        if aws_config.http_client().is_none() {
            let base_http = aws_smithy_http_client::Builder::new().build_with_connector_fn(
                |settings, components| {
                    let mut connector = aws_smithy_http_client::ConnectorBuilder::default()
                        .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                            aws_smithy_http_client::tls::rustls_provider::CryptoMode::AwsLc,
                        ));
                    connector.set_connector_settings(settings.cloned());
                    if let Some(components) = components {
                        connector.set_sleep_impl(components.sleep_impl());
                    }
                    connector.set_proxy_config(Some(
                        aws_smithy_http_client::proxy::ProxyConfig::from_env(),
                    ));
                    connector.build()
                },
            );
            s3_config_builder = s3_config_builder.http_client(base_http);
        }
        let client = Client::from_conf(s3_config_builder.build());
        tracing::info!("s3 backend client ready");

        Ok(Self {
            client,
            config,
            mutation_replay_forbidden: matches!(replay, S3MutationReplay::Forbidden),
        })
    }

    async fn range_stream_with_observer(
        &self,
        key: &str,
        offset: u64,
        length: u64,
        observed: Option<(
            crate::cadapter::read_observer::ReadContext,
            Arc<crate::cadapter::read_observer::ReadObserver>,
        )>,
    ) -> Result<ObjectByteStream> {
        Ok(self
            .object_stream_with_observer(key, Some((offset, length)), Some(length), observed)
            .await?
            .unwrap_or_else(|| Box::pin(futures_util::stream::empty())))
    }

    async fn object_stream_with_observer(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
        expected: Option<u64>,
        observed: Option<(
            crate::cadapter::read_observer::ReadContext,
            Arc<crate::cadapter::read_observer::ReadObserver>,
        )>,
    ) -> Result<Option<ObjectByteStream>> {
        let mut operation = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key);
        if let Some((offset, length)) = range {
            if length == 0 {
                return Ok(Some(Box::pin(futures_util::stream::empty())));
            }
            let end = offset
                .checked_add(length - 1)
                .ok_or_else(|| anyhow!("S3 range end overflows u64"))?;
            operation = operation.range(format!("bytes={offset}-{end}"));
        }
        let resp = match observed {
            Some((context, observer)) => {
                let delegate = self
                    .client
                    .config()
                    .http_client()
                    .ok_or_else(|| anyhow!("typed S3 HTTP client unavailable"))?;
                let observed =
                    crate::cadapter::read_observer::http::ObservedHttpClient::with_request_length(
                        delegate, observer, context, expected,
                    );
                operation
                    .customize()
                    .config_override(aws_sdk_s3::config::Builder::new().http_client(observed))
                    .send()
                    .await
            }
            None => operation.send().await,
        };

        match resp {
            Ok(object) => {
                let stream = ReaderStream::new(object.body.into_async_read()).map(|item| {
                    item.map_err(|error| {
                        let mut checksum_failure = false;
                        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
                        while let Some(cause) = source {
                            // Inspect locally; never log the SDK source chain, which can
                            // contain request headers, endpoint URLs or signatures.
                            checksum_failure |=
                                cause.to_string().to_ascii_lowercase().contains("checksum");
                            source = cause.source();
                        }
                        anyhow!(
                            "S3 range stream failure: kind={} checksum={checksum_failure}",
                            Self::safe_stream_error_kind(error.kind()),
                        )
                    })
                });
                Ok(Some(Box::pin(stream)))
            }
            Err(SdkError::ServiceError(error)) if error.err().is_no_such_key() => Ok(None),
            Err(SdkError::ServiceError(error)) => Err(anyhow!(
                "S3 range service failure: status={} code={}",
                error.raw().status().as_u16(),
                Self::safe_service_code(error.err().meta().code()),
            )),
            Err(SdkError::ConstructionFailure(_)) => {
                Err(anyhow!("S3 range request failure: class=construction"))
            }
            Err(SdkError::TimeoutError(_)) => {
                Err(anyhow!("S3 range request failure: class=timeout"))
            }
            Err(SdkError::DispatchFailure(error)) if error.is_timeout() => {
                Err(anyhow!("S3 range request failure: class=timeout"))
            }
            Err(SdkError::DispatchFailure(error)) => Err(anyhow!(
                "S3 range request failure: class=dispatch io={} user={}",
                error.is_io(),
                error.is_user(),
            )),
            Err(SdkError::ResponseError(error)) => Err(anyhow!(
                "S3 range request failure: class=response status={}",
                error.raw().status().as_u16(),
            )),
            Err(_) => Err(anyhow!("S3 range request failure: class=unknown")),
        }
    }

    async fn bounded_size_with_observer(
        &self,
        key: &str,
        observed: Option<(
            crate::cadapter::read_observer::ReadContext,
            Arc<crate::cadapter::read_observer::ReadObserver>,
        )>,
    ) -> Result<Option<u64>> {
        use std::io::{Error, ErrorKind};
        let operation = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key);
        let response = match observed {
            Some((context, observer)) => {
                let delegate = self.client.config().http_client().ok_or_else(|| {
                    Error::new(
                        ErrorKind::Unsupported,
                        "typed S3 HEAD HTTP client unavailable",
                    )
                })?;
                let observed = crate::cadapter::read_observer::http::ObservedHttpClient::new(
                    delegate, observer, context, 0,
                );
                operation
                    .customize()
                    .config_override(aws_sdk_s3::config::Builder::new().http_client(observed))
                    .send()
                    .await
            }
            None => operation.send().await,
        };
        match response {
            Ok(response) => {
                let length = response.content_length().ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        "S3 bounded HEAD is missing object length",
                    )
                })?;
                let length = u64::try_from(length).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidData,
                        "S3 bounded HEAD has negative object length",
                    )
                })?;
                Ok(Some(length))
            }
            Err(SdkError::ServiceError(error))
                if error.raw().status().as_u16() == 404
                    || error.err().meta().code() == Some("NoSuchKey") =>
            {
                Ok(None)
            }
            Err(SdkError::ServiceError(error)) => {
                let status = error.raw().status().as_u16();
                let kind = match status {
                    200..=299 => ErrorKind::InvalidData,
                    401 | 403 => ErrorKind::PermissionDenied,
                    _ => ErrorKind::Other,
                };
                // In particular malformed Content-Length can be a 2xx SDK
                // deserialization ServiceError. Do not expose its raw headers.
                Err(Error::new(
                    kind,
                    format!(
                        "S3 bounded HEAD service failure: status={status} code={}",
                        Self::safe_service_code(error.err().meta().code()),
                    ),
                )
                .into())
            }
            Err(SdkError::TimeoutError(_)) => Err(Error::new(
                ErrorKind::TimedOut,
                "S3 bounded HEAD request failure: class=timeout",
            )
            .into()),
            Err(SdkError::DispatchFailure(error)) if error.is_timeout() => Err(Error::new(
                ErrorKind::TimedOut,
                "S3 bounded HEAD request failure: class=timeout",
            )
            .into()),
            Err(SdkError::DispatchFailure(_)) => {
                Err(Error::other("S3 bounded HEAD request failure: class=dispatch").into())
            }
            Err(SdkError::ConstructionFailure(_)) => {
                Err(Error::other("S3 bounded HEAD request failure: class=construction").into())
            }
            Err(SdkError::ResponseError(error)) => {
                let status = error.raw().status().as_u16();
                let kind = if (200..300).contains(&status) {
                    ErrorKind::InvalidData
                } else {
                    ErrorKind::Other
                };
                Err(Error::new(
                    kind,
                    format!("S3 bounded HEAD request failure: class=response status={status}",),
                )
                .into())
            }
            Err(_) => Err(Error::other("S3 bounded HEAD request failure: class=unknown").into()),
        }
    }

    async fn etag_with_observer(
        &self,
        key: &str,
        observed: Option<(
            crate::cadapter::read_observer::ReadContext,
            Arc<crate::cadapter::read_observer::ReadObserver>,
        )>,
    ) -> Result<String> {
        let operation = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key);
        let response = match observed {
            Some((context, observer)) => {
                let delegate = self
                    .client
                    .config()
                    .http_client()
                    .ok_or_else(|| anyhow!("typed S3 HEAD HTTP client unavailable"))?;
                let observed = crate::cadapter::read_observer::http::ObservedHttpClient::new(
                    delegate, observer, context, 0,
                );
                operation
                    .customize()
                    .config_override(aws_sdk_s3::config::Builder::new().http_client(observed))
                    .send()
                    .await
            }
            None => operation.send().await,
        }?;
        Ok(response.e_tag().unwrap_or_default().to_string())
    }

    fn safe_stream_error_kind(kind: std::io::ErrorKind) -> &'static str {
        match kind {
            std::io::ErrorKind::TimedOut => "timeout",
            std::io::ErrorKind::ConnectionReset => "connection reset",
            std::io::ErrorKind::ConnectionRefused => "connection refused",
            std::io::ErrorKind::BrokenPipe => "broken pipe",
            std::io::ErrorKind::Interrupted => "interrupted",
            std::io::ErrorKind::UnexpectedEof => "unexpected eof",
            std::io::ErrorKind::InvalidData => "invalid data",
            std::io::ErrorKind::PermissionDenied => "permission denied",
            _ => "other",
        }
    }

    fn safe_service_code(code: Option<&str>) -> &'static str {
        match code {
            Some("AccessDenied") => "AccessDenied",
            Some("InvalidRange") => "InvalidRange",
            Some("NoSuchKey") => "NoSuchKey",
            Some("NoSuchBucket") => "NoSuchBucket",
            Some("SignatureDoesNotMatch") => "SignatureDoesNotMatch",
            Some("RequestTimeTooSkewed") => "RequestTimeTooSkewed",
            Some("SlowDown") => "SlowDown",
            Some("InternalError") => "InternalError",
            Some("ServiceUnavailable") => "ServiceUnavailable",
            _ => "unknown",
        }
    }

    fn md5_base64(data: &[u8]) -> String {
        let sum = md5::compute(data);
        B64.encode(sum.0)
    }

    fn md5_base64_chunks(chunks: &[Bytes]) -> String {
        let mut ctx = md5::Context::new();
        for chunk in chunks {
            ctx.consume(chunk);
        }
        B64.encode(ctx.compute().0)
    }

    fn stream_from_chunks(chunks: &[Bytes]) -> ByteStream {
        let owned = chunks.to_vec();
        let stream = futures::stream::iter(owned.into_iter().map(Ok::<Bytes, std::io::Error>));
        ByteStream::from_body_0_4(Body::wrap_stream(stream))
    }

    fn direct_stream_from_chunks(chunks: &[Bytes], total_size: usize) -> ByteStream {
        if let [chunk] = chunks {
            return SdkBody::from(chunk.clone()).into();
        }

        let mut data = Vec::with_capacity(total_size);
        for chunk in chunks {
            data.extend_from_slice(chunk);
        }
        SdkBody::from(data).into()
    }

    #[tracing::instrument(level = "debug", skip(self, chunks), fields(key, total_size))]
    async fn put_object_vectored_simple(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let total_size = chunks.iter().map(|c| c.len()).sum::<usize>();
        tracing::Span::current().record("total_size", total_size);
        let checksum = if self.config.enable_md5 && total_size > 0 {
            Some(Self::md5_base64_chunks(&chunks))
        } else {
            None
        };

        let mut attempt = 0;
        loop {
            attempt += 1;

            let body = Self::direct_stream_from_chunks(&chunks, total_size);
            let mut request = self
                .client
                .put_object()
                .bucket(&self.config.bucket)
                .key(key)
                .body(body)
                .content_length(total_size as i64);

            if let Some(sum) = checksum.as_ref() {
                request = request.content_md5(sum.clone());
            }

            let result = if self.config.disable_payload_checksum {
                request.customize().disable_payload_signing().send().await
            } else {
                request.send().await
            };

            match result {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Put small objects directly (simpler than multipart upload)
    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object_simple(&self, key: &str, data: &[u8]) -> Result<()> {
        let mut attempt = 0;
        loop {
            attempt += 1;

            let mut request = self
                .client
                .put_object()
                .bucket(&self.config.bucket)
                .key(key)
                .body(SdkBody::from(data.to_vec()).into());

            if self.config.enable_md5 {
                let checksum = Self::md5_base64(data);
                request = request.content_md5(checksum);
            }

            let result = if self.config.disable_payload_checksum {
                request.customize().disable_payload_signing().send().await
            } else {
                request.send().await
            };

            match result {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Handle multipart upload for large objects
    async fn multipart_upload(&self, key: &str, data: &[u8]) -> Result<()> {
        // Create multipart upload
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await?;

        let upload_id = create
            .upload_id()
            .ok_or_else(|| anyhow!("Missing upload_id in create_multipart_upload response"))?
            .to_string();

        // Ensure we clean up the multipart upload if it fails
        let cleanup_on_drop = MultipartCleanupGuard {
            client: self.client.clone(),
            bucket: self.config.bucket.clone(),
            key: key.to_string(),
            upload_id: upload_id.clone(),
        };

        let data_arc = Arc::new(data.to_vec());
        let sem = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrency));

        // Concurrent upload of parts
        let mut parts = Vec::new();
        let total = data.len();
        let mut idx = 0usize;
        let mut part_number = 1i32;

        while idx < total {
            let end = (idx + self.config.part_size).min(total);
            let chunk_vec = data_arc.as_slice()[idx..end].to_vec();
            let client = self.client.clone();
            let bucket = self.config.bucket.clone();
            let key = key.to_string();
            let upload_id_cloned = upload_id.clone();
            let pn = part_number;
            let sem_cloned = sem.clone();
            let enable_md5 = self.config.enable_md5;
            let max_retries = self.config.max_retries;
            let retry_base_delay = self.config.retry_base_delay;
            let disable_payload_checksum = self.config.disable_payload_checksum;

            let fut = async move {
                // Concurrency control
                let _permit = sem_cloned
                    .acquire_owned()
                    .await
                    .with_context(|| "Multipart upload semaphore closed unexpectedly");
                let mut attempt = 0;

                loop {
                    attempt += 1;
                    let mut request = client
                        .upload_part()
                        .bucket(&bucket)
                        .key(&key)
                        .upload_id(&upload_id_cloned)
                        .part_number(pn)
                        .body(SdkBody::from(chunk_vec.clone()).into());

                    if enable_md5 {
                        let part_md5 = Self::md5_base64(&chunk_vec);
                        request = request.content_md5(part_md5);
                    }

                    let result = if disable_payload_checksum {
                        request.customize().disable_payload_signing().send().await
                    } else {
                        request.send().await
                    };

                    match result {
                        Ok(ok) => break Ok((pn, ok.e_tag().map(|s| s.to_string()))),
                        Err(_e) if attempt < max_retries => {
                            let delay = retry_base_delay * (1 << (attempt - 1));
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        }
                        Err(e) => break Err(e),
                    }
                }
            };
            parts.push(fut);

            idx = end;
            part_number += 1;
        }

        // Execute all parts concurrently
        let results: Vec<(i32, Option<String>)> = match futures::future::try_join_all(parts).await {
            Ok(v) => v,
            Err(e) => return Err(e.into()),
        };

        // Build completed parts
        let completed_parts = results
            .into_iter()
            .map(|(pn, etag)| {
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(pn)
                    .set_e_tag(etag)
                    .build()
            })
            .collect::<Vec<_>>();

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        // Complete multipart upload
        self.client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await?;

        // Disarm cleanup guard since upload succeeded
        std::mem::forget(cleanup_on_drop);

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, chunks), fields(key, parts))]
    async fn multipart_upload_vectored(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await?;

        let upload_id = create
            .upload_id()
            .ok_or_else(|| anyhow!("Missing upload_id in create_multipart_upload response"))?
            .to_string();

        let cleanup_on_drop = MultipartCleanupGuard {
            client: self.client.clone(),
            bucket: self.config.bucket.clone(),
            key: key.to_string(),
            upload_id: upload_id.clone(),
        };

        let mut parts: Vec<Vec<Bytes>> = Vec::new();
        let mut cur_part: Vec<Bytes> = Vec::new();
        let mut cur_len: usize = 0;

        for chunk in chunks.into_iter() {
            let mut offset = 0usize;
            while offset < chunk.len() {
                let remaining_part = self.config.part_size - cur_len;
                let remaining_chunk = chunk.len() - offset;
                let take = remaining_part.min(remaining_chunk);
                cur_part.push(chunk.slice(offset..offset + take));
                cur_len += take;
                offset += take;

                if cur_len == self.config.part_size {
                    parts.push(cur_part);
                    cur_part = Vec::new();
                    cur_len = 0;
                }
            }
        }

        if cur_len > 0 {
            parts.push(cur_part);
        }

        let sem = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrency));
        let mut futures = Vec::new();

        for (idx, part_chunks) in parts.into_iter().enumerate() {
            let part_len = part_chunks.iter().map(|c| c.len()).sum::<usize>();
            let part_md5 = if self.config.enable_md5 && part_len > 0 {
                Some(Self::md5_base64_chunks(&part_chunks))
            } else {
                None
            };

            let client = self.client.clone();
            let bucket = self.config.bucket.clone();
            let key = key.to_string();
            let upload_id_cloned = upload_id.clone();
            let pn = (idx + 1) as i32;
            let sem_cloned = sem.clone();
            let max_retries = self.config.max_retries;
            let retry_base_delay = self.config.retry_base_delay;
            let disable_payload_checksum = self.config.disable_payload_checksum;

            let fut = async move {
                let _permit = sem_cloned.acquire_owned().await;
                let mut attempt = 0;

                loop {
                    attempt += 1;
                    let body = S3Backend::stream_from_chunks(&part_chunks);
                    let mut request = client
                        .upload_part()
                        .bucket(&bucket)
                        .key(&key)
                        .upload_id(&upload_id_cloned)
                        .part_number(pn)
                        .body(body)
                        .content_length(part_len as i64);

                    if let Some(md5) = part_md5.as_ref() {
                        request = request.content_md5(md5.clone());
                    }

                    let result = if disable_payload_checksum {
                        request.customize().disable_payload_signing().send().await
                    } else {
                        request.send().await
                    };

                    match result {
                        Ok(ok) => break Ok((pn, ok.e_tag().map(|s| s.to_string()))),
                        Err(_e) if attempt < max_retries => {
                            let delay = retry_base_delay * (1 << (attempt - 1));
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        }
                        Err(e) => break Err(e),
                    }
                }
            };
            futures.push(fut);
        }

        let results: Vec<(i32, Option<String>)> = match futures::future::try_join_all(futures).await
        {
            Ok(v) => v,
            Err(e) => return Err(e.into()),
        };

        let completed_parts = results
            .into_iter()
            .map(|(pn, etag)| {
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(pn)
                    .set_e_tag(etag)
                    .build()
            })
            .collect::<Vec<_>>();

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await?;

        std::mem::forget(cleanup_on_drop);
        Ok(())
    }
}

/// Guard to automatically clean up multipart uploads if they fail
struct MultipartCleanupGuard {
    client: Client,
    bucket: String,
    key: String,
    upload_id: String,
}

impl Drop for MultipartCleanupGuard {
    fn drop(&mut self) {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = self.key.clone();
        let upload_id = self.upload_id.clone();

        tokio::spawn(async move {
            let _ = client
                .abort_multipart_upload()
                .bucket(&bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await;
        });
    }
}

#[async_trait]
impl ObjectBackend for S3Backend {
    fn forbids_mutation_replay(&self) -> bool {
        self.mutation_replay_forbidden
    }
    #[tracing::instrument(level = "trace", skip(self, chunks), fields(key, chunk_count = chunks.len()))]
    async fn put_object_vectored(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let total_size = chunks.iter().map(|e| e.len()).sum::<usize>();

        if total_size == 0 {
            return self.put_object_simple(key, &[]).await;
        }
        if total_size <= self.config.part_size {
            // RustFS is more reliable with a direct small-object body; single-chunk
            // writes keep the Bytes allocation shared and multi-chunk writes copy once.
            return self.put_object_vectored_simple(key, chunks).await;
        }

        self.multipart_upload_vectored(key, chunks).await
    }

    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object(&self, key: &str, data: &[u8]) -> Result<()> {
        // Small objects use direct put_object; large objects use multipart upload
        if data.len() <= self.config.part_size {
            return self.put_object_simple(key, data).await;
        }

        // Multipart upload for large objects
        self.multipart_upload(key, data).await
    }

    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object_create_only(&self, key: &str, data: &[u8]) -> Result<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .if_none_match("*")
            .body(SdkBody::from(data.to_vec()).into());
        if self.config.enable_md5 {
            request = request.content_md5(Self::md5_base64(data));
        }
        let result = if self.config.disable_payload_checksum {
            request.customize().disable_payload_signing().send().await
        } else {
            request.send().await
        };
        if let Err(error) = result {
            // Alibaba OSS currently returns 501 NotImplemented for the
            // conditional `If-None-Match: *` form. Native frozen objects are
            // content-addressed and the ObjectSink caller verifies the exact
            // bytes after every PUT, so use an unconditional PUT only for this
            // explicit capability gap. Network and authorization failures
            // still follow the create-only conflict path below.
            let conditional_put_unsupported = matches!(
                &error,
                SdkError::ServiceError(service)
                    if service.raw().status().as_u16() == 501
                        || service.err().meta().code() == Some("NotImplemented")
            );
            if conditional_put_unsupported {
                self.put_object(key, data).await?;
                return Ok(());
            }
            if self.get_object(key).await?.as_deref() == Some(data) {
                return Ok(());
            }
            return Err(error.into());
        }
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self), fields(key))]
    async fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await;

        match resp {
            Ok(o) => {
                use tokio::io::AsyncReadExt;
                let capacity = o
                    .content_length()
                    .and_then(|len| usize::try_from(len).ok())
                    .unwrap_or_default();
                let mut body = o.body.into_async_read();
                let mut buf = Vec::with_capacity(capacity);
                body.read_to_end(&mut buf).await?;
                Ok(Some(buf))
            }
            Err(SdkError::ServiceError(err)) if err.err().is_no_such_key() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get a range of bytes from an object.
    /// Used for small range reads in intelligent read strategy.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(key, offset, len = buf.len()))]
    async fn get_object_range(&self, key: &str, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let end = offset + buf.len() as u64 - 1;
        let range_header = format!("bytes={}-{}", offset, end);

        let resp = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .range(range_header)
            .send()
            .await;

        match resp {
            Ok(o) => {
                use tokio::io::AsyncReadExt;
                let mut body = o.body.into_async_read();
                let mut read = 0;

                while read < buf.len() {
                    let n = body.read(&mut buf[read..]).await?;
                    if n == 0 {
                        break;
                    }
                    read += n;
                }

                Ok(read)
            }
            Err(SdkError::ServiceError(err)) if err.err().is_no_such_key() => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_object_stream(&self, key: &str) -> Result<Option<ObjectByteStream>> {
        self.object_stream_with_observer(key, None, None, None)
            .await
    }
    async fn get_object_stream_observed(
        &self,
        key: &str,
        expected: Option<u64>,
        context: crate::cadapter::read_observer::ReadContext,
        observer: Arc<crate::cadapter::read_observer::ReadObserver>,
    ) -> Result<Option<ObjectByteStream>> {
        self.object_stream_with_observer(key, None, expected, Some((context, observer)))
            .await
    }

    async fn get_object_range_stream(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> Result<ObjectByteStream> {
        self.range_stream_with_observer(key, offset, length, None)
            .await
    }

    async fn get_object_range_stream_observed(
        &self,
        key: &str,
        offset: u64,
        length: u64,
        context: crate::cadapter::read_observer::ReadContext,
        observer: Arc<crate::cadapter::read_observer::ReadObserver>,
    ) -> Result<ObjectByteStream> {
        self.range_stream_with_observer(key, offset, length, Some((context, observer)))
            .await
    }

    async fn get_object_size(&self, key: &str) -> Result<Option<u64>> {
        let resp = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await;
        match resp {
            Ok(response) => Ok(response.content_length().map(|length| length as u64)),
            Err(SdkError::ServiceError(error))
                if error.raw().status().as_u16() == 404
                    || error.err().meta().code() == Some("NoSuchKey") =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn get_object_size_bounded(&self, key: &str) -> Result<Option<u64>> {
        self.bounded_size_with_observer(key, None).await
    }

    async fn get_object_size_bounded_observed(
        &self,
        key: &str,
        context: crate::cadapter::read_observer::ReadContext,
        observer: Arc<crate::cadapter::read_observer::ReadObserver>,
    ) -> Result<Option<u64>> {
        self.bounded_size_with_observer(key, Some((context, observer)))
            .await
    }

    async fn get_etag(&self, key: &str) -> Result<String> {
        self.etag_with_observer(key, None).await
    }

    async fn get_etag_observed(
        &self,
        key: &str,
        context: crate::cadapter::read_observer::ReadContext,
        observer: Arc<crate::cadapter::read_observer::ReadObserver>,
    ) -> Result<String> {
        self.etag_with_observer(key, Some((context, observer)))
            .await
    }

    #[tracing::instrument(level = "debug", skip(self), fields(key))]
    async fn delete_object(&self, key: &str) -> Result<()> {
        let mut attempt = 0;

        loop {
            attempt += 1;
            match self
                .client
                .delete_object()
                .bucket(&self.config.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cadapter::client::ObjectClient;
    use crate::cadapter::read_observer::{Engine, Ledger, Origin, Phase, ReadClass, ReadObserver};
    use aws_smithy_runtime_api::client::http::{
        HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
    };
    use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
    use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
    use aws_smithy_runtime_api::http::StatusCode;

    #[derive(Clone, Debug)]
    struct HeadOnlyHttpClient {
        status: u16,
        length: Option<&'static str>,
    }

    impl HttpClient for HeadOnlyHttpClient {
        fn http_connector(
            &self,
            _: &HttpConnectorSettings,
            _: &RuntimeComponents,
        ) -> SharedHttpConnector {
            SharedHttpConnector::new(self.clone())
        }
    }

    impl HttpConnector for HeadOnlyHttpClient {
        fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
            assert_eq!(
                request.method(),
                "HEAD",
                "size validation must never download an object"
            );
            assert!(request.headers().get("range").is_none());
            let status = self.status;
            let length = self.length;
            HttpConnectorFuture::new(async move {
                let mut response =
                    HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::empty());
                if let Some(length) = length {
                    response.headers_mut().insert("content-length", length);
                }
                Ok(response)
            })
        }
    }

    fn bounded_head_backend(status: u16, length: Option<&'static str>) -> S3Backend {
        let config = Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(
                "test-key",
                "test-secret",
                None,
                None,
                "bounded-head-test",
            ))
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1))
            .http_client(HeadOnlyHttpClient { status, length })
            .build();
        S3Backend {
            client: Client::from_conf(config),
            mutation_replay_forbidden: false,
            config: S3Config {
                bucket: "test-bucket".into(),
                ..S3Config::default()
            },
        }
    }

    #[derive(Clone, Debug)]
    struct EtagOnlyHttpClient {
        status: u16,
        etag: Option<&'static str>,
    }

    impl HttpClient for EtagOnlyHttpClient {
        fn http_connector(
            &self,
            _: &HttpConnectorSettings,
            _: &RuntimeComponents,
        ) -> SharedHttpConnector {
            SharedHttpConnector::new(self.clone())
        }
    }

    impl HttpConnector for EtagOnlyHttpClient {
        fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
            assert_eq!(request.method(), "HEAD", "ETag validation must use HEAD");
            assert!(request.headers().get("range").is_none());
            let status = self.status;
            let etag = self.etag;
            HttpConnectorFuture::new(async move {
                let mut response =
                    HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::empty());
                if let Some(etag) = etag {
                    response.headers_mut().insert("etag", etag);
                }
                Ok(response)
            })
        }
    }

    fn etag_head_backend(status: u16, etag: Option<&'static str>) -> S3Backend {
        let config = Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(
                "test-key",
                "test-secret",
                None,
                None,
                "etag-head-test",
            ))
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1))
            .http_client(EtagOnlyHttpClient { status, etag })
            .build();
        S3Backend {
            client: Client::from_conf(config),
            mutation_replay_forbidden: false,
            config: S3Config {
                bucket: "test-bucket".into(),
                ..S3Config::default()
            },
        }
    }

    #[tokio::test]
    async fn bounded_head_uses_sdk_head_and_conserves_terminal_observation() {
        use std::io::ErrorKind;
        for (status, length, expected, error_kind) in [
            (200, Some("1099511627776"), Some(Some(1_u64 << 40)), None),
            (200, Some("0"), Some(Some(0)), None),
            (404, None, Some(None), None),
            (403, None, None, Some(ErrorKind::PermissionDenied)),
            (200, None, None, Some(ErrorKind::InvalidData)),
            (200, Some("-1"), None, Some(ErrorKind::InvalidData)),
            (
                200,
                Some("untrusted-secret-length"),
                None,
                Some(ErrorKind::InvalidData),
            ),
        ] {
            let observer = Arc::new(ReadObserver::default());
            let client = ObjectClient::new(bounded_head_backend(status, length))
                .with_read_observer(
                    Arc::clone(&observer),
                    Engine::PackedV3,
                    Phase::Startup,
                    Origin::Demand,
                );
            let context = client.read_context(ReadClass::ContainerIndex).unwrap();
            let result = client
                .typed_object_size(ReadClass::ContainerIndex, "private-object-key")
                .await;
            match (expected, error_kind) {
                (Some(expected), None) => assert_eq!(result.unwrap(), expected),
                (None, Some(kind)) => {
                    let error = result.unwrap_err();
                    assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), kind);
                    assert!(!error.to_string().contains("private-object-key"));
                    assert!(!error.to_string().contains("untrusted-secret-length"));
                }
                _ => unreachable!(),
            }
            let snapshot = observer.snapshot();
            assert!(snapshot.http_observed);
            assert!(!snapshot.overflowed);
            for ledger in [
                Ledger::HttpAttempt,
                Ledger::BackendBody,
                Ledger::ValidatedFetch,
            ] {
                let row = &snapshot.rows[&(ledger, context)];
                assert_eq!(
                    (
                        row.started,
                        row.cancelled,
                        row.inflight,
                        row.requested,
                        row.received
                    ),
                    (1, 0, 0, 0, 0)
                );
                let failed = if ledger == Ledger::HttpAttempt {
                    u64::from(status != 200)
                } else {
                    u64::from(error_kind.is_some())
                };
                assert_eq!((row.success, row.failed), (1 - failed, failed));
                assert!(row.conserved());
            }
        }
    }

    #[tokio::test]
    async fn typed_etag_uses_observed_head_and_rejects_unclassified_route() {
        let observer = Arc::new(ReadObserver::default());
        let client = ObjectClient::new(etag_head_backend(200, Some("\"etag-123\"")))
            .with_read_observer(
                Arc::clone(&observer),
                Engine::PackedV3,
                Phase::Startup,
                Origin::Demand,
            );
        let context = client.read_context(ReadClass::ContainerIndex).unwrap();
        assert!(client.get_etag("private-object-key").await.is_err());
        assert_eq!(
            client
                .typed_etag(ReadClass::ContainerIndex, "private-object-key")
                .await
                .unwrap(),
            "\"etag-123\""
        );

        let snapshot = observer.snapshot();
        assert!(snapshot.http_observed);
        assert!(!snapshot.overflowed);
        for ledger in [
            Ledger::HttpAttempt,
            Ledger::BackendBody,
            Ledger::ValidatedFetch,
        ] {
            let row = &snapshot.rows[&(ledger, context)];
            assert_eq!(
                (
                    row.started,
                    row.cancelled,
                    row.inflight,
                    row.requested,
                    row.received
                ),
                (1, 0, 0, 0, 0)
            );
            assert_eq!((row.success, row.failed), (1, 0));
            assert!(row.conserved());
        }
    }

    #[tokio::test]
    async fn typed_etag_records_head_http_failure_in_each_observed_ledger() {
        let observer = Arc::new(ReadObserver::default());
        let client = ObjectClient::new(etag_head_backend(403, None)).with_read_observer(
            Arc::clone(&observer),
            Engine::PackedV3,
            Phase::Runtime,
            Origin::Demand,
        );
        let context = client.read_context(ReadClass::ContainerIndex).unwrap();
        assert!(
            client
                .typed_etag(ReadClass::ContainerIndex, "private-object-key")
                .await
                .is_err()
        );

        let snapshot = observer.snapshot();
        for ledger in [
            Ledger::HttpAttempt,
            Ledger::BackendBody,
            Ledger::ValidatedFetch,
        ] {
            let row = &snapshot.rows[&(ledger, context)];
            assert_eq!((row.started, row.cancelled, row.inflight), (1, 0, 0));
            assert_eq!((row.success, row.failed), (0, 1));
            assert!(row.conserved());
        }
    }

    #[test]
    fn safe_range_diagnostics_preserve_retry_labels_and_reject_untrusted_codes() {
        use std::io::ErrorKind;
        for (kind, expected) in [
            (ErrorKind::TimedOut, "timeout"),
            (ErrorKind::BrokenPipe, "broken pipe"),
            (ErrorKind::ConnectionReset, "connection reset"),
        ] {
            assert_eq!(super::S3Backend::safe_stream_error_kind(kind), expected);
        }
        assert_eq!(
            super::S3Backend::safe_service_code(Some("untrusted-error-detail")),
            "unknown"
        );
        assert_eq!(
            super::S3Backend::safe_service_code(Some("AccessDenied")),
            "AccessDenied"
        );
    }

    use super::*;
    use aws_sdk_s3::Config;
    use aws_sdk_s3::config::{Credentials, Region};
    use tokio::io::AsyncReadExt;
    use tokio::time::timeout;

    #[test]
    fn s3_config_defaults_raise_parallelism() {
        let config = S3Config::default();

        assert_eq!(config.max_concurrency, 32);
    }

    #[tokio::test]
    async fn small_vectored_direct_stream_preserves_chunks() {
        let chunks = vec![
            Bytes::from_static(b"small-"),
            Bytes::from_static(b"vectored-"),
            Bytes::from_static(b"put"),
        ];
        let total_size = chunks.iter().map(|chunk| chunk.len()).sum();
        let mut reader =
            S3Backend::direct_stream_from_chunks(&chunks, total_size).into_async_read();
        let mut actual = Vec::new();

        reader.read_to_end(&mut actual).await.unwrap();

        assert_eq!(actual, b"small-vectored-put");
    }

    fn test_backend() -> S3Backend {
        let endpoint = std::env::var("BREWFS_S3_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:9000".to_string());
        let bucket =
            std::env::var("BREWFS_S3_BUCKET").unwrap_or_else(|_| "brewfs-data".to_string());
        let region = std::env::var("BREWFS_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        let s3_config = Config::builder()
            .endpoint_url(endpoint)
            .force_path_style(true)
            .region(Region::new(region))
            .credentials_provider(Credentials::new(
                std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "rustfsadmin".to_string()),
                std::env::var("AWS_SECRET_ACCESS_KEY")
                    .unwrap_or_else(|_| "rustfsadmin".to_string()),
                None,
                None,
                "rustfs-small-object-streaming-body-compat-test",
            ))
            .build();

        S3Backend {
            client: Client::from_conf(s3_config),
            mutation_replay_forbidden: false,
            config: S3Config {
                bucket,
                region: None,
                part_size: 8 * 1024 * 1024,
                max_concurrency: 1,
                max_retries: 1,
                retry_base_delay: 1,
                enable_md5: true,
                endpoint: None,
                force_path_style: true,
                disable_payload_checksum: true,
            },
        }
    }

    #[tokio::test]
    #[ignore = "requires live S3-compatible endpoint; set BREWFS_S3_ENDPOINT and BREWFS_S3_BUCKET"]
    async fn rustfs_small_object_vectored_put_compat() {
        let backend = test_backend();
        let prefix = format!("diagnostics/rustfs-vectored-put/{}/", std::process::id());
        let simple_key = format!("{prefix}simple");
        let vectored_key = format!("{prefix}vectored");
        let payload = b"small-object-vectored-put-compat-payload";
        let chunks = vec![
            Bytes::copy_from_slice(&payload[..7]),
            Bytes::copy_from_slice(&payload[7..24]),
            Bytes::copy_from_slice(&payload[24..]),
        ];

        backend
            .put_object_simple(&simple_key, payload)
            .await
            .expect("contiguous put_object should succeed before testing streaming body");
        assert_eq!(
            backend.get_object(&simple_key).await.unwrap().as_deref(),
            Some(payload.as_slice())
        );

        timeout(
            Duration::from_secs(10),
            backend.put_object_vectored_simple(&vectored_key, chunks),
        )
        .await
        .expect("vectored put_object timed out; contiguous put_object already succeeded")
        .expect("vectored put_object returned an error");

        assert_eq!(
            backend.get_object(&vectored_key).await.unwrap().as_deref(),
            Some(payload.as_slice())
        );

        let _ = backend.delete_object(&simple_key).await;
        let _ = backend.delete_object(&vectored_key).await;
    }
}

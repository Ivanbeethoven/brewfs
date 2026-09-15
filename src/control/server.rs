use crate::control::protocol::{
    CONTROL_IO_TIMEOUT, CONTROL_MAX_CONNECTIONS, CONTROL_MAX_REQUEST_BYTES,
    CONTROL_MAX_RESPONSE_BYTES, ControlRequest, ControlResponse,
};
use anyhow::Context;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[async_trait]
pub trait ControlHandler: Send + Sync + 'static {
    async fn handle(&self, request: ControlRequest) -> ControlResponse;
}

pub struct ControlServer {
    socket_path: PathBuf,
    task: JoinHandle<()>,
}

impl ControlServer {
    pub async fn bind<H>(socket_path: PathBuf, handler: H) -> anyhow::Result<Self>
    where
        H: ControlHandler,
    {
        if let Some(parent) = socket_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create {}", parent.display()))?;
        }

        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind {}", socket_path.display()))?;
        let handler = Arc::new(handler);
        let connection_limit = Arc::new(Semaphore::new(CONTROL_MAX_CONNECTIONS));

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };

                let Ok(permit) = connection_limit.clone().try_acquire_owned() else {
                    let response = ControlResponse::Error {
                        code: "server_busy".to_string(),
                        message: "control server is at its connection limit; retry later"
                            .to_string(),
                    };
                    if let Ok(payload) = serde_json::to_vec(&response) {
                        let _ = stream.write_all(&payload).await;
                    }
                    let _ = stream.shutdown().await;
                    continue;
                };
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut buf = Vec::new();
                    let response = match timeout(
                        CONTROL_IO_TIMEOUT,
                        (&mut stream)
                            .take((CONTROL_MAX_REQUEST_BYTES + 1) as u64)
                            .read_to_end(&mut buf),
                    )
                    .await
                    {
                        Ok(Ok(_)) if buf.len() > CONTROL_MAX_REQUEST_BYTES => {
                            ControlResponse::Error {
                                code: "request_too_large".to_string(),
                                message: format!(
                                    "control request exceeds {} bytes",
                                    CONTROL_MAX_REQUEST_BYTES
                                ),
                            }
                        }
                        Ok(Ok(_)) => match serde_json::from_slice::<ControlRequest>(&buf) {
                            Ok(request) => handler.handle(request).await,
                            Err(err) => ControlResponse::Error {
                                code: "invalid_request".to_string(),
                                message: err.to_string(),
                            },
                        },
                        Ok(Err(err)) => ControlResponse::Error {
                            code: "read_failed".to_string(),
                            message: err.to_string(),
                        },
                        Err(_) => ControlResponse::Error {
                            code: "request_timeout".to_string(),
                            message: format!(
                                "control request exceeded {:?} read timeout",
                                CONTROL_IO_TIMEOUT
                            ),
                        },
                    };

                    let payload = match serde_json::to_vec(&response) {
                        Ok(payload) if payload.len() <= CONTROL_MAX_RESPONSE_BYTES => payload,
                        Ok(_) => serde_json::to_vec(&ControlResponse::Error {
                            code: "response_too_large".to_string(),
                            message: format!(
                                "control response exceeds {} bytes",
                                CONTROL_MAX_RESPONSE_BYTES
                            ),
                        })
                        .expect("control error response serializes"),
                        Err(err) => serde_json::to_vec(&ControlResponse::Error {
                            code: "response_encode_failed".to_string(),
                            message: err.to_string(),
                        })
                        .expect("control error response serializes"),
                    };

                    if let Err(err) = stream.write_all(&payload).await {
                        tracing::debug!(error = %err, "control response write failed");
                    }
                    if let Err(err) = stream.shutdown().await {
                        tracing::debug!(error = %err, "control response shutdown failed");
                    }
                });
            }
        });

        Ok(Self { socket_path, task })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

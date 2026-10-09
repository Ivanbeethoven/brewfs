//! One wrapper per typed SDK operation; delegates to the existing shared
//! HTTP client/pool. Each retry connector future owns an independent guard.
use super::{FailureClass, Ledger, ReadContext, ReadObserver, TerminalGuard};
use aws_smithy_runtime_api::client::{
    connector_metadata::ConnectorMetadata,
    http::{
        HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
        SharedHttpConnector,
    },
    orchestrator::HttpRequest,
    runtime_components::RuntimeComponents,
};
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use http_body_1x::{Body, Frame, SizeHint};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

#[derive(Clone, Debug)]
pub struct ObservedHttpClient {
    delegate: SharedHttpClient,
    observer: Arc<ReadObserver>,
    context: ReadContext,
    requested: Option<u64>,
}
impl ObservedHttpClient {
    pub fn new(
        delegate: SharedHttpClient,
        observer: Arc<ReadObserver>,
        context: ReadContext,
        requested: u64,
    ) -> Self {
        Self::with_request_length(delegate, observer, context, Some(requested))
    }
    pub fn with_request_length(
        delegate: SharedHttpClient,
        observer: Arc<ReadObserver>,
        context: ReadContext,
        requested: Option<u64>,
    ) -> Self {
        observer.enable_http();
        Self {
            delegate,
            observer,
            context,
            requested,
        }
    }
}
impl HttpClient for ObservedHttpClient {
    fn http_connector(
        &self,
        settings: &HttpConnectorSettings,
        components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(ObservedConnector {
            delegate: self.delegate.http_connector(settings, components),
            observer: Arc::clone(&self.observer),
            context: self.context,
            requested: self.requested,
        })
    }
    fn connector_metadata(&self) -> Option<ConnectorMetadata> {
        self.delegate.connector_metadata()
    }
}
#[derive(Debug)]
struct ObservedConnector {
    delegate: SharedHttpConnector,
    observer: Arc<ReadObserver>,
    context: ReadContext,
    requested: Option<u64>,
}
impl HttpConnector for ObservedConnector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let delegate = self.delegate.clone();
        let observer = Arc::clone(&self.observer);
        let context = self.context;
        let requested = self.requested;
        HttpConnectorFuture::new(async move {
            // This code runs on first poll, not when the future is created.
            let guard = observer.start_request(Ledger::HttpAttempt, context, requested);
            match delegate.call(request).await {
                Ok(mut response) => {
                    let status_success = (200..300).contains(&response.status().as_u16());
                    let body = std::mem::replace(response.body_mut(), SdkBody::empty());
                    *response.body_mut() =
                        SdkBody::from_body_1_x(ObservedHttpBody::new(body, guard, status_success));
                    Ok(response)
                }
                Err(error) => {
                    guard.fail(FailureClass::Backend);
                    Err(error)
                }
            }
        })
    }
}
/// Wrap the connector response, including XML error bodies SDK retries read.
/// Headers alone never complete the attempt. Data frames count actual body
/// bytes; trailers retain their original values and do not count as body data.
struct ObservedHttpBody {
    inner: Option<SdkBody>,
    guard: Option<TerminalGuard>,
    status_success: bool,
}
impl ObservedHttpBody {
    fn new(body: SdkBody, guard: TerminalGuard, status_success: bool) -> Self {
        let mut value = Self {
            inner: Some(body),
            guard: Some(guard),
            status_success,
        };
        if value.inner.as_ref().unwrap().is_end_stream() {
            value.complete();
        }
        value
    }
    fn complete(&mut self) {
        if let Some(guard) = self.guard.take() {
            if self.status_success {
                guard.succeed();
            } else {
                guard.fail(FailureClass::HttpStatus);
            }
        }
        self.inner.take();
    }
}
impl Drop for ObservedHttpBody {
    fn drop(&mut self) {
        // An HTTP status is known as soon as headers arrive. SDK retry
        // policy may retire an error body before EOF, so letting the guard's
        // default Drop classify this as cancellation would hide a terminal
        // HTTP failure (and its actual bytes) in the attempt ledger.
        if let Some(guard) = self.guard.take() {
            if self.status_success {
                drop(guard);
            } else {
                guard.fail(FailureClass::HttpStatus);
            }
        }
        self.inner.take();
    }
}
impl Body for ObservedHttpBody {
    type Data = Bytes;
    type Error = aws_smithy_runtime_api::box_error::BoxError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    self.guard.as_mut().unwrap().receive(bytes.len() as u64);
                }
                // Some SDK bodies promise EOF on their final frame without
                // requiring an extra poll; a consumer may stop at that point.
                if self.inner.as_ref().unwrap().is_end_stream() {
                    self.complete();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.guard.take().unwrap().fail(FailureClass::Backend);
                self.inner.take();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.complete();
                Poll::Ready(None)
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_none()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), Body::size_hint)
    }
}

#[cfg(test)]
mod tests;

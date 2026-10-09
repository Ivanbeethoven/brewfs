// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

//! This module is the low-level mechanisms for getting timestamps from a PD
//! cluster. It should be used via the `get_timestamp` API in `PdClient`.
//!
//! Once a `TimestampOracle` is created, there will be two futures running in a background working
//! thread created automatically. The `get_timestamp` method creates a oneshot channel whose
//! transmitter is served as a `TimestampRequest`. `TimestampRequest`s are sent to the working
//! thread through a bounded multi-producer, single-consumer channel. Every time the first future
//! is polled, it tries to exhaust the channel to get as many requests as possible and sends a
//! single `TsoRequest` to the PD server. The other future receives `TsoResponse`s from the PD
//! server and allocates timestamps for the requests.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;

use futures::pin_mut;
use futures::prelude::*;
use futures::task::AtomicWaker;
use futures::task::Context;
use futures::task::Poll;
use log::debug;
use log::info;
use pin_project::pin_project;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use crate::internal_err;
use crate::proto::pdpb::pd_client::PdClient;
use crate::proto::pdpb::*;
use crate::Result;

type TimestampRequest = oneshot::Sender<Timestamp>;

/// The timestamp oracle (TSO) which provides monotonically increasing timestamps.
#[derive(Clone)]
pub(crate) struct TimestampOracle {
    /// The transmitter of a bounded channel which transports requests of getting a single
    /// timestamp to the TSO working thread. A bounded channel is used to prevent using
    /// too much memory unexpectedly.
    /// In the working thread, the `TimestampRequest`, which is actually a one channel sender,
    /// is used to send back the timestamp result.
    mode: Arc<OracleMode>,
}

enum OracleMode {
    OnDemand {
        cluster_id: u64,
        pd_client: PdClient<Channel>,
        timeout: std::time::Duration,
        _owner: Option<crate::ClientResourceOwner>,
    },
    Background {
        request_tx: mpsc::Sender<TimestampRequest>,
        task: TsoTask,
    },
}

struct TsoTask {
    stop: watch::Sender<bool>,
    join: Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
}

impl Drop for TsoTask {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // A dropped join handle alone would detach the task. Cancellation keeps
        // the captured owner until the runtime drops the task's future. Only
        // shutdown().await below is a terminal join acknowledgement.
        if let Some(join) = self.join.get_mut().take() {
            join.abort();
        }
    }
}

impl TimestampOracle {
    pub(crate) fn new(
        cluster_id: u64,
        pd_client: &PdClient<Channel>,
        config: &crate::Config,
    ) -> Result<TimestampOracle> {
        let pd_client = pd_client.clone();
        if config.tso_on_demand {
            return Ok(TimestampOracle {
                mode: Arc::new(OracleMode::OnDemand {
                    cluster_id,
                    pd_client,
                    timeout: config.timeout,
                    _owner: config.resource_owner.clone(),
                }),
            });
        }
        if config.tso_max_batch_size == 0
            || config.tso_max_batch_size > 1024
            || config.tso_max_pending_groups == 0
            || config.tso_max_pending_groups > 1 << 16
        {
            return Err(internal_err!("invalid TSO resource limits"));
        }
        let (request_tx, request_rx) = mpsc::channel(config.tso_max_batch_size);
        let (stop, mut stopped) = watch::channel(false);
        let max_pending = config.tso_max_pending_groups;
        let max_batch = config.tso_max_batch_size;
        let owner = config.resource_owner.clone();

        let join = tokio::spawn(async move {
            let result = tokio::select! {
                result = run_tso(cluster_id, pd_client, request_rx, max_pending, max_batch) => result,
                _ = stopped.changed() => Ok(()),
            };
            drop(owner);
            result
        });

        Ok(TimestampOracle {
            mode: Arc::new(OracleMode::Background {
                request_tx,
                task: TsoTask {
                    stop,
                    join: Mutex::new(Some(join)),
                },
            }),
        })
    }

    pub(crate) async fn get_timestamp(self) -> Result<Timestamp> {
        debug!("getting current timestamp");
        match self.mode.as_ref() {
            OracleMode::OnDemand {
                cluster_id,
                pd_client,
                timeout,
                ..
            } => {
                let mut client = pd_client.clone();
                let request = TsoRequest {
                    header: Some(RequestHeader {
                        cluster_id: *cluster_id,
                        sender_id: 0,
                    }),
                    count: 1,
                    dc_location: String::new(),
                };
                tokio::time::timeout(*timeout, async {
                    let mut responses = client
                        .tso(futures::stream::iter([request]))
                        .await?
                        .into_inner();
                    let response = responses
                        .message()
                        .await?
                        .ok_or_else(|| internal_err!("TSO stream ended without one response"))?;
                    if response.count != 1 {
                        return Err(internal_err!(
                            "PD timestamp count differs from one-shot request"
                        ));
                    }
                    response
                        .timestamp
                        .ok_or_else(|| internal_err!("No timestamp in TsoResponse"))
                })
                .await
                .map_err(|_| internal_err!("one-shot TSO request timed out"))?
            }
            OracleMode::Background { request_tx, task } => {
                if *task.stop.borrow() {
                    return Err(internal_err!("TimestampOracle is shut down"));
                }
                let (request, response) = oneshot::channel();
                request_tx
                    .send(request)
                    .await
                    .map_err(|_| internal_err!("TimestampRequest channel is closed"))?;
                Ok(response.await?)
            }
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        if let OracleMode::Background { task, .. } = self.mode.as_ref() {
            let _ = task.stop.send(true);
            // Keep the handle in shared storage while awaiting. Cancellation of
            // the shutdown future leaves it available for the next caller.
            let mut join = task.join.lock().await;
            if let Some(handle) = join.as_mut() {
                let result = handle
                    .await
                    .map_err(|e| internal_err!("TSO join failed: {}", e))?;
                join.take();
                result?;
            }
        }
        Ok(())
    }
}

async fn run_tso(
    cluster_id: u64,
    mut pd_client: PdClient<Channel>,
    request_rx: mpsc::Receiver<TimestampRequest>,
    max_pending: usize,
    max_batch: usize,
) -> Result<()> {
    // The `TimestampRequest`s which are waiting for the responses from the PD server
    let pending_requests = Arc::new(Mutex::new(VecDeque::with_capacity(max_pending)));

    // When there are too many pending requests, the `send_request` future will refuse to fetch
    // more requests from the bounded channel. This waker is used to wake up the sending future
    // if the queue containing pending requests is no longer full.
    let sending_future_waker = Arc::new(AtomicWaker::new());

    let request_stream = TsoRequestStream {
        cluster_id,
        request_rx,
        pending_requests: pending_requests.clone(),
        self_waker: sending_future_waker.clone(),
        max_pending,
        max_batch,
    };

    // let send_requests = rpc_sender.send_all(&mut request_stream);
    let mut responses = pd_client.tso(request_stream).await?.into_inner();

    while let Some(Ok(resp)) = responses.next().await {
        {
            let mut pending_requests = pending_requests.lock().await;
            allocate_timestamps(&resp, &mut pending_requests)?;
        }

        // Wake up the sending future blocked by too many pending requests or locked.
        sending_future_waker.wake();
    }
    // TODO: distinguish between unexpected stream termination and expected end of test
    info!("TSO stream terminated");
    Ok(())
}

struct RequestGroup {
    tso_request: TsoRequest,
    requests: Vec<TimestampRequest>,
}

#[pin_project]
struct TsoRequestStream {
    cluster_id: u64,
    #[pin]
    request_rx: mpsc::Receiver<oneshot::Sender<Timestamp>>,
    pending_requests: Arc<Mutex<VecDeque<RequestGroup>>>,
    self_waker: Arc<AtomicWaker>,
    max_pending: usize,
    max_batch: usize,
}

impl Stream for TsoRequestStream {
    type Item = TsoRequest;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let pending_requests = this.pending_requests.lock();
        pin_mut!(pending_requests);
        let mut pending_requests = if let Poll::Ready(pending_requests) = pending_requests.poll(cx)
        {
            pending_requests
        } else {
            this.self_waker.register(cx.waker());
            return Poll::Pending;
        };
        let mut requests = Vec::new();

        while requests.len() < *this.max_batch && pending_requests.len() < *this.max_pending {
            match this.request_rx.poll_recv(cx) {
                Poll::Ready(Some(sender)) => {
                    requests.push(sender);
                }
                Poll::Ready(None) if requests.is_empty() => return Poll::Ready(None),
                _ => break,
            }
        }

        if !requests.is_empty() {
            let req = TsoRequest {
                header: Some(RequestHeader {
                    cluster_id: *this.cluster_id,
                    sender_id: 0,
                }),
                count: requests.len() as u32,
                dc_location: String::new(),
            };

            let request_group = RequestGroup {
                tso_request: req.clone(),
                requests,
            };
            pending_requests.push_back(request_group);

            Poll::Ready(Some(req))
        } else {
            // Set the waker to the context, then the stream can be waked up after the pending queue
            // is no longer full.
            this.self_waker.register(cx.waker());
            Poll::Pending
        }
    }
}

fn allocate_timestamps(
    resp: &TsoResponse,
    pending_requests: &mut VecDeque<RequestGroup>,
) -> Result<()> {
    // PD returns the timestamp with the biggest logical value. We can send back timestamps
    // whose logical value is from `logical - count + 1` to `logical` using the senders
    // in `pending`.
    let tail_ts = resp
        .timestamp
        .as_ref()
        .ok_or_else(|| internal_err!("No timestamp in TsoResponse"))?;

    let mut offset = resp.count;
    if let Some(RequestGroup {
        tso_request,
        requests,
    }) = pending_requests.pop_front()
    {
        if tso_request.count != offset {
            return Err(internal_err!(
                "PD gives different number of timestamps than expected"
            ));
        }

        for request in requests {
            offset -= 1;
            let ts = Timestamp {
                physical: tail_ts.physical,
                logical: tail_ts.logical - offset as i64,
                suffix_bits: tail_ts.suffix_bits,
            };
            let _ = request.send(ts);
        }
    } else {
        return Err(internal_err!("PD gives more TsoResponse than expected"));
    };
    Ok(())
}

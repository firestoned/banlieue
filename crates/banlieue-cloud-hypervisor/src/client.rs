// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The client: one HTTP/1.1 request per connection over a VMM's Unix socket.
//!
//! A connection per call keeps the client stateless. The provider re-derives
//! everything from Kubernetes on each reconcile and never trusts its own
//! memory of a VMM (ADR-0063 Decision 2), so a pooled connection would only
//! be state to go stale. Every call is bounded by the client's timeout.
//! There is no retry here: retrying belongs to the reconciler, where backoff
//! already lives (ADR-0061 Decision 7).

use crate::error::{Error, Result};
use crate::socket::{ExpectedSocket, check_socket, socket_meta};
use crate::types::{GuestPlan, VmInfo, VmmPing};
use crate::wire::{self, Endpoint};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

/// Host header value. The VMM ignores it; HTTP/1.1 requires one.
const HOST: &str = "localhost";

/// A client for one VMM, addressed by its API socket.
#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
    expected: Option<ExpectedSocket>,
    timeout: Duration,
}

impl Client {
    /// A client for the VMM at `socket`, with `timeout` per call.
    pub fn new(socket: impl AsRef<Path>, timeout: Duration) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            expected: None,
            timeout,
        }
    }

    /// Refuse to connect unless the socket is owned as ADR-0063 creates it.
    /// The provider always sets this; tests of the transport may not.
    #[must_use]
    pub fn with_expected_owner(mut self, expected: ExpectedSocket) -> Self {
        self.expected = Some(expected);
        self
    }

    /// `vmm.ping`. Use [`wire::check_version`] on the result before driving
    /// a VMM (ADR-0061 Decision 5).
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn ping(&self) -> Result<VmmPing> {
        let body = self.call(Endpoint::VmmPing, None).await?;
        wire::decode_ping(&body)
    }

    /// `vm.create` from `plan`.
    ///
    /// # Errors
    /// Any [`Error`] from the call; [`Error::Api`] if a VM already exists.
    pub async fn create(&self, plan: &GuestPlan) -> Result<()> {
        self.call(Endpoint::VmCreate, Some(wire::encode_vm_create(plan)))
            .await
            .map(drop)
    }

    /// `vm.boot`.
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn boot(&self) -> Result<()> {
        self.call(Endpoint::VmBoot, None).await.map(drop)
    }

    /// `vm.info`. `None` when the VMM is up but no VM has been created.
    ///
    /// # Errors
    /// Any [`Error`] from the call other than "not created".
    pub async fn info(&self) -> Result<Option<VmInfo>> {
        match self.call(Endpoint::VmInfo, None).await {
            Ok(body) => wire::decode_info(&body).map(Some),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `vm.power-button`: ask the guest to shut down.
    ///
    /// # Errors
    /// Any [`Error`] from the call; [`Error::Api`] if the VM is not running.
    pub async fn power_button(&self) -> Result<()> {
        self.call(Endpoint::VmPowerButton, None).await.map(drop)
    }

    /// `vm.shutdown`: stop the VM now.
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn shutdown(&self) -> Result<()> {
        self.call(Endpoint::VmShutdown, None).await.map(drop)
    }

    /// `vm.delete`.
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn delete(&self) -> Result<()> {
        self.call(Endpoint::VmDelete, None).await.map(drop)
    }

    /// `vmm.shutdown`: end the VMM process.
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn shutdown_vmm(&self) -> Result<()> {
        self.call(Endpoint::VmmShutdown, None).await.map(drop)
    }

    /// `vm.remove-device`: hot-unplug the device with `id` (ADR-0065).
    ///
    /// # Errors
    /// Any [`Error`] from the call.
    pub async fn remove_device(&self, id: &str) -> Result<()> {
        self.call(
            Endpoint::VmRemoveDevice,
            Some(wire::encode_remove_device(id)),
        )
        .await
        .map(drop)
    }

    /// One request, bounded by the timeout. Returns the body of a 2xx reply.
    async fn call(&self, endpoint: Endpoint, body: Option<Vec<u8>>) -> Result<Bytes> {
        if let Some(expected) = self.expected {
            check_socket(socket_meta(&self.socket)?, expected)?;
        }
        tokio::time::timeout(self.timeout, self.exchange(endpoint, body))
            .await
            .map_err(|_| Error::Timeout(self.timeout))?
    }

    async fn exchange(&self, endpoint: Endpoint, body: Option<Vec<u8>>) -> Result<Bytes> {
        let stream = UnixStream::connect(&self.socket).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        // The connection task ends when the exchange does; a failure there
        // surfaces through `send_request` below, so its own result is not
        // needed.
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let mut req = http::Request::builder()
            .method(endpoint.method())
            .uri(endpoint.path())
            .header(http::header::HOST, HOST);
        if body.is_some() {
            req = req.header(http::header::CONTENT_TYPE, "application/json");
        }
        let req = req
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| Error::Http(e.to_string()))?;

        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Http(e.to_string()))?
            .to_bytes();
        if !status.is_success() {
            return Err(wire::api_error(status.as_u16(), &bytes));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;

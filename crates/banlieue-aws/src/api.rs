// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The client interface reconcilers depend on (ADR-0174 Decision 2).
//!
//! Reconcilers take `Arc<dyn Ec2Api>` so unit tests substitute
//! [`FakeEc2`](crate::fake::FakeEc2), which refuses what EC2 refuses. EC2
//! has no long-running operation handles: a create, stop, start or
//! terminate returns at once and the provider observes the instance's state
//! on its next reconcile. No method blocks on a state change.
//!
//! Terminating an instance that is already gone is success, as it is in
//! EC2 (the call answers with the `terminated` state, and a vanished ID is
//! `InvalidInstanceID.NotFound`, which [`Ec2Api::terminate_instance`]
//! returns as `Ok(None)`).

use async_trait::async_trait;

use crate::error::Result;
use crate::instance::{EkKeyType, RunInstancesRequest};
use crate::types::{
    AvailabilityZone, ImageInfo, Instance, InstanceState, InstanceTypeInfo, InstanceTypeOffering,
    TpmEkPub,
};

/// Everything banlieue's EC2 provider asks of EC2 (ADR-0174 Decision 3).
#[async_trait]
pub trait Ec2Api: Send + Sync {
    /// `RunInstances` for one hardened instance; returns its instance ID. A
    /// retry with the same client token and parameters returns the same ID.
    ///
    /// # Errors
    /// Typed per [`crate::error::classify`]: no capacity or not offered in
    /// the zone, quota, a reused token with new parameters, policy refusal.
    async fn run_instance(&self, request: &RunInstancesRequest) -> Result<String>;

    /// `DescribeInstances` for one ID; `Ok(None)` when EC2 does not know
    /// it (gone, or not visible yet just after the create).
    ///
    /// # Errors
    /// Anything but not-found.
    async fn describe_instance(&self, instance_id: &str) -> Result<Option<Instance>>;

    /// `DescribeInstances` filtered by `tag:<key>=<value>`: adoption before
    /// a create, the deletion check and the orphan sweep (ADR-0158).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn find_instances_by_tag(&self, key: &str, value: &str) -> Result<Vec<Instance>>;

    /// `TerminateInstances`; returns the new state, or `Ok(None)` when the
    /// instance is already gone.
    ///
    /// # Errors
    /// Anything but not-found.
    async fn terminate_instance(&self, instance_id: &str) -> Result<Option<InstanceState>>;

    /// `StopInstances` (`desiredPowerState: PoweredOff`); returns the new
    /// state.
    ///
    /// # Errors
    /// Any EC2 error.
    async fn stop_instance(&self, instance_id: &str) -> Result<InstanceState>;

    /// `StartInstances`; returns the new state.
    ///
    /// # Errors
    /// Any EC2 error.
    async fn start_instance(&self, instance_id: &str) -> Result<InstanceState>;

    /// `GetConsoleOutput` with `Latest`: the serial console text, empty
    /// before the first snapshot (ADR-0174 Decision 3, ADR-0175).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn console_output(&self, instance_id: &str) -> Result<String>;

    /// `GetInstanceTpmEkPub` in DER (ADR-0167).
    ///
    /// # Errors
    /// Any EC2 error; an instance without NitroTPM is refused by EC2.
    async fn tpm_ek_pub(&self, instance_id: &str, key_type: EkKeyType) -> Result<TpmEkPub>;

    /// `ModifyInstanceMetadataOptions` with `HttpEndpoint=disabled`: IMDS
    /// off once the guest is ready (ADR-0174 Decision 3).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn disable_instance_metadata(&self, instance_id: &str) -> Result<()>;

    /// `DescribeAvailabilityZones`: names and IDs (ADR-0158 Decision 3).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn availability_zones(&self) -> Result<Vec<AvailabilityZone>>;

    /// `DescribeInstanceTypeOfferings` by zone ID for these types
    /// (ADR-0160).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn instance_type_offerings(
        &self,
        instance_types: &[String],
    ) -> Result<Vec<InstanceTypeOffering>>;

    /// `DescribeInstanceTypes`: vCPUs, architectures, NitroTPM and boot
    /// modes (ADR-0160, ADR-0167 Decision 5).
    ///
    /// # Errors
    /// Any EC2 error.
    async fn instance_types(&self, instance_types: &[String]) -> Result<Vec<InstanceTypeInfo>>;

    /// `DescribeImages` for one AMI: its state, architecture, root device,
    /// boot mode and TPM support (roadmap 24 phase E). `Ok(None)` when EC2
    /// does not know it or the caller may not see it
    /// (`InvalidAMIID.NotFound`, or an empty set).
    ///
    /// # Errors
    /// [`crate::Error::Invalid`] for an ID that is not `ami-<hex>`, before
    /// anything is sent; any EC2 error but not-found
    /// (`InvalidAMIID.Malformed` is an invalid request).
    async fn describe_image(&self, image_id: &str) -> Result<Option<ImageInfo>>;
}

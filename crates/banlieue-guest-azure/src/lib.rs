// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The in-guest half of a Kairos image on Azure (ADR-0163 Decision 3,
//! ADR-0171).
//!
//! `banlieue-imagebuilder` adds this binary and a cloud-config layer to
//! every image built for an `azure` source. The layer does the declarative
//! parts; this binary does the two imperative ones:
//!
//! - [`customdata`]: read `customData` from the **local** provisioning disk
//!   (`ovf-env.xml` on a UDF CD-ROM), never from a metadata address, and
//!   write it to `/oem/91_azure_customdata.yaml` (mode 0600, atomically) so
//!   Kairos applies it on this and every later boot.
//! - [`ready`]: tell Azure's host-local WireServer the VM is ready
//!   (`comp=goalstate`, then a `Health` document to `comp=health`), retried
//!   with back-off until a 2xx or a deadline.
//!
//! Both do nothing and succeed when the machine is not on Azure
//! ([`azure::is_azure`], the DMI chassis asset tag). Both log to
//! `/dev/kmsg` ([`kmsg`]) and never log the content of `customData`.
//!
//! Every decision is a pure function over bytes or a directory, unit tested
//! without root; only [`customdata::mount_and_read`] needs a real machine.

pub mod azure;
pub mod cli;
pub mod customdata;
pub mod goalstate;
pub mod http;
pub mod kmsg;
pub mod ovf;
pub mod ready;

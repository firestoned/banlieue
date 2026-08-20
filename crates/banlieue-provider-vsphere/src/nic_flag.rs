// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `--nic key=value,key=value` CLI flag encoding for `VMImageTemplateNic`
//! (ADR-0031).
//!
//! `VMImageSpec.template.network` is a list — a template can declare several
//! NICs. `banlieue provider vsphere image-import`'s `ImportArgs` threads one
//! `--nic` occurrence per entry rather than three separately-repeated flags
//! (`--network`/`--network-adapter`/`--nic-pci-slot`), which would be
//! ambiguous the moment the repeat counts don't line up. This mirrors the
//! delimited-string pattern `banlieue_provider_sdk::scheduling` already uses
//! for `--toleration` / `--node-selector`.
//!
//! [`serialize_nic_flag`] (used by
//! [`crate::reconciler::vmimage::build_import_job`] to build the Job's argv)
//! and [`parse_nic_flag`] (used by `image-import` itself to read it back) are
//! exact inverses of each other for any value [`serialize_nic_flag`] can
//! produce.

use banlieue_api::banlieue::{NicAdapter, VMImageTemplateNic};

const KEY_NETWORK: &str = "network";
const KEY_ADAPTER: &str = "adapter";
const KEY_PCI_SLOT: &str = "pciSlot";

/// Encode one `VMImageTemplateNic` as a `--nic` flag value. Only fields that
/// are `Some` appear; a fully-default NIC (every field `None`) encodes as the
/// empty string — a valid, parseable `--nic ""` meaning "one more NIC, every
/// default applies."
#[must_use]
pub fn serialize_nic_flag(nic: &VMImageTemplateNic) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(network) = &nic.network {
        parts.push(format!("{KEY_NETWORK}={network}"));
    }
    if let Some(adapter) = &nic.adapter {
        parts.push(format!("{KEY_ADAPTER}={}", adapter.as_str()));
    }
    if let Some(pci_slot) = nic.pci_slot {
        parts.push(format!("{KEY_PCI_SLOT}={pci_slot}"));
    }
    parts.join(",")
}

/// Parse a `--nic` flag value into a `VMImageTemplateNic`. The empty string
/// parses to `VMImageTemplateNic::default()` (every field unset).
///
/// # Errors
/// A message naming the offending segment when a `key=value` pair is
/// malformed (no `=`), the key is not one of `network`/`adapter`/`pciSlot`,
/// or a recognized key's value fails to parse (`adapter` must be a known
/// [`NicAdapter`]; `pciSlot` must be a valid `i32`).
pub fn parse_nic_flag(s: &str) -> Result<VMImageTemplateNic, String> {
    let mut nic = VMImageTemplateNic::default();
    if s.is_empty() {
        return Ok(nic);
    }
    for segment in s.split(',') {
        let Some((key, value)) = segment.split_once('=') else {
            return Err(format!(
                "--nic segment {segment:?} must be key=value (expected one of \
                 {KEY_NETWORK}, {KEY_ADAPTER}, {KEY_PCI_SLOT})"
            ));
        };
        match key {
            KEY_NETWORK => nic.network = Some(value.to_string()),
            KEY_ADAPTER => {
                nic.adapter = Some(
                    value
                        .parse::<NicAdapter>()
                        .map_err(|e| format!("--nic adapter={value:?}: {e}"))?,
                );
            }
            KEY_PCI_SLOT => {
                nic.pci_slot = Some(
                    value
                        .parse::<i32>()
                        .map_err(|_| format!("--nic pciSlot={value:?} is not a valid integer"))?,
                );
            }
            other => {
                return Err(format!(
                    "--nic key {other:?} not recognized (expected one of \
                     {KEY_NETWORK}, {KEY_ADAPTER}, {KEY_PCI_SLOT})"
                ));
            }
        }
    }
    Ok(nic)
}

#[cfg(test)]
#[path = "nic_flag_tests.rs"]
mod nic_flag_tests;

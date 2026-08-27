// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Guest-data placeholder substitution, shared by every provider that
//! delivers a `VirtualMachine.spec.userData` cloud-config to a guest
//! (ADR-0024).
//!
//! Deliberately a fixed, documented placeholder set — not a general
//! templating engine (Tera/Handlebars/etc.) — matching the "explicit over
//! implicit" principle: the substitutions are exhaustive and auditable from
//! this one module, not Turing-complete.

use banlieue_api::common::StaticIpamConfig;

/// Values available for substitution into a raw cloud-config via
/// [`render_placeholders`]. Built once per VM from its name and (if any)
/// resolved static network override.
pub struct GuestDataContext<'a> {
    vm_name: &'a str,
    static_: Option<&'a StaticIpamConfig>,
}

impl<'a> GuestDataContext<'a> {
    /// Build a context from a VM name and its resolved static network
    /// override, if any (`None` for a plain `dhcp` interface — every
    /// network placeholder then substitutes to the empty string).
    pub fn from_static(vm_name: &'a str, static_: Option<&'a StaticIpamConfig>) -> Self {
        Self { vm_name, static_ }
    }
}

/// Substitute the fixed ADR-0024 placeholder set into `raw`:
///
/// | Placeholder | Source |
/// | --- | --- |
/// | `${VM_NAME}` | `ctx`'s VM name |
/// | `${FQDN}` | `<vm-name>.<domain>` (domain empty -> trailing dot) |
/// | `${IP}` | resolved static `address`, or empty |
/// | `${PREFIX}` | resolved static `prefix`, or empty |
/// | `${GATEWAY}` | resolved static `gateway`, or empty |
/// | `${DNS}` | resolved static `nameservers`, comma-joined, or empty |
/// | `${DOMAIN}` | resolved static `domain`, or empty |
///
/// Any other `${...}` is left untouched — this is literal string
/// replacement, not a templating engine.
pub fn render_placeholders(raw: &str, ctx: &GuestDataContext<'_>) -> String {
    let domain = ctx.static_.and_then(|s| s.domain.as_deref()).unwrap_or("");
    let address = ctx.static_.map(|s| s.address.as_str()).unwrap_or("");
    let prefix = ctx
        .static_
        .map(|s| s.prefix.to_string())
        .unwrap_or_default();
    let gateway = ctx.static_.and_then(|s| s.gateway.as_deref()).unwrap_or("");
    let dns = ctx
        .static_
        .map(|s| s.nameservers.join(","))
        .unwrap_or_default();

    raw.replace("${VM_NAME}", ctx.vm_name)
        .replace("${FQDN}", &fqdn(ctx.vm_name, domain))
        .replace("${IP}", address)
        .replace("${PREFIX}", &prefix)
        .replace("${GATEWAY}", gateway)
        .replace("${DNS}", &dns)
        .replace("${DOMAIN}", domain)
}

/// Combine `vm_name` and `domain` into an FQDN, matching `${FQDN}`'s
/// documented "domain empty -> trailing dot" behavior — but without
/// double-appending the domain when `vm_name` is already fully qualified
/// with it. `metadata.name` is a DNS-1123 subdomain, which permits dots
/// (confirmed live: a `VirtualMachine` named as a full FQDN applies
/// cleanly), so a VM named `db-01.example.com` with `domain =
/// "example.com"` must render as `db-01.example.com`, not
/// `db-01.example.com.example.com`. Suffix match is case-insensitive — DNS
/// names are case-insensitive.
fn fqdn(vm_name: &str, domain: &str) -> String {
    if domain.is_empty() {
        return format!("{vm_name}.");
    }
    let suffix = format!(".{}", domain.to_ascii_lowercase());
    if vm_name.to_ascii_lowercase().ends_with(&suffix) {
        vm_name.to_string()
    } else {
        format!("{vm_name}.{domain}")
    }
}

#[cfg(test)]
#[path = "guestdata_tests.rs"]
mod guestdata_tests;

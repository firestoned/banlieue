// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Collision-safe Kubernetes object names built from a set of identifying
//! parts.
//!
//! Found live, twice (`failure_domain_name`, then `import_job_name`): naively
//! truncating a hyphen-joined name at Kubernetes' 63-char DNS-label limit
//! collapses distinct inputs onto the same string whenever they share
//! everything except a trailing suffix — exactly what real vCenter
//! datacenter/cluster/failure-domain names do.

/// Kubernetes object names (DNS-1123 labels) cap at this many characters.
pub const MAX_NAME_LEN: usize = 63;

/// Build a DNS-label-safe, collision-resistant name by slugifying and
/// joining `parts` with `-`.
///
/// When the slugified join fits within [`MAX_NAME_LEN`], it is returned as
/// is — readable and stable. When it doesn't, the name is truncated to leave
/// room for a hash suffix computed over `parts` as *structured*,
/// NUL-separated fields — not over the joined/slugified string. Hashing the
/// already-joined string would still leave a subtler collision open: parts
/// commonly contain `-` themselves, so two different part sets can join to
/// the same string (or share a truncation-surviving prefix) without a
/// separator that can't occur inside a part.
pub fn collision_safe_name(parts: &[&str]) -> String {
    let slug = slugify(&parts.join("-"));
    if slug.chars().count() <= MAX_NAME_LEN {
        return slug;
    }
    let identity = parts.join("\u{0}");
    let hash = stable_hash8(&identity);
    // Reserve room for the separating '-' and the 8-char hash. slug is ASCII
    // (slugify emits only `[a-z0-9-]`), so byte-slicing is char-safe.
    let keep = MAX_NAME_LEN - hash.len() - 1;
    let head = slug[..keep].trim_end_matches('-');
    format!("{head}-{hash}")
}

/// Lowercase the input, replace any run of non-alphanumeric characters with
/// a single `-`, and strip leading/trailing dashes.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Deterministic 32-bit FNV-1a of `s`, as 8 lowercase hex chars.
///
/// Hand-rolled on purpose: `std`'s `DefaultHasher` output may change between
/// Rust releases, which would silently rename every truncated name on a
/// toolchain bump. FNV-1a is fixed forever, so the generated names are
/// stable.
fn stable_hash8(s: &str) -> String {
    const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:08x}")
}

#[cfg(test)]
#[path = "k8s_name_tests.rs"]
mod k8s_name_tests;

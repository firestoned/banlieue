// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Test support, behind the `testing` feature: play the upstream PKI that
//! signs a host's CSR (ADR-0146 Decision 7), so a crate that installs a host
//! CA can be tested without OpenBao. Never part of a production build.

use std::time::{Duration, SystemTime};

use crate::testutil::{UpstreamOpts, upstream_root, upstream_sign};

const ROOT_CN: &str = "test-attestation-root";
const SECONDS_PER_DAY: u64 = 86_400;
const VALID_FOR_DAYS: u64 = 365;

/// Sign `csr_pem` as a well-behaved upstream PKI would at `now`: an
/// intermediate named by the request, `CA:TRUE`, path length 0, `keyCertSign`,
/// valid for a year. Returns the host certificate followed by a fresh
/// self-signed root, as `sign-intermediate` plus the issuing chain would.
///
/// # Panics
/// On a request that does not parse: this is for tests.
#[must_use]
pub fn upstream_sign_csr(csr_pem: &str, now: SystemTime) -> String {
    let (root_key, root_pem) = upstream_root(ROOT_CN);
    let opts = UpstreamOpts {
        not_before: now - Duration::from_secs(SECONDS_PER_DAY),
        not_after: now + Duration::from_secs(VALID_FOR_DAYS * SECONDS_PER_DAY),
        ..UpstreamOpts::good()
    };
    let host = upstream_sign(csr_pem, &root_key, ROOT_CN, &opts);
    format!("{host}{root_pem}")
}

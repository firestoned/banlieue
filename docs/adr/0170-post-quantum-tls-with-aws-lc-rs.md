<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0170: Post-quantum TLS with the aws-lc-rs crypto provider

- **Status:** Accepted
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Supersedes:** ADR-0009's choice of the ring crypto provider (the rest of ADR-0009 stands)
- **Related:** [ADR-0009](0009-vim-rs-0.5-rustls-ring-retire-vendoring.md)
  (rustls everywhere, vim_rs in Bring-Your-Own-Client mode),
  [ADR-0159](0159-cloud-credentials-by-workload-identity-federation.md)
  Decision 5 (offer X25519MLKEM768 first), threat model §7.20 and §8.

## Context

ADR-0009 put every banlieue binary on rustls and chose `ring` as the one
crypto provider, because kube and hyper-rustls already used it and it needs no
C toolchain. `ring` implements X25519, P-256 and P-384 key exchange and no
ML-KEM. Every TLS connection banlieue makes (the Kubernetes API, vCenter,
Proxmox VE, libvirtd, OCI registries, the OTLP collector, OpenBao, GitHub
downloads, Azure Resource Manager and Entra ID) therefore uses classical key
exchange, and a recording of any of them can be decrypted later by an
adversary with a quantum computer ("harvest now, decrypt later"). The threat
model recorded that as an accepted risk (§8) and ADR-0159 Decision 5 asked
the cloud clients to offer X25519MLKEM768 first, which `ring` cannot do.

The maintainer's rule, 2026-11-25: no library anywhere in banlieue that
cannot do post-quantum cryptography.

rustls 0.23 ships a second provider, `aws-lc-rs` (AWS-LC, a BoringSSL
derivative with formally verified ML-KEM). It implements the hybrid group
`X25519MLKEM768` (X25519 with ML-KEM-768, the group browsers, Cloudflare,
Google and OpenSSL 3.5 deploy) and, with the crate feature
`prefer-post-quantum`, puts it first in its default key-exchange order.
kube 5.2, hyper-rustls, tokio-rustls, reqwest 0.13 and rcgen all have an
`aws-lc-rs` feature, and x509-parser has `verify-aws`. The cost is a C
library compiled by `aws-lc-sys`'s build script.

## Decision

1. **One process-wide rustls `CryptoProvider`: aws-lc-rs.** A new internal
   crate, `crates/banlieue-tls`, builds it (`provider()`) and installs it as
   the process default (`install_default_crypto_provider()`). Every binary
   role and every test that opens a TLS connection calls it; the per-crate
   `install_default_crypto_provider` functions in the vSphere, Proxmox and
   Azure providers and `banlieue_oci::install_crypto_provider` are re-exports
   or thin wrappers of it, not copies. Clients that build their own config
   (`banlieue-libvirt`'s mutual TLS, test servers) use
   `builder_with_provider(banlieue_tls::provider())`.
2. **X25519MLKEM768 first.** The provider's key-exchange order is explicit:
   `X25519MLKEM768`, then `X25519`, `secp256r1`, `secp384r1`. The workspace
   also enables rustls' `prefer-post-quantum` feature, so a provider that a
   dependency installs from aws-lc-rs's own defaults (kube-client does on some
   paths) agrees with ours. A unit test asserts the installed default's first
   group is `X25519MLKEM768`, which a `ring` provider cannot satisfy.
3. **Classical fallback stays, for peers only.** The classical groups remain
   in the list because peers that banlieue does not control do not accept
   ML-KEM yet: Azure Resource Manager and Entra ID (measured classical on
   2026-11-23 and again on 2026-11-26), and some hypervisor APIs and
   GnuTLS-based libvirtd builds. A handshake with such a peer negotiates a
   classical group; the same binary negotiates `X25519MLKEM768` with a peer
   that offers it, with no change on banlieue's side when the peer upgrades.
4. **`ring` is banned from the dependency graph.** kube's default features
   (which include `ring`) are off in the workspace; rcgen uses `aws_lc_rs`,
   tokio-rustls `aws_lc_rs`, x509-parser `verify-aws`. `deny.toml` carries
   `[bans] deny = [{ crate = "ring" }]`, so `cargo deny check` (CI's
   `cargo-deny` job) fails if any dependency brings it back.
5. **Ed25519 via aws-lc-rs.** The Azure provider's throwaway SSH key
   (ADR-0164) is generated with `aws_lc_rs::signature::Ed25519KeyPair`
   instead of `ring`.

## Consequences

- Every banlieue TLS client offers post-quantum key exchange first. Links to
  peers that accept it (a current Kubernetes API server built with Go 1.24 or
  later, which enables X25519MLKEM768 by default, and public endpoints such as
  Cloudflare's and Google's) are protected against harvest-now, decrypt-later
  today. The residual risk is only the peers that do not accept it, recorded
  in threat model §8.
- The build now compiles AWS-LC from C through `aws-lc-sys`: a C compiler is
  required. For Linux x86_64 and aarch64 (the only targets banlieue ships)
  `aws-lc-sys` uses its `cc` builder with pregenerated bindings, so neither
  `cmake`, `bindgen`/libclang nor NASM is needed; `cmake` becomes necessary
  only for a FIPS build or a target without pregenerated sources. The native
  Linux gnu builds in CI (ubuntu-24.04 and ubuntu-24.04-arm) already have a C
  compiler. The macOS-hosted cross build of `make kind-load` passes the gcc
  cross-toolchain through `CC_<target>`, which `aws-lc-sys` honours.
- AWS-LC is OpenSSL-derived C code, a larger audited-C surface than `ring`'s
  mostly-Rust-and-assembly core. It is maintained by AWS, has a FIPS 140-3
  validated mode (not enabled here), and is what rustls itself defaults to.
- Signatures stay classical (RSA, ECDSA, Ed25519) everywhere: certificates,
  tokens and the SSH key. Post-quantum signatures are not deployable in the
  WebPKI or Kubernetes PKI yet; that remains a separate accepted risk.
- Other crypto crates remain in the graph (RustCrypto hashes and the
  x509 parsing stack); none of them performs key exchange. Whether any of
  them must move is a separate decision.

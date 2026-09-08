# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Distroless production Dockerfile for banlieue binaries.
#
# This Dockerfile expects a pre-built Linux binary at
# `binaries/<TARGETARCH>/<BINARY>` — built by the Makefile via cross-compile or
# a host gcc cross-toolchain. We never compile inside the container. The binary
# uses rustls (no OpenSSL), so this is a plain single-stage COPY.
#
# Build with:
#     make docker-build           # auto host arch (BINARY defaults to banlieue)
#     make docker-build-amd64     # linux/amd64
#     make docker-build-arm64     # linux/arm64
#
# A single `banlieue` binary packages every role (controller + providers); the
# role is selected at runtime via container args, not by building a different
# binary. BINARY is still parameterized so the supply-chain plumbing stays
# generic, but it defaults to `banlieue`.

# Pinned by digest for supply-chain reproducibility. Dependabot (docker
# ecosystem) opens a PR with the new digest when upstream publishes a patched
# image. Do NOT revert to a floating tag.
#
# The digest MUST sit on a literal `FROM` line. Dependabot's Docker parser
# reads `FROM` instructions and does not expand `ARG` defaults
# (dependabot/dependabot-core#4597, #10190), so a digest hidden in an
# `ARG BASE_IMAGE=...` consumed via `FROM ${BASE_IMAGE}` is invisible to it and
# never gets re-pinned.
#
# BASE_IMAGE stays overridable for air-gapped / mirrored builds: it defaults to
# the *stage name*, so an unset override resolves to the pinned digest below
# and a set one resolves to the caller's mirror. An override deliberately
# bypasses the digest pin — the mirror is then the trusted source. When it is
# set, BuildKit prunes the unreferenced `pinned-base` stage from the build
# graph, so an air-gapped build never reaches upstream.
ARG BASE_IMAGE=pinned-base

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:c31ff9abcb1910f3ab25c7957bdaf0bfe12a01eb546e8df2282f1c8f682b606c AS pinned-base

FROM ${BASE_IMAGE}

ARG VERSION
ARG GIT_SHA
ARG TARGETARCH
# Name of the workspace binary to ship. A single `banlieue` binary packages
# every role; the role is chosen at runtime via container args
# (`["controller"]`, `["provider","vsphere"]`). See ADR-0004.
ARG BINARY=banlieue

# Reference recorded in org.opencontainers.image.base.name. Supplied by the
# Makefile, which resolves it to whatever the build actually used: the
# BASE_IMAGE override when set, otherwise the pinned `FROM` read out of this
# file. Never hardcode the upstream registry here — an air-gapped build from an
# internal mirror would then ship a label naming a registry it never contacted.
# `BASE_IMAGE` itself is unusable for this: it holds the stage name by default.
ARG BASE_IMAGE_REF

LABEL org.opencontainers.image.source="https://github.com/firestoned/banlieue" \
      org.opencontainers.image.description="banlieue — Kubernetes-native abstract virtualization API (${BINARY})" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${GIT_SHA}" \
      org.opencontainers.image.base.name="${BASE_IMAGE_REF}" \
      banlieue.io/binary="${BINARY}"

# Copy the pre-built binary for the target architecture. The Makefile stages
# binaries at `binaries/<arch>/<binary>`. The binary uses rustls (no OpenSSL),
# so the distroless/cc base needs no extra shared libraries.
COPY --chmod=755 binaries/${TARGETARCH}/${BINARY} /app

USER nonroot

EXPOSE 8080 8081

ENTRYPOINT ["/app"]

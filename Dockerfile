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
ARG BASE_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:8f960b7fc6a5d6e28bb07f982655925d6206678bd9a6cde2ad00ddb5e2077d78

FROM ${BASE_IMAGE}

ARG VERSION
ARG GIT_SHA
ARG TARGETARCH
ARG BASE_IMAGE
# Name of the workspace binary to ship. A single `banlieue` binary packages
# every role; the role is chosen at runtime via container args
# (`["controller"]`, `["provider","vsphere"]`). See ADR-0004.
ARG BINARY=banlieue

LABEL org.opencontainers.image.source="https://github.com/firestoned/banlieue" \
      org.opencontainers.image.description="banlieue — Kubernetes-native abstract virtualization API (${BINARY})" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${GIT_SHA}" \
      org.opencontainers.image.base.name="${BASE_IMAGE}" \
      banlieue.io/binary="${BINARY}"

# Copy the pre-built binary for the target architecture. The Makefile stages
# binaries at `binaries/<arch>/<binary>`. The binary uses rustls (no OpenSSL),
# so the distroless/cc base needs no extra shared libraries.
COPY --chmod=755 binaries/${TARGETARCH}/${BINARY} /app

USER nonroot

EXPOSE 8080 8081

ENTRYPOINT ["/app"]

# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Distroless production Dockerfile for banlieue binaries.
#
# This Dockerfile expects a pre-built Linux binary at
# `binaries/<TARGETARCH>/<BINARY>` — built by the Makefile via cross-compile or
# a host gcc cross-toolchain. We never compile inside the container.
#
# Build with:
#     make docker-build BINARY=banlieue-controller          # auto host arch
#     make docker-build-amd64 BINARY=banlieue-controller    # linux/amd64
#     make docker-build-arm64 BINARY=banlieue-controller    # linux/arm64
#
# The same Dockerfile is reused for every banlieue binary (controller and the
# upcoming providers) by passing BINARY at build time — keeps the supply-chain
# story uniform across the workspace.

# Pinned by digest for supply-chain reproducibility. Dependabot (docker
# ecosystem) opens a PR with the new digest when upstream publishes a patched
# image. Do NOT revert to a floating tag.
ARG BASE_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:8f960b7fc6a5d6e28bb07f982655925d6206678bd9a6cde2ad00ddb5e2077d78

FROM ${BASE_IMAGE}

ARG VERSION
ARG GIT_SHA
ARG TARGETARCH
ARG BASE_IMAGE
# Name of the workspace binary to ship in this image (e.g. banlieue-controller,
# banlieue-provider-vsphere). Defaults to the main controller.
ARG BINARY=banlieue-controller

LABEL org.opencontainers.image.source="https://github.com/firestoned/banlieue" \
      org.opencontainers.image.description="banlieue — Kubernetes-native abstract virtualization API (${BINARY})" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${GIT_SHA}" \
      org.opencontainers.image.base.name="${BASE_IMAGE}" \
      banlieue.io/binary="${BINARY}"

# Copy the pre-built binary for the target architecture. The Makefile stages
# binaries at `binaries/<arch>/<binary>`.
COPY --chmod=755 binaries/${TARGETARCH}/${BINARY} /app

USER nonroot

EXPOSE 8080 8081

ENTRYPOINT ["/app"]

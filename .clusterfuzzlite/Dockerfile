# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ClusterFuzzLite build image for banlieue's cargo-fuzz targets. CIFuzz builds
# this image, then runs /src/build.sh inside it to produce fuzzer binaries in
# $OUT. Base image pinned by digest (Scorecard Pinned-Dependencies); bump when
# oss-fuzz-base rolls forward. Pinned 2026-08: base-builder-rust:latest
FROM gcr.io/oss-fuzz-base/base-builder-rust@sha256:3428d0a90b1e7efbe2ba947bd1d0c9a70594986b4b6553804547f1fb24cfc92f

# The base image pins nightly-2025-09-05 (rustc 1.91), below the workspace
# MSRV (1.95), and upstream has not moved it since. Pin a newer nightly here:
# nightly-2026-04-10 is rustc 1.96.0-nightly on LLVM 22, the same LLVM major
# as the image's clang 22, so the sanitizer and libFuzzer builds agree.
# Drop this once base-builder-rust ships a nightly at or above the MSRV.
ENV RUSTUP_TOOLCHAIN=nightly-2026-04-10
RUN rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal

COPY . $SRC/banlieue
COPY .clusterfuzzlite/build.sh $SRC/build.sh
WORKDIR $SRC/banlieue

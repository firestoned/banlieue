# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ClusterFuzzLite build image for banlieue's cargo-fuzz targets. CIFuzz builds
# this image, then runs /src/build.sh inside it to produce fuzzer binaries in
# $OUT. Base image pinned by digest (Scorecard Pinned-Dependencies); bump when
# oss-fuzz-base rolls forward. Pinned 2026-08: base-builder-rust:latest
FROM gcr.io/oss-fuzz-base/base-builder-rust@sha256:1fea2c5bb1f452563710429f195f1d9661032866489dbcd9767aa8a1d1238758

COPY . $SRC/banlieue
COPY .clusterfuzzlite/build.sh $SRC/build.sh
WORKDIR $SRC/banlieue

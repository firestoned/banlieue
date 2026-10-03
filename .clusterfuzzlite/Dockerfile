# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ClusterFuzzLite build image for banlieue's cargo-fuzz targets. CIFuzz builds
# this image, then runs /src/build.sh inside it to produce fuzzer binaries in
# $OUT. Base image pinned by digest (Scorecard Pinned-Dependencies); bump when
# oss-fuzz-base rolls forward. Pinned 2026-08: base-builder-rust:latest
FROM gcr.io/oss-fuzz-base/base-builder-rust@sha256:9643b583ef5aa3d956b3c170e93ef8283878c70c6df8c99d8a70d73c5019677f

COPY . $SRC/banlieue
COPY .clusterfuzzlite/build.sh $SRC/build.sh
WORKDIR $SRC/banlieue

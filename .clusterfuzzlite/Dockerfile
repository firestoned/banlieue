# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ClusterFuzzLite build image for banlieue's cargo-fuzz targets. CIFuzz builds
# this image, then runs /src/build.sh inside it to produce fuzzer binaries in
# $OUT. Base image pinned by digest (Scorecard Pinned-Dependencies); bump when
# oss-fuzz-base rolls forward. Pinned 2026-08: base-builder-rust:latest
FROM gcr.io/oss-fuzz-base/base-builder-rust@sha256:433319ea1a6bbea6c1db1915a5d385021908cc62026d9f0efda2270ef6060de3

COPY . $SRC/banlieue
COPY .clusterfuzzlite/build.sh $SRC/build.sh
WORKDIR $SRC/banlieue

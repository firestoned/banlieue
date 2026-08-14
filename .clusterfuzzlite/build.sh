#!/bin/bash -eux
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ClusterFuzzLite build script — executed by CIFuzz INSIDE the image from
# .clusterfuzzlite/Dockerfile, with $SRC / $OUT set by the infra.

cd "$SRC/banlieue/crates/banlieue-libvirt/fuzz"

# The oss-fuzz base image compiles C++ (libfuzzer-sys's std::thread) against
# libc++ (std::__1:: symbols), but rustc's default linux-gnu link line passes
# -lstdc++, which does not provide them -> undefined reference to
# std::__1::__throw_system_error at link time. Link libc++/libc++abi as well.
# cargo-fuzz appends this env RUSTFLAGS to its own sanitizer flags; prepend to
# the RUSTFLAGS CIFuzz already set rather than replacing them.
export RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-lc++ -Clink-arg=-lc++abi"

cargo fuzz build -O
cp target/x86_64-unknown-linux-gnu/release/decode_message "$OUT"/

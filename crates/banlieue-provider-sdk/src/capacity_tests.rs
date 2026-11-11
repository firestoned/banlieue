// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `capacity.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn bytes_become_whole_mib_and_gib_rounded_down() {
        assert_eq!(bytes_to_mib(3 * MIB + 1), 3);
        assert_eq!(bytes_to_gib(2 * GIB - 1), 1);
        assert_eq!(bytes_to_gib(0), 0);
    }

    /// Free memory is published in whole GiB, so the status changes only
    /// when a GiB is gained or lost (ADR-0128 Decision 5).
    #[test]
    fn free_memory_rounds_down_to_a_whole_gib_in_mib() {
        assert_eq!(round_down_to_gib(40_000), 39 * 1024);
        assert_eq!(round_down_to_gib(1023), 0);
        assert_eq!(round_down_to_gib(2048), 2048);
    }

    /// A measured figure is never more than what guests may use.
    #[test]
    fn available_memory_is_capped_and_rounded() {
        assert_eq!(available_memory_mib(Some(40_000), Some(8192)), Some(8192));
        assert_eq!(available_memory_mib(Some(40_000), None), Some(39 * 1024));
        assert_eq!(available_memory_mib(None, Some(8192)), None);
    }

    #[test]
    fn qemu_overhead_is_published_per_vm() {
        assert_eq!(QEMU_MEMORY_OVERHEAD_MIB, 256);
    }
}

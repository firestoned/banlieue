// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `retry.rs`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;

    #[test]
    fn seconds_are_parsed() {
        // Seen live on a throttled delete: "Please try again after '1' seconds".
        assert_eq!(parse_retry_after("1"), Some(Duration::from_secs(1)));
        assert_eq!(parse_retry_after(" 17 "), Some(Duration::from_secs(17)));
    }

    #[test]
    fn zero_becomes_the_minimum_so_a_loop_never_spins() {
        assert_eq!(parse_retry_after("0"), Some(MIN_RETRY_AFTER));
    }

    #[test]
    fn large_values_are_capped() {
        assert_eq!(parse_retry_after("86400"), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn non_numeric_values_are_ignored() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-3"), None);
    }

    #[test]
    fn retry_delay_falls_back_to_the_default() {
        assert_eq!(retry_delay(None), DEFAULT_RETRY_AFTER);
        assert_eq!(retry_delay(Some("5")), Duration::from_secs(5));
        assert_eq!(retry_delay(Some("soon")), DEFAULT_RETRY_AFTER);
    }
}

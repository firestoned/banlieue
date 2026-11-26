// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `error.rs`. Every body below is the shape ARM returned to
//! the roadmap 23 phase 0 spike.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;

    fn body(code: &str, message: &str) -> Vec<u8> {
        format!(r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#).into_bytes()
    }

    #[test]
    fn a_429_is_throttled_with_the_hint() {
        let e = classify(
            429,
            Some(Duration::from_secs(1)),
            &body(
                "SubscriptionRequestsThrottled",
                "Number of 'delete' requests exceeded.",
            ),
        );
        assert!(
            matches!(e, Error::Throttled { retry_after } if retry_after == Duration::from_secs(1))
        );
        assert!(e.is_retryable());
    }

    #[test]
    fn a_429_without_a_hint_uses_the_default_delay() {
        let e = classify(429, None, b"");
        assert!(
            matches!(e, Error::Throttled { retry_after } if retry_after == crate::retry::DEFAULT_RETRY_AFTER)
        );
    }

    #[test]
    fn a_404_is_not_found() {
        let e = classify(
            404,
            None,
            &body("ResourceNotFound", "The Resource was not found."),
        );
        assert!(e.is_not_found());
        assert_eq!(e.code(), Some("ResourceNotFound"));
    }

    #[test]
    fn size_restrictions_are_their_own_kind() {
        for code in ["SkuNotAvailable", "NotAvailableForSubscription"] {
            let e = classify(409, None, &body(code, "not available in zone 2"));
            assert!(matches!(e, Error::SkuNotAvailable { .. }), "{code}");
            assert!(e.is_zone_specific(), "{code}");
        }
    }

    #[test]
    fn allocation_failures_are_zone_specific_and_retryable_elsewhere() {
        for code in [
            "ZonalAllocationFailed",
            "AllocationFailed",
            "OverconstrainedAllocationRequest",
            "OverconstrainedZonalAllocationRequest",
        ] {
            let e = classify(409, None, &body(code, "no capacity"));
            assert!(matches!(e, Error::AllocationFailed { .. }), "{code}");
            assert!(e.is_zone_specific(), "{code}");
        }
    }

    #[test]
    fn quota_exceeded_is_its_own_kind() {
        let e = classify(
            409,
            None,
            &body(
                "QuotaExceeded",
                "Operation could not be completed as it results in exceeding approved Total Regional Cores quota.",
            ),
        );
        assert!(matches!(e, Error::QuotaExceeded { .. }));
        assert!(!e.is_retryable());
    }

    #[test]
    fn invalid_tag_and_controller_mismatch_are_invalid_requests() {
        let e = classify(
            400,
            None,
            &body("InvalidTagNameCharacters", "reserved characters"),
        );
        assert!(
            matches!(e, Error::InvalidRequest { ref code, .. } if code == "InvalidTagNameCharacters")
        );

        let e = classify(
            400,
            None,
            &body(
                "InvalidParameter",
                "The VM size 'Standard_D2s_v7' cannot boot with OS image or disk.",
            ),
        );
        assert!(matches!(e, Error::InvalidRequest { ref code, .. } if code == "InvalidParameter"));
        assert!(!e.is_retryable());
    }

    #[test]
    fn a_409_with_another_code_is_a_conflict() {
        let e = classify(
            409,
            None,
            &body(
                "Conflict",
                "does not have a matching diskControllerType feature support",
            ),
        );
        assert!(matches!(e, Error::Conflict { .. }));
    }

    #[test]
    fn auth_failures_are_distinguished() {
        assert!(matches!(
            classify(401, None, &body("InvalidAuthenticationToken", "expired")),
            Error::Unauthorized { .. }
        ));
        let e = classify(
            403,
            None,
            &body("AuthorizationFailed", "does not have authorization"),
        );
        assert!(matches!(e, Error::Forbidden { .. }));
        assert!(!e.is_retryable());
    }

    #[test]
    fn server_errors_are_retryable() {
        let e = classify(503, None, b"");
        assert!(matches!(e, Error::Server { status: 503, .. }));
        assert!(e.is_retryable());
    }

    #[test]
    fn an_unparseable_body_still_classifies_by_status() {
        let e = classify(400, None, b"<html>bad gateway</html>");
        assert!(matches!(e, Error::InvalidRequest { ref code, .. } if code.is_empty()));
    }

    #[test]
    fn messages_are_truncated_so_a_huge_body_cannot_flood_logs() {
        let long = "x".repeat(MAX_MESSAGE_LEN * 2);
        let e = classify(400, None, &body("InvalidParameter", &long));
        let Error::InvalidRequest { message, .. } = e else {
            panic!("expected InvalidRequest");
        };
        assert!(message.chars().count() <= MAX_MESSAGE_LEN);
    }

    #[test]
    fn a_failed_operation_with_a_placement_code_reads_as_a_placement_error() {
        let zonal = operation_failure("ZonalAllocationFailed", "no capacity");
        assert!(matches!(zonal, Error::AllocationFailed { .. }));
        assert!(zonal.is_zone_specific());
        assert!(operation_failure("SkuNotAvailable", "").is_zone_specific());
        assert!(matches!(
            operation_failure("QuotaExceeded", ""),
            Error::QuotaExceeded { .. }
        ));
    }

    #[test]
    fn a_failed_operation_with_any_other_code_is_operation_failed() {
        let e = operation_failure("OSProvisioningTimedOut", "the guest never reported");
        assert!(matches!(e, Error::OperationFailed { .. }));
        assert_eq!(e.code(), Some("OSProvisioningTimedOut"));
        assert!(!e.is_zone_specific());
        assert!(!e.is_retryable());
    }
}

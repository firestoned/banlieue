// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `error.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn fault(status: u16, code: &str) -> AwsFault {
        AwsFault::new(status, code, "message", "req-1")
    }

    #[test]
    fn ec2_throttling_on_503_is_throttled() {
        assert!(classify(fault(503, "RequestLimitExceeded")).is_throttled());
    }

    #[test]
    fn sts_and_s3_throttling_codes_are_throttled() {
        assert!(classify(fault(400, "Throttling")).is_throttled());
        assert!(classify(fault(503, "SlowDown")).is_throttled());
    }

    #[test]
    fn an_unknown_code_on_429_or_503_is_throttled() {
        assert!(classify(fault(429, "")).is_throttled());
        assert!(classify(fault(503, "Whatever")).is_throttled());
    }

    #[test]
    fn insufficient_capacity_re_places_the_machine() {
        let e = classify(fault(500, "InsufficientInstanceCapacity"));
        assert!(matches!(e, Error::InsufficientCapacity(_)));
        assert!(e.is_zone_unavailable());
    }

    #[test]
    fn a_type_not_offered_in_the_zone_re_places_the_machine() {
        let e = classify(fault(400, "Unsupported"));
        assert!(matches!(e, Error::NotOffered(_)));
        assert!(e.is_zone_unavailable());
    }

    #[test]
    fn vcpu_quota_is_quota_exceeded_not_a_zone_problem() {
        let e = classify(fault(400, "VcpuLimitExceeded"));
        assert!(matches!(e, Error::QuotaExceeded(_)));
        assert!(!e.is_zone_unavailable());
    }

    #[test]
    fn a_reused_client_token_with_new_parameters_is_its_own_error() {
        assert!(matches!(
            classify(fault(400, "IdempotentParameterMismatch")),
            Error::IdempotentParameterMismatch(_)
        ));
    }

    #[test]
    fn ec2_dot_not_found_codes_and_s3_no_such_codes_are_not_found() {
        assert!(classify(fault(400, "InvalidInstanceID.NotFound")).is_not_found());
        assert!(classify(fault(400, "InvalidSnapshot.NotFound")).is_not_found());
        assert!(classify(fault(404, "NoSuchUpload")).is_not_found());
    }

    #[test]
    fn policy_refusal_and_bad_credentials_are_told_apart() {
        assert!(matches!(
            classify(fault(403, "UnauthorizedOperation")),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            classify(fault(403, "AccessDenied")),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            classify(fault(401, "AuthFailure")),
            Error::Unauthenticated(_)
        ));
        assert!(matches!(
            classify(fault(400, "ExpiredToken")),
            Error::Unauthenticated(_)
        ));
        assert!(matches!(
            classify(fault(400, "InvalidIdentityToken")),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn unknown_codes_fall_back_to_the_status_class() {
        assert!(matches!(
            classify(fault(400, "InvalidParameterValue")),
            Error::InvalidRequest(_)
        ));
        assert!(matches!(
            classify(fault(500, "InternalError")),
            Error::Server(_)
        ));
    }

    #[test]
    fn an_error_body_on_a_success_status_is_never_success() {
        assert!(matches!(classify(fault(200, "")), Error::Server(_)));
    }

    #[test]
    fn a_long_message_is_cut_on_a_character_boundary() {
        let long = "é".repeat(MAX_MESSAGE_LEN + 10);
        let f = AwsFault::new(400, "X", &long, "");
        assert_eq!(f.message.chars().count(), MAX_MESSAGE_LEN);
    }

    #[test]
    fn display_names_status_code_message_and_request_id() {
        let s = fault(400, "Unsupported").to_string();
        assert!(s.contains("HTTP 400"));
        assert!(s.contains("Unsupported"));
        assert!(s.contains("req-1"));
    }

    #[test]
    fn local_errors_carry_no_fault() {
        assert!(Error::Invalid("x".into()).fault().is_none());
        assert!(Error::Decode("x".into()).fault().is_none());
        assert!(Error::Transport("x".into()).fault().is_none());
    }
}

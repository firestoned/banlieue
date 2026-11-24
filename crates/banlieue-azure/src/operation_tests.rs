// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `operation.rs`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;

    const BASE: &str = "https://management.azure.com";
    const ASYNC: &str = "https://management.azure.com/subscriptions/s/providers/Microsoft.Compute/locations/eastus2/operations/op-1?api-version=2024-07-01";
    const LOCATION: &str = "https://management.azure.com/subscriptions/s/providers/Microsoft.Compute/locations/eastus2/operations/op-1?monitor=true&api-version=2024-07-01";

    fn headers<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn azure_async_operation_is_preferred_over_location() {
        let h = OperationHandle::from_headers(
            headers(&[
                ("Location", LOCATION),
                ("Azure-AsyncOperation", ASYNC),
                ("Retry-After", "10"),
            ]),
            BASE,
        )
        .expect("ok")
        .expect("handle");
        assert_eq!(h.url, ASYNC);
        assert_eq!(h.kind, OperationKind::AsyncOperation);
        assert_eq!(h.retry_after, Some(Duration::from_secs(10)));
    }

    #[test]
    fn location_is_used_when_it_is_the_only_header() {
        let h = OperationHandle::from_headers(headers(&[("location", LOCATION)]), BASE)
            .expect("ok")
            .expect("handle");
        assert_eq!(h.kind, OperationKind::Location);
    }

    #[test]
    fn no_header_means_the_call_completed_synchronously() {
        assert!(
            OperationHandle::from_headers(headers(&[]), BASE)
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn a_handle_on_another_host_is_refused_so_the_token_never_leaves_arm() {
        let evil = "https://attacker.example.com/operations/1";
        let err = OperationHandle::from_headers(headers(&[("Azure-AsyncOperation", evil)]), BASE)
            .unwrap_err();
        assert!(err.to_string().contains("attacker.example.com"));

        // Also a look-alike host and a downgrade to http.
        for bad in [
            "https://management.azure.com.attacker.example.com/x",
            "http://management.azure.com/x",
        ] {
            assert!(
                OperationHandle::from_headers(headers(&[("Location", bad)]), BASE).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn statuses_parse() {
        assert_eq!(
            parse_operation_status(br#"{"status":"InProgress"}"#).expect("ok"),
            OperationStatus::InProgress
        );
        assert_eq!(
            parse_operation_status(br#"{"status":"Succeeded","startTime":"x"}"#).expect("ok"),
            OperationStatus::Succeeded
        );
        assert_eq!(
            parse_operation_status(br#"{"status":"Canceled"}"#).expect("ok"),
            OperationStatus::Canceled
        );
    }

    #[test]
    fn a_failed_operation_carries_its_error() {
        let s = parse_operation_status(
            br#"{"status":"Failed","error":{"code":"OSProvisioningTimedOut","message":"OS Provisioning for VM did not finish in the allotted time."}}"#,
        )
        .expect("ok");
        assert_eq!(
            s,
            OperationStatus::Failed {
                code: "OSProvisioningTimedOut".to_string(),
                message: "OS Provisioning for VM did not finish in the allotted time.".to_string(),
            }
        );
    }

    #[test]
    fn unknown_statuses_are_still_in_progress() {
        // ARM documents only the terminal states; anything else is not done.
        assert_eq!(
            parse_operation_status(br#"{"status":"Updating"}"#).expect("ok"),
            OperationStatus::InProgress
        );
    }

    #[test]
    fn a_body_without_status_is_an_error() {
        assert!(parse_operation_status(b"{}").is_err());
        assert!(parse_operation_status(b"not json").is_err());
    }
}

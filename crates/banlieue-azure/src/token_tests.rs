// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `token.rs`.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::*;

    const TENANT: &str = "00000000-0000-0000-0000-000000000000";
    const CLIENT: &str = "11111111-1111-1111-1111-111111111111";

    #[test]
    fn the_endpoint_is_the_tenant_v2_token_url() {
        assert_eq!(
            token_endpoint(DEFAULT_AUTHORITY, TENANT).expect("ok"),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
        );
    }

    #[test]
    fn a_tenant_that_could_change_the_path_is_refused() {
        for bad in ["", "../x", "a/b", "t?x=1", "t#x", "tenant with space"] {
            assert!(token_endpoint(DEFAULT_AUTHORITY, bad).is_err(), "{bad:?}");
        }
        assert!(token_endpoint(DEFAULT_AUTHORITY, "contoso.onmicrosoft.com").is_ok());
    }

    #[test]
    fn the_form_is_a_client_credentials_grant_with_the_assertion() {
        let form = exchange_form(CLIENT, "header.payload.sig", ARM_SCOPE);
        let pairs: Vec<(String, String)> = form_urlencoded::parse(form.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("grant_type"), Some("client_credentials"));
        assert_eq!(get("client_id"), Some(CLIENT));
        assert_eq!(get("scope"), Some("https://management.azure.com/.default"));
        assert_eq!(
            get("client_assertion_type"),
            Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
        );
        assert_eq!(get("client_assertion"), Some("header.payload.sig"));
    }

    #[test]
    fn a_token_response_parses_with_its_expiry() {
        let now = Instant::now();
        let t = parse_token_response(
            br#"{"token_type":"Bearer","expires_in":86399,"access_token":"eyJ.x.y"}"#,
            now,
        )
        .expect("ok");
        assert_eq!(t.secret(), "eyJ.x.y");
        assert_eq!(t.expires_at(), now + Duration::from_secs(86_399));
    }

    #[test]
    fn the_token_is_never_printed() {
        let t = parse_token_response(
            br#"{"token_type":"Bearer","expires_in":60,"access_token":"SECRET-VALUE"}"#,
            Instant::now(),
        )
        .expect("ok");
        assert!(!format!("{t:?}").contains("SECRET-VALUE"));
    }

    #[test]
    fn refresh_is_due_inside_the_margin() {
        let now = Instant::now();
        let t = parse_token_response(
            br#"{"token_type":"Bearer","expires_in":3600,"access_token":"a"}"#,
            now,
        )
        .expect("ok");
        assert!(!t.needs_refresh(now));
        assert!(t.needs_refresh(now + Duration::from_secs(3600) - REFRESH_MARGIN));
        assert!(t.needs_refresh(now + Duration::from_secs(7200)));
    }

    #[test]
    fn a_response_without_a_token_is_an_error() {
        assert!(parse_token_response(br#"{"token_type":"Bearer"}"#, Instant::now()).is_err());
        assert!(parse_token_response(b"nope", Instant::now()).is_err());
    }

    #[test]
    fn entra_errors_keep_their_aadsts_code() {
        // Seen live: a token for another ServiceAccount.
        let e = parse_token_error(
            br#"{"error":"invalid_client","error_description":"AADSTS700213: No matching federated identity record found for presented assertion subject 'system:serviceaccount:ns:default'.","error_codes":[700213]}"#,
        );
        let crate::Error::TokenExchange { code, message } = e else {
            panic!("expected TokenExchange");
        };
        assert_eq!(code, "AADSTS700213");
        assert!(message.contains("No matching federated identity record"));
    }

    #[test]
    fn an_entra_error_without_codes_falls_back_to_the_error_field() {
        let e = parse_token_error(br#"{"error":"invalid_request","error_description":"bad"}"#);
        assert!(
            matches!(e, crate::Error::TokenExchange { ref code, .. } if code == "invalid_request")
        );
    }
}

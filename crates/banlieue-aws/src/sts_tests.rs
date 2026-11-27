// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sts.rs`, against AWS's published sample response
//! (`tests/fixtures/sts/`).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;
    use crate::sigv4::uri_encode;

    const FIXTURE: &str = include_str!("../tests/fixtures/sts/assume-role-with-web-identity.xml");
    const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/banlieue-ec2";
    const SESSION: &str = "banlieue-system.ec2";
    const TOKEN: &str = "eyJ.sa-token.sig";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY";
    const SESSION_TOKEN: &str = "AQoDYXdzEE0a8ANXXXXXXXXNO1ewxE5TijQyp+IEXAMPLE";

    fn call(duration: Duration) -> AssumeRoleWithWebIdentity<'static> {
        AssumeRoleWithWebIdentity {
            role_arn: ROLE_ARN,
            role_session_name: SESSION,
            web_identity_token: TOKEN,
            duration,
        }
    }

    #[test]
    fn the_published_sample_response_is_read() {
        let session = parse_assume_role_response(FIXTURE).expect("parse");
        assert_eq!(session.credentials.access_key_id, "ASgeIAIOSFODNN7EXAMPLE");
        assert_eq!(session.credentials.secret_access_key, SECRET);
        assert_eq!(
            session.credentials.session_token.as_deref(),
            Some(SESSION_TOKEN)
        );
        assert_eq!(
            session.expiration,
            "2014-10-24T23:00:23Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn the_session_debug_rendering_redacts_the_secret_and_token() {
        let session = parse_assume_role_response(FIXTURE).expect("parse");
        let shown = format!("{session:?}");
        assert!(!shown.contains(SECRET));
        assert!(!shown.contains(SESSION_TOKEN));
    }

    #[test]
    fn a_response_without_credentials_is_a_decode_error() {
        let body = "<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult>\
                    </AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>";
        let err = parse_assume_role_response(body).unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
        assert!(err.to_string().contains("Credentials"));
    }

    #[test]
    fn a_missing_session_token_is_named_and_the_secret_is_not_quoted() {
        let body = FIXTURE.replace(&format!("<SessionToken>{SESSION_TOKEN}</SessionToken>"), "");
        let err = parse_assume_role_response(&body).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("SessionToken"), "{text}");
        assert!(!text.contains(SECRET));
    }

    #[test]
    fn an_empty_access_key_is_refused() {
        let body = FIXTURE.replace("ASgeIAIOSFODNN7EXAMPLE", "  ");
        let err = parse_assume_role_response(&body).unwrap_err();
        assert!(err.to_string().contains("AccessKeyId"));
    }

    #[test]
    fn a_bad_expiration_is_a_decode_error() {
        let body = FIXTURE.replace("2014-10-24T23:00:23Z", "tomorrow");
        let err = parse_assume_role_response(&body).unwrap_err();
        assert!(err.to_string().contains("Expiration"), "{err}");
    }

    #[test]
    fn malformed_xml_is_a_decode_error_without_the_document() {
        let body = format!("<a><SecretAccessKey>{SECRET}</b>");
        let err = parse_assume_role_response(&body).unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
        assert!(!err.to_string().contains(SECRET));
    }

    #[test]
    fn the_form_carries_every_parameter_at_the_floor_duration() {
        let form = call(SESSION_DURATION).form().expect("form");
        assert_eq!(
            form,
            format!(
                "Action=AssumeRoleWithWebIdentity&DurationSeconds=900&RoleArn={}\
                 &RoleSessionName={SESSION}&Version=2011-06-15&WebIdentityToken={TOKEN}",
                uri_encode(ROLE_ARN, true)
            )
        );
    }

    #[test]
    fn the_session_duration_is_stss_floor() {
        assert_eq!(SESSION_DURATION, Duration::from_secs(900));
        assert_eq!(STS_API_VERSION, "2011-06-15");
    }

    #[test]
    fn a_duration_under_the_floor_is_refused() {
        let err = call(SESSION_DURATION - Duration::from_secs(1))
            .form()
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }

    #[test]
    fn a_duration_over_the_ceiling_is_refused_and_the_ceiling_is_accepted() {
        assert!(call(MAX_SESSION_DURATION).form().is_ok());
        assert!(
            call(MAX_SESSION_DURATION + Duration::from_secs(1))
                .form()
                .is_err()
        );
    }

    #[test]
    fn a_short_token_or_arn_is_refused_without_quoting_the_token() {
        let mut short = call(SESSION_DURATION);
        short.web_identity_token = "abc";
        let err = short.form().unwrap_err();
        assert!(!err.to_string().contains("abc"));
        let mut arn = call(SESSION_DURATION);
        arn.role_arn = "arn:aws:iam::1";
        assert!(arn.form().is_err());
    }

    #[test]
    fn the_request_debug_rendering_redacts_the_token() {
        let shown = format!("{:?}", call(SESSION_DURATION));
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn session_names_follow_stss_length_rule() {
        assert!(validate_role_session_name("ab").is_ok());
        assert!(validate_role_session_name("a").is_err());
        assert!(validate_role_session_name("").is_err());
        assert!(validate_role_session_name(&"a".repeat(MAX_ROLE_SESSION_NAME_LEN)).is_ok());
        assert!(validate_role_session_name(&"a".repeat(MAX_ROLE_SESSION_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn session_names_follow_stss_character_rule() {
        assert!(validate_role_session_name("Az09_+=,.@-").is_ok());
        assert!(validate_role_session_name("has space").is_err());
        assert!(validate_role_session_name("ns/name").is_err());
        assert!(validate_role_session_name("caf\u{e9}").is_err());
    }

    #[test]
    fn the_session_name_joins_the_providers_namespace_and_name() {
        assert_eq!(
            role_session_name("banlieue-system", "ec2-west").unwrap(),
            "banlieue-system.ec2-west"
        );
        assert!(role_session_name("ns", &"n".repeat(MAX_ROLE_SESSION_NAME_LEN)).is_err());
    }

    #[test]
    fn the_endpoint_is_regional() {
        assert_eq!(
            sts_endpoint("us-east-1").unwrap(),
            "https://sts.us-east-1.amazonaws.com/"
        );
        assert!(sts_endpoint("evil.example.com/x").is_err());
    }
}

// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `ovf.rs`. The fixtures follow the shape Azure writes for a
//! Linux VM (namespaced `wa:` provisioning section, an unprefixed
//! `LinuxProvisioningConfigurationSet` with a default namespace), with
//! placeholder values only.

#[cfg(test)]
mod tests {
    use super::super::*;
    use base64::engine::general_purpose::STANDARD;

    const CLOUD_CONFIG: &str =
        "#cloud-config\nstages:\n  boot:\n    - name: hello\n      commands: [\"true\"]\n";

    /// An `ovf-env.xml` as Azure writes it, with `custom` as the content of
    /// `<CustomData>` (already base64, or not, as the test needs).
    fn ovf_env(custom: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Environment xmlns="http://schemas.dmtf.org/ovf/environment/1" xmlns:oe="http://schemas.dmtf.org/ovf/environment/1" xmlns:wa="http://schemas.microsoft.com/windowsazure" xmlns:i="http://www.w3.org/2001/XMLSchema-instance">
  <wa:ProvisioningSection>
    <wa:Version>1.0</wa:Version>
    <LinuxProvisioningConfigurationSet xmlns="http://schemas.microsoft.com/windowsazure" xmlns:i="http://www.w3.org/2001/XMLSchema-instance">
      <ConfigurationSetType>LinuxProvisioningConfiguration</ConfigurationSetType>
      <HostName>az-01</HostName>
      <UserName>admin</UserName>
      <UserPassword>placeholder-password</UserPassword>
      <DisableSshPasswordAuthentication>true</DisableSshPasswordAuthentication>
      <CustomData>{custom}</CustomData>
    </LinuxProvisioningConfigurationSet>
  </wa:ProvisioningSection>
  <wa:PlatformSettingsSection>
    <PlatformSettings xmlns="http://schemas.microsoft.com/windowsazure"><KmsServerHostname>kms.example.com</KmsServerHostname><ProvisionGuestAgent>false</ProvisionGuestAgent></PlatformSettings>
  </wa:PlatformSettingsSection>
</Environment>
"#
        )
    }

    fn b64(s: &str) -> String {
        STANDARD.encode(s.as_bytes())
    }

    // ---- the happy path ----

    #[test]
    fn extracts_and_decodes_custom_data() {
        let doc = ovf_env(&b64(CLOUD_CONFIG));
        let got = cloud_config_from_ovf_env(doc.as_bytes()).expect("accepted");
        assert_eq!(got.as_deref(), Some(CLOUD_CONFIG.as_bytes()));
    }

    #[test]
    fn a_prefixed_custom_data_element_is_found() {
        let doc = format!(
            r#"<Environment xmlns:ns1="urn:x"><ns1:CustomData>{}</ns1:CustomData></Environment>"#,
            b64(CLOUD_CONFIG)
        );
        let got = cloud_config_from_ovf_env(doc.as_bytes()).expect("accepted");
        assert_eq!(got.as_deref(), Some(CLOUD_CONFIG.as_bytes()));
    }

    #[test]
    fn base64_wrapped_over_lines_is_accepted() {
        let encoded = b64(CLOUD_CONFIG);
        let (a, b) = encoded.split_at(encoded.len() / 2);
        let doc = ovf_env(&format!("\n  {a}\n  {b}\n"));
        let got = cloud_config_from_ovf_env(doc.as_bytes()).expect("accepted");
        assert_eq!(got.as_deref(), Some(CLOUD_CONFIG.as_bytes()));
    }

    // ---- no customData is not an error ----

    #[test]
    fn no_custom_data_element_is_none() {
        let doc = r#"<?xml version="1.0"?><Environment><wa:ProvisioningSection/></Environment>"#;
        assert_eq!(cloud_config_from_ovf_env(doc.as_bytes()), Ok(None));
    }

    #[test]
    fn an_empty_or_self_closing_custom_data_is_none() {
        assert_eq!(cloud_config_from_ovf_env(ovf_env("").as_bytes()), Ok(None));
        let doc = "<Environment><CustomData/></Environment>";
        assert_eq!(cloud_config_from_ovf_env(doc.as_bytes()), Ok(None));
    }

    // ---- the password never leaks into what is extracted ----

    #[test]
    fn only_custom_data_is_extracted_never_the_password() {
        let doc = ovf_env(&b64(CLOUD_CONFIG));
        let text = custom_data_text(doc.as_bytes())
            .expect("parsed")
            .expect("present");
        assert!(!text.contains("placeholder-password"));
        let err = OvfError::NotBase64.to_string();
        assert!(!err.contains("placeholder-password"));
    }

    // ---- malformed documents are refused ----

    #[test]
    fn a_doctype_is_refused() {
        let doc = r#"<?xml version="1.0"?><!DOCTYPE Environment [<!ENTITY x "y">]><Environment><CustomData>&x;</CustomData></Environment>"#;
        assert_eq!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::Refused("a DTD"))
        );
    }

    #[test]
    fn cdata_is_refused() {
        let doc = "<Environment><CustomData><![CDATA[abc]]></CustomData></Environment>";
        assert_eq!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::Refused("a CDATA section"))
        );
    }

    #[test]
    fn an_entity_reference_in_custom_data_is_refused() {
        let doc = "<Environment><CustomData>YQ==&amp;</CustomData></Environment>";
        assert!(matches!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::Refused(_))
        ));
    }

    #[test]
    fn two_custom_data_elements_are_refused() {
        let doc = format!(
            "<Environment><CustomData>{0}</CustomData><CustomData>{0}</CustomData></Environment>",
            b64(CLOUD_CONFIG)
        );
        assert_eq!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::DuplicateCustomData)
        );
    }

    #[test]
    fn an_element_inside_custom_data_is_refused() {
        let doc = "<Environment><CustomData>YQ==<b>x</b></CustomData></Environment>";
        assert!(matches!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::Malformed(_))
        ));
    }

    #[test]
    fn a_wrong_root_element_is_refused() {
        let doc = format!(
            "<Other><CustomData>{}</CustomData></Other>",
            b64(CLOUD_CONFIG)
        );
        assert!(matches!(
            custom_data_text(doc.as_bytes()),
            Err(OvfError::Malformed(_))
        ));
    }

    #[test]
    fn a_truncated_document_is_refused() {
        let doc = ovf_env(&b64(CLOUD_CONFIG));
        let cut = &doc[..doc.find("</CustomData>").expect("end tag")];
        assert!(matches!(
            custom_data_text(cut.as_bytes()),
            Err(OvfError::Malformed(_))
        ));
        assert!(matches!(
            custom_data_text(b"<Environment><CustomData"),
            Err(OvfError::Malformed(_))
        ));
    }

    #[test]
    fn an_empty_document_is_refused() {
        assert!(matches!(custom_data_text(b""), Err(OvfError::Malformed(_))));
    }

    #[test]
    fn a_document_that_is_not_utf8_is_refused() {
        assert_eq!(
            custom_data_text(b"<Environment>\xff</Environment>"),
            Err(OvfError::NotUtf8)
        );
    }

    #[test]
    fn an_oversized_document_is_refused_before_parsing() {
        let doc = vec![b' '; OVF_ENV_MAX_BYTES + 1];
        assert_eq!(custom_data_text(&doc), Err(OvfError::DocumentTooLarge));
    }

    // ---- customData content ----

    #[test]
    fn invalid_base64_is_refused() {
        let doc = ovf_env("not*base64");
        assert_eq!(
            cloud_config_from_ovf_env(doc.as_bytes()),
            Err(OvfError::NotBase64)
        );
    }

    #[test]
    fn custom_data_over_64_kib_is_refused() {
        let big = format!("{CLOUD_CONFIG}#{}\n", "x".repeat(CUSTOM_DATA_MAX_BYTES));
        let doc = ovf_env(&b64(&big));
        assert_eq!(
            cloud_config_from_ovf_env(doc.as_bytes()),
            Err(OvfError::CustomDataTooLarge)
        );
    }

    #[test]
    fn custom_data_of_exactly_64_kib_is_accepted() {
        let header = "#cloud-config\n#";
        let pad = CUSTOM_DATA_MAX_BYTES - header.len();
        let exact = format!("{header}{}", "x".repeat(pad));
        assert_eq!(exact.len(), CUSTOM_DATA_MAX_BYTES);
        let got = decode_custom_data(&b64(&exact)).expect("decoded");
        assert_eq!(got.len(), CUSTOM_DATA_MAX_BYTES);
    }

    #[test]
    fn a_shell_script_is_refused() {
        let doc = ovf_env(&b64("#!/bin/sh\necho hi\n"));
        assert!(matches!(
            cloud_config_from_ovf_env(doc.as_bytes()),
            Err(OvfError::NotCloudConfig(_))
        ));
    }

    #[test]
    fn binary_custom_data_is_refused() {
        assert!(matches!(
            validate_cloud_config(b"#cloud-config\n\0"),
            Err(OvfError::NotCloudConfig(_))
        ));
        assert!(matches!(
            validate_cloud_config(b"#cloud-config\n\xff"),
            Err(OvfError::NotCloudConfig(_))
        ));
    }

    #[test]
    fn the_header_may_carry_trailing_whitespace_and_crlf() {
        assert_eq!(
            validate_cloud_config(b"#cloud-config \r\nfoo: 1\r\n"),
            Ok(())
        );
        assert!(validate_cloud_config(b" #cloud-config\n").is_err());
        assert!(validate_cloud_config(b"#cloud-configx\n").is_err());
    }
}

// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Checks this crate against the vendored upstream API document
//! (ADR-0061 Decision 2).
//!
//! Two properties, both offline:
//!
//! 1. the vendored file is the one `spec/PIN` names, so it cannot drift from
//!    the version the pin claims;
//! 2. every field this crate sends exists in that document with a compatible
//!    type, so a field renamed or removed upstream fails here, not silently at
//!    a VMM that ignores it.

#[cfg(test)]
mod tests {
    use crate::types::{GuestPlan, PlannedDisk, PlannedNic, VmConfigRequest};
    use serde_yaml::Value as Yaml;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    const SPEC: &str = include_str!("../spec/cloud-hypervisor-v53.0.yaml");
    const PIN: &str = include_str!("../spec/PIN");

    fn pin_value(key: &str) -> String {
        PIN.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == key).then(|| v.trim().to_string())
            })
            .unwrap_or_else(|| panic!("spec/PIN has no {key}"))
    }

    #[test]
    fn the_vendored_spec_matches_its_pin() {
        let digest = Sha256::digest(SPEC.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, pin_value("sha256"), "spec file changed without PIN");
    }

    #[test]
    fn the_pin_names_the_version_the_client_gates_on() {
        let pinned = pin_value("version");
        let v = crate::wire::PINNED_VERSION;
        assert_eq!(pinned, format!("v{}.{}", v.major, v.minor));
    }

    fn schemas() -> Yaml {
        let doc: Yaml = serde_yaml::from_str(SPEC).expect("spec parses");
        doc["components"]["schemas"].clone()
    }

    /// Resolve `{"$ref": "#/components/schemas/X"}` to the schema of X.
    fn resolve<'a>(schemas: &'a Yaml, s: &'a Yaml) -> &'a Yaml {
        match s.get("$ref").and_then(Yaml::as_str) {
            Some(r) => &schemas[r.rsplit('/').next().unwrap()],
            None => s,
        }
    }

    /// Walk a JSON value against a schema, collecting every mismatch.
    fn check(
        schemas: &Yaml,
        schema: &Yaml,
        value: &serde_json::Value,
        at: &str,
        bad: &mut Vec<String>,
    ) {
        let schema = resolve(schemas, schema);
        let ty = schema.get("type").and_then(Yaml::as_str);
        match value {
            serde_json::Value::Object(map) => {
                if ty.is_some_and(|t| t != "object") {
                    bad.push(format!("{at}: sent an object, spec says {ty:?}"));
                    return;
                }
                let props = &schema["properties"];
                for (k, v) in map {
                    let p = &props[k.as_str()];
                    if p.is_null() {
                        bad.push(format!("{at}.{k}: not in the pinned spec"));
                        continue;
                    }
                    check(schemas, p, v, &format!("{at}.{k}"), bad);
                }
            }
            serde_json::Value::Array(items) => {
                if ty != Some("array") {
                    bad.push(format!("{at}: sent an array, spec says {ty:?}"));
                    return;
                }
                for (i, v) in items.iter().enumerate() {
                    check(schemas, &schema["items"], v, &format!("{at}[{i}]"), bad);
                }
            }
            serde_json::Value::Bool(_) if ty != Some("boolean") => {
                bad.push(format!("{at}: sent a boolean, spec says {ty:?}"));
            }
            serde_json::Value::Number(_) if !matches!(ty, Some("integer" | "number")) => {
                bad.push(format!("{at}: sent a number, spec says {ty:?}"));
            }
            serde_json::Value::String(s) => {
                if ty != Some("string") {
                    bad.push(format!("{at}: sent a string, spec says {ty:?}"));
                    return;
                }
                if let Some(allowed) = schema.get("enum").and_then(Yaml::as_sequence)
                    && !allowed.iter().any(|a| a.as_str() == Some(s))
                {
                    bad.push(format!("{at}: {s:?} is not one of the spec's enum values"));
                }
            }
            _ => {}
        }
    }

    /// A plan using every optional feature, so every field the crate can
    /// send is exercised.
    fn full_plan() -> GuestPlan {
        GuestPlan {
            firmware: PathBuf::from("/opt/banlieue/firmware/f/CLOUDHV.fd"),
            boot_vcpus: 2,
            max_vcpus: 4,
            memory_mib: 2048,
            hugepages: true,
            disks: vec![PlannedDisk {
                id: "os".into(),
                path: PathBuf::from("/srv/os.raw"),
                readonly: false,
            }],
            nics: vec![PlannedNic {
                id: "eth0".into(),
                tap: "bch0".into(),
                mac: "52:54:00:00:00:01".into(),
            }],
            tpm_socket: Some(PathBuf::from("/run/swtpm.sock")),
            serial_file: PathBuf::from("/srv/serial.log"),
            landlock: true,
        }
    }

    #[test]
    fn every_field_in_vm_create_exists_in_the_pinned_spec() {
        let schemas = schemas();
        let body = serde_json::to_value(VmConfigRequest::for_guest(&full_plan())).unwrap();
        let mut bad = Vec::new();
        check(&schemas, &schemas["VmConfig"], &body, "VmConfig", &mut bad);
        assert!(
            bad.is_empty(),
            "vm.create drifted from the spec:\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn the_remove_device_body_exists_in_the_pinned_spec() {
        let schemas = schemas();
        let body: serde_json::Value =
            serde_json::from_slice(&crate::wire::encode_remove_device("install")).unwrap();
        let mut bad = Vec::new();
        check(
            &schemas,
            &schemas["VmRemoveDevice"],
            &body,
            "VmRemoveDevice",
            &mut bad,
        );
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Every endpoint path the client calls is declared by the spec.
    #[test]
    fn every_endpoint_path_exists_in_the_pinned_spec() {
        let doc: Yaml = serde_yaml::from_str(SPEC).unwrap();
        for e in crate::wire::Endpoint::ALL {
            let rel = e.path().trim_start_matches(crate::wire::API_PREFIX);
            let entry = &doc["paths"][format!("/{rel}").as_str()];
            assert!(!entry.is_null(), "{rel} is not in the pinned spec");
            let verb = e.method().as_str().to_ascii_lowercase();
            assert!(
                !entry[verb.as_str()].is_null(),
                "{rel} has no {verb} in the spec"
            );
        }
    }
}

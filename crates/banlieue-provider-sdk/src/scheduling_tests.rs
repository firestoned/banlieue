// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for build-pod scheduling constraints.

#[cfg(test)]
mod tests {
    use super::super::*;

    // ---------- node selector --------------------------------------------

    #[test]
    fn a_selector_parses_into_a_label_match() {
        let m = parse_node_selector(&["banlieue.io/imagebuild=true".to_string()])
            .expect("valid selector");
        assert_eq!(
            m.get("banlieue.io/imagebuild").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn several_selectors_accumulate() {
        let m = parse_node_selector(&["a=1".to_string(), "kubernetes.io/arch=amd64".to_string()])
            .expect("valid");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn a_selector_without_a_value_is_rejected() {
        // `key=` is a legal label match on the empty value, but a bare `key`
        // is a typo — silently dropping it would schedule builds anywhere.
        let err = parse_node_selector(&["justakey".to_string()]).unwrap_err();
        assert!(err.contains("key=value"), "{err}");
        assert!(err.contains("justakey"), "{err}");
    }

    #[test]
    fn a_selector_value_may_contain_no_equals_of_its_own() {
        // Split once: a value containing `=` is not a label-safe value anyway,
        // and accepting it would mask a malformed flag.
        let err = parse_node_selector(&["a=b=c".to_string()]).unwrap_err();
        assert!(err.contains("a=b=c"), "{err}");
    }

    #[test]
    fn no_selectors_means_no_constraint() {
        assert!(parse_node_selector(&[]).expect("empty is valid").is_empty());
    }

    // ---------- tolerations ----------------------------------------------

    #[test]
    fn a_toleration_parses_key_value_and_effect() {
        let t = parse_tolerations(&["dedicated=imagebuild:NoSchedule".to_string()])
            .expect("valid toleration");
        assert_eq!(t[0].key.as_deref(), Some("dedicated"));
        assert_eq!(t[0].value.as_deref(), Some("imagebuild"));
        assert_eq!(t[0].effect.as_deref(), Some("NoSchedule"));
        assert_eq!(
            t[0].operator.as_deref(),
            Some("Equal"),
            "a key with a value is an Equal match"
        );
    }

    #[test]
    fn a_valueless_toleration_uses_exists() {
        // `dedicated:NoSchedule` tolerates the taint whatever its value —
        // which is `Exists`, not `Equal` against an empty string.
        let t = parse_tolerations(&["dedicated:NoSchedule".to_string()]).expect("valid");
        assert_eq!(t[0].key.as_deref(), Some("dedicated"));
        assert_eq!(t[0].operator.as_deref(), Some("Exists"));
        assert!(t[0].value.is_none());
    }

    #[test]
    fn every_taint_effect_is_accepted() {
        for effect in ["NoSchedule", "PreferNoSchedule", "NoExecute"] {
            let t = parse_tolerations(&[format!("k=v:{effect}")]).expect("valid");
            assert_eq!(t[0].effect.as_deref(), Some(effect));
        }
    }

    #[test]
    fn an_unknown_effect_is_rejected() {
        // Kubernetes silently ignores a toleration with a bogus effect, so the
        // pod stays unschedulable with no clue why. Fail at parse instead.
        let err = parse_tolerations(&["k=v:Nope".to_string()]).unwrap_err();
        assert!(err.contains("Nope"), "{err}");
        assert!(
            err.contains("NoSchedule"),
            "{err} should list valid effects"
        );
    }

    #[test]
    fn a_toleration_without_an_effect_is_rejected() {
        let err = parse_tolerations(&["dedicated=imagebuild".to_string()]).unwrap_err();
        assert!(err.contains("effect"), "{err}");
    }

    #[test]
    fn no_tolerations_means_none() {
        assert!(parse_tolerations(&[]).expect("empty is valid").is_empty());
    }
}

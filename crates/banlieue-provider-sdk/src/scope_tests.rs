// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::scope`] (ADR-0130, ADR-0131).

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::{reflector, watcher::Event};

    use super::super::*;

    const SELECTOR: &str = "banlieue.io/tenant=true";
    const FLAGGED: &str = "team-a";
    const CURRENT: &str = "banlieue-system";

    fn namespace(name: &str) -> Namespace {
        let mut ns = Namespace::default();
        ns.metadata.name = Some(name.to_string());
        ns
    }

    #[test]
    fn selector_wins_over_namespace_and_current() {
        let scope = resolve_scope(Some(SELECTOR), Some(FLAGGED), Some(CURRENT)).unwrap();
        assert_eq!(scope, Scope::Selector(SELECTOR.to_string()));
    }

    #[test]
    fn namespace_wins_over_current() {
        let scope = resolve_scope(None, Some(FLAGGED), Some(CURRENT)).unwrap();
        assert_eq!(scope, Scope::Namespace(FLAGGED.to_string()));
    }

    #[test]
    fn current_namespace_is_the_default() {
        let scope = resolve_scope(None, None, Some(CURRENT)).unwrap();
        assert_eq!(scope, Scope::Namespace(CURRENT.to_string()));
    }

    /// The whole point of ADR-0130: with nothing to go on, refuse to start
    /// rather than serve every namespace.
    #[test]
    fn no_scope_at_all_is_an_error_not_cluster_wide() {
        assert_eq!(resolve_scope(None, None, None), Err(ScopeError::NoScope));
    }

    #[test]
    fn blank_values_count_as_unset() {
        let scope = resolve_scope(Some("  "), Some(""), Some(CURRENT)).unwrap();
        assert_eq!(scope, Scope::Namespace(CURRENT.to_string()));
        assert_eq!(
            resolve_scope(Some(" "), Some(" "), Some("")),
            Err(ScopeError::NoScope)
        );
    }

    fn api_error(code: u16) -> kube::Error {
        kube::Error::Api(Box::new(
            kube::core::Status::failure("nope", "Reason").with_code(code),
        ))
    }

    /// The API server's own parser is the authority on selector syntax: a 400
    /// to the probe LIST means the selector is invalid.
    #[test]
    fn a_bad_request_from_the_probe_is_an_invalid_selector() {
        let err = selector_probe_error("team in (", &api_error(400));
        assert!(matches!(err, ScopeError::InvalidSelector { .. }), "{err:?}");
    }

    /// Anything else (forbidden, network) is not the selector's fault and
    /// must not be reported as such.
    #[test]
    fn other_probe_failures_are_not_blamed_on_the_selector() {
        let err = selector_probe_error("team=a", &api_error(403));
        assert!(matches!(err, ScopeError::Probe(_)), "{err:?}");
    }

    #[test]
    fn set_based_selectors_are_accepted_without_local_parsing() {
        assert!(resolve_scope(Some("team in (a,b)"), None, None).is_ok());
        assert!(resolve_scope(Some("banlieue.io/tenant"), None, None).is_ok());
    }

    #[test]
    fn only_namespace_scope_narrows_the_watch() {
        assert_eq!(Scope::Namespace(FLAGGED.into()).namespace(), Some(FLAGGED));
        assert_eq!(Scope::Selector(SELECTOR.into()).namespace(), None);
    }

    #[test]
    fn selector_is_exposed_only_in_selector_mode() {
        assert_eq!(Scope::Selector(SELECTOR.into()).selector(), Some(SELECTOR));
        assert_eq!(Scope::Namespace(FLAGGED.into()).selector(), None);
    }

    #[test]
    fn describe_names_the_rule_that_applied() {
        assert!(
            Scope::Selector(SELECTOR.into())
                .describe()
                .contains("selector")
        );
        assert!(
            Scope::Namespace(FLAGGED.into())
                .describe()
                .contains(FLAGGED)
        );
    }

    #[test]
    fn no_gate_serves_everything_the_watch_delivers() {
        assert!(NamespaceGate::none().serves("anything"));
    }

    #[test]
    fn gate_serves_only_selected_namespaces() {
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace(FLAGGED)));
        let gate = NamespaceGate::selected(reader);
        assert!(gate.serves(FLAGGED));
        assert!(!gate.serves("team-b"));
    }

    /// Un-labelling reaches the gate as a Delete (the watch is label
    /// filtered), and must take the namespace out of the served set.
    #[test]
    fn gate_stops_serving_a_namespace_that_leaves_the_selection() {
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace(FLAGGED)));
        writer.apply_watcher_event(&Event::Delete(namespace(FLAGGED)));
        assert!(!NamespaceGate::selected(reader).serves(FLAGGED));
    }

    /// An empty selected set serves nothing — never "everything".
    #[test]
    fn empty_selection_serves_nothing() {
        let (reader, _writer) = reflector::store::<Namespace>();
        assert!(!NamespaceGate::selected(reader).serves(FLAGGED));
    }

    #[test]
    fn gate_names_are_the_selected_namespaces() {
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace("team-b")));
        writer.apply_watcher_event(&Event::Apply(namespace(FLAGGED)));
        let names = NamespaceGate::selected(reader).names();
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec![FLAGGED.to_string(), "team-b".to_string()]
        );
        assert!(NamespaceGate::none().names().is_empty());
    }

    /// A selector such as `!foo` matches every namespace without that label,
    /// kube-system included. Serving one is allowed, but it must be visible.
    #[test]
    fn system_namespaces_among_the_served_are_reported() {
        let served: std::collections::BTreeSet<String> = ["team-a", "kube-system", "kube-public"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            system_namespaces(&served),
            vec!["kube-public".to_string(), "kube-system".to_string()]
        );
        assert!(system_namespaces(&std::iter::once(FLAGGED.to_string()).collect()).is_empty());
    }

    /// The default narrowed from every namespace to the controller's own, so
    /// falling back to it is worth a warning; choosing it is not.
    #[test]
    fn scope_is_implicit_only_when_no_flag_chose_it() {
        assert!(scope_is_implicit(None, None));
        assert!(scope_is_implicit(Some(" "), Some("")));
        assert!(!scope_is_implicit(Some(SELECTOR), None));
        assert!(!scope_is_implicit(None, Some(FLAGGED)));
    }

    /// A reconcile for an unserved namespace does nothing and asks to be woken
    /// only by a change: labelling the namespace in triggers a reconcile-all.
    #[test]
    fn an_unserved_namespace_is_skipped_until_something_changes() {
        let (reader, _writer) = reflector::store::<Namespace>();
        let gate = NamespaceGate::selected(reader);
        assert!(gate.skip_unserved("team-b", false).is_some());
        assert!(
            NamespaceGate::none()
                .skip_unserved("team-b", false)
                .is_none()
        );
    }

    /// Un-labelling must not strand a deletion: its finalizer still runs.
    #[test]
    fn a_deletion_is_never_skipped() {
        let (reader, _writer) = reflector::store::<Namespace>();
        let gate = NamespaceGate::selected(reader);
        assert!(gate.skip_unserved("team-b", true).is_none());
    }

    #[test]
    fn a_served_namespace_is_not_skipped() {
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace(FLAGGED)));
        let gate = NamespaceGate::selected(reader);
        assert!(gate.skip_unserved(FLAGGED, false).is_none());
    }

    /// ADR-0131: a provider has no selector. `--namespace` wins, else the
    /// namespace it runs in; never every namespace.
    #[test]
    fn a_provider_serves_its_flag_else_its_own_namespace() {
        assert_eq!(provider_namespace(Some(FLAGGED), CURRENT).unwrap(), FLAGGED);
        assert_eq!(provider_namespace(None, CURRENT).unwrap(), CURRENT);
        assert_eq!(provider_namespace(Some(" "), CURRENT).unwrap(), CURRENT);
    }

    #[test]
    fn a_provider_with_no_namespace_at_all_refuses_to_widen() {
        assert_eq!(provider_namespace(None, ""), Err(ScopeError::NoScope));
        assert_eq!(provider_namespace(Some(""), " "), Err(ScopeError::NoScope));
    }
}

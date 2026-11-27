// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::scope`] (ADR-0130, ADR-0131).

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::{reflector, watcher::Event};

    use super::super::*;
    use kube::Config;
    use kube::api::GroupVersionKind;

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

    /// A client that is never used to make a request: building one does not
    /// connect, so no cluster is needed.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    fn machine_resource() -> ApiResource {
        let gvk = GroupVersionKind::gvk("cluster.x-k8s.io", "v1beta2", "Machine");
        ApiResource::from_gvk_with_plural(&gvk, "machines")
    }

    #[tokio::test]
    async fn a_namespace_scope_gives_a_namespaced_dynamic_api() {
        let api = Scope::Namespace(FLAGGED.into()).api_with(&offline_client(), &machine_resource());
        assert_eq!(
            api.resource_url(),
            format!("/apis/cluster.x-k8s.io/v1beta2/namespaces/{FLAGGED}/machines")
        );
    }

    #[tokio::test]
    async fn a_selector_scope_gives_a_cluster_wide_dynamic_api() {
        let api = Scope::Selector(SELECTOR.into()).api_with(&offline_client(), &machine_resource());
        assert_eq!(
            api.resource_url(),
            "/apis/cluster.x-k8s.io/v1beta2/machines"
        );
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

    // ---- selector_to_string (ADR-0133 Decision 11) ------------------------

    use banlieue_api::common::{LabelSelector, LabelSelectorOperator, LabelSelectorRequirement};
    use std::collections::BTreeMap;

    fn sel_labels(pairs: &[(&str, &str)]) -> LabelSelector {
        LabelSelector {
            match_labels: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
            ..Default::default()
        }
    }

    fn req(key: &str, op: LabelSelectorOperator, values: &[&str]) -> LabelSelectorRequirement {
        LabelSelectorRequirement {
            key: key.to_string(),
            operator: op,
            values: values.iter().map(|v| (*v).to_string()).collect(),
        }
    }

    #[test]
    fn a_single_label_renders_as_key_equals_value() {
        assert_eq!(
            selector_to_string(&sel_labels(&[("banlieue.io/tenant", "true")])).as_deref(),
            Some("banlieue.io/tenant=true")
        );
    }

    /// The flag is rendered the same way every time, so an operator
    /// reconcile that changes nothing does not roll the provider.
    #[test]
    fn several_labels_render_sorted_and_comma_joined() {
        assert_eq!(
            selector_to_string(&sel_labels(&[("b", "2"), ("a", "1")])).as_deref(),
            Some("a=1,b=2")
        );
    }

    #[test]
    fn set_based_operators_render_in_label_selector_syntax() {
        let sel = LabelSelector {
            match_expressions: vec![
                req("team", LabelSelectorOperator::In, &["a", "b"]),
                req("env", LabelSelectorOperator::NotIn, &["prod"]),
                req("tenant", LabelSelectorOperator::Exists, &[]),
                req("legacy", LabelSelectorOperator::DoesNotExist, &[]),
            ],
            ..Default::default()
        };
        assert_eq!(
            selector_to_string(&sel).as_deref(),
            Some("team in (a,b),env notin (prod),tenant,!legacy")
        );
    }

    #[test]
    fn labels_and_expressions_combine() {
        let mut sel = sel_labels(&[("banlieue.io/tenant", "true")]);
        sel.match_expressions = vec![req("env", LabelSelectorOperator::NotIn, &["prod"])];
        assert_eq!(
            selector_to_string(&sel).as_deref(),
            Some("banlieue.io/tenant=true,env notin (prod)")
        );
    }

    /// `{}` is Kubernetes' "every namespace". A blank flag means unset, so
    /// rendering it would silently become "home only" on the provider while the
    /// controller read it as "all"; it serves nothing on both sides instead.
    #[test]
    fn an_empty_selector_renders_nothing() {
        assert_eq!(selector_to_string(&LabelSelector::default()), None);
    }

    // ---- ProviderScope (ADR-0133 Decision 4) ------------------------------

    #[test]
    fn a_provider_with_no_selector_serves_its_home_only() {
        let scope = ProviderScope::resolve(Some(CURRENT), "ignored", None).unwrap();
        assert_eq!(scope.home(), CURRENT);
        assert_eq!(scope.selector(), None);
    }

    #[test]
    fn the_home_namespace_defaults_to_the_current_one() {
        let scope = ProviderScope::resolve(None, CURRENT, None).unwrap();
        assert_eq!(scope.home(), CURRENT);
    }

    #[test]
    fn a_blank_selector_is_no_selector() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", Some("  ")).unwrap();
        assert_eq!(scope.selector(), None);
    }

    #[test]
    fn a_selector_is_carried() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", Some(SELECTOR)).unwrap();
        assert_eq!(scope.selector(), Some(SELECTOR));
        assert_eq!(scope.home(), CURRENT);
    }

    #[test]
    fn a_provider_with_no_home_at_all_fails_rather_than_widening() {
        assert_eq!(
            ProviderScope::resolve(None, "  ", Some(SELECTOR)),
            Err(ScopeError::NoScope)
        );
    }

    /// Without a selector the gate is the "none" gate that passes everything;
    /// the provider must NOT inherit that, or it would serve every namespace.
    #[test]
    fn without_a_selector_only_home_is_served_whatever_the_gate_says() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", None).unwrap();
        let gate = NamespaceGate::none();
        assert!(scope.serves(&gate, CURRENT));
        assert!(!scope.serves(&gate, FLAGGED));
    }

    #[test]
    fn with_a_selector_home_and_selected_namespaces_are_served() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", Some(SELECTOR)).unwrap();
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace(FLAGGED)));
        let gate = NamespaceGate::selected(reader);
        assert!(scope.serves(&gate, CURRENT), "home is always served");
        assert!(scope.serves(&gate, FLAGGED));
        assert!(!scope.serves(&gate, "team-b"));
    }

    /// An empty selected set serves home and nothing else.
    #[test]
    fn an_empty_selection_still_serves_home() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", Some(SELECTOR)).unwrap();
        let (reader, _writer) = reflector::store::<Namespace>();
        let gate = NamespaceGate::selected(reader);
        assert!(scope.serves(&gate, CURRENT));
        assert!(!scope.serves(&gate, FLAGGED));
    }

    #[test]
    fn an_unserved_namespace_is_skipped_but_a_deleting_object_never_is() {
        let scope = ProviderScope::resolve(Some(CURRENT), "x", Some(SELECTOR)).unwrap();
        let (reader, _writer) = reflector::store::<Namespace>();
        let gate = NamespaceGate::selected(reader);
        assert!(scope.skip_unserved(&gate, FLAGGED, false).is_some());
        assert!(
            scope.skip_unserved(&gate, FLAGGED, true).is_none(),
            "a finalizer must still run when a namespace is un-labelled"
        );
        assert!(scope.skip_unserved(&gate, CURRENT, false).is_none());
    }

    #[test]
    fn the_provider_scope_describes_itself_for_the_startup_log() {
        let none = ProviderScope::resolve(Some(CURRENT), "x", None).unwrap();
        assert!(none.describe().contains(CURRENT));
        let sel = ProviderScope::resolve(Some(CURRENT), "x", Some(SELECTOR)).unwrap();
        assert!(sel.describe().contains(SELECTOR));
        assert!(sel.describe().contains(CURRENT));
    }

    // ---- SHARED_PROVIDER_BACKENDS (ADR-0133 Decision 10) -------------------

    /// Every provider binary that parses `--watch-namespace-selector` and reads
    /// its `Provider` from its home namespace is in the list; anything else is
    /// not. The operator renders the flag and the controller offers a foreign
    /// `Provider` only for these, so the two cannot disagree.
    #[test]
    fn every_implemented_backend_supports_shared_serving() {
        for backend in [
            "vsphere",
            "libvirt",
            "proxmox",
            "cloud-hypervisor",
            "azure",
            "ec2",
        ] {
            assert!(supports_shared_provider(backend), "{backend}");
        }
    }

    #[test]
    fn an_unknown_backend_does_not_support_shared_serving() {
        assert!(!supports_shared_provider("firecracker"));
        assert!(!supports_shared_provider(""));
        assert!(!supports_shared_provider("VSphere"));
        assert!(!supports_shared_provider("Azure"));
        assert!(!supports_shared_provider("EC2"));
    }

    // ---- SharedServing (ADR-0133 Decisions 4 and 5), shared by every provider ----

    use banlieue_api::common::ProviderReference;

    const HOME: &str = "banlieue-system";
    const TENANT: &str = "team-a";
    const OTHER: &str = "team-b";

    fn pref(name: &str, namespace: Option<&str>) -> ProviderReference {
        ProviderReference {
            name: name.to_string(),
            namespace: namespace.map(str::to_string),
        }
    }

    fn selected(names: &[&str]) -> NamespaceGate {
        let (reader, mut writer) = reflector::store::<Namespace>();
        for n in names {
            writer.apply_watcher_event(&Event::Apply(namespace(n)));
        }
        NamespaceGate::selected(reader)
    }

    fn serving(selector: Option<&str>, served: &[&str]) -> SharedServing {
        let scope = ProviderScope::resolve(Some(HOME), "x", selector).unwrap();
        let gate = if selector.is_some() {
            selected(served)
        } else {
            NamespaceGate::none()
        };
        SharedServing::new(scope, gate)
    }

    /// The home namespace is where a provider reads its `Provider`, credentials
    /// and CA bundle, whatever namespace a machine is in: this is what keeps the
    /// credentials out of tenant namespaces.
    #[test]
    fn shared_serving_reads_credentials_from_home_whatever_the_machine_namespace() {
        let s = serving(Some(SELECTOR), &[TENANT]);
        assert_eq!(s.home_namespace(None, TENANT), HOME);
        assert_eq!(s.home_namespace(None, HOME), HOME);
    }

    /// With no scope installed (a unit test, or the legacy cluster-wide shape)
    /// the serving namespace is the home, and the machine's own only as a last
    /// resort: today's behaviour.
    #[test]
    fn without_a_scope_home_falls_back_to_the_serving_namespace_then_the_machines() {
        let none = SharedServing::default();
        assert_eq!(none.home_namespace(Some(HOME), TENANT), HOME);
        assert_eq!(none.home_namespace(None, TENANT), TENANT);
    }

    #[test]
    fn shared_serving_takes_a_tenant_machine_that_names_it() {
        let s = serving(Some(SELECTOR), &[TENANT]);
        assert!(s.serves_machine(Some("vcenter"), None, TENANT, &pref("vcenter", Some(HOME))));
    }

    #[test]
    fn shared_serving_ignores_a_tenant_machine_that_names_only_the_providers_name() {
        let s = serving(Some(SELECTOR), &[TENANT]);
        assert!(!s.serves_machine(Some("vcenter"), None, TENANT, &pref("vcenter", None)));
    }

    #[test]
    fn shared_serving_ignores_another_providers_machine() {
        let s = serving(Some(SELECTOR), &[TENANT]);
        assert!(!s.serves_machine(Some("vcenter"), None, TENANT, &pref("dr", Some(HOME))));
    }

    #[test]
    fn shared_serving_takes_a_bare_ref_in_the_home_namespace() {
        let s = serving(Some(SELECTOR), &[]);
        assert!(s.serves_machine(Some("vcenter"), None, HOME, &pref("vcenter", None)));
    }

    #[test]
    fn shared_serving_skips_an_unselected_namespace_and_serves_home_and_selected() {
        let s = serving(Some(SELECTOR), &[TENANT]);
        assert!(s.skip_unserved(TENANT, false).is_none());
        assert!(s.skip_unserved(HOME, false).is_none());
        assert!(s.skip_unserved(OTHER, false).is_some());
    }

    /// A finalizer must still run when a namespace is un-labelled, or the
    /// machine, and its guest on the backend, would be stranded.
    #[test]
    fn shared_serving_never_skips_a_deleting_object() {
        let s = serving(Some(SELECTOR), &[]);
        assert!(s.skip_unserved(OTHER, true).is_none());
    }

    #[test]
    fn without_a_selector_shared_serving_serves_home_only() {
        let s = serving(None, &[]);
        assert!(s.skip_unserved(HOME, false).is_none());
        assert!(s.skip_unserved(TENANT, false).is_some());
    }

    /// No scope installed serves everything the watch delivers, as before.
    #[test]
    fn a_default_shared_serving_skips_nothing() {
        assert!(
            SharedServing::default()
                .skip_unserved(TENANT, false)
                .is_none()
        );
    }

    // ----- start_gate -----

    /// How long a stream that must stay pending is given to prove otherwise.
    const PENDING_PROBE: std::time::Duration = std::time::Duration::from_millis(50);

    /// Single-namespace mode: the gate passes everything and no request is
    /// made (the client here could not make one).
    #[tokio::test]
    async fn without_a_selector_the_gate_serves_everything() {
        let (gate, _changes) = start_gate(&offline_client(), None, Some("--flag"))
            .await
            .expect("no selector, nothing to start");
        assert!(gate.serves(FLAGGED));
        assert!(gate.serves("kube-system"));
    }

    /// The reason the idle sender is kept: a trigger stream that ended would
    /// end the controller it feeds. It must stay pending even after the
    /// `ScopeChanges` it came from is gone.
    #[tokio::test]
    async fn without_a_selector_the_trigger_stays_pending() {
        use futures::StreamExt;

        let (_gate, changes) = start_gate(&offline_client(), None, None)
            .await
            .expect("no selector, nothing to start");
        let mut trigger = Box::pin(changes.trigger());
        drop(changes);
        let next = tokio::time::timeout(PENDING_PROBE, trigger.next()).await;
        assert!(next.is_err(), "the stream ended or fired: {next:?}");
    }

    /// A rejected selector reads as the context each binary attached by hand:
    /// the stage as the message, the probe's failure as its source.
    #[test]
    fn a_gate_error_reads_as_stage_then_cause() {
        use std::error::Error as _;

        let err = GateError::Check {
            what: "--watch-namespace-selector",
            source: selector_probe_error("team in (", &api_error(400)),
        };
        assert_eq!(err.to_string(), "validating --watch-namespace-selector");
        assert!(err.source().is_some());

        let err = GateError::Watch(ScopeError::Sync("closed".into()));
        assert_eq!(err.to_string(), "starting the namespace selector watch");
        assert!(err.source().is_some());
    }
}

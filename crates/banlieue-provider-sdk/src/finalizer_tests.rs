// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::finalizer`].

#[cfg(test)]
mod tests {
    use super::super::*;

    const F: &str = "banlieue.io/virtualmachine";

    #[test]
    fn add_returns_none_when_finalizer_already_present() {
        let current = vec![F.to_string()];
        assert_eq!(finalizer_list_with(&current, F), None);
    }

    #[test]
    fn add_returns_appended_list_when_absent() {
        let current: Vec<String> = vec!["other.example.com/x".into()];
        let next = finalizer_list_with(&current, F).expect("Some");
        assert_eq!(next, vec!["other.example.com/x".to_string(), F.to_string()]);
    }

    #[test]
    fn add_preserves_existing_finalizers() {
        let current: Vec<String> = vec!["a/1".into(), "b/2".into()];
        let next = finalizer_list_with(&current, F).expect("Some");
        assert_eq!(next.len(), 3);
        assert!(next.contains(&"a/1".to_string()));
        assert!(next.contains(&"b/2".to_string()));
        assert!(next.contains(&F.to_string()));
    }

    #[test]
    fn remove_returns_none_when_finalizer_absent() {
        let current: Vec<String> = vec!["other.example.com/x".into()];
        assert_eq!(finalizer_list_without(&current, F), None);
    }

    #[test]
    fn remove_returns_filtered_list_when_present() {
        let current: Vec<String> = vec!["a/1".into(), F.into(), "b/2".into()];
        let next = finalizer_list_without(&current, F).expect("Some");
        assert_eq!(next, vec!["a/1".to_string(), "b/2".to_string()]);
    }

    #[test]
    fn remove_handles_duplicate_entries() {
        let current: Vec<String> = vec![F.into(), F.into(), "other/x".into()];
        let next = finalizer_list_without(&current, F).expect("Some");
        assert_eq!(next, vec!["other/x".to_string()]);
    }
}

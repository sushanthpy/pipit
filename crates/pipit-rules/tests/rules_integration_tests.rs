//! Integration tests for the pipit-rules crate.

use pipit_core::capability::{Capability, CapabilitySet};
use pipit_core::proof::ImplementationTier;
use pipit_rules::rule::{RuleId, RuleKind, RuleTrustTier};
use pipit_rules::{RuleLoader, RuleRegistry};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// ── Task #1: Rule as Typed Claim ────────────────────────────────────────

#[test]
fn test_load_typed_rules() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    assert!(registry.total_count() >= 5, "Expected at least 5 rules from fixtures");
    assert!(registry.active_count() >= 3, "Expected at least 3 unconditional rules active");
}

#[test]
fn test_mandate_rule_parsed() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let mandate = registry
        .all_rules()
        .find(|r| r.name.contains("no-prod-writes"))
        .expect("no-prod-writes rule not found");

    assert_eq!(mandate.kind, RuleKind::Mandate);
    assert_eq!(mandate.tier, ImplementationTier::Validated);
    assert!(mandate.required_capabilities.has(Capability::FsWrite));
    assert!(mandate.required_capabilities.has(Capability::FsWriteExternal));
    assert!(!mandate.forbidden_paths.is_empty());
    assert!(mandate.body.contains("Never write to production"));
}

#[test]
fn test_procedure_rule_parsed() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let proc = registry
        .all_rules()
        .find(|r| r.name.contains("test-before-commit"))
        .expect("test-before-commit rule not found");

    assert_eq!(proc.kind, RuleKind::Procedure);
    assert!(!proc.required_sequence.is_empty());
    assert_eq!(proc.required_sequence, vec!["test", "commit"]);
}

#[test]
fn test_invariant_rule_parsed() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let inv = registry
        .all_rules()
        .find(|r| r.name.contains("no-circular-deps"))
        .expect("no-circular-deps rule not found");

    assert_eq!(inv.kind, RuleKind::Invariant);
    assert!(inv.kind.is_hard());
}

#[test]
fn test_preference_rule_is_conditional() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let pref = registry
        .all_rules()
        .find(|r| r.name.contains("functional"))
        .expect("functional rule not found");

    assert_eq!(pref.kind, RuleKind::Preference);
    assert!(pref.is_conditional(), "Rule with paths/languages should be conditional");
}

#[test]
fn test_plain_rule_defaults_to_preference() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let plain = registry
        .all_rules()
        .find(|r| r.name.contains("plain-rule"))
        .expect("plain-rule not found");

    assert_eq!(plain.kind, RuleKind::Preference);
    assert_eq!(plain.tier, ImplementationTier::Heuristic);
}

#[test]
fn test_rule_id_content_addressed() {
    let id1 = RuleId::compute("security/no-prod", "Never write production");
    let id2 = RuleId::compute("security/no-prod", "Never write production");
    assert_eq!(id1, id2);

    let id3 = RuleId::compute("security/no-prod", "Never write production!");
    assert_ne!(id1, id3);
}

// ── Task #2: Activation via Trie ────────────────────────────────────────

#[test]
fn test_activation_index() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let mut index = pipit_rules::RuleActivationIndex::new();
    for rule in registry.all_rules() {
        if rule.is_conditional() {
            index.add_rule(rule);
        }
    }

    // The functional rule activates on "crates/*/src/**"
    let activated = index.activate(&["crates/pipit-core/src/lib.rs"], &["rust"]);
    assert!(!activated.is_empty(), "Expected functional rule to activate for Rust crate src");
}

// ── Task #3: Capability-Scoped Rules ────────────────────────────────────

#[test]
fn test_capability_filtering() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let fs_write_cap = CapabilitySet::EMPTY.grant(Capability::FsWrite);
    let filtered = registry.rules_for_capabilities(fs_write_cap);

    // Should include no-prod-writes (has FsWrite) and universal rules (EMPTY caps).
    let has_prod_rule = filtered.iter().any(|r| r.name.contains("no-prod-writes"));
    assert!(has_prod_rule, "no-prod-writes should match FsWrite capability");

    let network_cap = CapabilitySet::EMPTY.grant(Capability::NetworkRead);
    let net_filtered = registry.rules_for_capabilities(network_cap);
    let has_prod_in_net = net_filtered.iter().any(|r| r.name.contains("no-prod-writes"));
    assert!(!has_prod_in_net, "no-prod-writes should NOT match NetworkRead");
}

// ── Task #5: Verification Steps ─────────────────────────────────────────

#[test]
fn test_verifiable_rules() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let verifiable = registry.verifiable_rules();
    assert!(!verifiable.is_empty(), "Should have at least one verifiable rule");

    for rule in &verifiable {
        assert!(
            rule.is_verifiable(),
            "Rule {} should be verifiable",
            rule.name
        );
        let step = rule.as_verification_step();
        assert!(!step.is_empty());
    }
}

// ── Task #7: Budget-Aware Rendering ─────────────────────────────────────

#[test]
fn test_budget_rendering_full() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let (text, mode, included, _truncated) =
        pipit_rules::budget::render_within_budget(&registry, 100_000);

    assert!(!text.is_empty());
    assert_eq!(mode, pipit_rules::budget::RenderMode::Full);
    assert!(included > 0);
}

#[test]
fn test_budget_rendering_compact() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    // Tiny budget forces compact or listing mode.
    let (text, mode, _included, _truncated) =
        pipit_rules::budget::render_within_budget(&registry, 200);

    assert!(!text.is_empty());
    assert!(
        mode == pipit_rules::budget::RenderMode::Compact
            || mode == pipit_rules::budget::RenderMode::ListingOnly,
        "Expected compact or listing mode for tiny budget, got {:?}",
        mode
    );
}

// ── Task #8: Causal Snapshot ────────────────────────────────────────────

#[test]
fn test_rules_snapshot() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let watermarks = vec![pipit_core::causal_snapshot::SourceWatermark::available(
        "rules",
        1,
        1000,
    )];
    let snapshot = pipit_rules::snapshot::RulesSnapshot::assemble(&registry, watermarks);

    assert!(!snapshot.active_rule_ids.is_empty());
    assert_ne!(snapshot.content_root, "empty");
    assert!(snapshot.fully_available);
}

// ── Task #9: Plan Constraint Compilation ────────────────────────────────

#[test]
fn test_plan_constraint_compilation() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let constraints = pipit_rules::compile::compile_constraints(&registry);
    assert!(!constraints.is_empty(), "Should have constraints from mandate/procedure rules");

    // Check path constraints from no-prod-writes.
    let path_constraints: Vec<_> = constraints
        .iter()
        .filter(|c| matches!(c, pipit_rules::compile::PlanConstraint::PathForbidden { .. }))
        .collect();
    assert!(!path_constraints.is_empty(), "Should have PathForbidden constraints");

    // Test violation detection.
    let violations = pipit_rules::compile::check_path_constraints(
        &constraints,
        &[(0, "production/config.yaml")],
    );
    assert!(!violations.is_empty(), "production/ path should violate no-prod-writes");
}

#[test]
fn test_sequence_constraint_checking() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let constraints = pipit_rules::compile::compile_constraints(&registry);

    // Correct order: test before commit.
    let no_violations = pipit_rules::compile::check_sequence_constraints(
        &constraints,
        &[(0, "test"), (1, "commit")],
    );
    assert!(
        no_violations.is_empty(),
        "Correct sequence should have no violations"
    );

    // Wrong order: commit before test.
    let violations = pipit_rules::compile::check_sequence_constraints(
        &constraints,
        &[(0, "commit"), (1, "test")],
    );
    assert!(
        !violations.is_empty(),
        "Reversed sequence should violate test-before-commit"
    );
}

// ── Task #6: Lineage Inheritance ────────────────────────────────────────

#[test]
fn test_rule_inheritance_narrowing() {
    use pipit_rules::inheritance::InheritedRuleSet;
    use std::collections::BTreeSet;

    let parent_ids: BTreeSet<RuleId> = vec![
        RuleId("aaa".into()),
        RuleId("bbb".into()),
        RuleId("ccc".into()),
    ]
    .into_iter()
    .collect();

    let parent = InheritedRuleSet::from_active(parent_ids);
    assert_eq!(parent.count(), 3);

    // Child only gets aaa and bbb.
    let child_permitted: BTreeSet<RuleId> =
        vec![RuleId("aaa".into()), RuleId("bbb".into())]
            .into_iter()
            .collect();

    let child = parent.narrow_for_child(&child_permitted, "subagent scope");
    assert_eq!(child.count(), 2);
    assert_eq!(child.disabled_rules.len(), 1);
    assert_eq!(child.disabled_rules[0].rule_id, RuleId("ccc".into()));
}

#[test]
fn test_rule_inheritance_broadening_detected() {
    use pipit_rules::inheritance::InheritedRuleSet;
    use std::collections::BTreeSet;

    let parent_ids: BTreeSet<RuleId> = vec![RuleId("aaa".into())].into_iter().collect();
    let parent = InheritedRuleSet::from_active(parent_ids);

    let child_wants: BTreeSet<RuleId> = vec![RuleId("aaa".into()), RuleId("zzz".into())]
        .into_iter()
        .collect();

    let broadened = parent.detect_broadening(&child_wants);
    assert_eq!(broadened.len(), 1);
    assert_eq!(broadened[0], RuleId("zzz".into()));
}

// ── Task #13: Conflict Detection ────────────────────────────────────────

#[test]
fn test_conflict_detection_and_resolution() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let active = registry.active_rules();
    let conflicts = pipit_rules::conflict::detect_conflicts(&active);

    // May or may not have conflicts depending on fixtures — just verify it runs.
    for conflict in &conflicts {
        let rule_a = registry.get(&conflict.rule_a);
        let rule_b = registry.get(&conflict.rule_b);
        if let (Some(a), Some(b)) = (rule_a, rule_b) {
            let resolution = pipit_rules::conflict::auto_resolve(conflict, a, b);
            assert!(!resolution.reasoning.is_empty());
            assert!(resolution.confidence > 0.0);
        }
    }
}

// ── Task #14: Cache Key Stability ───────────────────────────────────────

#[test]
fn test_cache_key_deterministic() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let reg1 = loader.load().unwrap();
    let reg2 = loader.load().unwrap();

    let key1 = pipit_rules::cache_key::rules_cache_key("rules", &reg1);
    let key2 = pipit_rules::cache_key::rules_cache_key("rules", &reg2);

    assert_eq!(key1, key2, "Same rule set should produce same cache key");
}

#[test]
fn test_merkle_root_deterministic() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let reg1 = loader.load().unwrap();
    let reg2 = loader.load().unwrap();

    assert_eq!(
        reg1.active_merkle_root(),
        reg2.active_merkle_root(),
        "Same active set should produce same Merkle root"
    );
}

// ── Task #10: Signing ───────────────────────────────────────────────────

#[test]
fn test_canonical_content_hash() {
    let hash1 = pipit_rules::signing::canonical_content_hash("hello\nworld\n");
    let hash2 = pipit_rules::signing::canonical_content_hash("hello  \nworld  \n\n");
    assert_eq!(hash1, hash2, "Canonicalization should normalize trailing whitespace");
}

// ── Task #15: Evolution Store ───────────────────────────────────────────

#[test]
fn test_evolution_store() {
    use pipit_rules::evolution::*;

    let mut store = RuleEvolutionStore::new();
    let id = RuleId("test-rule".into());

    store.record(RuleEvolutionEvent {
        rule_id: id.clone(),
        before_hash: None,
        after_hash: Some("abc123".into()),
        author: Some("dev@example.com".into()),
        timestamp_ms: 1000,
        reason: Some("Initial creation".into()),
        change_kind: RuleChangeKind::Created,
    });

    let history = store.history(&id).unwrap();
    assert_eq!(history.events.len(), 1);
    assert!(history.days_since_last_change(1000 + 86_400_000 * 5).unwrap() >= 5);

    let stale = store.stale_rules(1000 + 86_400_000 * 100, 90);
    assert_eq!(stale.len(), 1);
}

// ── Backward Compatibility: load_rules still works ──────────────────────

#[test]
fn test_render_prompt_section_backward_compat() {
    let loader = RuleLoader::new(vec![fixtures_dir()]);
    let registry = loader.load().unwrap();

    let rendered = RuleLoader::render_prompt_section(&registry);
    assert!(!rendered.is_empty());
    assert!(rendered.contains("Rule:"), "Should contain rule headers");
}

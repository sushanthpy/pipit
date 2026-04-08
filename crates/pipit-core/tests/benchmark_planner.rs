//! Benchmark-derived planner tests.
//!
//! These tests validate the heuristic planner produces the correct strategy
//! selections for the exact prompts used in E2E benchmarks (Tiers 1-5).
//! A regression here means benchmark pass rates will drop.

use pipit_core::planner::{Planner, StrategyKind, is_question_task};
use pipit_core::proof::{ConfidenceReport, EvidenceArtifact, Objective, VerificationKind};

fn selected_strategy(prompt: &str) -> StrategyKind {
    let planner = Planner;
    let objective = Objective::from_prompt(prompt);
    let confidence = ConfidenceReport::default();
    let evidence: Vec<EvidenceArtifact> = vec![];
    let plan = planner.select_plan_with_evidence(&objective, &confidence, &evidence);
    plan.strategy
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 1: Basic Reliability — strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t1_file_creation_gets_greenfield() {
    // Test 1: "Create a helper module" — no fix/test/refactor keywords, has "create"
    // This is a greenfield task: building something new from scratch.
    let s =
        selected_strategy("Create a Python module string_utils.py with 5 string helper functions");
    assert_eq!(
        s,
        StrategyKind::Greenfield,
        "File creation should use Greenfield"
    );
}

#[test]
fn t1_bug_fix_gets_minimal_patch() {
    // Test 2: "Fix the bugs" — has "fix" keyword
    let s = selected_strategy(
        "Fix the 3 bugs in calculator.py: divide-by-zero, power implementation, modulo check",
    );
    assert_eq!(
        s,
        StrategyKind::MinimalPatch,
        "Bug fix should use MinimalPatch (highest value with 'fix' keyword)"
    );
}

#[test]
fn t1_test_generation_gets_characterization_first() {
    // Test 5: "Write tests" — has "test" keyword → CharacterizationFirst
    let s = selected_strategy("Write comprehensive pytest tests for the calculator module");
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Test generation should use CharacterizationFirst"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 2: Engineering Competence — strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t2_hidden_test_gets_characterization_first() {
    // Test 9: Discovering hidden edge cases — has "test" keyword
    let s = selected_strategy(
        "There are hidden failing tests in the pagination module. Find and fix all edge cases. Run the test suite to verify",
    );
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Hidden test discovery should use CharacterizationFirst"
    );
}

#[test]
fn t2_backward_compat_refactor() {
    // Test 12: "Refactor" + "test" — CharacterizationFirst wins when tests are emphasized
    let s = selected_strategy(
        "Refactor the internal data layer. All 13 API contract tests must still pass",
    );
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Refactor+test emphasis should select CharacterizationFirst"
    );
}

#[test]
fn t2_pure_refactor_gets_architectural_repair() {
    // Pure refactor without test emphasis
    let s = selected_strategy(
        "Refactor the architecture of the data access layer for better separation of concerns",
    );
    assert_eq!(
        s,
        StrategyKind::ArchitecturalRepair,
        "Pure refactor should select ArchitecturalRepair"
    );
}

#[test]
fn t2_performance_fix() {
    // Test 11: "fix" + "regression" — regression triggers verification_heavy,
    // CharacterizationFirst wins because tests should be run first
    let s =
        selected_strategy("Fix the performance regression. Four functions have O(n²) complexity");
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Performance regression fix should use CharacterizationFirst"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 3: Adversarial Debugging — strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t3_schema_migration_gets_characterization() {
    // Test 17: Schema migration — has "test" in "contract tests"
    let s =
        selected_strategy("Migrate the API from V1 to V2 schema. All 12 contract tests must pass");
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Schema migration with tests should use CharacterizationFirst"
    );
}

#[test]
fn t3_minimal_diff_fix() {
    // Test 18: "fix" present
    let s = selected_strategy(
        "Fix the broken function with the minimal possible diff. Do not modify any other code",
    );
    assert_eq!(
        s,
        StrategyKind::MinimalPatch,
        "Minimal diff should use MinimalPatch"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 4: Full-System Adversarial — strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t4_test_repair_gets_characterization() {
    // Test 22: "test" keyword
    let s =
        selected_strategy("Fix the broken tests in test_inventory.py. Do not modify inventory.py");
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Test repair should use CharacterizationFirst"
    );
}

#[test]
fn t4_unicode_bug_fix() {
    // Test 23: "fix" + "bug"
    let s = selected_strategy("Fix all 10 Unicode bugs in the text processing module");
    assert_eq!(
        s,
        StrategyKind::MinimalPatch,
        "Unicode bug fix should use MinimalPatch"
    );
}

#[test]
fn t4_regression_bundle_fix() {
    // Test 30: "fix" + "regression" — regression triggers verification_heavy
    let s = selected_strategy("Fix 5 regression bugs across the cart module");
    assert_eq!(
        s,
        StrategyKind::CharacterizationFirst,
        "Regression bundle should use CharacterizationFirst"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 5: Production Chaos — strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t5_broken_ci_fix() {
    // Test 31: "fix" keyword
    let s =
        selected_strategy("Fix the broken CI pipeline. Ignore deprecation warnings and lint noise");
    assert_eq!(
        s,
        StrategyKind::MinimalPatch,
        "Broken CI fix should use MinimalPatch"
    );
}

#[test]
fn t5_retry_storm_fix() {
    // Test 37: "fix" keyword
    let s =
        selected_strategy("Fix the retry storm causing cascading failures in the payment service");
    assert_eq!(
        s,
        StrategyKind::MinimalPatch,
        "Retry storm fix should use MinimalPatch"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Plan pivot: failed verification streak triggers CharacterizationFirst
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn plan_pivot_after_failed_verification() {
    use pipit_core::proof::VerificationKind;

    let planner = Planner;
    let objective = Objective::from_prompt("Fix the bug in calculator.py");
    let confidence = ConfidenceReport::default();

    // Simulate 2 failed test evidence artifacts
    let evidence = vec![
        EvidenceArtifact::CommandResult {
            kind: VerificationKind::Test,
            command: "pytest".to_string(),
            output: "FAILED: 3 failures".to_string(),
            success: false,
        },
        EvidenceArtifact::CommandResult {
            kind: VerificationKind::Test,
            command: "pytest".to_string(),
            output: "FAILED: 2 failures".to_string(),
            success: false,
        },
    ];

    let plan = planner.select_plan_with_evidence(&objective, &confidence, &evidence);
    assert_eq!(
        plan.strategy,
        StrategyKind::CharacterizationFirst,
        "After 2 failed verifications, planner should pivot to CharacterizationFirst"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// is_question_task — benchmark prompt classification
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn benchmark_prompts_are_not_questions() {
    // All benchmark prompts contain action verbs and should NOT be classified as Q&A
    let benchmark_prompts = vec![
        "Create a Python module with 5 helper functions",
        "Fix the 3 bugs in calculator.py",
        "Add SHA256 hashing and KeyError handling",
        "Create logger.py and integrate across 3 files",
        "Write comprehensive pytest tests for the calculator module",
        "Implement a notification system from this spec",
        "Fix the pricing bug in the 18-file repo",
        "Find and fix all hidden edge cases in pagination",
        "Fix the timing deps and shared state causing test flakiness",
        "Optimize the 4 O(n²) functions to O(n)",
        "Refactor the data layer, all 13 API tests must pass",
        "Fix all 9 pandas 2.0 breaking changes",
        "Fix 10 security vulnerabilities",
        "Add proper locking with deadlock avoidance",
        "Migrate V1 to V2 schema, 12 contract tests must pass",
        "Fix the function with the minimal diff",
        "Add structured logging without logging sensitive data",
        "Add a validate subcommand, all 20 tests must pass",
        "Fix 3 Python+shell boundary bugs",
        "Fix the broken tests in test_inventory.py",
        "Fix all 10 Unicode bugs",
        "Fix get_nested, set_nested, validate_config",
        "Implement fuzzy search + volume discount behind flags",
    ];

    for prompt in benchmark_prompts {
        assert!(
            !is_question_task(prompt),
            "Benchmark prompt should NOT be classified as Q&A: {:?}",
            prompt
        );
    }
}

#[test]
fn qa_prompts_are_questions() {
    let qa_prompts = vec![
        "what files are in this project?",
        "how does the auth system work?",
        "explain the architecture",
        "current directory",
        "project overview",
        "status",
        "show me the config file",
        "describe the project structure",
    ];

    for prompt in qa_prompts {
        assert!(
            is_question_task(prompt),
            "Q&A prompt should be classified as question: {:?}",
            prompt
        );
    }
}

#[test]
fn question_form_with_task_verb_is_task() {
    // "Can you fix X?" should be a task, not a question
    let task_prompts = vec![
        "can you fix the bug in main.rs",
        "could you explain and then fix the failing test",
        "how should I implement the cache",
        "can you create a new module for auth",
        "where should I add the validation and fix the bug",
    ];

    for prompt in task_prompts {
        assert!(
            !is_question_task(prompt),
            "Task-verb in question form should be classified as TASK: {:?}",
            prompt
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SWE-bench prompt classification
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn swe_bench_style_prompt_is_task() {
    // SWE-bench issues are always action-oriented
    let swe_prompt = r#"You are fixing a bug in this repository.

## Issue Description

The separability_matrix function returns incorrect results when using nested CompoundModels.

## Instructions

1. Read the relevant code to understand the issue
2. Make the minimal necessary changes to fix it
3. Only modify source files (not tests)"#;

    assert!(
        !is_question_task(swe_prompt),
        "SWE-bench prompt must not be classified as Q&A"
    );
}

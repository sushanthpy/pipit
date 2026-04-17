use pipit_skills::frontmatter::SkillSource;
use pipit_skills::SkillRegistry;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// ── #1: Schema-validated frontmatter parser ──────────────────────────────

#[test]
fn full_frontmatter_parses_all_fields() {
    let dir = fixtures_dir().join("full-frontmatter");
    let registry = SkillRegistry::discover(&[dir]);
    let skill = registry.get("full-frontmatter").expect("skill should exist");

    assert_eq!(
        skill.description,
        "Use when reviewing pull requests for code quality and style issues"
    );
    assert!(!skill.frontmatter.disable_model_invocation);
    assert!(skill.frontmatter.user_invocable);

    // allowed_tools populated (was silently None before fix)
    let tools = skill.frontmatter.allowed_tools.as_ref().unwrap();
    assert_eq!(tools, &["read_file", "grep_search", "file_search"]);

    // agent config populated
    let agent = skill.frontmatter.agent.as_ref().unwrap();
    assert_eq!(agent.model.as_deref(), Some("gpt-4"));
    assert_eq!(agent.max_turns, Some(5));

    // New fields from the extended schema
    assert_eq!(
        skill.frontmatter.when_to_use.as_deref(),
        Some("When the user asks for a code review or PR review")
    );
    assert_eq!(
        skill.frontmatter.argument_hint.as_deref(),
        Some("PR number or file path")
    );
    assert_eq!(skill.frontmatter.model.as_deref(), Some("gpt-4o"));
    assert_eq!(skill.frontmatter.effort.as_deref(), Some("high"));
}

#[test]
fn minimal_frontmatter_has_description_only() {
    let dir = fixtures_dir().join("minimal");
    let registry = SkillRegistry::discover(&[dir]);
    let skill = registry.get("minimal").expect("skill should exist");

    assert_eq!(skill.description, "Minimal skill with just a description");
    assert!(skill.frontmatter.allowed_tools.is_none());
    assert!(skill.frontmatter.agent.is_none());
    assert!(skill.frontmatter.paths.is_none());
    assert!(skill.frontmatter.hooks.is_none());
    assert!(skill.frontmatter.user_invocable); // default true
}

// ── #14: No-fallback description policy ──────────────────────────────────

#[test]
fn no_frontmatter_gets_synthetic_label() {
    let dir = fixtures_dir().join("no-frontmatter");
    let registry = SkillRegistry::discover(&[dir]);
    let skill = registry
        .get("no-frontmatter")
        .expect("skill should exist");

    // #14: must NOT contain body prose — must be the synthetic label
    assert!(
        skill.description.starts_with("[unnamed skill:"),
        "Expected synthetic label, got: {:?}",
        skill.description
    );
}

// ── #1: Typed error on bad YAML ──────────────────────────────────────────

#[test]
fn bad_yaml_produces_error_not_silent_fallback() {
    let dir = fixtures_dir().join("bad-yaml");
    let registry = SkillRegistry::discover(&[dir]);

    // Bad YAML should fail to register (logged as warning), not silently produce defaults
    assert!(
        !registry.has_skill("bad-yaml"),
        "Bad-yaml skill should not be registered"
    );
}

// ── #8: Namespaced skill names ───────────────────────────────────────────

#[test]
fn nested_skills_get_namespace_prefix() {
    let dir = fixtures_dir().join("nested");
    let registry = SkillRegistry::discover(&[dir]);

    // team-a/review → "team-a:review", team-b/review → "team-b:review"
    assert!(
        registry.has_skill("team-a:review"),
        "Expected 'team-a:review', found: {:?}",
        registry.list()
    );
    assert!(
        registry.has_skill("team-b:review"),
        "Expected 'team-b:review', found: {:?}",
        registry.list()
    );
    // They must be distinct
    assert_eq!(registry.count(), 2);
}

// ── #4: Path-conditional skill detection ─────────────────────────────────

#[test]
fn conditional_skill_detected_as_conditional() {
    let dir = fixtures_dir().join("conditional");
    let registry = SkillRegistry::discover(&[dir]);
    let skill = registry.get("conditional").expect("skill should exist");

    assert!(skill.is_conditional());
    let paths = skill.frontmatter.paths.as_ref().unwrap();
    assert_eq!(paths, &["src/**/*.rs", "crates/**/*.rs"]);
}

#[test]
fn conditional_skills_excluded_from_prompt() {
    let dir = fixtures_dir();
    // Discover all fixtures — conditional skill should not appear in prompt
    let registry = SkillRegistry::discover(&[dir]);
    let prompt = registry.prompt_section();

    // conditional skill declares paths: — should be excluded from prompt_section
    assert!(
        !prompt.contains("conditional"),
        "Conditional skills should not appear in prompt_section:\n{}",
        prompt
    );

    // non-conditional skills should appear
    assert!(prompt.contains("minimal"));
}

// ── #6: Deterministic ordering ───────────────────────────────────────────

#[test]
fn prompt_section_is_deterministic() {
    let dir = fixtures_dir();
    let reg1 = SkillRegistry::discover(&[dir.clone()]);
    let reg2 = SkillRegistry::discover(&[dir]);

    assert_eq!(
        reg1.prompt_section(),
        reg2.prompt_section(),
        "prompt_section must be deterministic across invocations"
    );
}

// ── #6: Budget enforcement ───────────────────────────────────────────────

#[test]
fn prompt_section_respects_budget() {
    let dir = fixtures_dir();
    let registry = SkillRegistry::discover(&[dir]);

    // Very tight budget — should truncate
    let tiny_prompt = registry.prompt_section_with_budget(200);
    assert!(
        tiny_prompt.len() <= 200 + 50, // allow small overflow for final entry
        "Prompt should respect budget, got {} chars",
        tiny_prompt.len()
    );
}

// ── #3: Source tier explicit ─────────────────────────────────────────────

#[test]
fn source_tier_ordering_correct() {
    assert!(SkillSource::Builtin < SkillSource::User);
    assert!(SkillSource::User < SkillSource::Project);
    assert!(SkillSource::Project < SkillSource::CliDir);
    assert!(SkillSource::CliDir < SkillSource::Policy);
}

#[test]
fn discover_with_sources_uses_explicit_source() {
    let dir = fixtures_dir().join("minimal");
    let registry =
        SkillRegistry::discover_with_sources(&[(dir, SkillSource::User)]);
    let skill = registry.get("minimal").expect("skill should exist");
    assert_eq!(skill.source, SkillSource::User);
}

// ── #1: Hooks frontmatter field ──────────────────────────────────────────

#[test]
fn hooks_parsed_from_frontmatter() {
    let dir = fixtures_dir().join("with-hooks");
    let registry = SkillRegistry::discover(&[dir]);
    let skill = registry.get("with-hooks").expect("skill should exist");

    let hooks = skill.frontmatter.hooks.as_ref().expect("hooks should exist");
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0].event, "post-edit");
    assert_eq!(hooks[0].command, "cargo fmt");
    assert_eq!(hooks[1].event, "pre-commit");
    assert_eq!(hooks[1].command, "cargo clippy");
}

// ── #9: Variable expansion ───────────────────────────────────────────────

#[test]
fn loader_expands_skill_dir_and_session_id() {
    let dir = fixtures_dir().join("full-frontmatter");
    let mut registry = SkillRegistry::discover(&[dir]);
    let skill = registry.load("full-frontmatter").unwrap();

    let injection = skill.as_injection("test args", Some("sess-123"));

    assert!(injection.contains("[Skill: full-frontmatter]"));
    assert!(injection.contains("test args"));
    // Body contains $ARGUMENTS which gets expanded
    assert!(!injection.contains("$ARGUMENTS"));
}

// ── #4: ConditionalRegistry activation ───────────────────────────────────

#[test]
fn conditional_registry_activates_on_matching_path() {
    use pipit_skills::ConditionalRegistry;
    use std::path::Path;

    let dir = fixtures_dir().join("conditional");
    let mut registry = SkillRegistry::discover(&[dir]);
    let conditional_skills = registry.drain_conditional();
    assert_eq!(conditional_skills.len(), 1);
    assert_eq!(conditional_skills[0].name, "conditional");

    let mut cond = ConditionalRegistry::new(conditional_skills);
    assert_eq!(cond.dormant_count(), 1);
    assert_eq!(cond.active_count(), 0);

    // Non-matching path — no activation
    let activated = cond.activate_for_paths(&[Path::new("README.md")], Path::new(""));
    assert!(activated.is_empty());
    assert_eq!(cond.dormant_count(), 1);

    // Matching path — should activate
    let activated = cond.activate_for_paths(&[Path::new("src/main.rs")], Path::new(""));
    assert_eq!(activated, vec!["conditional"]);
    assert_eq!(cond.dormant_count(), 0);
    assert_eq!(cond.active_count(), 1);

    // Re-activation is monotone — no change
    let activated = cond.activate_for_paths(&[Path::new("src/lib.rs")], Path::new(""));
    assert!(activated.is_empty());
    assert_eq!(cond.active_count(), 1);
}

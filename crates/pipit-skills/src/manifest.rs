//! Skill Package Manifest — Skill.toml format and SkillPackage struct.
//!
//! Transforms loose SKILL.md files into versioned, typed packages with:
//! - Input/output contracts (typed schemas)
//! - Dependency declarations (skill → skill DAG edges)
//! - Tool access policy (allowlist, never parsed-but-ignored again)
//! - Trust tier classification (sandboxed → privileged)
//! - Test suite references for eval
//!
//! Design: A `SkillPackage` wraps the existing `SkillMetadata` (Tier 1) and
//! `LoadedSkill` (Tier 2) with a typed manifest layer. Skills without a
//! Skill.toml degrade gracefully to legacy behavior.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::frontmatter::SkillMetadata;

// ── Schema types for I/O contracts ──────────────────────────────────

/// Typed schema for skill inputs and outputs.
/// Mirrors JSON Schema subset — just enough for inter-skill wiring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SchemaType {
    String,
    Integer,
    Number,
    Boolean,
    /// Ordered list of a single element type.
    Array(Box<SchemaType>),
    /// Key-value map with typed values.
    Object(HashMap<String, SchemaType>),
    /// Unconstrained — accepts anything.
    Any,
}

/// A named, typed parameter with optional default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Human-readable description.
    pub description: String,
    /// The parameter's type.
    #[serde(rename = "type")]
    pub schema_type: SchemaType,
    /// Whether this parameter is required (default: true).
    #[serde(default = "default_true")]
    pub required: bool,
    /// Default value as JSON.
    pub default: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

// ── Trust model ─────────────────────────────────────────────────────

/// Trust tier determines what a skill is allowed to do.
/// Monotonically increasing privilege: each tier includes all below it.
///     Sandbox ⊂ Standard ⊂ Elevated ⊂ Privileged
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// Read-only tools, no network, no shell. Default for untrusted skills.
    Sandbox,
    /// File read/write within project, no shell execution.
    Standard,
    /// Shell execution, network access to configured endpoints.
    Elevated,
    /// Unrestricted — SSH, sudo, arbitrary network. Requires explicit approval.
    Privileged,
}

impl Default for TrustTier {
    fn default() -> Self {
        TrustTier::Standard
    }
}

// ── Policy constraints ──────────────────────────────────────────────

/// Declarative policy constraints for a skill package.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyConstraints {
    /// Maximum number of agent turns allowed per invocation.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Maximum cost in USD per invocation.
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    /// Maximum wall-clock time in seconds.
    #[serde(default)]
    pub max_time_secs: Option<u64>,
    /// File path globs this skill is allowed to read.
    #[serde(default)]
    pub allowed_read_paths: Vec<String>,
    /// File path globs this skill is allowed to write.
    #[serde(default)]
    pub allowed_write_paths: Vec<String>,
    /// Network endpoints this skill may contact (scheme://host:port).
    #[serde(default)]
    pub allowed_endpoints: Vec<String>,
    /// Environment variables this skill may read.
    #[serde(default)]
    pub allowed_env_vars: Vec<String>,
}

// ── Dependency declarations ─────────────────────────────────────────

/// A dependency on another skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDependency {
    /// Name of the required skill.
    pub skill: String,
    /// Semver constraint on the dependency (e.g., ">=1.0").
    #[serde(default)]
    pub version: Option<String>,
    /// Whether this dependency is optional (soft dependency).
    #[serde(default)]
    pub optional: bool,
}

// ── Test suite reference ────────────────────────────────────────────

/// Reference to a skill's test/eval suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTestSuite {
    /// Relative path to the test script (from skill directory).
    pub script: String,
    /// Expected pass rate (0.0-1.0) for regression gating.
    #[serde(default = "default_pass_threshold")]
    pub pass_threshold: f64,
    /// Timeout per eval task in seconds.
    #[serde(default = "default_eval_timeout")]
    pub timeout_secs: u64,
}

fn default_pass_threshold() -> f64 {
    0.8
}

fn default_eval_timeout() -> u64 {
    120
}

// ── The manifest itself ─────────────────────────────────────────────

/// The parsed contents of a `Skill.toml` file.
///
/// ```toml
/// [package]
/// name = "code-review"
/// version = "1.2.0"
/// description = "Automated code review with security analysis"
/// authors = ["team@example.com"]
/// trust_tier = "elevated"
///
/// [inputs]
/// diff = { type = "string", description = "Git diff to review", required = true }
/// severity = { type = "string", description = "Minimum severity", default = "medium" }
///
/// [outputs]
/// findings = { type = "array", description = "List of review findings" }
/// summary = { type = "string", description = "Executive summary" }
///
/// [tools]
/// allowed = ["read_file", "grep", "bash"]
/// denied = ["rm", "curl"]
///
/// [dependencies]
/// security-scan = { skill = "security-scan", version = ">=1.0" }
/// lint = { skill = "lint", optional = true }
///
/// [policy]
/// max_turns = 15
/// max_cost_usd = 0.50
/// allowed_read_paths = ["src/**", "tests/**"]
/// allowed_write_paths = []
///
/// [test]
/// script = "tests/eval.sh"
/// pass_threshold = 0.9
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub package: ManifestPackage,
    #[serde(default)]
    pub inputs: HashMap<String, ParamSpec>,
    #[serde(default)]
    pub outputs: HashMap<String, ParamSpec>,
    #[serde(default)]
    pub tools: ToolsSpec,
    #[serde(default)]
    pub dependencies: HashMap<String, SkillDependency>,
    #[serde(default)]
    pub policy: PolicyConstraints,
    #[serde(default)]
    pub test: Option<SkillTestSuite>,
}

/// The [package] section of Skill.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPackage {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub trust_tier: TrustTier,
}

/// The [tools] section — allowed/denied tool lists.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsSpec {
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
}

// ── SkillPackage: the composed runtime object ───────────────────────

/// A fully resolved skill package, combining the existing lightweight
/// metadata (Tier 1), loaded body (Tier 2), and typed manifest (Tier 3).
///
/// Skills without a Skill.toml degrade: manifest is synthesized from
/// frontmatter fields with conservative defaults.
#[derive(Debug, Clone)]
pub struct SkillPackage {
    /// Original Tier 1 metadata.
    pub metadata: SkillMetadata,
    /// Typed manifest (from Skill.toml or synthesized).
    pub manifest: SkillManifest,
    /// Whether manifest was explicit (Skill.toml) or synthesized.
    pub manifest_source: ManifestSource,
    /// Path to the skill directory.
    pub skill_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// Parsed from an explicit Skill.toml file.
    Explicit,
    /// Synthesized from SKILL.md frontmatter with defaults.
    Synthesized,
}

impl SkillPackage {
    /// Load a SkillPackage from a skill directory.
    /// Checks for Skill.toml first; falls back to synthesis from frontmatter.
    pub fn load(metadata: SkillMetadata) -> Result<Self, crate::SkillError> {
        let skill_dir = if metadata.path.is_dir() {
            metadata.path.clone()
        } else {
            metadata
                .path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf()
        };

        let toml_path = skill_dir.join("Skill.toml");

        let (manifest, source) = if toml_path.exists() {
            let content = std::fs::read_to_string(&toml_path)?;
            let manifest: SkillManifest = toml::from_str(&content).map_err(|e| {
                crate::SkillError::FrontmatterParse(format!(
                    "Skill.toml parse error in {}: {}",
                    toml_path.display(),
                    e
                ))
            })?;
            (manifest, ManifestSource::Explicit)
        } else {
            (Self::synthesize_manifest(&metadata), ManifestSource::Synthesized)
        };

        Ok(Self {
            metadata,
            manifest,
            manifest_source: source,
            skill_dir,
        })
    }

    /// Synthesize a manifest from frontmatter when no Skill.toml exists.
    /// Conservative defaults: Standard trust, $ARGUMENTS as sole input, no outputs.
    fn synthesize_manifest(meta: &SkillMetadata) -> SkillManifest {
        let mut tools = ToolsSpec::default();
        if let Some(ref allowed) = meta.frontmatter.allowed_tools {
            tools.allowed = allowed.clone();
        }

        let mut inputs = HashMap::new();
        inputs.insert(
            "arguments".to_string(),
            ParamSpec {
                description: "User arguments passed via $ARGUMENTS".to_string(),
                schema_type: SchemaType::String,
                required: false,
                default: None,
            },
        );

        let mut policy = PolicyConstraints::default();
        if let Some(ref agent) = meta.frontmatter.agent {
            policy.max_turns = agent.max_turns;
        }

        SkillManifest {
            package: ManifestPackage {
                name: meta.name.clone(),
                version: "0.0.0".to_string(),
                description: Some(meta.description.clone()),
                authors: Vec::new(),
                trust_tier: TrustTier::default(),
            },
            inputs,
            outputs: HashMap::new(),
            tools,
            dependencies: HashMap::new(),
            policy,
            test: None,
        }
    }

    /// Check if this package is allowed to use a specific tool.
    /// Returns true if (a) no tool restrictions exist, or (b) tool is in allowed and not denied.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        let spec = &self.manifest.tools;
        // Explicit deny always wins
        if spec.denied.iter().any(|d| d == tool_name) {
            return false;
        }
        // If allowed list is empty, all tools permitted (not denied)
        if spec.allowed.is_empty() {
            return true;
        }
        // Otherwise must be in allowed list
        spec.allowed.iter().any(|a| a == tool_name)
    }

    /// Check if invoking a specific skill dependency is satisfied.
    pub fn has_dependency(&self, skill_name: &str) -> bool {
        self.manifest.dependencies.contains_key(skill_name)
    }

    /// Get all required (non-optional) dependency names.
    pub fn required_dependencies(&self) -> Vec<&str> {
        self.manifest
            .dependencies
            .iter()
            .filter(|(_, dep)| !dep.optional)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Validate inputs against the input schema.
    /// Returns list of validation errors (empty = valid).
    pub fn validate_inputs(
        &self,
        inputs: &HashMap<String, serde_json::Value>,
    ) -> Vec<String> {
        let mut errors = Vec::new();

        for (name, spec) in &self.manifest.inputs {
            if spec.required && !inputs.contains_key(name) && spec.default.is_none() {
                errors.push(format!("Missing required input: {}", name));
            }
            if let Some(value) = inputs.get(name) {
                if !type_matches(&spec.schema_type, value) {
                    errors.push(format!(
                        "Input '{}' type mismatch: expected {:?}, got {}",
                        name,
                        spec.schema_type,
                        value_type_name(value)
                    ));
                }
            }
        }

        errors
    }
}

/// Check if a JSON value matches the expected schema type.
fn type_matches(schema: &SchemaType, value: &serde_json::Value) -> bool {
    match (schema, value) {
        (SchemaType::Any, _) => true,
        (SchemaType::String, serde_json::Value::String(_)) => true,
        (SchemaType::Integer, serde_json::Value::Number(n)) => {
            n.as_i64().is_some() || n.as_u64().is_some()
        }
        (SchemaType::Number, serde_json::Value::Number(_)) => true,
        (SchemaType::Boolean, serde_json::Value::Bool(_)) => true,
        (SchemaType::Array(_), serde_json::Value::Array(_)) => true,
        (SchemaType::Object(_), serde_json::Value::Object(_)) => true,
        _ => false,
    }
}

fn value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::{SkillFrontmatter, SkillSource};

    fn make_metadata(name: &str) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            description: "test skill".to_string(),
            path: PathBuf::from("/tmp/test-skill"),
            source: SkillSource::Project,
            frontmatter: SkillFrontmatter {
                allowed_tools: Some(vec!["read_file".to_string(), "bash".to_string()]),
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_synthesized_manifest_uses_frontmatter_tools() {
        let meta = make_metadata("test");
        let manifest = SkillPackage::synthesize_manifest(&meta);
        assert_eq!(manifest.tools.allowed, vec!["read_file", "bash"]);
        assert_eq!(manifest.package.trust_tier, TrustTier::Standard);
    }

    #[test]
    fn test_tool_access_with_deny() {
        let meta = make_metadata("test");
        let mut pkg = SkillPackage {
            metadata: meta,
            manifest: SkillPackage::synthesize_manifest(&make_metadata("test")),
            manifest_source: ManifestSource::Synthesized,
            skill_dir: PathBuf::from("/tmp"),
        };
        pkg.manifest.tools.denied = vec!["rm".to_string()];

        assert!(pkg.is_tool_allowed("read_file"));
        assert!(pkg.is_tool_allowed("bash"));
        assert!(!pkg.is_tool_allowed("rm"));
        assert!(!pkg.is_tool_allowed("curl")); // not in allowed list
    }

    #[test]
    fn test_input_validation() {
        let meta = make_metadata("test");
        let mut pkg = SkillPackage {
            metadata: meta,
            manifest: SkillPackage::synthesize_manifest(&make_metadata("test")),
            manifest_source: ManifestSource::Synthesized,
            skill_dir: PathBuf::from("/tmp"),
        };
        pkg.manifest.inputs.insert(
            "count".to_string(),
            ParamSpec {
                description: "number of items".to_string(),
                schema_type: SchemaType::Integer,
                required: true,
                default: None,
            },
        );

        // Missing required input
        let errors = pkg.validate_inputs(&HashMap::new());
        assert!(!errors.is_empty());

        // Wrong type
        let mut inputs = HashMap::new();
        inputs.insert("count".to_string(), serde_json::Value::String("abc".into()));
        let errors = pkg.validate_inputs(&inputs);
        assert!(!errors.is_empty());

        // Correct
        let mut inputs = HashMap::new();
        inputs.insert("count".to_string(), serde_json::json!(42));
        let errors = pkg.validate_inputs(&inputs);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_trust_tier_ordering() {
        assert!(TrustTier::Sandbox < TrustTier::Standard);
        assert!(TrustTier::Standard < TrustTier::Elevated);
        assert!(TrustTier::Elevated < TrustTier::Privileged);
    }
}

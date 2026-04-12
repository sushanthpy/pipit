// ─────────────────────────────────────────────────────────────────────────────
//  domain_architect.rs — Domain Architecture Synthesizer
// ─────────────────────────────────────────────────────────────────────────────
//
//  Converts raw user requirements into an intermediate Architecture IR:
//
//    R  →  ArchitectureIR { entities, relations, invariants, workflows, interfaces }
//
//  This IR is injected into both the Planner and Executor system prompts,
//  giving the LLM explicit domain-level structure to work with instead of
//  forcing it to infer architecture from raw prose.
//
//  Complexity: O(n) over requirement tokens for extraction, O(v + e) for
//  entity/relation graph construction.
//
// ─────────────────────────────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};

/// An extracted entity from the requirements (table, model, resource, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub name: String,
    pub attributes: Vec<String>,
    pub is_primary: bool,
}

/// A relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub from: String,
    pub to: String,
    pub kind: RelationKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelationKind {
    OneToOne,
    OneToMany,
    ManyToMany,
    BelongsTo,
}

impl std::fmt::Display for RelationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelationKind::OneToOne => write!(f, "1:1"),
            RelationKind::OneToMany => write!(f, "1:N"),
            RelationKind::ManyToMany => write!(f, "N:M"),
            RelationKind::BelongsTo => write!(f, "belongs_to"),
        }
    }
}

/// A business rule or domain invariant extracted from requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainInvariant {
    pub description: String,
    pub entities_involved: Vec<String>,
    pub kind: InvariantKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvariantKind {
    /// Data validation constraint (e.g., "URL must be valid format").
    Validation,
    /// Business rule (e.g., "refund cannot exceed original fare").
    BusinessRule,
    /// Referential integrity (e.g., "booking must reference a valid flight").
    ReferentialIntegrity,
    /// State machine constraint (e.g., "booking status: pending → confirmed → cancelled").
    StateMachine,
    /// Uniqueness constraint (e.g., "PNR must be unique").
    Uniqueness,
}

/// A workflow or use case extracted from requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<String>,
    pub entities_involved: Vec<String>,
}

/// An API endpoint or interface extracted from requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    pub method: String,
    pub path: String,
    pub description: String,
    pub entities_involved: Vec<String>,
}

/// The Architecture IR — an intermediate representation of the domain model
/// extracted from user requirements before planning begins.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchitectureIR {
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    pub invariants: Vec<DomainInvariant>,
    pub workflows: Vec<Workflow>,
    pub interfaces: Vec<Interface>,
    /// Non-functional requirements (performance, security, etc.)
    pub nfrs: Vec<String>,
    /// Detected project archetype.
    pub archetype: Option<ProjectArchetype>,
}

/// High-level project archetype for selecting constraint templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProjectArchetype {
    CrudWebApp,
    RestApi,
    CliTool,
    Library,
    EventDriven,
    DataPipeline,
    FullStackWeb,
}

impl std::fmt::Display for ProjectArchetype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectArchetype::CrudWebApp => write!(f, "CRUD Web Application"),
            ProjectArchetype::RestApi => write!(f, "REST API"),
            ProjectArchetype::CliTool => write!(f, "CLI Tool"),
            ProjectArchetype::Library => write!(f, "Library/SDK"),
            ProjectArchetype::EventDriven => write!(f, "Event-Driven System"),
            ProjectArchetype::DataPipeline => write!(f, "Data Pipeline"),
            ProjectArchetype::FullStackWeb => write!(f, "Full-Stack Web Application"),
        }
    }
}

impl ArchitectureIR {
    /// Check if the IR contains any meaningful domain structure.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
            && self.relations.is_empty()
            && self.invariants.is_empty()
            && self.workflows.is_empty()
            && self.interfaces.is_empty()
    }

    /// Render the architecture IR as a prompt section for injection into
    /// the planner or executor system prompt.
    pub fn render_for_prompt(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(2048);
        out.push_str("\n## Domain Architecture Analysis\n\n");

        if let Some(ref archetype) = self.archetype {
            out.push_str(&format!("**Project Type**: {}\n\n", archetype));
        }

        // Entities
        if !self.entities.is_empty() {
            out.push_str("### Entities (data model)\n");
            for entity in &self.entities {
                let marker = if entity.is_primary { " ★" } else { "" };
                out.push_str(&format!("- **{}**{}", entity.name, marker));
                if !entity.attributes.is_empty() {
                    out.push_str(&format!(": {}", entity.attributes.join(", ")));
                }
                out.push('\n');
            }
            out.push('\n');
        }

        // Relations
        if !self.relations.is_empty() {
            out.push_str("### Relationships\n");
            for rel in &self.relations {
                out.push_str(&format!(
                    "- {} → {} ({}) — {}\n",
                    rel.from, rel.to, rel.kind, rel.description
                ));
            }
            out.push('\n');
        }

        // Invariants
        if !self.invariants.is_empty() {
            out.push_str("### Domain Invariants\n");
            for inv in &self.invariants {
                out.push_str(&format!("- {}\n", inv.description));
            }
            out.push('\n');
        }

        // Workflows
        if !self.workflows.is_empty() {
            out.push_str("### Workflows\n");
            for wf in &self.workflows {
                out.push_str(&format!("- **{}**: {}\n", wf.name, wf.steps.join(" → ")));
            }
            out.push('\n');
        }

        // Interfaces
        if !self.interfaces.is_empty() {
            out.push_str("### API Endpoints\n");
            for iface in &self.interfaces {
                out.push_str(&format!(
                    "- `{} {}` — {}\n",
                    iface.method, iface.path, iface.description
                ));
            }
            out.push('\n');
        }

        // NFRs
        if !self.nfrs.is_empty() {
            out.push_str("### Non-Functional Requirements\n");
            for nfr in &self.nfrs {
                out.push_str(&format!("- {}\n", nfr));
            }
            out.push('\n');
        }

        // Design guidance based on archetype
        if let Some(ref archetype) = self.archetype {
            out.push_str("### Design Guidance\n");
            out.push_str(&archetype_guidance(archetype));
            out.push('\n');
        }

        out
    }

    /// Compute requirement coverage: how many entities are covered by interfaces.
    pub fn entity_coverage(&self) -> f32 {
        if self.entities.is_empty() {
            return 1.0;
        }
        let covered: std::collections::HashSet<&str> = self
            .interfaces
            .iter()
            .flat_map(|i| i.entities_involved.iter().map(|s| s.as_str()))
            .collect();
        let total = self.entities.len() as f32;
        let matched = self
            .entities
            .iter()
            .filter(|e| covered.contains(e.name.as_str()))
            .count() as f32;
        matched / total
    }
}

/// Detect the project archetype from the user prompt.
pub fn detect_archetype(prompt: &str) -> Option<ProjectArchetype> {
    let lower = prompt.to_ascii_lowercase();

    // Score each archetype by keyword presence
    let web_score = count_matches(
        &lower,
        &[
            "web app", "website", "frontend", "backend", "html", "css", "template", "render",
            "page", "dashboard", "admin panel",
        ],
    );
    let api_score = count_matches(
        &lower,
        &[
            "api", "rest", "endpoint", "route", "crud", "json", "get ", "post ", "put ", "delete ",
            "status code", "http",
        ],
    );
    let fullstack_score = count_matches(
        &lower,
        &[
            "full-stack",
            "fullstack",
            "database",
            "schema",
            "table",
            "migration",
            "model",
            "orm",
            "sqlite",
            "postgres",
            "mysql",
            "booking",
            "user",
            "authentication",
        ],
    );
    let cli_score = count_matches(
        &lower,
        &[
            "cli", "command line", "terminal", "argument", "flag", "subcommand", "stdin", "stdout",
        ],
    );
    let lib_score = count_matches(
        &lower,
        &[
            "library", "sdk", "crate", "package", "module", "trait", "interface", "abstract",
        ],
    );
    let event_score = count_matches(
        &lower,
        &[
            "event",
            "queue",
            "publish",
            "subscribe",
            "message",
            "async",
            "stream",
            "webhook",
        ],
    );
    let pipeline_score = count_matches(
        &lower,
        &[
            "pipeline",
            "etl",
            "transform",
            "ingest",
            "batch",
            "data",
            "csv",
            "parquet",
        ],
    );

    let scores = [
        (fullstack_score + api_score, ProjectArchetype::FullStackWeb),
        (api_score * 2, ProjectArchetype::RestApi),
        (web_score * 2, ProjectArchetype::CrudWebApp),
        (cli_score * 2, ProjectArchetype::CliTool),
        (lib_score * 2, ProjectArchetype::Library),
        (event_score * 2, ProjectArchetype::EventDriven),
        (pipeline_score * 2, ProjectArchetype::DataPipeline),
    ];

    let (best_score, best_archetype) = scores
        .into_iter()
        .max_by_key(|(s, _)| *s)
        .unwrap();

    if best_score >= 3 {
        Some(best_archetype)
    } else {
        None
    }
}

fn count_matches(text: &str, keywords: &[&str]) -> usize {
    keywords.iter().filter(|kw| text.contains(**kw)).count()
}

/// Extract entities from requirement text using keyword/pattern heuristics.
/// This is the heuristic path — the LLM path is handled by the planner prompt.
pub fn extract_entities_heuristic(prompt: &str) -> Vec<Entity> {
    let lower = prompt.to_ascii_lowercase();
    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Common entity patterns: "X table", "X model", "X entity", "X resource"
    let entity_markers = [
        "table",
        "model",
        "entity",
        "resource",
        "collection",
        "schema",
    ];

    for line in prompt.lines() {
        let line_lower = line.to_ascii_lowercase();
        for marker in &entity_markers {
            // Pattern: "'bookmarks' table" or "bookmarks table"
            if let Some(pos) = line_lower.find(marker) {
                let before = line_lower[..pos].trim();
                // Take the last word before the marker
                if let Some(name) = before.split_whitespace().last() {
                    let clean = name
                        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                        .to_string();
                    if clean.len() >= 2 && seen.insert(clean.clone()) {
                        entities.push(Entity {
                            name: clean,
                            attributes: Vec::new(),
                            is_primary: false,
                        });
                    }
                }
            }
        }

        // Pattern: "Endpoints: GET /resources" → extract resource name
        if line_lower.contains("get ") || line_lower.contains("post ") ||
           line_lower.contains("put ") || line_lower.contains("delete ")
        {
            for segment in line.split('/') {
                let trimmed = segment
                    .split(|c: char| c == '>' || c == '<' || c == '?' || c == ' ' || c == ')')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if trimmed.len() >= 3
                    && !trimmed.contains(':')
                    && !trimmed.starts_with("id")
                    && trimmed.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    && seen.insert(trimmed.clone())
                {
                    entities.push(Entity {
                        name: trimmed,
                        attributes: Vec::new(),
                        is_primary: false,
                    });
                }
            }
        }
    }

    // Extract attributes from parenthetical lists: "(id, url, title, tags)"
    for entity in &mut entities {
        let name_lower = entity.name.to_ascii_lowercase();
        for line in prompt.lines() {
            let line_lower = line.to_ascii_lowercase();
            if line_lower.contains(&name_lower) {
                // Look for parenthetical attribute list
                if let Some(start) = line.find('(') {
                    if let Some(end) = line[start..].find(')') {
                        let attrs_str = &line[start + 1..start + end];
                        entity.attributes = attrs_str
                            .split(',')
                            .map(|a| a.trim().to_string())
                            .filter(|a| !a.is_empty() && a.len() < 40)
                            .collect();
                    }
                }
            }
        }
    }

    // Mark primary entity (most referenced)
    if !entities.is_empty() {
        let mut max_refs = 0;
        let mut primary_idx = 0;
        for (i, entity) in entities.iter().enumerate() {
            let refs = lower.matches(&entity.name).count();
            if refs > max_refs {
                max_refs = refs;
                primary_idx = i;
            }
        }
        entities[primary_idx].is_primary = true;
    }

    entities
}

/// Extract API interfaces from requirement text.
pub fn extract_interfaces_heuristic(prompt: &str) -> Vec<Interface> {
    let mut interfaces = Vec::new();
    let methods = ["GET", "POST", "PUT", "DELETE", "PATCH"];

    for line in prompt.lines() {
        let trimmed = line.trim();
        for method in &methods {
            if let Some(pos) = trimmed.find(method) {
                let rest = trimmed[pos + method.len()..].trim();
                // Extract the path: /something/...
                if let Some(path_start) = rest.find('/') {
                    let path = rest[path_start..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(|c: char| c == ',' || c == ')')
                        .to_string();
                    if !path.is_empty() {
                        // Extract entity from path
                        let entities: Vec<String> = path
                            .split('/')
                            .filter(|s| {
                                s.len() >= 3
                                    && !s.starts_with('<')
                                    && !s.starts_with(':')
                                    && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                            })
                            .map(|s| s.to_string())
                            .collect();

                        // Description: everything after the path on the same line
                        let desc_start = pos + method.len() + path.len() + 1;
                        let description = if desc_start < trimmed.len() {
                            trimmed[desc_start..].trim().trim_start_matches(|c: char| {
                                c == '(' || c == '-' || c == '—' || c == ':' || c == ' '
                            }).to_string()
                        } else {
                            String::new()
                        };

                        interfaces.push(Interface {
                            method: method.to_string(),
                            path,
                            description,
                            entities_involved: entities,
                        });
                    }
                }
            }
        }
    }

    interfaces
}

/// Extract domain invariants from requirement text.
pub fn extract_invariants_heuristic(prompt: &str) -> Vec<DomainInvariant> {
    let mut invariants = Vec::new();
    let lower = prompt.to_ascii_lowercase();

    // Validation patterns
    let validation_keywords = [
        "valid", "required", "must be", "must have", "cannot be empty",
        "not null", "format", "validate",
    ];
    for line in prompt.lines() {
        let line_lower = line.to_ascii_lowercase();
        for kw in &validation_keywords {
            if line_lower.contains(kw) && line.trim().len() > 10 {
                invariants.push(DomainInvariant {
                    description: line.trim().to_string(),
                    entities_involved: Vec::new(),
                    kind: InvariantKind::Validation,
                });
                break;
            }
        }
    }

    // Uniqueness patterns
    if lower.contains("unique") || lower.contains("pnr") || lower.contains("reference") {
        for line in prompt.lines() {
            let line_lower = line.to_ascii_lowercase();
            if line_lower.contains("unique") || line_lower.contains("pnr") {
                invariants.push(DomainInvariant {
                    description: line.trim().to_string(),
                    entities_involved: Vec::new(),
                    kind: InvariantKind::Uniqueness,
                });
            }
        }
    }

    // State machine patterns
    let state_keywords = [
        "status", "state", "pending", "confirmed", "cancelled", "completed",
        "approved", "rejected", "active", "inactive",
    ];
    let state_hit_count = state_keywords
        .iter()
        .filter(|kw| lower.contains(**kw))
        .count();
    if state_hit_count >= 2 {
        invariants.push(DomainInvariant {
            description: "State machine transitions must be validated (e.g., status can only move forward through valid states).".to_string(),
            entities_involved: Vec::new(),
            kind: InvariantKind::StateMachine,
        });
    }

    // Referential integrity: FK mentions
    if lower.contains("foreign key")
        || lower.contains("references")
        || lower.contains("belongs to")
        || lower.contains("associated with")
    {
        invariants.push(DomainInvariant {
            description: "Foreign key constraints must be enforced — referenced entities must exist."
                .to_string(),
            entities_involved: Vec::new(),
            kind: InvariantKind::ReferentialIntegrity,
        });
    }

    invariants
}

/// Synthesize a full ArchitectureIR from a user prompt.
/// This is the main entry point — called before planning.
pub fn synthesize(prompt: &str) -> ArchitectureIR {
    let archetype = detect_archetype(prompt);
    let entities = extract_entities_heuristic(prompt);
    let interfaces = extract_interfaces_heuristic(prompt);
    let invariants = extract_invariants_heuristic(prompt);

    // Infer relations from entities and interfaces
    let relations = infer_relations(&entities, &interfaces);

    // Infer workflows from interfaces
    let workflows = infer_workflows(&entities, &interfaces);

    ArchitectureIR {
        entities,
        relations,
        invariants,
        workflows,
        interfaces,
        nfrs: Vec::new(),
        archetype,
    }
}

/// Infer entity relationships from the entity list and interfaces.
fn infer_relations(entities: &[Entity], interfaces: &[Interface]) -> Vec<Relation> {
    let mut relations = Vec::new();

    // If an interface path contains two entity names, they're related
    for iface in interfaces {
        let involved: Vec<&Entity> = entities
            .iter()
            .filter(|e| {
                iface
                    .path
                    .to_ascii_lowercase()
                    .contains(&e.name.to_ascii_lowercase())
            })
            .collect();
        if involved.len() >= 2 {
            // Nested resource pattern: /entity1/<id>/entity2 → 1:N
            relations.push(Relation {
                from: involved[0].name.clone(),
                to: involved[1].name.clone(),
                kind: RelationKind::OneToMany,
                description: format!(
                    "{} has many {} (inferred from {})",
                    involved[0].name, involved[1].name, iface.path
                ),
            });
        }
    }

    // Check for singular/plural naming patterns suggesting relationships
    for i in 0..entities.len() {
        for j in (i + 1)..entities.len() {
            let a = &entities[i].name;
            let b = &entities[j].name;
            // If entity A's attributes mention entity B's name, they're related
            for attr in &entities[i].attributes {
                let attr_lower = attr.to_ascii_lowercase();
                if attr_lower.contains(&b.to_ascii_lowercase())
                    || attr_lower.contains(&format!("{}_id", b.to_ascii_lowercase()))
                {
                    relations.push(Relation {
                        from: a.clone(),
                        to: b.clone(),
                        kind: RelationKind::BelongsTo,
                        description: format!(
                            "{} references {} (via attribute '{}')",
                            a, b, attr
                        ),
                    });
                }
            }
        }
    }

    relations
}

/// Infer workflows from the available interfaces.
fn infer_workflows(_entities: &[Entity], interfaces: &[Interface]) -> Vec<Workflow> {
    let mut workflows = Vec::new();

    // Group interfaces by primary entity
    let mut entity_ops: std::collections::HashMap<String, Vec<&Interface>> =
        std::collections::HashMap::new();
    for iface in interfaces {
        for e in &iface.entities_involved {
            entity_ops.entry(e.clone()).or_default().push(iface);
        }
    }

    // For each entity with CRUD operations, create a lifecycle workflow
    for (entity, ops) in &entity_ops {
        let methods: Vec<&str> = ops.iter().map(|o| o.method.as_str()).collect();
        if methods.contains(&"POST") && (methods.contains(&"GET") || methods.contains(&"PUT")) {
            let mut steps = Vec::new();
            if methods.contains(&"POST") {
                steps.push(format!("Create {}", entity));
            }
            if methods.contains(&"GET") {
                steps.push(format!("Read/List {}", entity));
            }
            if methods.contains(&"PUT") || methods.contains(&"PATCH") {
                steps.push(format!("Update {}", entity));
            }
            if methods.contains(&"DELETE") {
                steps.push(format!("Delete {}", entity));
            }
            workflows.push(Workflow {
                name: format!("{} lifecycle", entity),
                steps,
                entities_involved: vec![entity.clone()],
            });
        }
    }

    workflows
}

/// Design guidance text for a detected archetype.
pub fn archetype_guidance(archetype: &ProjectArchetype) -> String {
    match archetype {
        ProjectArchetype::CrudWebApp | ProjectArchetype::FullStackWeb => {
            r#"- **Normalize the database schema** to at least 3NF. Each entity should have its own table.
- **Use foreign keys** to enforce referential integrity between related entities.
- **Generate unique identifiers** (UUIDs or auto-increment IDs) for every entity.
- **Implement input validation** at the API boundary — validate types, formats, required fields.
- **Return proper HTTP status codes**: 201 for creation, 404 for not found, 400 for bad input, 409 for conflicts.
- **Add pagination** for list endpoints when the dataset could grow large.
- **Include created_at/updated_at timestamps** on all mutable entities.
- **Write tests** that cover CRUD operations, validation edge cases, and relationship integrity."#.to_string()
        }
        ProjectArchetype::RestApi => {
            r#"- **Design resource-oriented endpoints** following REST conventions (plural nouns, HTTP verbs).
- **Normalize the data model** — each resource maps to a distinct entity.
- **Return proper HTTP status codes** and consistent error response format.
- **Implement input validation** at API boundaries.
- **Add pagination, filtering, and sorting** for collection endpoints.
- **Include proper error handling** with descriptive messages."#.to_string()
        }
        ProjectArchetype::CliTool => {
            r#"- **Use a proper argument parser** (clap, argparse, commander, etc.)
- **Handle errors gracefully** with descriptive stderr messages and proper exit codes.
- **Support stdin/stdout piping** for composability.
- **Add --help documentation** for all subcommands and flags."#.to_string()
        }
        ProjectArchetype::Library => {
            r#"- **Define clear public API boundaries** — minimize the exported surface.
- **Use strong typing** for domain concepts — avoid stringly-typed APIs.
- **Write documentation** for all public types and functions.
- **Include unit tests** for core logic and integration tests for public API."#.to_string()
        }
        ProjectArchetype::EventDriven => {
            r#"- **Define event schemas** explicitly — events are the contract.
- **Use idempotent handlers** — events may be delivered more than once.
- **Implement dead letter queues** for failed event processing.
- **Log event flow** for observability and debugging."#.to_string()
        }
        ProjectArchetype::DataPipeline => {
            r#"- **Validate input data** at ingestion boundaries.
- **Make transformations idempotent** — re-running should produce the same result.
- **Handle partial failures** gracefully — checkpoint progress.
- **Log row counts and timing** at each pipeline stage."#.to_string()
        }
    }
}

/// Generate the LLM prompt section for domain architecture synthesis.
/// This is injected into the planner system prompt to guide the LLM
/// in producing architecture-aware plans.
pub fn architecture_synthesis_prompt() -> &'static str {
    r#"
## Architecture Analysis Instructions

Before generating the plan, analyze the user's requirements for domain structure:

1. **Identify all entities** (nouns that need persistence: users, bookings, flights, etc.)
2. **Map relationships** between entities (one-to-many, many-to-many, belongs-to)
3. **Extract business rules** (validation constraints, state machines, calculations)
4. **Identify API endpoints** or user-facing interfaces implied by the requirements
5. **Note non-functional requirements** (performance, security, scalability)

Include your analysis in the plan's `invariants` field as concrete, testable assertions:
- "Entity X must have a dedicated table with columns: ..."
- "X references Y via foreign key"
- "X.status must follow state machine: A → B → C"
- "Input validation: X.field must be valid URL format"

The `files_to_modify` should reflect normalized entity-per-table design, not flat schemas."#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_fullstack_archetype() {
        let prompt = "Build a Flask REST API for a bookmark manager with SQLite database, \
                       bookmarks table (id, url, title, tags), endpoints GET/POST/PUT/DELETE";
        let archetype = detect_archetype(prompt);
        assert!(archetype.is_some());
    }

    #[test]
    fn detect_cli_archetype() {
        let prompt = "Create a command line tool that reads stdin, parses arguments with flags, \
                       and outputs to stdout";
        let archetype = detect_archetype(prompt);
        assert!(matches!(archetype, Some(ProjectArchetype::CliTool)));
    }

    #[test]
    fn extract_entities_from_srs() {
        let prompt = "Build a booking system with:\n\
                       1. SQLite database with 'bookmarks' table (id, url, title, tags, created_at)\n\
                       2. Endpoints: GET /bookmarks, POST /bookmarks, PUT /bookmarks/<id>";
        let entities = extract_entities_heuristic(prompt);
        assert!(!entities.is_empty());
        assert!(entities.iter().any(|e| e.name.contains("bookmark")));
    }

    #[test]
    fn extract_interfaces_from_endpoints() {
        let prompt = "Endpoints: GET /bookmarks (with ?tag= filter), POST /bookmarks, \
                       PUT /bookmarks/<id>, DELETE /bookmarks/<id>";
        let interfaces = extract_interfaces_heuristic(prompt);
        assert!(interfaces.len() >= 3);
        assert!(interfaces.iter().any(|i| i.method == "GET"));
        assert!(interfaces.iter().any(|i| i.method == "POST"));
    }

    #[test]
    fn empty_prompt_produces_empty_ir() {
        let ir = synthesize("fix the bug in line 42");
        assert!(ir.archetype.is_none() || ir.entities.is_empty());
    }

    #[test]
    fn fullstack_prompt_produces_rich_ir() {
        let prompt = "Build a Flask REST API for a bookmark manager:\n\
                       1. SQLite database with 'bookmarks' table (id, url, title, tags, created_at, is_favorite)\n\
                       2. Endpoints: GET /bookmarks (with ?tag= filter, ?search= query, ?fav=true), \
                          POST /bookmarks, PUT /bookmarks/<id>, DELETE /bookmarks/<id>, \
                          POST /bookmarks/<id>/favorite\n\
                       3. Input validation (valid URL format, title required, tags as comma-separated string)\n\
                       4. Error handling with proper HTTP status codes\n\
                       5. Add a requirements.txt\n\
                       6. Add a test file test_app.py with pytest tests covering all endpoints";
        let ir = synthesize(prompt);

        // Should detect archetype
        assert!(ir.archetype.is_some());

        // Should find entities
        assert!(!ir.entities.is_empty());

        // Should find interfaces
        assert!(!ir.interfaces.is_empty());

        // Should find validation invariants
        assert!(!ir.invariants.is_empty());

        // Should render non-empty prompt
        let rendered = ir.render_for_prompt();
        assert!(!rendered.is_empty());
        assert!(rendered.contains("Domain Architecture"));
    }

    #[test]
    fn archetype_guidance_non_empty() {
        let guidance = archetype_guidance(&ProjectArchetype::FullStackWeb);
        assert!(guidance.contains("Normalize"));
        assert!(guidance.contains("foreign key"));
    }
}

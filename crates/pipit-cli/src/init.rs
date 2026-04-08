//! `pipit init --profile <framework>` — project scaffolding with framework-specific config.
//!
//! Creates a `.pipit/` directory with configuration tailored to the chosen framework:
//! - PIPIT.md project instructions
//! - Rules for the language/framework
//! - Skill discovery paths
//! - Hooks (optional)
//! - MCP server suggestions

use anyhow::Result;
use std::path::Path;

/// Known framework profiles with their configurations.
struct Profile {
    name: &'static str,
    language: &'static str,
    instructions: &'static str,
    rules: Vec<(&'static str, &'static str)>,
    gitignore_entries: Vec<&'static str>,
    test_command: Option<&'static str>,
    lint_command: Option<&'static str>,
}

fn get_profile(name: &str) -> Option<Profile> {
    match name.to_lowercase().as_str() {
        "react" | "nextjs" | "next" => Some(Profile {
            name: "react",
            language: "TypeScript",
            instructions: "## Project Type\nReact/Next.js TypeScript application.\n\n## Conventions\n- Use functional components with hooks\n- Prefer named exports over default exports\n- Use TypeScript strict mode\n- CSS: Tailwind utility classes preferred\n- State: React Query for server state, Zustand/Context for client state\n- Testing: Vitest + React Testing Library\n\n## File Organization\n- `src/components/` — reusable UI components\n- `src/pages/` or `src/app/` — route pages\n- `src/hooks/` — custom hooks\n- `src/lib/` — utilities and helpers\n- `src/types/` — TypeScript type definitions\n",
            rules: vec![
                ("coding-style", "- Use TypeScript strict mode (`\"strict\": true` in tsconfig)\n- Prefer `const` over `let`; never use `var`\n- Use template literals over string concatenation\n- Arrow functions for callbacks; named functions for top-level\n- Destructure props in component signatures\n"),
                ("testing", "- Write tests alongside components: `Component.test.tsx`\n- Use React Testing Library with user-event\n- Test behavior, not implementation details\n- Mock API calls with MSW (Mock Service Worker)\n- Minimum 80% coverage for new code\n"),
                ("security", "- Sanitize all user input before rendering (XSS prevention)\n- Use `DOMPurify` for HTML content\n- Never store secrets in client-side code\n- Use HTTPS for all API calls\n- Validate environment variables at startup\n"),
            ],
            gitignore_entries: vec!["node_modules/", ".next/", "dist/", ".env.local", ".env"],
            test_command: Some("npx vitest run"),
            lint_command: Some("npx eslint . --ext .ts,.tsx"),
        }),
        "node" | "express" => Some(Profile {
            name: "node",
            language: "TypeScript",
            instructions: "## Project Type\nNode.js/Express TypeScript backend.\n\n## Conventions\n- Use Express with TypeScript\n- Repository pattern for data access\n- Middleware chain for auth, validation, error handling\n- Structured logging with pino/winston\n- Environment config via dotenv + validation\n\n## File Organization\n- `src/routes/` — Express route handlers\n- `src/middleware/` — Express middleware\n- `src/models/` — data models / ORM entities\n- `src/services/` — business logic\n- `src/utils/` — shared utilities\n",
            rules: vec![
                ("coding-style", "- Use TypeScript strict mode\n- Async/await over raw promises\n- Error handling: custom AppError class with status codes\n- Input validation: zod or joi schemas\n- API responses: consistent envelope `{ data, error, meta }`\n"),
                ("testing", "- Unit tests: Vitest or Jest\n- Integration tests: supertest for API endpoints\n- Test database: use test containers or SQLite in-memory\n- Minimum 80% coverage\n"),
            ],
            gitignore_entries: vec!["node_modules/", "dist/", ".env", ".env.local"],
            test_command: Some("npx vitest run"),
            lint_command: Some("npx eslint ."),
        }),
        "python" | "django" | "flask" | "fastapi" => Some(Profile {
            name: "python",
            language: "Python",
            instructions: "## Project Type\nPython application.\n\n## Conventions\n- Python 3.11+ with type hints everywhere\n- Use virtual environments (venv/poetry/uv)\n- Format: black + isort\n- Lint: ruff or flake8\n- Docstrings: Google style\n\n## File Organization\n- `src/` or project name package — main source\n- `tests/` — pytest test suite\n- `pyproject.toml` — project metadata and dependencies\n",
            rules: vec![
                ("coding-style", "- Type hints on all function signatures\n- Prefer dataclasses or Pydantic models over dicts\n- Use `pathlib.Path` over `os.path`\n- Context managers for resource management\n- f-strings over format() or %\n"),
                ("testing", "- Use pytest with fixtures\n- Test files: `test_*.py` in `tests/` directory\n- Use `pytest-cov` for coverage (minimum 80%)\n- Mock external services with `unittest.mock` or `pytest-mock`\n"),
            ],
            gitignore_entries: vec!["__pycache__/", "*.pyc", ".venv/", "venv/", ".env", "dist/", "*.egg-info/"],
            test_command: Some("pytest"),
            lint_command: Some("ruff check ."),
        }),
        "rust" | "cargo" => Some(Profile {
            name: "rust",
            language: "Rust",
            instructions: "## Project Type\nRust application or library.\n\n## Conventions\n- Use Rust 2021 edition\n- `cargo clippy` must pass with no warnings\n- `cargo fmt` for formatting\n- Error handling: thiserror for libraries, anyhow for applications\n- Prefer owned types in public APIs; borrow internally\n\n## File Organization\n- `src/lib.rs` — library root\n- `src/main.rs` — binary entry point\n- `src/` — module files\n- `tests/` — integration tests\n- `benches/` — benchmarks\n",
            rules: vec![
                ("coding-style", "- No `unwrap()` in production code — use `?` or explicit error handling\n- Prefer `impl Trait` over `dyn Trait` when possible\n- Use `#[must_use]` on functions that return important values\n- Minimize `unsafe` — document every use with SAFETY comments\n"),
                ("testing", "- Unit tests in `#[cfg(test)] mod tests` at bottom of each file\n- Integration tests in `tests/` directory\n- Use `proptest` or `quickcheck` for property-based testing\n- `cargo test` must pass before commit\n"),
            ],
            gitignore_entries: vec!["target/", "Cargo.lock"],
            test_command: Some("cargo test"),
            lint_command: Some("cargo clippy -- -D warnings"),
        }),
        "go" | "golang" => Some(Profile {
            name: "go",
            language: "Go",
            instructions: "## Project Type\nGo application.\n\n## Conventions\n- Go 1.22+ with modules\n- Standard library preferred over dependencies\n- Error wrapping with `fmt.Errorf(\"...: %w\", err)`\n- Interfaces: accept interfaces, return structs\n- Package names: short, lowercase, no underscores\n\n## File Organization\n- `cmd/` — binary entry points\n- `internal/` — private packages\n- `pkg/` — public packages (if library)\n- `*_test.go` — tests alongside source\n",
            rules: vec![
                ("coding-style", "- `gofmt` and `go vet` must pass\n- Exported names have doc comments\n- Error handling: always check errors, no `_` for error values\n- Context: pass `context.Context` as first parameter\n"),
                ("testing", "- Table-driven tests with `t.Run()` subtests\n- Use `testing.T` helper methods\n- `go test ./...` must pass\n- Use testify for assertions if needed\n"),
            ],
            gitignore_entries: vec!["bin/", "vendor/"],
            test_command: Some("go test ./..."),
            lint_command: Some("golangci-lint run"),
        }),
        "typescript" | "ts" => Some(Profile {
            name: "typescript",
            language: "TypeScript",
            instructions: "## Project Type\nTypeScript project.\n\n## Conventions\n- TypeScript strict mode\n- ESM modules preferred\n- Consistent import ordering\n- Explicit return types on public functions\n",
            rules: vec![
                ("coding-style", "- Use TypeScript strict mode\n- Prefer `const` over `let`\n- Use discriminated unions over type assertions\n- Avoid `any` — use `unknown` + type guards\n"),
            ],
            gitignore_entries: vec!["node_modules/", "dist/", ".env"],
            test_command: Some("npx vitest run"),
            lint_command: Some("npx eslint ."),
        }),
        "minimal" | "default" => Some(Profile {
            name: "minimal",
            language: "Generic",
            instructions: "## Project\nA software project managed with pipit.\n\n## Conventions\n- Follow existing code style and patterns\n- Write tests for new functionality\n- Keep functions focused and small\n",
            rules: vec![],
            gitignore_entries: vec![".pipit/sessions/", ".pipit/proofs/", ".pipit/repomap.cache"],
            test_command: None,
            lint_command: None,
        }),
        _ => None,
    }
}

/// Initialize a project with the given profile.
pub fn run_init(profile: &str, path: &str) -> Result<()> {
    let project_dir = if path == "." {
        std::env::current_dir()?
    } else {
        std::path::PathBuf::from(path)
    };

    let prof = get_profile(profile).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown profile: '{}'. Available: react, nextjs, node, python, rust, go, typescript, minimal",
            profile
        )
    })?;

    eprintln!("\x1b[1;36m🐦 Initializing pipit for {} ({})\x1b[0m\n", prof.name, prof.language);

    let pipit_dir = project_dir.join(".pipit");
    std::fs::create_dir_all(&pipit_dir)?;
    std::fs::create_dir_all(pipit_dir.join("skills"))?;
    std::fs::create_dir_all(pipit_dir.join("rules"))?;
    std::fs::create_dir_all(pipit_dir.join("agents"))?;
    std::fs::create_dir_all(pipit_dir.join("hooks"))?;
    std::fs::create_dir_all(pipit_dir.join("plugins"))?;

    // Write PIPIT.md
    let pipit_md = project_dir.join("PIPIT.md");
    if !pipit_md.exists() {
        std::fs::write(&pipit_md, prof.instructions)?;
        eprintln!("  \x1b[32m✓\x1b[0m Created PIPIT.md");
    } else {
        eprintln!("  \x1b[33m·\x1b[0m PIPIT.md already exists (skipped)");
    }

    // Write rules
    for (name, content) in &prof.rules {
        let rule_file = pipit_dir.join("rules").join(format!("{}.md", name));
        if !rule_file.exists() {
            std::fs::write(&rule_file, content)?;
            eprintln!("  \x1b[32m✓\x1b[0m Created .pipit/rules/{}.md", name);
        }
    }

    // Write .gitignore entries
    let gitignore = project_dir.join(".gitignore");
    if !prof.gitignore_entries.is_empty() {
        let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
        let mut additions = Vec::new();
        for entry in &prof.gitignore_entries {
            if !existing.contains(entry) {
                additions.push(*entry);
            }
        }
        if !additions.is_empty() {
            let mut content = existing;
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push_str("\n# pipit\n");
            for entry in &additions {
                content.push_str(entry);
                content.push('\n');
            }
            std::fs::write(&gitignore, content)?;
            eprintln!("  \x1b[32m✓\x1b[0m Updated .gitignore ({} entries)", additions.len());
        }
    }

    // Write project config
    let config_file = pipit_dir.join("config.toml");
    if !config_file.exists() {
        let mut config = String::from("[project]\n");
        config.push_str(&format!("language = \"{}\"\n", prof.language));
        config.push_str(&format!("profile = \"{}\"\n", prof.name));
        if let Some(test_cmd) = prof.test_command {
            config.push_str(&format!("test_command = \"{}\"\n", test_cmd));
        }
        if let Some(lint_cmd) = prof.lint_command {
            config.push_str(&format!("lint_command = \"{}\"\n", lint_cmd));
        }
        std::fs::write(&config_file, config)?;
        eprintln!("  \x1b[32m✓\x1b[0m Created .pipit/config.toml");
    }

    eprintln!("\n\x1b[1;32mDone!\x1b[0m Project initialized with '{}' profile.", prof.name);
    eprintln!("\x1b[2mRun `pipit` to start coding.\x1b[0m\n");

    Ok(())
}

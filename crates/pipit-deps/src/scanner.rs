//! Dependency manifest scanner.
//! Detects which package managers are in use and extracts dependency lists.

use std::path::Path;

/// Detected package ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Cargo,
    Npm,
    Python,
    Go,
}

/// Detect which ecosystems are present in a project.
pub fn detect_ecosystems(project_root: &Path) -> Vec<Ecosystem> {
    let mut ecosystems = Vec::new();
    if project_root.join("Cargo.toml").exists() {
        ecosystems.push(Ecosystem::Cargo);
    }
    if project_root.join("package.json").exists() {
        ecosystems.push(Ecosystem::Npm);
    }
    if project_root.join("pyproject.toml").exists()
        || project_root.join("requirements.txt").exists()
    {
        ecosystems.push(Ecosystem::Python);
    }
    if project_root.join("go.mod").exists() {
        ecosystems.push(Ecosystem::Go);
    }
    ecosystems
}

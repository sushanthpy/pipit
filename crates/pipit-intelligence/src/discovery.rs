use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Walk the project, respecting .gitignore and binary detection.
pub fn discover_files(root: &Path, max_file_size: u64) -> Vec<PathBuf> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_filesize(Some(max_file_size))
        .filter_entry(|entry| {
            let name = entry.file_name().to_str().unwrap_or("");
            !matches!(
                name,
                "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | ".git"
                    | "__pycache__"
                    | ".venv"
                    | "vendor"
                    | ".next"
                    | ".nuxt"
            )
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| !is_binary_file(e.path()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|p| p.to_path_buf()))
        .collect()
}

/// Detect if a file is binary by checking first 8KB for null bytes.
fn is_binary_file(path: &Path) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };
    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return true,
    };
    buf[..n].contains(&0)
}

/// Detect programming language from file extension.
pub fn detect_language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some("cpp"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "zig" => Some("zig"),
        "kt" | "kts" => Some("kotlin"),
        "scala" => Some("scala"),
        "cs" => Some("csharp"),
        "lua" => Some("lua"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "json" => Some("json"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Sandbox configuration for bash command isolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub mode: SandboxMode,
    pub filesystem: FilesystemPolicy,
    pub network: NetworkPolicy,
    pub excluded_commands: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: cfg!(any(target_os = "linux", target_os = "macos")),
            mode: SandboxMode::AutoAllow,
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            excluded_commands: vec!["docker".into(), "kubectl".into(), "podman".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    AutoAllow,  // sandboxed commands auto-approved
    Supervised, // all commands shown, sandboxed marked low-risk
    Disabled,   // no sandbox
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemPolicy {
    pub allow_write: Vec<String>,
    pub deny_read: Vec<String>,
    pub read_only_system: bool,
}

impl Default for FilesystemPolicy {
    fn default() -> Self {
        Self {
            allow_write: vec![".".into()],
            deny_read: vec![
                "~/.ssh".into(),
                "~/.aws".into(),
                "~/.gnupg".into(),
                "~/.config/pipit/secrets".into(),
            ],
            read_only_system: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub allowed_domains: Vec<String>,
    pub block_all: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            allowed_domains: vec![
                "github.com".into(), "npmjs.com".into(),
                "pypi.org".into(), "crates.io".into(),
                "registry.npmjs.org".into(),
            ],
            block_all: false,
        }
    }
}

/// Wrap a command in OS-level sandbox if available.
pub fn sandboxed_command(
    command: &str,
    cwd: &Path,
    config: &SandboxConfig,
) -> tokio::process::Command {
    if !config.enabled {
        return plain_command(command, cwd);
    }

    // Check if command is excluded from sandboxing
    for excluded in &config.excluded_commands {
        if command.starts_with(excluded) || command.contains(&format!(" {}", excluded)) {
            return plain_command(command, cwd);
        }
    }

    #[cfg(target_os = "linux")]
    if which_exists("bwrap") {
        return bwrap_command(command, cwd, config);
    }

    #[cfg(target_os = "macos")]
    if which_exists("sandbox-exec") {
        return seatbelt_command(command, cwd, config);
    }

    plain_command(command, cwd)
}

fn plain_command(command: &str, cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(cwd);
    cmd
}

#[cfg(target_os = "linux")]
fn bwrap_command(command: &str, cwd: &Path, config: &SandboxConfig) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("bwrap");
    let cwd_str = cwd.to_str().unwrap_or(".");

    cmd.args(&["--ro-bind", "/usr", "/usr"])
        .args(&["--ro-bind", "/lib", "/lib"])
        .args(&["--ro-bind", "/bin", "/bin"])
        .args(&["--ro-bind", "/etc", "/etc"])
        .args(&["--symlink", "usr/lib64", "/lib64"])
        .args(&["--proc", "/proc"])
        .args(&["--dev", "/dev"])
        .args(&["--tmpfs", "/tmp"]);

    // Read-write bind for project directory
    cmd.args(&["--bind", cwd_str, cwd_str]);

    // Additional write paths
    for write_path in &config.filesystem.allow_write {
        let abs_path = if write_path == "." {
            cwd_str.to_string()
        } else {
            cwd.join(write_path).to_string_lossy().to_string()
        };
        cmd.args(&["--bind", &abs_path, &abs_path]);
    }

    cmd.args(&["--chdir", cwd_str])
        .args(&["--unshare-pid"])
        .args(&["--die-with-parent"]);

    if config.network.block_all {
        cmd.args(&["--unshare-net"]);
    }

    cmd.args(&["--", "sh", "-c", command]);
    cmd
}

#[cfg(target_os = "macos")]
fn seatbelt_command(command: &str, cwd: &Path, config: &SandboxConfig) -> tokio::process::Command {
    let cwd_str = cwd.to_str().unwrap_or(".");

    // Generate a Seatbelt profile
    let mut profile = String::from("(version 1)\n(deny default)\n");
    profile.push_str("(allow process-exec)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str(&format!(
        "(allow file-read* (subpath \"{}\"))\n",
        cwd_str
    ));
    profile.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        cwd_str
    ));
    // Read-only system paths
    profile.push_str("(allow file-read* (subpath \"/usr\"))\n");
    profile.push_str("(allow file-read* (subpath \"/bin\"))\n");
    profile.push_str("(allow file-read* (subpath \"/sbin\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System\"))\n");
    profile.push_str("(allow file-read* (subpath \"/private/tmp\"))\n");
    profile.push_str("(allow file-read* (subpath \"/private/var\"))\n");
    // Deny sensitive directories
    for deny in &config.filesystem.deny_read {
        let expanded = shellexpand_tilde(deny);
        profile.push_str(&format!("(deny file-read* (subpath \"{}\"))\n", expanded));
    }
    if !config.network.block_all {
        profile.push_str("(allow network*)\n");
    }

    let mut cmd = tokio::process::Command::new("sandbox-exec");
    cmd.arg("-p").arg(&profile).arg("sh").arg("-c").arg(command);
    cmd.current_dir(cwd);
    cmd
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn shellexpand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}{}", home, &path[1..]);
        }
    }
    path.to_string()
}

/// Load sandbox config from .pipit/sandbox.toml.
/// If no config file exists, sandbox is auto-enabled only when a `.pipit/` directory
/// is present (indicating a pipit-managed project, not a temp/test dir).
pub fn load_sandbox_config(project_root: &Path) -> SandboxConfig {
    let config_path = project_root.join(".pipit").join("sandbox.toml");
    if config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(config) = toml::from_str(&content) {
                return config;
            }
        }
    }
    // Only auto-enable sandbox for real projects with .pipit/ dir
    let mut config = SandboxConfig::default();
    if !project_root.join(".pipit").exists() {
        config.enabled = false;
    }
    config
}

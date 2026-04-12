//! Distribution manifest — the TOML file that tells the kernel which
//! provider, policy, tools, and frontend to run. Read by both the
//! daemon (for provider) and `dist-code-agent` (for policy, tools,
//! frontend).
//!
//! The types in this file are pure data — the runtime construction
//! (e.g., turning `ProviderConfig::Anthropic` into an `AnthropicProvider`)
//! happens in whichever crate owns the concrete impl (`kernel-providers`
//! via `kernel-daemon`, etc.). That's why this module lives here on
//! the stable API side: distributions implementing their own providers
//! or tools need the shared file format, not the first-party impls.
//!
//! File format (v0.2, extended by spec 0012):
//!
//! ```toml
//! [distribution]
//! name = "code-agent"
//! version = "0.1.0"
//!
//! [provider]
//! type = "anthropic"
//! model = "claude-sonnet-4-5"
//! api_key_env = "ANTHROPIC_API_KEY"
//! fallback = "echo"
//!
//! [policy]
//! file = "../policies/permissive.yaml"   # relative to this file
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionManifest {
    pub distribution: DistributionMeta,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub policy: Option<PolicyConfig>,
    #[serde(default)]
    pub frontend: Option<FrontendConfig>,
    /// Toolset entries. Each entry names a `kind` the kernel dispatches
    /// on (via a factory registry), plus an opaque `config` block passed
    /// through to the factory. Empty = no toolsets.
    #[serde(default, rename = "toolset")]
    pub toolsets: Vec<ToolsetEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionMeta {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderConfig {
    Anthropic {
        model: String,
        api_key_env: String,
        #[serde(default)]
        fallback: Option<ProviderFallback>,
    },
    Echo,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderFallback {
    Echo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Path to a YAML policy file, relative to the manifest file's
    /// directory. Absolute paths also work.
    pub file: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FrontendKind {
    Tui,
    Repl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontendConfig {
    #[serde(rename = "type")]
    pub kind: FrontendKind,
}

/// One `[[toolset]]` entry from the distribution manifest.
///
/// `kind` is routed through a factory registry in the daemon; the
/// `config` block is opaque at parse time and forwarded to whichever
/// factory matches. `id` is optional — if absent, the factory picks a
/// sensible default (typically derived from `kind`).
///
/// Example manifest TOML:
///
/// ```toml
/// [[toolset]]
/// kind = "workspace.local"
/// id = "workspace"
/// [toolset.config]
/// root = "."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsetEntry {
    pub kind: String,
    #[serde(default)]
    pub id: Option<String>,
    /// Opaque per-toolset configuration. The kernel does not interpret
    /// this; it is passed through to the factory for `kind`. Defaults
    /// to an empty TOML table so `config.get(...)` works whether or not
    /// the manifest included a `[toolset.config]` block.
    #[serde(default = "empty_toml_table")]
    pub config: toml::Value,
}

fn empty_toml_table() -> toml::Value {
    toml::Value::Table(toml::value::Table::new())
}

impl PolicyConfig {
    /// Resolve the policy file path against the manifest file's
    /// directory. If `self.file` is absolute, returns it unchanged.
    pub fn resolve(&self, manifest_dir: &Path) -> PathBuf {
        let p = Path::new(&self.file);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            manifest_dir.join(p)
        }
    }
}

/// Load a distribution manifest from a TOML file.
///
/// Returns a human-readable error on file-not-found, parse error, or
/// missing required fields. Never panics.
pub fn load_manifest(path: &Path) -> Result<DistributionManifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read manifest {}: {e}", path.display()))?;
    let manifest: DistributionManifest = toml::from_str(&text)
        .map_err(|e| format!("failed to parse manifest {}: {e}", path.display()))?;
    Ok(manifest)
}

/// Return the parent directory of a manifest file, for resolving
/// relative paths like `PolicyConfig::file`. Falls back to `.` if the
/// path has no parent (e.g., bare filename).
pub fn manifest_dir(manifest_path: &Path) -> PathBuf {
    manifest_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_toml(body: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_minimal_manifest() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
fallback = "echo"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert_eq!(manifest.distribution.name, "code-agent");
        assert!(matches!(
            manifest.provider,
            ProviderConfig::Anthropic { .. }
        ));
        assert!(manifest.policy.is_none());
    }

    #[test]
    fn parses_manifest_with_policy_section() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"

[policy]
file = "../policies/permissive.yaml"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        let policy = manifest.policy.expect("policy section present");
        assert_eq!(policy.file, "../policies/permissive.yaml");
    }

    #[test]
    fn policy_config_resolves_relative_to_manifest_dir() {
        let cfg = PolicyConfig {
            file: "../policies/permissive.yaml".into(),
        };
        let resolved = cfg.resolve(Path::new("/home/user/project/distros"));
        assert_eq!(
            resolved,
            PathBuf::from("/home/user/project/distros/../policies/permissive.yaml")
        );
    }

    #[test]
    fn policy_config_absolute_path_unchanged() {
        let cfg = PolicyConfig {
            file: "/abs/path/policy.yaml".into(),
        };
        let resolved = cfg.resolve(Path::new("/irrelevant"));
        assert_eq!(resolved, PathBuf::from("/abs/path/policy.yaml"));
    }

    #[test]
    fn manifest_dir_handles_bare_filename() {
        assert_eq!(
            manifest_dir(Path::new("code-agent.toml")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn parses_manifest_with_toolset_section() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"

[[toolset]]
kind = "workspace.local"
id = "workspace"
[toolset.config]
root = "."
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert_eq!(manifest.toolsets.len(), 1);
        let entry = &manifest.toolsets[0];
        assert_eq!(entry.kind, "workspace.local");
        assert_eq!(entry.id.as_deref(), Some("workspace"));
        assert_eq!(entry.config.get("root").and_then(|v| v.as_str()), Some("."));
    }

    #[test]
    fn parses_manifest_with_multiple_toolsets() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"

[[toolset]]
kind = "workspace.local"

[[toolset]]
kind = "mcp.stdio"
id = "github"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert_eq!(manifest.toolsets.len(), 2);
        assert_eq!(manifest.toolsets[0].kind, "workspace.local");
        assert_eq!(manifest.toolsets[1].kind, "mcp.stdio");
        assert_eq!(manifest.toolsets[1].id.as_deref(), Some("github"));
    }

    #[test]
    fn legacy_tools_section_fails_to_parse() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"

[tools]
enabled = ["file_read"]
"#,
        );
        assert!(
            load_manifest(f.path()).is_err(),
            "legacy [tools] section should be rejected by deny_unknown_fields"
        );
    }

    #[test]
    fn parses_frontend_tui() {
        let f = write_toml(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
type = "echo"

[frontend]
type = "tui"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert_eq!(
            manifest.frontend.expect("frontend section").kind,
            FrontendKind::Tui
        );
    }

    #[test]
    fn parses_frontend_repl() {
        let f = write_toml(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
type = "echo"

[frontend]
type = "repl"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert_eq!(
            manifest.frontend.expect("frontend section").kind,
            FrontendKind::Repl
        );
    }

    #[test]
    fn missing_toolset_section_is_empty_vec() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        assert!(manifest.toolsets.is_empty());
    }
}

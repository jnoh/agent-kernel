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
pub struct DistributionManifest {
    pub distribution: DistributionMeta,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub policy: Option<PolicyConfig>,
    #[serde(default)]
    pub tools: Option<ToolsConfig>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    /// Tool IDs to enable, in any order. Each ID must match a tool
    /// the distribution actually implements. Unknown IDs are logged
    /// as warnings but don't error.
    ///
    /// An empty list means "no tools" — explicit. A missing
    /// `[tools]` section (`tools: None` on the manifest) means
    /// "enable every tool the distribution implements" for
    /// backwards compatibility.
    #[serde(default)]
    pub enabled: Vec<String>,
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
    fn parses_manifest_with_tools_section() {
        let f = write_toml(
            r#"
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "echo"

[tools]
enabled = ["file_read", "grep"]
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        let tools = manifest.tools.expect("tools section present");
        assert_eq!(tools.enabled, vec!["file_read", "grep"]);
    }

    #[test]
    fn missing_tools_section_is_none() {
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
        assert!(manifest.tools.is_none());
    }
}

//! Distribution manifest parsing.
//!
//! A distribution manifest is a TOML file that tells the kernel-daemon
//! which provider to use, which policy to load, which tools to offer,
//! and which frontend to expect. This spec (0011) lands the parsing
//! infrastructure and wires provider selection. Policy, tools, and
//! frontend fields remain absent until specs 0012/0013/0014 add them.
//!
//! The file format (minimal, v0.2):
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
//! fallback = "echo"          # optional; without it, missing key = hard error
//! ```

use kernel_interfaces::provider::ProviderInterface;
use kernel_providers::{AnthropicProvider, EchoProvider};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// A factory closure the daemon calls once per session-create to
/// construct a fresh provider instance. `Arc` so the daemon can share
/// the same factory across multiple connections (the accept loop
/// constructs a new router per connection).
pub type ProviderFactory = Arc<dyn Fn() -> Box<dyn ProviderInterface + Send> + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionManifest {
    pub distribution: DistributionMeta,
    pub provider: ProviderConfig,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderFallback {
    Echo,
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

impl DistributionManifest {
    /// Build a provider factory closure from the manifest's `[provider]`
    /// section. Reads any required env vars eagerly so that a missing
    /// API key becomes a startup error, not a first-turn error.
    pub fn provider_factory(&self) -> Result<ProviderFactory, String> {
        match &self.provider {
            ProviderConfig::Echo => Ok(Arc::new(|| {
                Box::new(EchoProvider) as Box<dyn ProviderInterface + Send>
            })),
            ProviderConfig::Anthropic {
                model,
                api_key_env,
                fallback,
            } => {
                let key = std::env::var(api_key_env).ok();
                match (key, fallback) {
                    (Some(k), _) => {
                        let model = model.clone();
                        Ok(Arc::new(move || {
                            Box::new(AnthropicProvider::new(k.clone(), model.clone()))
                                as Box<dyn ProviderInterface + Send>
                        }))
                    }
                    (None, Some(ProviderFallback::Echo)) => {
                        eprintln!(
                            "manifest: env var {api_key_env} not set; using fallback echo provider"
                        );
                        Ok(Arc::new(|| {
                            Box::new(EchoProvider) as Box<dyn ProviderInterface + Send>
                        }))
                    }
                    (None, None) => Err(format!(
                        "manifest declares anthropic provider but env var {api_key_env} \
                         is not set and no fallback is configured"
                    )),
                }
            }
        }
    }
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
        assert_eq!(manifest.distribution.version, "0.1.0");
        match manifest.provider {
            ProviderConfig::Anthropic {
                model,
                api_key_env,
                fallback,
            } => {
                assert_eq!(model, "claude-sonnet-4-5");
                assert_eq!(api_key_env, "ANTHROPIC_API_KEY");
                assert!(matches!(fallback, Some(ProviderFallback::Echo)));
            }
            _ => panic!("expected anthropic provider"),
        }
    }

    #[test]
    fn rejects_missing_provider_type() {
        let f = write_toml(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(
            err.contains("provider") || err.contains("type"),
            "error should mention the missing field: {err}"
        );
    }

    #[test]
    fn echo_provider_no_env_var_needed() {
        let f = write_toml(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
type = "echo"
"#,
        );
        let manifest = load_manifest(f.path()).expect("parse");
        // Should succeed regardless of env vars.
        let _factory = manifest.provider_factory().expect("factory");
    }

    /// Sentinel env var name that is extremely unlikely to be set in any
    /// test environment. If it somehow is, this test will give a false
    /// negative — accept it.
    const SENTINEL_ENV: &str = "DEFINITELY_NOT_SET_12345_AGENT_KERNEL_SENTINEL";

    #[test]
    fn anthropic_fallback_to_echo_when_env_var_missing() {
        let toml_text = format!(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
type = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "{SENTINEL_ENV}"
fallback = "echo"
"#
        );
        let f = write_toml(&toml_text);
        let manifest = load_manifest(f.path()).expect("parse");
        // Should produce a factory that constructs an echo provider.
        let factory = manifest
            .provider_factory()
            .expect("factory should fall back");
        // We can't type-check the returned provider directly without
        // adding a "kind" accessor, but we can verify the factory
        // builds *something* and that a complete call returns the
        // echo stub's characteristic response.
        let provider = factory();
        let prompt = kernel_interfaces::types::Prompt {
            system: String::new(),
            messages: vec![kernel_interfaces::types::Message {
                role: kernel_interfaces::types::Role::User,
                content: vec![kernel_interfaces::types::Content::Text("hi".into())],
            }],
            tool_definitions: Vec::new(),
        };
        let config = kernel_interfaces::types::CompletionConfig::default();
        let response = provider.complete(&prompt, &config).expect("complete");
        let text: String = response
            .content
            .iter()
            .filter_map(|c| match c {
                kernel_interfaces::types::Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("[echo provider]"),
            "expected echo provider response, got: {text}"
        );
    }

    #[test]
    fn anthropic_no_fallback_is_hard_error() {
        let toml_text = format!(
            r#"
[distribution]
name = "x"
version = "0.0.0"

[provider]
type = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "{SENTINEL_ENV}"
"#
        );
        let f = write_toml(&toml_text);
        let manifest = load_manifest(f.path()).expect("parse");
        match manifest.provider_factory() {
            Ok(_) => panic!("expected hard error when env var is missing"),
            Err(e) => assert!(
                e.contains(SENTINEL_ENV),
                "error should mention the env var: {e}"
            ),
        }
    }
}

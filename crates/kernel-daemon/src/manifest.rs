//! Daemon-side manifest plumbing.
//!
//! The data types (`DistributionManifest`, `ProviderConfig`, etc.) and
//! the file parser live in `kernel-interfaces::manifest` so that both
//! the daemon and `dist-code-agent` can read the same TOML file (spec
//! 0012). This module only holds the daemon-specific bit: turning a
//! `ProviderConfig` into an Arc'd factory closure that constructs real
//! `AnthropicProvider` / `EchoProvider` values from `kernel-providers`.

// Re-exports from the interfaces crate so daemon code can import
// everything manifest-related from one place. Some of these aren't
// used in the daemon binary directly but are exposed so future
// daemon-local code (and tests) can pattern-match on the full shapes.
#[allow(unused_imports)]
pub use kernel_interfaces::manifest::{
    DistributionManifest, DistributionMeta, PolicyConfig, ProviderConfig, ProviderFallback,
    load_manifest, manifest_dir,
};
use kernel_interfaces::provider::ProviderInterface;
use kernel_providers::{AnthropicProvider, EchoProvider};
use std::sync::Arc;

/// A factory closure the daemon calls once per session-create to
/// construct a fresh provider instance. `Arc` so the daemon can share
/// the same factory across multiple connections (the accept loop
/// constructs a new router per connection).
pub type ProviderFactory = Arc<dyn Fn() -> Box<dyn ProviderInterface + Send> + Send + Sync>;

/// Build a provider factory from a parsed distribution manifest.
///
/// Reads any required env vars eagerly so that a missing API key
/// becomes a startup error, not a first-turn error. The `fallback`
/// field on the manifest's provider config lets a missing key
/// degrade gracefully to the echo provider with a stderr warning.
pub fn build_provider_factory(manifest: &DistributionManifest) -> Result<ProviderFactory, String> {
    match &manifest.provider {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_manifest() -> DistributionManifest {
        DistributionManifest {
            distribution: DistributionMeta {
                name: "test".into(),
                version: "0.0.0".into(),
            },
            provider: ProviderConfig::Echo,
            policy: None,
            toolsets: Vec::new(),
            frontend: None,
        }
    }

    fn anthropic_manifest(
        api_key_env: &str,
        fallback: Option<ProviderFallback>,
    ) -> DistributionManifest {
        DistributionManifest {
            distribution: DistributionMeta {
                name: "test".into(),
                version: "0.0.0".into(),
            },
            provider: ProviderConfig::Anthropic {
                model: "claude-sonnet-4-5".into(),
                api_key_env: api_key_env.into(),
                fallback,
            },
            policy: None,
            toolsets: Vec::new(),
            frontend: None,
        }
    }

    #[test]
    fn echo_factory_needs_no_env_var() {
        let _f = build_provider_factory(&echo_manifest()).expect("factory");
    }

    const SENTINEL_ENV: &str = "DEFINITELY_NOT_SET_12345_AGENT_KERNEL_SENTINEL";

    #[test]
    fn anthropic_fallback_to_echo_when_env_var_missing() {
        let manifest = anthropic_manifest(SENTINEL_ENV, Some(ProviderFallback::Echo));
        let factory = build_provider_factory(&manifest).expect("factory");
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
        let manifest = anthropic_manifest(SENTINEL_ENV, None);
        match build_provider_factory(&manifest) {
            Ok(_) => panic!("expected hard error"),
            Err(e) => assert!(e.contains(SENTINEL_ENV), "error: {e}"),
        }
    }
}

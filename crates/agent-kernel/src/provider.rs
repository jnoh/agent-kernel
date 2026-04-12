//! Provider factory — turns a manifest's `[provider]` config into an
//! Arc'd closure that constructs concrete provider instances.

use kernel_interfaces::manifest::{DistributionManifest, ProviderConfig, ProviderFallback};
use kernel_interfaces::provider::ProviderInterface;
use kernel_providers::{AnthropicProvider, EchoProvider};
use std::sync::Arc;

pub type ProviderFactory = Arc<dyn Fn() -> Box<dyn ProviderInterface + Send> + Send + Sync>;

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

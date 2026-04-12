//! First-party `ProviderInterface` implementations.
//!
//! One crate, one module per provider. The crate boundary delivers the
//! modularity story (a distribution that wants to use these impls
//! depends on `kernel-providers` directly; a distribution with its own
//! provider depends only on `kernel-interfaces` and doesn't pull these
//! in); the module split keeps each provider's code isolated without
//! multiplying Cargo manifests.
//!
//! Current members:
//! - `anthropic` — real Claude API via HTTPS (requires `ANTHROPIC_API_KEY`)
//! - `echo` — stub provider for tests and for running without an API key

pub mod anthropic;
pub mod echo;

pub use anthropic::AnthropicProvider;
pub use echo::EchoProvider;

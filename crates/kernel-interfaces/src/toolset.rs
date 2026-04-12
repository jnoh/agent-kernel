//! `ToolSet` trait — the discovery-style interface toolsets register through.
//!
//! A toolset is a named collection of tools. The kernel builds a `ToolSet`
//! instance per `[[toolset]]` manifest entry at daemon startup via a factory
//! function keyed on the entry's `kind` field. Once built, the kernel calls
//! `tools()` to discover what the toolset exposes and merges the results into
//! the per-session tool list.
//!
//! Spec 0015 ships exactly one toolset kind: `workspace.local`, provided by
//! the `kernel-workspace-local` library crate. In that world, `ToolSet`
//! is an in-process trait and construction is a regular Rust function call.
//! Spec 0016 introduces an `mcp.stdio` kind whose `ToolSet` impl wraps a
//! subprocess connection — same trait, different transport.

use crate::tool::ToolRegistration;

/// A named collection of tools, constructed from a manifest `[[toolset]]`
/// entry by a registered factory function.
pub trait ToolSet: Send + Sync {
    /// Identifier for this toolset, used in logs and collision errors.
    /// Typically derived from the manifest entry's `id` field or a default
    /// based on the `kind`.
    fn id(&self) -> &str;

    /// The tools this toolset exposes. Called once at pool construction
    /// time and then cached; toolsets must not rely on this being called
    /// again mid-session.
    fn tools(&self) -> Vec<Box<dyn ToolRegistration>>;
}

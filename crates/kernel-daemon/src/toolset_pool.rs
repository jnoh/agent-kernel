//! Daemon-side toolset pool.
//!
//! Builds a set of `ToolSet` instances at daemon startup, one per
//! `[[toolset]]` entry in the distribution manifest. Each entry's `kind`
//! is routed through a factory registry to a constructor function; the
//! entry's opaque `config` table is handed through. Spec 0015 registers
//! exactly one factory: `workspace.local → kernel_workspace_local::from_entry`.
//!
//! After build, the pool holds each toolset's discovered tools as an
//! immutable per-session snapshot. `tools_for_session()` returns a freshly
//! boxed copy for the session's `EventLoopConfig`. Name collisions across
//! toolsets are a hard startup error.
//!
//! Spec 0016 replaces the in-process `workspace.local` kind with
//! `mcp.stdio`, whose factory spawns a subprocess (typically
//! `kernel-workspace-local`) and proxies tool calls across JSON-RPC.
//! The daemon no longer depends on the workspace-local library crate
//! at all — the tool implementations are loaded via subprocess.

use kernel_interfaces::manifest::ToolsetEntry;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::toolset::ToolSet;
use std::collections::{HashMap, HashSet};

/// A factory function for a given `kind` value. Receives the entry so
/// it can read both `id` and `config`, and returns a boxed `ToolSet`.
pub type ToolsetFactory = fn(&ToolsetEntry) -> Result<Box<dyn ToolSet>, String>;

/// Maps manifest `kind` strings to their constructor functions. The
/// daemon passes an instance of this into `ToolsetPool::build`.
pub type FactoryRegistry = HashMap<&'static str, ToolsetFactory>;

/// The built-in factory registry for the daemon. Spec 0016 registers
/// one kind: `mcp.stdio`, backed by `kernel_core::mcp_stdio::from_entry`,
/// which spawns a subprocess and proxies tool calls across JSON-RPC.
/// More kinds slot in alongside this without touching existing code.
pub fn default_registry() -> FactoryRegistry {
    let mut m: FactoryRegistry = HashMap::new();
    m.insert(
        "mcp.stdio",
        kernel_core::mcp_stdio::from_entry as ToolsetFactory,
    );
    m
}

/// A pool of live toolsets built from a manifest.
///
/// Ownership note: the pool owns the `ToolSet` instances for the daemon's
/// lifetime. Each session-create call grabs a fresh `Vec<Box<dyn
/// ToolRegistration>>` via `tools_for_session()`, which re-calls each
/// toolset's `tools()` method. That means a single toolset can produce
/// multiple independent tool instance sets — one per session — which is
/// the right behavior: tools that hold per-call state (buffers, counters)
/// stay isolated across sessions.
pub struct ToolsetPool {
    toolsets: Vec<Box<dyn ToolSet>>,
}

impl ToolsetPool {
    /// Construct a pool by invoking each entry's factory in order.
    ///
    /// Errors if:
    ///   - An entry's `kind` has no registered factory.
    ///   - A factory function itself returns an error.
    ///   - Two toolsets advertise a tool with the same name (collision
    ///     detection runs once the full pool is built).
    pub fn build(entries: &[ToolsetEntry], registry: &FactoryRegistry) -> Result<Self, String> {
        let mut toolsets: Vec<Box<dyn ToolSet>> = Vec::with_capacity(entries.len());
        for entry in entries {
            let factory = registry.get(entry.kind.as_str()).ok_or_else(|| {
                format!(
                    "manifest entry [[toolset]] has unknown kind {:?}; known kinds: {:?}",
                    entry.kind,
                    registry.keys().collect::<Vec<_>>()
                )
            })?;
            let toolset = factory(entry).map_err(|e| {
                format!(
                    "failed to construct toolset kind={} id={:?}: {e}",
                    entry.kind, entry.id
                )
            })?;
            toolsets.push(toolset);
        }

        // Collision check: every advertised tool name must be unique
        // across the whole pool. Report the first conflict with both
        // owning toolset ids so the operator can identify which entries
        // to adjust.
        let mut seen: HashMap<String, String> = HashMap::new();
        for ts in &toolsets {
            let owner = ts.id().to_string();
            let tools = ts.tools();
            let mut local_seen: HashSet<String> = HashSet::new();
            for tool in &tools {
                let name = tool.name().to_string();
                if !local_seen.insert(name.clone()) {
                    return Err(format!(
                        "toolset {owner:?} advertises tool {name:?} more than once"
                    ));
                }
                if let Some(prev) = seen.insert(name.clone(), owner.clone()) {
                    return Err(format!(
                        "tool name collision: {name:?} provided by both {prev:?} and {owner:?}"
                    ));
                }
            }
        }

        Ok(Self { toolsets })
    }

    /// Snapshot the pool's full tool list for a new session. Each call
    /// re-invokes every toolset's `tools()` method, so tools can hold
    /// per-call state safely.
    pub fn tools_for_session(&self) -> Vec<Box<dyn ToolRegistration>> {
        let mut all = Vec::new();
        for ts in &self.toolsets {
            all.extend(ts.tools());
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::tool::{ToolError, ToolExecutionCtx, ToolOutput, ToolRegistration};
    use kernel_interfaces::types::{CapabilitySet, RelevanceSignal, TokenEstimate};

    struct DummyTool {
        name: String,
        caps: CapabilitySet,
        rel: RelevanceSignal,
    }

    impl DummyTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                caps: CapabilitySet::new(),
                rel: RelevanceSignal {
                    keywords: Vec::new(),
                    tags: Vec::new(),
                },
            }
        }
    }

    impl ToolRegistration for DummyTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn capabilities(&self) -> &CapabilitySet {
            &self.caps
        }
        fn schema(&self) -> &serde_json::Value {
            &serde_json::Value::Null
        }
        fn cost(&self) -> TokenEstimate {
            TokenEstimate(0)
        }
        fn relevance(&self) -> &RelevanceSignal {
            &self.rel
        }
        fn execute(
            &self,
            _: serde_json::Value,
            _: &ToolExecutionCtx<'_>,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::readonly(serde_json::Value::Null))
        }
    }

    struct DummyToolset {
        id: String,
        tool_names: Vec<String>,
    }

    impl ToolSet for DummyToolset {
        fn id(&self) -> &str {
            &self.id
        }
        fn tools(&self) -> Vec<Box<dyn ToolRegistration>> {
            self.tool_names
                .iter()
                .map(|n| Box::new(DummyTool::new(n)) as Box<dyn ToolRegistration>)
                .collect()
        }
    }

    fn empty_entry(kind: &str) -> ToolsetEntry {
        ToolsetEntry {
            kind: kind.into(),
            id: None,
            config: toml::Value::Table(Default::default()),
        }
    }

    #[test]
    fn unknown_kind_is_hard_error() {
        let registry = FactoryRegistry::new();
        match ToolsetPool::build(&[empty_entry("nope")], &registry) {
            Ok(_) => panic!("expected failure"),
            Err(e) => assert!(e.contains("unknown kind"), "err was {e}"),
        }
    }

    #[test]
    fn collision_across_toolsets_fails_build() {
        fn alpha(_: &ToolsetEntry) -> Result<Box<dyn ToolSet>, String> {
            Ok(Box::new(DummyToolset {
                id: "alpha".into(),
                tool_names: vec!["shell".into()],
            }))
        }
        fn beta(_: &ToolsetEntry) -> Result<Box<dyn ToolSet>, String> {
            Ok(Box::new(DummyToolset {
                id: "beta".into(),
                tool_names: vec!["shell".into()],
            }))
        }
        let mut registry = FactoryRegistry::new();
        registry.insert("alpha", alpha as ToolsetFactory);
        registry.insert("beta", beta as ToolsetFactory);

        match ToolsetPool::build(&[empty_entry("alpha"), empty_entry("beta")], &registry) {
            Ok(_) => panic!("expected collision error"),
            Err(e) => {
                assert!(e.contains("collision"), "err was {e}");
                assert!(e.contains("shell"));
            }
        }
    }

    #[test]
    fn default_registry_has_mcp_stdio() {
        let reg = default_registry();
        assert!(reg.contains_key("mcp.stdio"));
        assert!(!reg.contains_key("workspace.local"));
    }

    #[test]
    fn mcp_stdio_entry_without_command_is_rejected() {
        // The mcp.stdio factory needs `command` in config; without it
        // the factory returns Err and ToolsetPool::build surfaces that
        // as a hard startup error. Building an entry with no command
        // lets us verify the wiring without actually spawning anything.
        let reg = default_registry();
        let entry = ToolsetEntry {
            kind: "mcp.stdio".into(),
            id: Some("ws".into()),
            config: toml::Value::Table(Default::default()),
        };
        let err = match ToolsetPool::build(&[entry], &reg) {
            Ok(_) => panic!("expected mcp.stdio without command to fail"),
            Err(e) => e,
        };
        assert!(err.contains("command"), "err was {err}");
    }
}

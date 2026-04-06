use kernel_interfaces::policy::Policy;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::Decision;

/// The dispatch gate. Intercepts every tool invocation before execution.
/// Provides mechanism only — policy is external configuration.
///
/// Must be in the core because if it were a module, it could be unloaded or bypassed.
pub struct PermissionEvaluator {
    policy: Policy,
}

impl PermissionEvaluator {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    /// Replace the loaded policy (e.g., hot-reload from file change).
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Evaluate whether a tool invocation should be allowed.
    /// Checks every capability the tool declares against the loaded policy.
    /// All capabilities must be allowed for the tool call to proceed.
    pub fn evaluate(&self, tool: &dyn ToolRegistration) -> Decision {
        let capabilities = tool.capabilities();

        // Kernel-internal tools (empty capability set) are always allowed.
        if capabilities.is_empty() {
            return Decision::Allow;
        }

        let mut needs_ask = false;

        for cap in capabilities {
            match self.policy.evaluate(cap) {
                Decision::Allow => continue,
                Decision::Deny(reason) => return Decision::Deny(reason),
                Decision::Ask => needs_ask = true,
            }
        }

        if needs_ask {
            Decision::Ask
        } else {
            Decision::Allow
        }
    }
}

/// Load a policy from a YAML string.
pub fn load_policy_from_yaml(yaml: &str) -> Result<Policy, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

/// Load a policy from a YAML file.
pub fn load_policy_from_file(path: &std::path::Path) -> Result<Policy, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let policy = load_policy_from_yaml(&content)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::policy::{PolicyAction, PolicyRule};
    use kernel_interfaces::tool::{ToolError, ToolOutput};
    use kernel_interfaces::types::{Capability, CapabilitySet, RelevanceSignal, TokenEstimate};

    /// A minimal test tool that declares specific capabilities.
    struct FakeTool {
        name: &'static str,
        capabilities: CapabilitySet,
        relevance: RelevanceSignal,
    }

    impl FakeTool {
        fn with_caps(name: &'static str, caps: &[&str]) -> Self {
            Self {
                name,
                capabilities: caps.iter().map(|c| Capability::new(*c)).collect(),
                relevance: RelevanceSignal {
                    keywords: Vec::new(),
                    tags: Vec::new(),
                },
            }
        }

        fn internal(name: &'static str) -> Self {
            Self {
                name,
                capabilities: CapabilitySet::new(),
                relevance: RelevanceSignal {
                    keywords: Vec::new(),
                    tags: Vec::new(),
                },
            }
        }
    }

    impl ToolRegistration for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn capabilities(&self) -> &CapabilitySet {
            &self.capabilities
        }
        fn schema(&self) -> &serde_json::Value {
            &serde_json::Value::Null
        }
        fn cost(&self) -> TokenEstimate {
            TokenEstimate(0)
        }
        fn relevance(&self) -> &RelevanceSignal {
            &self.relevance
        }
        fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::readonly(serde_json::Value::Null))
        }
    }

    fn permissive_policy() -> Policy {
        Policy {
            version: 1,
            name: "permissive".into(),
            rules: vec![
                PolicyRule {
                    match_capabilities: vec!["fs:read".into()],
                    action: PolicyAction::Allow,
                    scope_paths: Vec::new(),
                    scope_commands: Vec::new(),
                    except: Vec::new(),
                },
                PolicyRule {
                    match_capabilities: vec!["fs:write".into()],
                    action: PolicyAction::Allow,
                    scope_paths: Vec::new(),
                    scope_commands: Vec::new(),
                    except: Vec::new(),
                },
                PolicyRule {
                    match_capabilities: vec!["shell:exec".into()],
                    action: PolicyAction::Ask,
                    scope_paths: Vec::new(),
                    scope_commands: Vec::new(),
                    except: Vec::new(),
                },
                PolicyRule {
                    match_capabilities: vec!["net:*".into()],
                    action: PolicyAction::Deny,
                    scope_paths: Vec::new(),
                    scope_commands: Vec::new(),
                    except: Vec::new(),
                },
            ],
            resource_budgets: None,
        }
    }

    #[test]
    fn kernel_internal_tools_always_allowed() {
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::internal("request_tool");
        assert_eq!(evaluator.evaluate(&tool), Decision::Allow);
    }

    #[test]
    fn read_only_tool_allowed() {
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("file_read", &["fs:read"]);
        assert_eq!(evaluator.evaluate(&tool), Decision::Allow);
    }

    #[test]
    fn write_tool_allowed() {
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("file_write", &["fs:write"]);
        assert_eq!(evaluator.evaluate(&tool), Decision::Allow);
    }

    #[test]
    fn shell_tool_asks() {
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("shell", &["shell:exec"]);
        assert_eq!(evaluator.evaluate(&tool), Decision::Ask);
    }

    #[test]
    fn network_tool_denied() {
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("web_fetch", &["net:api.github.com"]);
        assert!(matches!(evaluator.evaluate(&tool), Decision::Deny(_)));
    }

    #[test]
    fn multi_cap_tool_deny_wins_over_ask() {
        // A tool that needs both shell:exec (ask) and net:* (deny) should be denied
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("risky_tool", &["shell:exec", "net:api.example.com"]);
        assert!(matches!(evaluator.evaluate(&tool), Decision::Deny(_)));
    }

    #[test]
    fn multi_cap_tool_ask_if_no_deny() {
        // A tool that needs fs:read (allow) and shell:exec (ask) should ask
        let evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("git", &["fs:read", "shell:exec"]);
        assert_eq!(evaluator.evaluate(&tool), Decision::Ask);
    }

    #[test]
    fn policy_swap_changes_behavior() {
        let mut evaluator = PermissionEvaluator::new(permissive_policy());
        let tool = FakeTool::with_caps("shell", &["shell:exec"]);

        // Initially asks
        assert_eq!(evaluator.evaluate(&tool), Decision::Ask);

        // Swap to a policy that allows shell
        let mut new_policy = permissive_policy();
        new_policy.rules[2].action = PolicyAction::Allow;
        evaluator.set_policy(new_policy);

        // Now allowed
        assert_eq!(evaluator.evaluate(&tool), Decision::Allow);
    }

    #[test]
    fn load_policy_from_yaml_roundtrip() {
        let yaml = r#"
version: 1
name: test-policy
rules:
  - match:
      - "fs:read"
    action: allow
  - match:
      - "net:*"
    action: deny
"#;
        let policy = load_policy_from_yaml(yaml).expect("should parse");
        assert_eq!(policy.name, "test-policy");
        assert_eq!(policy.rules.len(), 2);
    }
}

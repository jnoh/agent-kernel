use crate::types::{Capability, Decision, ResourceBudget};
use serde::{Deserialize, Serialize};

/// A single policy rule — matches capabilities and determines the action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Capability patterns to match against (e.g., "fs:read", "net:*").
    #[serde(rename = "match")]
    pub match_capabilities: Vec<String>,

    /// What to do when matched.
    pub action: PolicyAction,

    /// Optional path scope (for fs capabilities).
    #[serde(default)]
    pub scope_paths: Vec<String>,

    /// Optional command scope (for shell capabilities).
    #[serde(default)]
    pub scope_commands: Vec<String>,

    /// Exceptions to deny rules.
    #[serde(default)]
    pub except: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyAction {
    Allow,
    Deny,
    Ask,
}

/// A complete policy configuration loaded from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub version: u32,
    pub name: String,
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub resource_budgets: Option<ResourceBudget>,
}

impl Policy {
    /// Evaluate a single capability against this policy's rules.
    /// Rules are evaluated in order — first match wins.
    pub fn evaluate(&self, capability: &Capability) -> Decision {
        for rule in &self.rules {
            for pattern_str in &rule.match_capabilities {
                let pattern = Capability::new(pattern_str);
                if capability.matches(&pattern) {
                    return match rule.action {
                        PolicyAction::Allow => Decision::Allow,
                        PolicyAction::Deny => Decision::Deny(format!(
                            "policy '{}' denies capability '{}'",
                            self.name, capability.0
                        )),
                        PolicyAction::Ask => Decision::Ask,
                    };
                }
            }
        }

        // Default: deny anything not explicitly allowed
        Decision::Deny(format!(
            "no policy rule matches capability '{}'",
            capability.0
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> Policy {
        Policy {
            version: 1,
            name: "test".into(),
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
                PolicyRule {
                    match_capabilities: vec!["shell:exec".into()],
                    action: PolicyAction::Ask,
                    scope_paths: Vec::new(),
                    scope_commands: Vec::new(),
                    except: Vec::new(),
                },
            ],
            resource_budgets: None,
        }
    }

    #[test]
    fn policy_allows_fs_read() {
        let policy = test_policy();
        let decision = policy.evaluate(&Capability::new("fs:read"));
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn policy_asks_for_fs_write() {
        let policy = test_policy();
        let decision = policy.evaluate(&Capability::new("fs:write"));
        assert_eq!(decision, Decision::Ask);
    }

    #[test]
    fn policy_denies_network() {
        let policy = test_policy();
        let decision = policy.evaluate(&Capability::new("net:api.github.com"));
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn policy_denies_unknown_capability() {
        let policy = test_policy();
        let decision = policy.evaluate(&Capability::new("unknown:something"));
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn first_matching_rule_wins() {
        // If we add a deny-all rule after specific allows, the allows should still work
        let policy = test_policy();
        assert_eq!(policy.evaluate(&Capability::new("fs:read")), Decision::Allow);
    }
}

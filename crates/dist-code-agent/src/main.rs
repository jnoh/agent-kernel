mod frontend;
mod provider;
mod tools;

use kernel_core::context::ContextConfig;
use kernel_core::permission::load_policy_from_file;
use kernel_core::session::{SessionConfig, SessionManager};

use kernel_interfaces::types::{CompletionConfig, ResourceBudget, SessionMode};

use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

fn main() {
    let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Load policy — default to permissive, override with --policy flag
    let policy_path = env::args()
        .position(|a| a == "--policy")
        .and_then(|i| env::args().nth(i + 1))
        .map(PathBuf::from);

    let policy = if let Some(ref path) = policy_path {
        match load_policy_from_file(path) {
            Ok(p) => {
                eprintln!("Loaded policy: {} (from {})", p.name, path.display());
                p
            }
            Err(e) => {
                eprintln!("Error loading policy from {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    } else {
        // Try to find a default policy file
        let default_paths = [
            workspace.join("policies/permissive.yaml"),
            workspace.join("policy.yaml"),
        ];
        default_paths
            .iter()
            .find_map(|p| load_policy_from_file(p).ok())
            .unwrap_or_else(|| {
                // Fallback: inline permissive policy
                kernel_interfaces::policy::Policy {
                    version: 1,
                    name: "default-permissive".into(),
                    rules: vec![
                        kernel_interfaces::policy::PolicyRule {
                            match_capabilities: vec!["fs:read".into(), "fs:write".into()],
                            action: kernel_interfaces::policy::PolicyAction::Allow,
                            scope_paths: Vec::new(),
                            scope_commands: Vec::new(),
                            except: Vec::new(),
                        },
                        kernel_interfaces::policy::PolicyRule {
                            match_capabilities: vec!["shell:exec".into(), "net:*".into()],
                            action: kernel_interfaces::policy::PolicyAction::Ask,
                            scope_paths: Vec::new(),
                            scope_commands: Vec::new(),
                            except: Vec::new(),
                        },
                    ],
                    resource_budgets: None,
                }
            })
    };

    let resource_budget = policy
        .resource_budgets
        .clone()
        .unwrap_or_default();

    // Create session
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let tools = tools::create_tools(&workspace);

    let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    eprintln!("agent-kernel v0.1.0 — code-agent distribution");
    eprintln!("Workspace: {}", workspace.display());
    eprintln!("Policy: {}", policy.name);
    eprintln!("Tools: {}", tool_names.join(", "));
    eprintln!("---");

    let session_config = SessionConfig {
        mode: SessionMode::Interactive,
        system_prompt: format!(
            "You are a coding assistant. You have access to the following tools: {}. \
             The workspace root is {}. \
             Use tools to help the user with their coding tasks. \
             Be concise and direct.",
            tool_names.join(", "),
            workspace.display()
        ),
        context_config: ContextConfig::default(),
        completion_config: CompletionConfig::default(),
        policy,
        resource_budget,
        workspace: workspace.clone(),
    };

    let session_id = mgr.spawn_interactive(session_config, tools);
    let session = mgr.get_mut(session_id).unwrap();

    let provider = provider::EchoProvider;
    let fe = frontend::ReplFrontend;

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    loop {
        eprint!("> ");
        io::stderr().flush().ok();

        let mut input = String::new();
        match reader.read_line(&mut input) {
            Ok(0) => break, // EOF
            Err(e) => {
                eprintln!("Error reading input: {e}");
                break;
            }
            Ok(_) => {}
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" || input == "/exit" {
            break;
        }

        session.add_user_input(input.to_string());

        // Agent loop: keep running turns until the model stops making tool calls
        loop {
            match session.run_turn(&provider, &fe) {
                Ok(result) => {
                    if !result.continues {
                        break;
                    }
                    // Model made tool calls — continue the loop
                }
                Err(e) => {
                    eprintln!("Turn error: {e}");
                    break;
                }
            }
        }
    }

    eprintln!("\nGoodbye.");
}

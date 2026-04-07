//! Pluggable storage backend for conversation turns.
//!
//! The `ContextStore` trait decouples the `ContextManager` from its storage
//! strategy. The default `InMemoryContextStore` wraps a `Vec<Turn>` (current
//! behavior). Future implementations can persist to SQLite, Postgres, etc.
//! for long-running and crash-recoverable agents.

use crate::context::Turn;

/// Storage backend for conversation turns.
pub trait ContextStore: Send {
    /// Append a new turn.
    fn append_turn(&mut self, turn: Turn);

    /// Number of stored turns.
    fn turn_count(&self) -> usize;

    /// Read-only access to all turns.
    fn turns(&self) -> &[Turn];

    /// Mutable access to all turns (needed for compaction).
    fn turns_mut(&mut self) -> &mut [Turn];

    /// Access the last turn mutably (for appending responses/tool exchanges).
    fn last_turn_mut(&mut self) -> Option<&mut Turn>;
}

/// In-memory turn storage — wraps a `Vec<Turn>`.
/// This is the default and matches the pre-refactor behavior.
pub struct InMemoryContextStore {
    turns: Vec<Turn>,
}

impl InMemoryContextStore {
    pub fn new() -> Self {
        Self { turns: Vec::new() }
    }
}

impl Default for InMemoryContextStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextStore for InMemoryContextStore {
    fn append_turn(&mut self, turn: Turn) {
        self.turns.push(turn);
    }

    fn turn_count(&self) -> usize {
        self.turns.len()
    }

    fn turns(&self) -> &[Turn] {
        &self.turns
    }

    fn turns_mut(&mut self) -> &mut [Turn] {
        &mut self.turns
    }

    fn last_turn_mut(&mut self) -> Option<&mut Turn> {
        self.turns.last_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::types::{Content, Message, Role};

    fn sample_turn(text: &str) -> Turn {
        Turn {
            input: Message {
                role: Role::User,
                content: vec![Content::Text(text.into())],
            },
            response: None,
            tool_exchanges: Vec::new(),
            token_estimate: text.len() / 4,
            summarized: false,
        }
    }

    #[test]
    fn in_memory_store_basic_operations() {
        let mut store = InMemoryContextStore::new();
        assert_eq!(store.turn_count(), 0);

        store.append_turn(sample_turn("Hello"));
        assert_eq!(store.turn_count(), 1);

        store.append_turn(sample_turn("World"));
        assert_eq!(store.turn_count(), 2);

        assert_eq!(store.turns().len(), 2);

        // Mutable access
        let last = store.last_turn_mut().unwrap();
        last.token_estimate = 999;
        assert_eq!(store.turns()[1].token_estimate, 999);
    }

    #[test]
    fn in_memory_store_mutable_slice() {
        let mut store = InMemoryContextStore::new();
        for i in 0..5 {
            store.append_turn(sample_turn(&format!("Turn {}", i)));
        }

        // Mark first 3 as summarized via turns_mut
        for turn in &mut store.turns_mut()[..3] {
            turn.summarized = true;
        }

        assert!(store.turns()[0].summarized);
        assert!(store.turns()[2].summarized);
        assert!(!store.turns()[3].summarized);
    }
}

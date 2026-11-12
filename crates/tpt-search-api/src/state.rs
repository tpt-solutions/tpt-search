//! Shared server state: the indexes and the change feeds subscribers listen to.
//!
//! Locks are standard library locks, and none is held across an `.await`. A write takes
//! the registry's write lock for the duration of one index update, then publishes an
//! event to the index's subscribers after releasing it.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use serde_json::Value;
use tokio::sync::broadcast;
use tpt_search_query::IndexRegistry;

/// Events buffered per index for slow subscribers. A subscriber that falls further
/// behind misses the oldest events.
pub const EVENT_BUFFER: usize = 256;

/// Everything the HTTP handlers share.
#[derive(Default)]
pub struct AppState {
    /// The indexes.
    pub registry: RwLock<IndexRegistry>,
    events: Mutex<HashMap<String, broadcast::Sender<String>>>,
}

impl AppState {
    /// Creates an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// A new subscription to an index's change feed. Events published before this call
    /// are not delivered.
    pub fn subscribe(&self, index: &str) -> broadcast::Receiver<String> {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events
            .entry(index.to_owned())
            .or_insert_with(|| broadcast::channel(EVENT_BUFFER).0)
            .subscribe()
    }

    /// Sends an event to the index's subscribers, if there are any.
    pub fn publish(&self, index: &str, event: &Value) {
        let sender = {
            let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            events.get(index).cloned()
        };
        if let Some(sender) = sender {
            // No receivers is fine: nobody is listening.
            let _ = sender.send(event.to_string());
        }
    }
}

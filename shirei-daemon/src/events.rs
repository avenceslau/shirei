use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShireiEvent {
    pub event_id: String,
    pub ts: String,
    pub project_id: String,
    pub subject_id: String, // job_id or prwatch:<id>
    pub r#type: String,
    pub payload: serde_json::Value,
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ShireiEvent>,
}

impl EventBus {
    pub fn new(cap: usize) -> Self {
        let (tx, _) = broadcast::channel(cap);
        Self { tx }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<ShireiEvent> {
        self.tx.subscribe()
    }
    pub fn publish(&self, evt: ShireiEvent) {
        let _ = self.tx.send(evt);
    }
}

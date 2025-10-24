use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct HealthSnapshot {
    pub last_cex_event_ms: Option<u128>,
    pub last_deepbook_event_ms: Option<u128>,
}

#[derive(Default)]
pub struct HealthMetrics {
    snapshot: Mutex<HealthSnapshot>,
}

impl HealthMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(HealthSnapshot::default()),
        })
    }

    pub async fn note_cex_event(&self, ts: SystemTime) {
        let mut guard = self.snapshot.lock().await;
        guard.last_cex_event_ms = Some(to_millis(ts));
    }

    pub async fn note_deepbook_event(&self, ts: SystemTime) {
        let mut guard = self.snapshot.lock().await;
        guard.last_deepbook_event_ms = Some(to_millis(ts));
    }

    pub async fn snapshot(&self) -> HealthSnapshot {
        self.snapshot.lock().await.clone()
    }
}

fn to_millis(ts: SystemTime) -> u128 {
    ts.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

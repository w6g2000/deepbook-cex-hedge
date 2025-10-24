use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Debug, Clone, PartialEq)]
pub enum RiskAlert {
    PriceSpike {
        pair: String,
        diff_bps: f64,
        threshold_bps: f64,
    },
    DepthInsufficient {
        pair: String,
        required_depth: f64,
    },
    BorrowHealth {
        borrow_ratio: f64,
        threshold: f64,
    },
}

#[async_trait::async_trait]
pub trait RiskNotifier: Send + Sync {
    async fn notify(&self, alert: RiskAlert);
}

#[derive(Default, Clone)]
pub struct LoggerNotifier;

#[async_trait::async_trait]
impl RiskNotifier for LoggerNotifier {
    async fn notify(&self, alert: RiskAlert) {
        match &alert {
            RiskAlert::PriceSpike {
                pair,
                diff_bps,
                threshold_bps,
            } => {
                warn!(
                    pair = %pair,
                    diff_bps,
                    threshold_bps,
                    "risk alert: price spike detected"
                );
            }
            RiskAlert::DepthInsufficient {
                pair,
                required_depth,
            } => {
                warn!(
                    pair = %pair,
                    required_depth,
                    "risk alert: insufficient deepbook depth"
                );
            }
            RiskAlert::BorrowHealth {
                borrow_ratio,
                threshold,
            } => {
                warn!(
                    borrow_ratio,
                    threshold, "risk alert: borrow health exceeded threshold"
                );
            }
        }
    }
}

#[derive(Clone)]
pub struct RiskManager<N: RiskNotifier> {
    notifier: Arc<N>,
}

impl<N: RiskNotifier> RiskManager<N> {
    pub fn new(notifier: N) -> Self {
        Self {
            notifier: Arc::new(notifier),
        }
    }

    pub async fn price_spike(&self, pair: &str, diff_bps: f64, threshold_bps: f64) {
        let alert = RiskAlert::PriceSpike {
            pair: pair.to_string(),
            diff_bps,
            threshold_bps,
        };
        self.notifier.notify(alert).await;
    }

    pub async fn depth_insufficient(&self, pair: &str, required_depth: f64) {
        let alert = RiskAlert::DepthInsufficient {
            pair: pair.to_string(),
            required_depth,
        };
        self.notifier.notify(alert).await;
    }

    pub async fn borrow_health(&self, borrow_ratio: f64, threshold: f64) {
        let alert = RiskAlert::BorrowHealth {
            borrow_ratio,
            threshold,
        };
        self.notifier.notify(alert).await;
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub struct MemoryNotifier {
    pub alerts: Arc<Mutex<Vec<RiskAlert>>>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl RiskNotifier for MemoryNotifier {
    async fn notify(&self, alert: RiskAlert) {
        let mut guard = self.alerts.lock().await;
        guard.push(alert);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_notifier_records_alerts() {
        let notifier = MemoryNotifier::default();
        let manager = RiskManager::new(notifier.clone());
        manager.price_spike("WAL/USDC", 150.0, 100.0).await;
        manager.depth_insufficient("WAL/USDC", 500.0).await;
        manager.borrow_health(0.65, 0.6).await;

        let guard = notifier.alerts.lock().await;
        assert_eq!(guard.len(), 3);
        assert!(matches!(guard[0], RiskAlert::PriceSpike { .. }));
        assert!(matches!(guard[1], RiskAlert::DepthInsufficient { .. }));
        assert!(matches!(guard[2], RiskAlert::BorrowHealth { .. }));
    }
}

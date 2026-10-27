//! Fail-closed admission and cancellation of all previously admitted sessions.
use crate::checker::{IpProbe, Observation};
use anyhow::{Result, bail};
use std::{net::IpAddr, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct Gate {
    pinned: IpAddr,
    probe: Arc<dyn IpProbe>,
    root: CancellationToken,
    state: Mutex<CancellationToken>,
}

impl Gate {
    pub fn new(pinned: IpAddr, probe: Arc<dyn IpProbe>, root: CancellationToken) -> Self {
        Self {
            pinned,
            probe,
            state: Mutex::new(root.child_token()),
            root,
        }
    }

    /// Verification is serialized; no stale positive cache can admit a request.
    pub async fn admit(&self) -> Result<(Observation, CancellationToken)> {
        let mut epoch = self.state.lock().await;
        let result = tokio::select! {
            biased;
            _ = self.root.cancelled() => bail!("gate is shutting down"),
            result = self.probe.fetch() => result,
        };
        match result {
            Ok(observation) if observation.ip == self.pinned => {
                Ok((observation, epoch.child_token()))
            }
            other => {
                epoch.cancel();
                *epoch = self.root.child_token();
                match other {
                    Ok(observation) => bail!(
                        "external IP mismatch: {} != {}",
                        observation.ip,
                        self.pinned
                    ),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checker::ProbeFuture;
    use std::sync::atomic::{AtomicU8, Ordering};

    struct Probe(AtomicU8);
    impl IpProbe for Probe {
        fn fetch(&self) -> ProbeFuture<'_> {
            Box::pin(async {
                match self.0.load(Ordering::SeqCst) {
                    0 => Ok(Observation {
                        ip: "203.0.113.1".parse().unwrap(),
                        country: "NL".into(),
                    }),
                    1 => Ok(Observation {
                        ip: "203.0.113.2".parse().unwrap(),
                        country: "NL".into(),
                    }),
                    _ => bail!("trace service unavailable"),
                }
            })
        }
    }

    #[tokio::test]
    async fn mismatch_or_probe_failure_cancels_every_session_and_can_recover() {
        let probe = Arc::new(Probe(AtomicU8::new(0)));
        let gate = Gate::new(
            "203.0.113.1".parse().unwrap(),
            probe.clone(),
            CancellationToken::new(),
        );
        let (_, a) = gate.admit().await.unwrap();
        let (_, b) = gate.admit().await.unwrap();
        probe.0.store(1, Ordering::SeqCst);
        assert!(gate.admit().await.is_err());
        assert!(a.is_cancelled() && b.is_cancelled());
        probe.0.store(0, Ordering::SeqCst);
        let (_, c) = gate.admit().await.unwrap();
        assert!(!c.is_cancelled());
        probe.0.store(2, Ordering::SeqCst);
        assert!(gate.admit().await.is_err());
        assert!(c.is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_cannot_admit_new_requests() {
        let root = CancellationToken::new();
        root.cancel();
        let gate = Gate::new(
            "203.0.113.1".parse().unwrap(),
            Arc::new(Probe(AtomicU8::new(0))),
            root,
        );
        assert!(gate.admit().await.is_err());
    }
}

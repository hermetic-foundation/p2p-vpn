use std::time::Duration;

use tokio::sync::watch;

pub(crate) const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(5 * 60);

const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Default)]
pub(crate) struct RecoveryBackoff {
    consecutive_failures: u32,
}

impl RecoveryBackoff {
    pub(crate) fn record_failure(&mut self, network_id: &str, healthy_for: Duration) -> Duration {
        if healthy_for >= HEALTHY_RESET_AFTER {
            self.consecutive_failures = 0;
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        equal_jitter_delay(network_id, self.consecutive_failures)
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

pub(crate) async fn wait_or_shutdown(
    delay: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    if *shutdown.borrow() {
        return false;
    }
    tokio::select! {
        () = tokio::time::sleep(delay) => true,
        changed = shutdown.changed() => changed.is_ok() && !*shutdown.borrow(),
    }
}

fn equal_jitter_delay(network_id: &str, consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(31);
    let upper_millis = BASE_DELAY
        .as_millis()
        .saturating_mul(1_u128 << exponent)
        .min(MAX_DELAY.as_millis());
    let lower_millis = upper_millis / 2;
    let jitter_range = upper_millis.saturating_sub(lower_millis);
    let sample = u128::from(stable_sample(network_id, consecutive_failures));
    let jitter = sample % jitter_range.saturating_add(1);
    Duration::from_millis(u64::try_from(lower_millis.saturating_add(jitter)).unwrap_or(u64::MAX))
}

fn stable_sample(network_id: &str, consecutive_failures: u32) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325_u64;
    for byte in network_id.bytes().chain(consecutive_failures.to_le_bytes()) {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    state
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn backoff_is_deterministic_bounded_and_capped() {
        let mut alpha = RecoveryBackoff::default();
        let mut replay = RecoveryBackoff::default();
        for failure in 1..=32 {
            let delay = alpha.record_failure("alpha", Duration::ZERO);
            assert_eq!(
                delay,
                replay.record_failure("alpha", Duration::ZERO),
                "failure {failure}"
            );
            let exponent = (failure - 1).min(31);
            let upper = BASE_DELAY
                .as_millis()
                .saturating_mul(1_u128 << exponent)
                .min(MAX_DELAY.as_millis());
            assert!(delay.as_millis() >= upper / 2, "failure {failure}");
            assert!(delay.as_millis() <= upper, "failure {failure}");
        }
        assert!(
            alpha.record_failure("alpha", Duration::ZERO) <= MAX_DELAY,
            "capped retries must not exceed the maximum"
        );
    }

    #[test]
    fn healthy_runtime_resets_the_failure_sequence() {
        let mut backoff = RecoveryBackoff::default();
        backoff.record_failure("alpha", Duration::ZERO);
        backoff.record_failure("alpha", Duration::ZERO);
        assert_eq!(backoff.consecutive_failures(), 2);

        let reset_delay = backoff.record_failure("alpha", HEALTHY_RESET_AFTER);

        assert_eq!(backoff.consecutive_failures(), 1);
        assert!(reset_delay >= BASE_DELAY / 2);
        assert!(reset_delay <= BASE_DELAY);
    }

    #[test]
    fn shutdown_interrupts_recovery_wait() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (shutdown, mut receiver) = watch::channel(false);
            let started = Instant::now();
            let waiter = tokio::spawn(async move {
                wait_or_shutdown(Duration::from_secs(60), &mut receiver).await
            });
            tokio::task::yield_now().await;
            shutdown.send(true).expect("shutdown signal");

            assert!(!waiter.await.expect("waiter"));
            assert!(started.elapsed() < Duration::from_secs(1));
        });
    }
}

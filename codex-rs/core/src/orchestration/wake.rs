use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakePriority {
    Normal,
    High,
    Urgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeEnvelope {
    pub(crate) wake_id: String,
    pub(crate) created_at: Instant,
    pub(crate) ttl: Duration,
    pub(crate) priority: WakePriority,
    pub(crate) ack_required: bool,
    pub(crate) max_attempts: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeDelivery {
    DeliverNow,
    Duplicate,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeResolution {
    Acked,
    Retrying,
    Escalated,
    Unknown,
}

#[derive(Debug, Clone)]
struct WakeState {
    expires_at: Instant,
    ack_required: bool,
    attempts: u8,
    max_attempts: u8,
}

#[derive(Debug, Default)]
pub(crate) struct WakeCoordinator {
    wakes: HashMap<String, WakeState>,
}

impl WakeCoordinator {
    pub(crate) fn enqueue(&mut self, envelope: WakeEnvelope, now: Instant) -> WakeDelivery {
        if envelope.wake_id.trim().is_empty() {
            return WakeDelivery::Expired;
        }
        if now.duration_since(envelope.created_at) >= envelope.ttl {
            return WakeDelivery::Expired;
        }
        if self.wakes.contains_key(&envelope.wake_id) {
            return WakeDelivery::Duplicate;
        }

        self.wakes.insert(
            envelope.wake_id,
            WakeState {
                expires_at: envelope.created_at + envelope.ttl,
                ack_required: envelope.ack_required,
                attempts: 1,
                max_attempts: envelope.max_attempts.max(1),
            },
        );
        WakeDelivery::DeliverNow
    }

    pub(crate) fn ack(&mut self, wake_id: &str) -> WakeResolution {
        if self.wakes.remove(wake_id).is_some() {
            WakeResolution::Acked
        } else {
            WakeResolution::Unknown
        }
    }

    pub(crate) fn nack(&mut self, wake_id: &str) -> WakeResolution {
        let Some(state) = self.wakes.get_mut(wake_id) else {
            return WakeResolution::Unknown;
        };

        if state.attempts >= state.max_attempts {
            self.wakes.remove(wake_id);
            WakeResolution::Escalated
        } else {
            state.attempts += 1;
            WakeResolution::Retrying
        }
    }

    pub(crate) fn poll_timeouts(&mut self, now: Instant) -> Vec<String> {
        let expired_ids = self
            .wakes
            .iter()
            .filter_map(|(wake_id, state)| (now >= state.expires_at).then_some(wake_id.clone()))
            .collect::<Vec<_>>();

        let mut escalated = Vec::new();
        for wake_id in &expired_ids {
            if let Some(state) = self.wakes.remove(wake_id)
                && state.ack_required
            {
                escalated.push(wake_id.clone());
            }
        }

        escalated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wake(wake_id: &str, created_at: Instant) -> WakeEnvelope {
        WakeEnvelope {
            wake_id: wake_id.to_string(),
            created_at,
            ttl: Duration::from_secs(30),
            priority: WakePriority::High,
            ack_required: true,
            max_attempts: 2,
        }
    }

    fn wake_without_ack(wake_id: &str, created_at: Instant) -> WakeEnvelope {
        WakeEnvelope {
            wake_id: wake_id.to_string(),
            created_at,
            ttl: Duration::from_secs(1),
            priority: WakePriority::Normal,
            ack_required: false,
            max_attempts: 1,
        }
    }

    #[test]
    fn wake_signal_delivery_and_dedupe() {
        let mut coordinator = WakeCoordinator::default();
        let now = Instant::now();

        assert_eq!(
            coordinator.enqueue(wake("wake-1", now), now),
            WakeDelivery::DeliverNow
        );
        assert_eq!(
            coordinator.enqueue(wake("wake-1", now), now),
            WakeDelivery::Duplicate
        );

        let expired_now = now + Duration::from_secs(31);
        assert_eq!(
            coordinator.enqueue(wake("wake-2", now), expired_now),
            WakeDelivery::Expired
        );

        assert_eq!(
            coordinator.enqueue(wake_without_ack("wake-no-ack", now), now),
            WakeDelivery::DeliverNow
        );
        let timed_out = coordinator.poll_timeouts(now + Duration::from_secs(2));
        assert!(
            timed_out.is_empty(),
            "ack-optional timeouts should evict state without escalation"
        );
        assert_eq!(
            coordinator.enqueue(wake_without_ack("wake-no-ack", now), now),
            WakeDelivery::DeliverNow
        );
    }

    #[test]
    fn wake_ack_nack_timeout_escalation() {
        let mut coordinator = WakeCoordinator::default();
        let now = Instant::now();
        assert_eq!(
            coordinator.enqueue(wake("wake-ack", now), now),
            WakeDelivery::DeliverNow
        );

        assert_eq!(coordinator.nack("wake-ack"), WakeResolution::Retrying);
        assert_eq!(coordinator.nack("wake-ack"), WakeResolution::Escalated);
        assert_eq!(coordinator.nack("wake-ack"), WakeResolution::Unknown);

        assert_eq!(
            coordinator.enqueue(wake("wake-acked", now), now),
            WakeDelivery::DeliverNow
        );
        assert_eq!(coordinator.ack("wake-acked"), WakeResolution::Acked);
        assert_eq!(coordinator.ack("wake-acked"), WakeResolution::Unknown);

        assert_eq!(
            coordinator.enqueue(wake("wake-timeout", now), now),
            WakeDelivery::DeliverNow
        );
        let escalated = coordinator.poll_timeouts(now + Duration::from_secs(31));
        assert!(escalated.contains(&"wake-timeout".to_string()));
    }
}

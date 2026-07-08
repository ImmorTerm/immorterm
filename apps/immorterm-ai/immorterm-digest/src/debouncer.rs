//! Per-session debounce state machine.
//!
//! Three trigger sources per v4 §7 #1:
//! 1. **BurstQuiet** — N bytes/messages of activity, then 2-min silence.
//! 2. **Milestone** — externally signaled (test_pass, git_commit, …); fires now if pending.
//! 3. **Fallback15m** — 15 minutes since last digest with pending content.
//!
//! Adaptive idle-grace per v4 §9 Q4: `idle_grace = max(IDLE_GRACE_FLOOR,
//! 2 × p95_inter_turn_gap)` — not implemented here; the floor lives in
//! the orchestrator. Debouncer just receives the configured quiet_period.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    BurstQuiet,
    Milestone,
    Fallback15m,
}

impl Trigger {
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::BurstQuiet => "burst_quiet",
            Self::Milestone => "milestone",
            Self::Fallback15m => "fallback_15m",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DebouncerConfig {
    pub quiet_period: Duration,
    pub min_burst_units: u64, // bytes for JSONL; messages for RewriteHash
    pub fallback_interval: Duration,
}

impl Default for DebouncerConfig {
    fn default() -> Self {
        Self {
            quiet_period: Duration::from_secs(120),
            min_burst_units: 500,
            fallback_interval: Duration::from_secs(900),
        }
    }
}

#[derive(Debug, Clone)]
enum State {
    Idle,
    Bursting { last_event_at: Instant },
}

#[derive(Debug)]
pub struct Debouncer {
    cfg: DebouncerConfig,
    state: State,
    last_digest_at: Instant,
    pending_units: u64,
}

impl Debouncer {
    /// `anchor` is "as-of when did we last digest." On daemon cold-start
    /// for a session with byte_offset > 0, pass Instant::now() so the
    /// fallback timer doesn't fire immediately.
    pub fn new(cfg: DebouncerConfig, anchor: Instant) -> Self {
        Self {
            cfg,
            state: State::Idle,
            last_digest_at: anchor,
            pending_units: 0,
        }
    }

    pub fn on_activity(&mut self, units_added: u64, now: Instant) {
        self.pending_units = self.pending_units.saturating_add(units_added);
        if units_added >= self.cfg.min_burst_units {
            self.state = State::Bursting { last_event_at: now };
        } else if let State::Bursting { last_event_at } = &mut self.state {
            *last_event_at = now;
        }
    }

    /// Caller signaled a milestone (e.g. grep'd test_pass in last 20 KB).
    /// Fires immediately if pending units > 0.
    pub fn on_milestone(&mut self, now: Instant) -> Option<Trigger> {
        if self.pending_units > 0 {
            self.mark_fired(now);
            Some(Trigger::Milestone)
        } else {
            None
        }
    }

    /// Periodic tick. Checks burst-quiet first, then fallback.
    pub fn tick(&mut self, now: Instant) -> Option<Trigger> {
        if let State::Bursting { last_event_at } = self.state
            && now.duration_since(last_event_at) >= self.cfg.quiet_period {
                self.mark_fired(now);
                return Some(Trigger::BurstQuiet);
            }
        if self.pending_units > 0
            && now.duration_since(self.last_digest_at) >= self.cfg.fallback_interval
        {
            self.mark_fired(now);
            return Some(Trigger::Fallback15m);
        }
        None
    }

    /// Mark a digest as run (manual override, e.g. recovery).
    pub fn mark_fired(&mut self, now: Instant) {
        self.state = State::Idle;
        self.pending_units = 0;
        self.last_digest_at = now;
    }

    pub fn pending(&self) -> u64 {
        self.pending_units
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DebouncerConfig {
        DebouncerConfig {
            quiet_period: Duration::from_secs(2),
            min_burst_units: 100,
            fallback_interval: Duration::from_secs(10),
        }
    }

    #[test]
    fn fresh_idle_no_trigger() {
        let mut d = Debouncer::new(cfg(), Instant::now());
        assert!(d.tick(Instant::now() + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn small_writes_dont_enter_bursting() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        d.on_activity(50, t0); // below threshold
        // Tick after long quiet: no burst trigger (never entered Bursting)
        // and below fallback window
        assert!(d.tick(t0 + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn burst_then_quiet_fires_burst_trigger() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        d.on_activity(200, t0);
        assert!(d.tick(t0 + Duration::from_secs(1)).is_none(), "still quiet");
        let trig = d.tick(t0 + Duration::from_secs(2)).unwrap();
        assert_eq!(trig, Trigger::BurstQuiet);
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn additional_writes_extend_quiet_window() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        d.on_activity(200, t0);
        d.on_activity(10, t0 + Duration::from_secs(1));
        // 2s after t0 — last event was t0+1, so only 1s of quiet
        assert!(d.tick(t0 + Duration::from_secs(2)).is_none());
        assert!(d.tick(t0 + Duration::from_secs(3)).is_some());
    }

    #[test]
    fn fallback_after_max_interval() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        d.on_activity(50, t0); // below burst threshold but creates pending
        let trig = d.tick(t0 + Duration::from_secs(11)).unwrap();
        assert_eq!(trig, Trigger::Fallback15m);
    }

    #[test]
    fn fallback_does_not_fire_without_pending() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        assert!(d.tick(t0 + Duration::from_secs(20)).is_none());
    }

    #[test]
    fn milestone_fires_with_pending() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        d.on_activity(50, t0);
        assert_eq!(d.on_milestone(t0 + Duration::from_secs(1)), Some(Trigger::Milestone));
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn milestone_no_op_without_pending() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(cfg(), t0);
        assert!(d.on_milestone(t0).is_none());
    }
}

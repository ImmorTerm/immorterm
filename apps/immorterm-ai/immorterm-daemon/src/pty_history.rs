//! Bounded raw-PTY event ring buffer.
//!
//! Records every byte fed through `terminal.process()` AND every resize event
//! the terminal received, in chronological order. This lets `replay_pty_history`
//! reproduce the live terminal's state faithfully — processing bytes at the
//! cols that were actually active when those bytes arrived, instead of forcing
//! the entire history through a single (current) cols value.
//!
//! Older-than-buffer content is still visible through the existing
//! scroll-to-top scrollback loader; the ring buffer only bounds replay
//! fidelity, not content retention.

use std::collections::VecDeque;

/// Default capacity: 4 MB of bytes (resize events are tiny and not counted).
/// At ~50 KB/s of typical CC output that's ~80s of history.
pub const DEFAULT_CAP_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum HistoryEvent {
    /// Raw PTY output bytes.
    Bytes(Vec<u8>),
    /// Terminal dimensions changed (SIGWINCH-equivalent).
    Resize { cols: u16, rows: u16 },
}

pub struct PtyHistory {
    events: VecDeque<HistoryEvent>,
    /// Total bytes stored across all Bytes events. Drives eviction.
    bytes_len: usize,
    cap_bytes: usize,
}

impl PtyHistory {
    pub fn new(cap_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes_len: 0,
            cap_bytes,
        }
    }

    /// Record a chunk of PTY output bytes at the current cols.
    pub fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // If a single write exceeds capacity, keep only its tail and reset.
        if data.len() >= self.cap_bytes {
            self.events.clear();
            self.bytes_len = 0;
            let tail = &data[data.len() - self.cap_bytes..];
            self.events.push_back(HistoryEvent::Bytes(tail.to_vec()));
            self.bytes_len = tail.len();
            return;
        }
        self.bytes_len += data.len();
        self.events.push_back(HistoryEvent::Bytes(data.to_vec()));
        self.evict_until_under_cap();
    }

    /// Record a resize event at its position in the byte stream.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        // Coalesce adjacent resizes (no bytes between → only the latest matters).
        if let Some(HistoryEvent::Resize { .. }) = self.events.back() {
            self.events.pop_back();
        }
        self.events.push_back(HistoryEvent::Resize { cols, rows });
    }

    fn evict_until_under_cap(&mut self) {
        while self.bytes_len > self.cap_bytes {
            match self.events.pop_front() {
                Some(HistoryEvent::Bytes(b)) => self.bytes_len -= b.len(),
                Some(HistoryEvent::Resize { .. }) => {} // free, doesn't help cap
                None => break,
            }
        }
        // Drop leading Resize events that are no longer anchored to any bytes
        // (they came before the oldest surviving Bytes — meaningless on replay).
        while matches!(self.events.front(), Some(HistoryEvent::Resize { .. }))
            && self.events.len() > 1
            && matches!(self.events.get(1), Some(HistoryEvent::Resize { .. }))
        {
            self.events.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.bytes_len
    }

    pub fn is_empty(&self) -> bool {
        self.bytes_len == 0
    }

    /// Iterator over events in chronological order. Used by replay.
    pub fn events(&self) -> impl Iterator<Item = &HistoryEvent> {
        self.events.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_total(h: &PtyHistory) -> Vec<u8> {
        h.events()
            .filter_map(|e| match e {
                HistoryEvent::Bytes(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn write_below_cap_appends() {
        let mut h = PtyHistory::new(100);
        h.write(b"hello");
        h.write(b" world");
        assert_eq!(bytes_total(&h), b"hello world");
    }

    #[test]
    fn write_over_cap_drops_oldest_bytes() {
        let mut h = PtyHistory::new(5);
        h.write(b"abcde");
        h.write(b"fgh");
        // Eviction drops the first 3 bytes of "abcde" event (whole event for simplicity).
        // bytes_len after drop is 3 ("fgh"), within cap.
        assert_eq!(bytes_total(&h), b"fgh");
    }

    #[test]
    fn single_write_bigger_than_cap_keeps_tail() {
        let mut h = PtyHistory::new(5);
        h.write(b"abcdefghij");
        assert_eq!(bytes_total(&h), b"fghij");
    }

    #[test]
    fn empty_write_is_noop() {
        let mut h = PtyHistory::new(10);
        h.write(b"abc");
        h.write(b"");
        assert_eq!(bytes_total(&h), b"abc");
    }

    #[test]
    fn resize_events_interleave_with_bytes() {
        let mut h = PtyHistory::new(100);
        h.write(b"abc");
        h.record_resize(80, 24);
        h.write(b"def");
        h.record_resize(120, 30);
        h.write(b"ghi");
        let kinds: Vec<&str> = h
            .events()
            .map(|e| match e {
                HistoryEvent::Bytes(_) => "b",
                HistoryEvent::Resize { .. } => "r",
            })
            .collect();
        assert_eq!(kinds, vec!["b", "r", "b", "r", "b"]);
    }

    #[test]
    fn adjacent_resizes_coalesce() {
        let mut h = PtyHistory::new(100);
        h.record_resize(80, 24);
        h.record_resize(100, 30);
        h.record_resize(120, 40);
        // Only the latest survives (no bytes between to anchor older ones).
        assert_eq!(h.events.len(), 1);
        if let HistoryEvent::Resize { cols, rows } = &h.events[0] {
            assert_eq!(*cols, 120);
            assert_eq!(*rows, 40);
        } else {
            panic!("expected Resize");
        }
    }
}

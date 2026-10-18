//! Round-robin API key rotation.
//!
//! Each request takes the next healthy key, which spreads load across keys.
//! A key that hits a rate limit rests for a while; a key the provider rejects
//! (invalid or revoked) is taken out until restart.

use serde::Serialize;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Slot {
    key: String,
    cooling_until: Option<Instant>,
    disabled: bool,
}

#[derive(Debug)]
struct Inner {
    slots: Vec<Slot>,
    cursor: usize,
}

#[derive(Debug)]
pub struct KeyPool {
    inner: Mutex<Inner>,
}

/// A key handed out for one request. `slot` identifies it when reporting back.
#[derive(Debug, Clone)]
pub struct Lease {
    pub slot: usize,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PoolStatus {
    pub total: usize,
    pub available: usize,
    pub cooling_down: usize,
    pub disabled: usize,
}

impl KeyPool {
    pub fn new(keys: impl IntoIterator<Item = String>) -> Self {
        let slots = keys
            .into_iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .map(|key| Slot { key, cooling_until: None, disabled: false })
            .collect();
        Self { inner: Mutex::new(Inner { slots, cursor: 0 }) }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Next usable key in rotation, or `None` if every key is resting or disabled.
    pub fn acquire(&self) -> Option<Lease> {
        let mut g = self.lock();
        let n = g.slots.len();
        let now = Instant::now();
        for _ in 0..n {
            let i = g.cursor % n;
            g.cursor = (i + 1) % n;
            let slot = &mut g.slots[i];
            if slot.disabled {
                continue;
            }
            if let Some(until) = slot.cooling_until {
                if until > now {
                    continue;
                }
                slot.cooling_until = None;
            }
            return Some(Lease { slot: i, key: slot.key.clone() });
        }
        None
    }

    pub fn cool_down(&self, slot: usize, duration: Duration) {
        if let Some(s) = self.lock().slots.get_mut(slot) {
            s.cooling_until = Some(Instant::now() + duration);
        }
    }

    pub fn disable(&self, slot: usize) {
        if let Some(s) = self.lock().slots.get_mut(slot) {
            s.disabled = true;
        }
    }

    /// How long until some key can be used again. `Some(0)` if one is ready
    /// now, `None` if every key is disabled.
    pub fn soonest_available(&self) -> Option<Duration> {
        let now = Instant::now();
        self.lock()
            .slots
            .iter()
            .filter(|s| !s.disabled)
            .map(|s| s.cooling_until.map_or(Duration::ZERO, |t| t.saturating_duration_since(now)))
            .min()
    }

    pub fn status(&self) -> PoolStatus {
        let now = Instant::now();
        let g = self.lock();
        let disabled = g.slots.iter().filter(|s| s.disabled).count();
        let cooling_down = g
            .slots
            .iter()
            .filter(|s| !s.disabled && s.cooling_until.is_some_and(|t| t > now))
            .count();
        PoolStatus {
            total: g.slots.len(),
            available: g.slots.len() - disabled - cooling_down,
            cooling_down,
            disabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(n: usize) -> KeyPool {
        KeyPool::new((0..n).map(|i| format!("k{i}")))
    }

    #[test]
    fn rotates_round_robin() {
        let p = pool(3);
        let keys: Vec<_> = (0..6).map(|_| p.acquire().unwrap().key).collect();
        assert_eq!(keys, ["k0", "k1", "k2", "k0", "k1", "k2"]);
    }

    #[test]
    fn skips_cooling_and_disabled_keys() {
        let p = pool(3);
        p.cool_down(0, Duration::from_secs(60));
        p.disable(1);
        assert_eq!(p.acquire().unwrap().key, "k2");
        assert_eq!(p.acquire().unwrap().key, "k2");
        assert_eq!(p.status(), PoolStatus { total: 3, available: 1, cooling_down: 1, disabled: 1 });
    }

    #[test]
    fn reports_exhaustion() {
        let p = pool(2);
        p.cool_down(0, Duration::from_secs(30));
        p.disable(1);
        assert!(p.acquire().is_none());
        let wait = p.soonest_available().unwrap();
        assert!(wait > Duration::from_secs(25));
        p.disable(0);
        assert!(p.soonest_available().is_none());
    }

    #[test]
    fn ignores_blank_keys() {
        let p = KeyPool::new(vec![" ".into(), "".into(), " a ".into()]);
        assert_eq!(p.len(), 1);
        assert_eq!(p.acquire().unwrap().key, "a");
    }
}

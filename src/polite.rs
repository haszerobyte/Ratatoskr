//! Being a good citizen: a minimum spacing between requests to one engine, a
//! circuit breaker that stops asking an engine that said no, and a small TTL
//! cache so repeated questions never leave the machine.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::engine::Hit;

/// At most one request per `interval` to one engine, plus up to half again at
/// random: requests exactly `interval` apart are a robot's rhythm. Callers
/// queue fairly.
pub struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<Instant>,
}

impl RateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self { interval, next: tokio::sync::Mutex::new(Instant::now()) }
    }

    /// Wait until this caller may send.
    pub async fn wait_turn(&self) {
        let mut next = self.next.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        let jitter = self.interval.mul_f64((crate::engine::browser::random_u64() % 1000) as f64 / 2000.0);
        *next = Instant::now() + self.interval + jitter;
    }

    /// How long a caller would wait now, without taking a turn: the meta-search
    /// sends a search to the engines free soonest.
    pub fn ready_in(&self) -> Duration {
        match self.next.try_lock() {
            Ok(next) => next.saturating_duration_since(Instant::now()),
            // Someone is waiting for a turn right now.
            Err(_) => self.interval,
        }
    }
}

/// Open after a refusal for `cooldown`; doubles (to 4x) on refusals in a row.
pub struct CircuitBreaker {
    cooldown: Duration,
    state: Mutex<(Option<Instant>, u32)>,
}

impl CircuitBreaker {
    pub fn new(cooldown: Duration) -> Self {
        Self { cooldown, state: Mutex::new((None, 0)) }
    }

    /// How much longer the engine is skipped, if it is.
    pub fn open_for(&self) -> Option<Duration> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0.and_then(|until| until.checked_duration_since(Instant::now())).filter(|d| !d.is_zero())
    }

    /// Open after a refusal. A refusal while already open is the same rest
    /// (concurrent searches refused at once), not one more in a row.
    pub fn trip(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.0.is_some_and(|until| until > Instant::now()) {
            return;
        }
        state.1 = (state.1 + 1).min(3);
        let factor = 1u32 << (state.1 - 1);
        state.0 = Some(Instant::now() + self.cooldown * factor);
    }

    pub fn record_success(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = (None, 0);
    }
}

/// One cached answer: when, from which engine, what.
type Entry<T> = (Instant, &'static str, T);
/// The entries by key, and their keys oldest first.
type Entries<T> = (HashMap<String, Entry<T>>, VecDeque<String>);

/// Search results by normalized query, expiring after `ttl`, oldest evicted
/// beyond `capacity`.
pub struct Cache<T = Vec<Hit>> {
    ttl: Duration,
    capacity: usize,
    entries: Mutex<Entries<T>>,
}

impl<T: Clone> Cache<T> {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self { ttl, capacity: capacity.max(1), entries: Mutex::new((HashMap::new(), VecDeque::new())) }
    }

    pub fn get(&self, key: &str) -> Option<(&'static str, T)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (stored, engine, hits) = entries.0.get(key)?;
        (stored.elapsed() < self.ttl).then(|| (*engine, hits.clone()))
    }

    pub fn put(&self, key: String, engine: &'static str, hits: T) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (map, order) = &mut *entries;
        if map.insert(key.clone(), (Instant::now(), engine, hits)).is_none() {
            order.push_back(key);
        }
        while order.len() > self.capacity {
            if let Some(oldest) = order.pop_front() {
                map.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limiter_spaces_requests() {
        let limiter = RateLimiter::new(Duration::from_millis(100));
        let started = Instant::now();
        for _ in 0..3 {
            limiter.wait_turn().await;
        }
        assert!(started.elapsed() >= Duration::from_millis(200), "{:?}", started.elapsed());
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "jitter is at most half again: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn breaker_opens_backs_off_and_resets() {
        let breaker = CircuitBreaker::new(Duration::from_millis(40));
        let expire = || std::thread::sleep(breaker.open_for().unwrap_or_default() + Duration::from_millis(2));
        assert_eq!(breaker.open_for(), None);
        breaker.trip();
        assert!(breaker.open_for().unwrap() > Duration::from_millis(30));
        expire();
        breaker.trip();
        assert!(breaker.open_for().unwrap() > Duration::from_millis(70), "a refusal after the rest doubles it");
        for _ in 0..2 {
            expire();
            breaker.trip();
        }
        assert!(breaker.open_for().unwrap() <= Duration::from_millis(160), "capped at 4x");
        breaker.record_success();
        assert_eq!(breaker.open_for(), None);
    }

    /// Regression: two searches refused at the same moment each tripped the
    /// breaker, doubling the rest twice for one refusal.
    #[test]
    fn refusals_while_resting_do_not_lengthen_the_rest() {
        let breaker = CircuitBreaker::new(Duration::from_secs(10));
        breaker.trip();
        breaker.trip();
        breaker.trip();
        assert!(breaker.open_for().unwrap() <= Duration::from_secs(10));
    }

    #[tokio::test]
    async fn the_wait_can_be_read_without_taking_a_turn() {
        let limiter = RateLimiter::new(Duration::from_millis(200));
        assert_eq!(limiter.ready_in(), Duration::ZERO);
        limiter.wait_turn().await;
        assert!(limiter.ready_in() > Duration::from_millis(150));
    }

    #[test]
    fn cache_expires_and_evicts_oldest() {
        let hit = |u: &str| vec![Hit { title: u.into(), url: u.into(), snippet: String::new() }];
        let cache = Cache::new(Duration::from_millis(50), 2);
        cache.put("a".into(), "e", hit("a"));
        cache.put("b".into(), "e", hit("b"));
        cache.put("c".into(), "e", hit("c"));
        assert!(cache.get("a").is_none(), "oldest evicted at capacity");
        assert_eq!(cache.get("c").unwrap().1[0].url, "c");
        std::thread::sleep(Duration::from_millis(60));
        assert!(cache.get("c").is_none(), "expired");
    }
}

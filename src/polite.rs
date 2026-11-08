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

/// How much one answer moves an engine's [`Health`]: recent answers count
/// most, and three bad ones in a row bring a healthy engine below an
/// untried one.
const HEALTH_WEIGHT: f64 = 0.3;

/// How an engine has been doing lately, as moving averages over its answers:
/// how often it gave results, how long it took, and how many it gave.
/// Searches ask the healthiest engines first, and as many as their usual
/// yield says a search needs.
pub struct Health(Mutex<HealthState>);

#[derive(Clone, Copy)]
struct HealthState {
    /// 1 for results, 0.5 for an empty answer (a niche query, not a fault), 0 for a failure.
    success: f64,
    seconds: f64,
    results: f64,
}

impl Default for Health {
    /// An untried engine: assumed to work, at an ordinary speed and yield.
    fn default() -> Self {
        Self(Mutex::new(HealthState { success: 0.8, seconds: 1.5, results: 10.0 }))
    }
}

impl Health {
    /// Record one answer: `Some(n)` results, or `None` for a failure.
    pub fn record(&self, results: Option<usize>, took: Duration) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let toward = |old: f64, new: f64| old + HEALTH_WEIGHT * (new - old);
        state.seconds = toward(state.seconds, took.as_secs_f64());
        match results {
            Some(0) => state.success = toward(state.success, 0.5),
            Some(n) => {
                state.success = toward(state.success, 1.0);
                state.results = toward(state.results, n as f64);
            }
            None => state.success = toward(state.success, 0.0),
        }
    }

    /// Higher is better: how often it answers, discounted by how slowly.
    pub fn score(&self) -> f64 {
        let state = *self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.success / (1.0 + state.seconds)
    }

    /// How many results an answer of its usually holds.
    pub fn usual_yield(&self) -> f64 {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).results
    }
}

/// `seats` in the order to ask them: those free now (or soonest) first, the
/// healthiest first among equally free ones. Each score is scaled by a
/// random 0.75 to 1.25, so engines of similar health share the searches and
/// none sees every query.
pub(crate) fn asking_order<S>(seats: Vec<S>, parts: impl Fn(&S) -> (&RateLimiter, &Health)) -> Vec<S> {
    let mut keyed: Vec<(Duration, f64, S)> = seats
        .into_iter()
        .map(|seat| {
            let (limiter, health) = parts(&seat);
            let spread = 0.75 + (crate::engine::browser::random_u64() % 1000) as f64 / 2000.0;
            let key = (limiter.ready_in(), health.score() * spread);
            (key.0, key.1, seat)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)));
    keyed.into_iter().map(|(_, _, seat)| seat).collect()
}

/// How many engines to ask at once for `limit` results when one usually
/// gives `usual_yield`: enough for a quarter more than the limit (answers
/// overlap), and two when there are two, so one failing does not cost a
/// second round trip.
pub(crate) fn engines_needed(limit: usize, usual_yield: f64, providers: usize) -> usize {
    let needed = (limit as f64 * 1.25 / usual_yield.max(1.0)).ceil() as usize;
    needed.max(2).min(providers).max(1)
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

//! Searches beyond web pages: news, images, videos and books.
//!
//! Each kind has its own typed result and its own [`Pool`] of sources. A pool
//! works as the web meta-search does ([`crate::engine::meta`]):
//! - the sources are asked free-soonest and healthiest first, as many at
//!   once as their usual yield says the limit needs, and one per result
//!   provider (DuckDuckGo's news and images come from Bing's index, so Bing's
//!   own engine is not asked as well);
//! - it stops once there are enough results;
//! - results are merged by key (the article's or image's URL) and interleaved,
//!   so every source's best results come first;
//! - every source has its own pace and circuit breaker.

pub mod bing;
pub mod duckduckgo;
pub mod openlibrary;
pub mod yahoo;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::join_all;

use crate::engine::EngineError;
use crate::engine::meta::url_key;
use crate::polite::{CircuitBreaker, Health, RateLimiter, asking_order, engines_needed};
use crate::request::SearchRequest;

/// A news article.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewsResult {
    pub title: String,
    pub url: String,
    pub body: String,
    /// When it was published, as ISO 8601 UTC (`2026-11-08T14:00:00Z`) when
    /// the source gives a date or an age; otherwise as the source wrote it.
    pub date: String,
    pub source: String,
    pub image: String,
}

/// An image.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageResult {
    pub title: String,
    /// The full-size image.
    pub image: String,
    pub thumbnail: String,
    /// The page the image is on.
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub source: String,
}

/// A video.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoResult {
    pub title: String,
    /// The video's page (YouTube, Vimeo, …).
    pub url: String,
    pub description: String,
    /// As the source writes it, e.g. `9:20`.
    pub duration: String,
    pub embed_url: String,
    pub thumbnail: String,
    pub publisher: String,
    pub uploader: String,
    /// ISO 8601, as the source gives it.
    pub published: String,
    pub views: Option<u64>,
}

/// A book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookResult {
    pub title: String,
    pub authors: Vec<String>,
    pub publisher: String,
    pub year: Option<i32>,
    /// The book's page.
    pub url: String,
    pub thumbnail: String,
    /// Editions, languages and the like, in a few words.
    pub info: String,
}

/// What results are merged by.
pub trait Keyed {
    fn key(&self) -> String;
}

impl Keyed for NewsResult {
    fn key(&self) -> String {
        url_key(&self.url)
    }
}

impl Keyed for ImageResult {
    fn key(&self) -> String {
        url_key(&self.image)
    }
}

impl Keyed for VideoResult {
    fn key(&self) -> String {
        url_key(if self.url.is_empty() { &self.embed_url } else { &self.url })
    }
}

impl Keyed for BookResult {
    fn key(&self) -> String {
        url_key(&self.url)
    }
}

pub type SourceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<Vec<T>, EngineError>> + Send + 'a>>;

/// A source of one kind of result.
pub trait Source<T>: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whose index the results come from: one answer per provider is enough.
    fn provider(&self) -> &'static str {
        self.name()
    }
    fn search<'a>(&'a self, request: &'a SearchRequest) -> SourceFuture<'a, T>;
}

struct Seat<T> {
    source: Arc<dyn Source<T>>,
    breaker: CircuitBreaker,
    limiter: RateLimiter,
    health: Health,
}

/// The sources of one kind of result, asked as described in the module docs.
pub struct Pool<T> {
    seats: Vec<Seat<T>>,
}

impl<T: Keyed + Clone + Send + 'static> Pool<T> {
    /// `interval` spaces the requests to each source; `cooldown` rests one that refused.
    pub fn new(sources: Vec<Arc<dyn Source<T>>>, cooldown: Duration, interval: Duration) -> Self {
        let seat = |source| Seat {
            source,
            breaker: CircuitBreaker::new(cooldown),
            limiter: RateLimiter::new(interval),
            health: Health::default(),
        };
        Self { seats: sources.into_iter().map(seat).collect() }
    }

    /// The names of the sources, with how long each is still resting, if it is.
    pub fn status(&self) -> Vec<(&'static str, Option<Duration>)> {
        self.seats.iter().map(|s| (s.source.name(), s.breaker.open_for())).collect()
    }

    /// Up to `request.limit` results, or why there are none.
    pub async fn search(&self, request: &SearchRequest) -> Result<(Vec<T>, Vec<&'static str>), EngineError> {
        let ready: Vec<&Seat<T>> =
            asking_order(self.seats.iter().filter(|s| s.breaker.open_for().is_none()).collect(), |seat| {
                (&seat.limiter, &seat.health)
            });
        if ready.is_empty() {
            return Err(EngineError::Refused("every source is cooling down".into()));
        }
        let providers: HashSet<&str> = ready.iter().map(|s| s.source.provider()).collect();
        let usual_yield = ready.iter().map(|s| s.health.usual_yield()).sum::<f64>() / ready.len() as f64;
        let workers = engines_needed(request.limit, usual_yield, providers.len());
        let (mut lists, mut errors, mut asked, mut refusals) = (Vec::new(), Vec::new(), 0, 0);
        let (mut answered, mut found, mut used) = (HashSet::new(), HashSet::new(), Vec::new());
        let mut queue: std::collections::VecDeque<&Seat<T>> = ready.into();
        loop {
            let (mut batch, mut later): (Vec<&Seat<T>>, Vec<&Seat<T>>) = (Vec::new(), Vec::new());
            while batch.len() < workers
                && let Some(seat) = queue.pop_front()
            {
                let provider = seat.source.provider();
                if answered.contains(provider) {
                    continue;
                }
                if batch.iter().any(|s| s.source.provider() == provider) {
                    later.push(seat);
                    continue;
                }
                // A busy source is waited for only when no free one is left.
                if !batch.is_empty() && !seat.limiter.ready_in().is_zero() {
                    queue.push_front(seat);
                    break;
                }
                batch.push(seat);
            }
            for seat in later.into_iter().rev() {
                queue.push_front(seat);
            }
            if batch.is_empty() {
                break;
            }
            let replies = join_all(batch.iter().map(|seat| async move {
                seat.limiter.wait_turn().await;
                let started = Instant::now();
                let reply = seat.source.search(request).await;
                seat.health.record(reply.as_ref().ok().map(Vec::len), started.elapsed());
                reply
            }))
            .await;
            for (seat, reply) in batch.iter().zip(replies) {
                asked += 1;
                let name = seat.source.name();
                match reply {
                    Ok(results) => {
                        seat.breaker.record_success();
                        if !results.is_empty() {
                            answered.insert(seat.source.provider());
                            found.extend(results.iter().map(Keyed::key));
                            used.push(name);
                            lists.push(results);
                        }
                    }
                    Err(error) => {
                        if error.is_refusal() {
                            seat.breaker.trip();
                            refusals += 1;
                        }
                        tracing::info!("ratatoskr: {name} gave nothing ({error})");
                        errors.push(format!("{name}: {error}"));
                    }
                }
            }
            if found.len() >= request.limit {
                break;
            }
        }
        let mut merged = interleave(lists);
        merged.truncate(request.limit);
        if !merged.is_empty() {
            return Ok((merged, used));
        }
        if asked > 0 && refusals == asked {
            return Err(EngineError::Refused(errors.join("; ")));
        }
        if errors.is_empty() { Ok((Vec::new(), used)) } else { Err(EngineError::Transport(errors.join("; "))) }
    }
}

/// The lists' first results, then their second, … with repeats (by key) dropped.
pub fn interleave<T: Keyed>(lists: Vec<Vec<T>>) -> Vec<T> {
    let mut seen = HashSet::new();
    let mut iterators: Vec<_> = lists.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    loop {
        let mut any = false;
        for iterator in &mut iterators {
            if let Some(item) = iterator.next() {
                any = true;
                if seen.insert(item.key()) {
                    out.push(item);
                }
            }
        }
        if !any {
            return out;
        }
    }
}

/// `secs` since the Unix epoch as ISO 8601 UTC.
pub fn iso_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Now, in seconds since the Unix epoch.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// An age such as "20 hours ago", "3 days ago" or "2h" as an ISO date
/// relative to `now`; the text unchanged when it is not one.
pub fn date_from_age(text: &str, now: i64) -> String {
    let lower = text.to_ascii_lowercase();
    let digits: String = lower.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    let Ok(count) = digits.parse::<i64>() else { return text.trim().to_string() };
    let after = lower.split_once(&digits).map_or("", |(_, rest)| rest).trim_start();
    let unit = match after {
        u if u.starts_with("min") || u.starts_with('m') && !u.starts_with("mo") => 60,
        u if u.starts_with('h') => 3_600,
        u if u.starts_with('d') => 86_400,
        u if u.starts_with('w') => 7 * 86_400,
        u if u.starts_with("mo") => 30 * 86_400,
        u if u.starts_with('y') => 365 * 86_400,
        _ => return text.trim().to_string(),
    };
    iso_from_unix(now - count * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ages_become_iso() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_794_092_400), "2026-11-07T23:00:00Z");
        assert_eq!(iso_from_unix(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
        let now = 1_794_092_400;
        assert_eq!(date_from_age("20 hours ago · ", now), iso_from_unix(now - 20 * 3600));
        assert_eq!(date_from_age("3 days ago", now), iso_from_unix(now - 3 * 86_400));
        assert_eq!(date_from_age("2h", now), iso_from_unix(now - 7200));
        assert_eq!(date_from_age("5 mins ago", now), iso_from_unix(now - 300));
        assert_eq!(date_from_age("2 months ago", now), iso_from_unix(now - 60 * 86_400));
        assert_eq!(date_from_age("Nov 3, 2026", now), "Nov 3, 2026", "not an age: kept as written");
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Item(&'static str);
    impl Keyed for Item {
        fn key(&self) -> String {
            self.0.to_string()
        }
    }

    #[test]
    fn interleaving_keeps_every_sources_best_first() {
        let merged = interleave(vec![vec![Item("a"), Item("b"), Item("c")], vec![Item("x"), Item("a")]]);
        assert_eq!(merged, vec![Item("a"), Item("x"), Item("b"), Item("c")]);
    }

    struct Fake(&'static str, &'static str, Result<Vec<Item>, EngineError>);
    impl Source<Item> for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn provider(&self) -> &'static str {
            self.1
        }
        fn search<'a>(&'a self, _request: &'a SearchRequest) -> SourceFuture<'a, Item> {
            let reply = self.2.clone();
            Box::pin(async move { reply })
        }
    }

    #[tokio::test]
    async fn a_pool_merges_rests_refusers_and_asks_one_per_provider() {
        let pool = Pool::new(
            vec![
                Arc::new(Fake("ddg", "bing", Err(EngineError::Refused("challenge".into())))) as Arc<dyn Source<Item>>,
                Arc::new(Fake("bing", "bing", Ok(vec![Item("b1"), Item("b2")]))),
                Arc::new(Fake("yahoo", "yahoo", Ok(vec![Item("y1"), Item("b1")]))),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        for _ in 0..10 {
            let (items, used) = pool.search(&SearchRequest::new("x", 10)).await.unwrap();
            assert!(used.contains(&"yahoo"), "{used:?}");
            assert_eq!(items.len(), 3, "b1 once: {items:?}");
        }
        assert!(pool.status().iter().any(|(name, rest)| *name == "ddg" && rest.is_some()), "the refuser rests");
        let refusing = Pool::new(
            vec![Arc::new(Fake("ddg", "bing", Err(EngineError::Refused("challenge".into())))) as Arc<dyn Source<Item>>],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert!(matches!(refusing.search(&SearchRequest::new("x", 5)).await, Err(EngineError::Refused(_))));
    }
}

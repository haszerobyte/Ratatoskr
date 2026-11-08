//! Meta-search, the way ddgs's `backend="auto"` gets dependable answers
//! (ddgs 9.14, `DDGS._search_sync`):
//!
//! - the encyclopedias (Wikipedia, Grokipedia) each add their one best page;
//! - the web engines are asked in a shuffled order, a few at a time
//!   (`ceil(limit / 10) + 1`, at most one per result provider: Yahoo and
//!   DuckDuckGo both serve Bing's), and no more once there are enough
//!   results, so a search costs two engines a request rather than all of
//!   them, and no engine sees every query;
//! - results are de-duplicated by URL, those several engines found first,
//!   then ranked as ddgs's SimpleFilterRanker does.
//!
//! Each engine has its own circuit breaker, so one that refuses is left alone
//! while the others carry on, and its own pace (ddgs has none; ratatoskr
//! keeps one, per engine): searches made at the same time go first to the
//! engines free soonest, rather than queueing one behind another. Only the
//! web engines make an answer: when none answered, the encyclopedias' one page
//! each is not passed off as a search, so the sources after this one get
//! their turn. Refused only when every engine asked refused.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;

use super::{Engine, EngineError, Hit, SearchFuture, browser};
use crate::polite::{CircuitBreaker, RateLimiter};

/// One engine of the meta-search.
pub struct Member {
    pub engine: Arc<dyn Engine>,
    /// Whose index the results come from: engines of one provider give the
    /// same results, so one answer per provider is enough.
    pub provider: &'static str,
    /// An encyclopedia: asked on every search, for its one best page.
    pub encyclopedia: bool,
}

impl Member {
    pub fn web(engine: Arc<dyn Engine>, provider: &'static str) -> Self {
        Self { engine, provider, encyclopedia: false }
    }

    pub fn encyclopedia(engine: Arc<dyn Engine>) -> Self {
        let provider = engine.name();
        Self { engine, provider, encyclopedia: true }
    }
}

/// A member with its breaker and its pace.
struct Seat {
    member: Member,
    breaker: CircuitBreaker,
    limiter: RateLimiter,
}

pub struct Meta {
    seats: Vec<Seat>,
}

impl Meta {
    /// `interval` spaces the requests to each engine; `cooldown` rests one that refused.
    pub fn new(members: Vec<Member>, cooldown: Duration, interval: Duration) -> Self {
        let seat = |member| Seat { member, breaker: CircuitBreaker::new(cooldown), limiter: RateLimiter::new(interval) };
        Self { seats: members.into_iter().map(seat).collect() }
    }
}

/// The URL as a de-duplication key: scheme-less, no trailing slash, lower-case host.
fn url_key(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => format!("{}{}{}", parsed.host_str().unwrap_or("").trim_start_matches("www.").to_lowercase(),
                              parsed.path().trim_end_matches('/'), parsed.query().map(|q| format!("?{q}")).unwrap_or_default()),
        Err(_) => url.to_string(),
    }
}

/// ddgs's ResultsAggregator: one hit per URL (the first seen), those more
/// engines returned first, ties in the order they came.
pub fn aggregate(lists: Vec<Vec<Hit>>) -> Vec<Hit> {
    let mut order: Vec<(String, Hit)> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for hit in lists.into_iter().flatten() {
        let key = url_key(&hit.url);
        let count = counts.entry(key.clone()).or_insert(0);
        if *count == 0 {
            order.push((key, hit));
        }
        *count += 1;
    }
    let mut ranked: Vec<(usize, usize, Hit)> = order.into_iter().enumerate().map(|(i, (key, hit))| (counts[&key], i, hit)).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, hit)| hit).collect()
}

/// ddgs's SimpleFilterRanker: Wikipedia pages first, then hits with a query
/// word (three letters or more) in both title and snippet, in the title only,
/// in the snippet only, then the rest; Wikimedia category pages dropped.
pub fn rank(hits: Vec<Hit>, query: &str) -> Vec<Hit> {
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.chars().count() >= 3).collect();
    let has_word = |text: &str| {
        let text = text.to_lowercase();
        words.iter().any(|w| text.contains(w))
    };
    let mut buckets: [Vec<Hit>; 5] = Default::default();
    for hit in hits {
        if hit.title.contains("Category:") && hit.title.contains("Wikimedia") {
            continue;
        }
        let bucket = if hit.url.contains("wikipedia.org") {
            0
        } else {
            match (has_word(&hit.title), has_word(&hit.snippet)) {
                (true, true) => 1,
                (true, false) => 2,
                (false, true) => 3,
                (false, false) => 4,
            }
        };
        buckets[bucket].push(hit);
    }
    buckets.into_iter().flatten().collect()
}

/// How many web engines to ask at once for `limit` results (ddgs's max_workers).
fn workers(limit: usize, providers: usize) -> usize {
    (limit.div_ceil(10) + 1).min(providers).max(1)
}

/// `items` in a random order (Fisher-Yates).
fn shuffled<T>(mut items: Vec<T>) -> Vec<T> {
    for i in (1..items.len()).rev() {
        items.swap(i, (browser::random_u64() % (i as u64 + 1)) as usize);
    }
    items
}

#[derive(Default)]
struct Answers {
    lists: Vec<Vec<Hit>>,
    /// The URLs the web engines found, de-duplicated: what counts towards the limit.
    web_found: HashSet<String>,
    errors: Vec<String>,
    /// Web engines asked, and how many of them refused.
    web_asked: usize,
    web_refusals: usize,
}

impl Meta {
    /// Ask `batch` together; record each answer and trip the breakers of the
    /// engines that refused. Returns the providers that answered.
    async fn ask(&self, batch: &[&Seat], query: &str, limit: usize, country: Option<&str>,
                 answers: &mut Answers) -> Vec<&'static str> {
        let replies = join_all(batch.iter().map(|seat| async move {
            let wanted = if seat.member.encyclopedia { 1 } else { limit };
            seat.limiter.wait_turn().await;
            seat.member.engine.search_in(query, wanted, country).await
        }))
        .await;
        let mut answered = Vec::new();
        for (seat, reply) in batch.iter().zip(replies) {
            let (member, breaker) = (&seat.member, &seat.breaker);
            answers.web_asked += usize::from(!member.encyclopedia);
            let name = member.engine.name();
            match reply {
                Ok(hits) => {
                    breaker.record_success();
                    if !hits.is_empty() {
                        answered.push(member.provider);
                        if !member.encyclopedia {
                            answers.web_found.extend(hits.iter().map(|hit| url_key(&hit.url)));
                        }
                        answers.lists.push(hits);
                    }
                }
                Err(error) => {
                    if error.is_refusal() {
                        breaker.trip();
                        answers.web_refusals += usize::from(!member.encyclopedia);
                    }
                    tracing::info!("ratatoskr meta: {name} gave nothing ({error})");
                    answers.errors.push(format!("{name}: {error}"));
                }
            }
        }
        answered
    }
}

impl Engine for Meta {
    fn name(&self) -> &'static str {
        "meta"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        self.search_in(query, limit, None)
    }

    fn paces_itself(&self) -> bool {
        true
    }

    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        Box::pin(async move {
            let ready = |encyclopedia: bool| -> Vec<&Seat> {
                self.seats.iter().filter(|s| s.member.encyclopedia == encyclopedia && s.breaker.open_for().is_none()).collect()
            };
            // A random order, then the engines free soonest first (a stable
            // sort: equally free ones keep their random order).
            let mut web = shuffled(ready(false));
            web.sort_by_key(|seat| seat.limiter.ready_in());
            // An encyclopedia adds one page: not worth waiting for its turn.
            let encyclopedias: Vec<&Seat> = ready(true).into_iter().filter(|s| s.limiter.ready_in().is_zero()).collect();
            if web.is_empty() {
                return Err(EngineError::Refused("every web engine is cooling down".into()));
            }
            let providers: HashSet<&str> = web.iter().map(|s| s.member.provider).collect();
            let workers = workers(limit, providers.len());
            let mut answers = Answers::default();
            let mut answered: HashSet<&'static str> = HashSet::new();
            let mut queue: std::collections::VecDeque<_> = web.into();
            // The encyclopedias go with the first web batch: one round trip, not two.
            let mut batch = encyclopedias;
            loop {
                // An engine whose provider is already in this batch waits for
                // the next one, in case that answer fails.
                let mut later = Vec::new();
                let mut taken = 0;
                while taken < workers && let Some(member) = queue.pop_front() {
                    if answered.contains(member.member.provider) {
                        continue;
                    }
                    // A busy engine (still inside its pace) is waited for only
                    // when no free one is left for this batch.
                    if taken > 0 && !member.limiter.ready_in().is_zero() {
                        queue.push_front(member);
                        break;
                    }
                    if batch.iter().any(|s| !s.member.encyclopedia && s.member.provider == member.member.provider) {
                        later.push(member);
                        continue;
                    }
                    batch.push(member);
                    taken += 1;
                }
                for member in later.into_iter().rev() {
                    queue.push_front(member);
                }
                if batch.is_empty() {
                    break;
                }
                answered.extend(self.ask(&batch, query, limit, country, &mut answers).await);
                batch = Vec::new();
                if answers.web_found.len() >= limit {
                    break;
                }
            }
            if !answers.web_found.is_empty() {
                let mut hits = rank(aggregate(std::mem::take(&mut answers.lists)), query);
                hits.truncate(limit);
                return Ok(hits);
            }
            if answers.web_refusals == answers.web_asked {
                return Err(EngineError::Refused(answers.errors.join("; ")));
            }
            if answers.errors.is_empty() { Ok(Vec::new()) } else { Err(EngineError::Transport(answers.errors.join("; "))) }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn hit(url: &str) -> Hit {
        Hit { title: url.into(), url: url.into(), snippet: String::new() }
    }

    #[test]
    fn aggregation_dedupes_and_puts_what_several_engines_found_first() {
        let yahoo = vec![hit("https://a.example/"), hit("https://rust-lang.org/"), hit("https://doc.rust-lang.org/book")];
        let yandex = vec![hit("https://www.rust-lang.org"), hit("https://b.example/x"), hit("https://doc.rust-lang.org/book/")];
        let urls: Vec<String> = aggregate(vec![yahoo, yandex]).into_iter().map(|h| h.url).collect();
        assert_eq!(urls, ["https://rust-lang.org/", "https://doc.rust-lang.org/book", "https://a.example/", "https://b.example/x"]);
    }

    #[test]
    fn ranking_follows_ddgs_simple_filter_ranker() {
        let h = |url: &str, title: &str, snippet: &str| Hit { title: title.into(), url: url.into(), snippet: snippet.into() };
        let hits = vec![
            h("https://n.example/", "Nothing", "else"),
            h("https://s.example/", "Other", "about tokio"),
            h("https://t.example/", "Tokio guide", "x"),
            h("https://b.example/", "Tokio select", "the select macro"),
            h("https://en.wikipedia.org/wiki/Tokio", "Tokio", ""),
            h("https://commons.wikimedia.org/c", "Category:Tokio - Wikimedia Commons", ""),
        ];
        let urls: Vec<String> = rank(hits, "tokio select").into_iter().map(|h| h.url).collect();
        assert_eq!(urls, ["https://en.wikipedia.org/wiki/Tokio", "https://b.example/", "https://t.example/", "https://s.example/",
                          "https://n.example/"]);
    }

    #[test]
    fn a_few_engines_at_a_time_as_ddgs_does() {
        assert_eq!(workers(5, 3), 2);
        assert_eq!(workers(10, 3), 2);
        assert_eq!(workers(25, 3), 3, "never more than the providers");
        assert_eq!(workers(5, 0), 1);
        let mut seen = HashSet::new();
        for _ in 0..50 {
            seen.insert(shuffled(vec![1, 2, 3]));
        }
        assert!(seen.len() > 1, "the order varies");
    }

    /// A fake engine recording that it was asked.
    struct Fake {
        name: &'static str,
        reply: Result<Vec<Hit>, EngineError>,
        asked: Arc<Mutex<Vec<(&'static str, usize)>>>,
    }

    impl Engine for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        fn search<'a>(&'a self, _query: &'a str, limit: usize) -> SearchFuture<'a> {
            self.asked.lock().unwrap().push((self.name, limit));
            let reply = self.reply.clone().map(|hits| hits.into_iter().take(limit).collect());
            Box::pin(async move { reply })
        }
    }

    fn web_hits(prefix: &str) -> Vec<Hit> {
        (0..10).map(|i| hit(&format!("https://{prefix}.example/{i}"))).collect()
    }

    #[tokio::test]
    async fn enough_results_stop_the_search_and_one_answer_per_provider_is_enough() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(vec![
            Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
            Member::web(fake("yahoo", Ok(web_hits("y"))), "bing"),
            Member::web(fake("duckduckgo", Ok(web_hits("d"))), "bing"),
            Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
        ], Duration::from_secs(60), Duration::ZERO);
        let hits = meta.search("x", 5).await.unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].url, "https://en.wikipedia.org/wiki/X");
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 3, "the encyclopedia and two web engines, one batch: {asked:?}");
        assert!(asked.contains(&("wikipedia", 1)), "an encyclopedia gives one page");
        assert!(asked.contains(&("yandex", 5)));
        assert!(asked.iter().filter(|(n, _)| *n == "yahoo" || *n == "duckduckgo").count() == 1, "one Bing engine: {asked:?}");
    }

    /// Regression: with every web engine resting, the encyclopedias' one page
    /// each came back as the answer, and the sources after the meta-search
    /// (the keyed APIs, Wikipedia's full search) were never asked.
    #[tokio::test]
    async fn encyclopedias_alone_are_no_answer() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(vec![
            Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
            Member::web(fake("yahoo", Ok(vec![])), "bing"),
        ], Duration::from_secs(60), Duration::ZERO);
        assert_eq!(meta.search("x", 5).await.unwrap(), vec![]);
        let resting = Meta::new(vec![
            Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
            Member::web(fake("duckduckgo", Err(EngineError::Refused("challenge".into()))), "bing"),
        ], Duration::from_secs(60), Duration::ZERO);
        assert!(matches!(resting.search("x", 5).await, Err(EngineError::Refused(_))));
        assert!(matches!(resting.search("x", 5).await, Err(EngineError::Refused(why)) if why.contains("cooling down")));
    }

    /// Regression: the stop counted every hit, so two engines returning the
    /// same pages ended the search short of `limit` distinct results.
    #[tokio::test]
    async fn the_limit_counts_distinct_pages() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let same: Vec<Hit> = (0..3).map(|i| hit(&format!("https://same.example/{i}"))).collect();
        let meta = Meta::new(vec![
            Member::web(fake("yahoo", Ok(same.clone())), "bing"),
            Member::web(fake("yandex", Ok(same)), "yandex"),
            Member::web(fake("other", Ok(web_hits("o"))), "other"),
        ], Duration::from_secs(60), Duration::ZERO);
        // The batch of two is random; whenever it is the two that agree, only
        // the third engine can make up the five.
        for _ in 0..30 {
            assert_eq!(meta.search("x", 5).await.unwrap().len(), 5);
        }
    }

    /// Searches at the same time spread over the engines free soonest instead
    /// of waiting for one engine's next turn.
    #[tokio::test]
    async fn a_busy_engine_is_asked_last() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(vec![
            Member::web(fake("yahoo", Ok(web_hits("y"))), "bing"),
            Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
            Member::web(fake("other", Ok(web_hits("o"))), "other"),
        ], Duration::from_secs(60), Duration::from_secs(30));
        let started = std::time::Instant::now();
        assert_eq!(meta.search("q1", 5).await.unwrap().len(), 5);
        assert_eq!(meta.search("q2", 5).await.unwrap().len(), 5);
        assert!(started.elapsed() < Duration::from_secs(1), "no wait: an engine was free");
        let asked: Vec<&str> = asked.lock().unwrap().iter().map(|(n, _)| *n).collect();
        assert_eq!(asked.len(), 3, "two for the first search, the free third for the second: {asked:?}");
        assert!(!asked[..2].contains(&asked[2]), "{asked:?}");
    }

    #[tokio::test]
    async fn refusals_trip_breakers_and_the_rest_carry_on() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(vec![
            Member::web(fake("duckduckgo", Err(EngineError::Refused("challenge".into()))), "bing"),
            Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
        ], Duration::from_secs(60), Duration::ZERO);
        assert_eq!(meta.search("x", 5).await.unwrap().len(), 5);
        assert_eq!(meta.search("y", 5).await.unwrap().len(), 5);
        let ddg_asks = asked.lock().unwrap().iter().filter(|(n, _)| *n == "duckduckgo").count();
        assert_eq!(ddg_asks, 1, "a refusing engine rests");

        let all_refuse = Meta::new(vec![Member::web(fake("yahoo", Err(EngineError::Refused("throttled".into()))), "bing")],
                                   Duration::from_secs(60), Duration::ZERO);
        assert!(matches!(all_refuse.search("x", 5).await, Err(EngineError::Refused(_))));
        assert!(matches!(all_refuse.search("x", 5).await, Err(EngineError::Refused(why)) if why.contains("cooling down")));
    }
}

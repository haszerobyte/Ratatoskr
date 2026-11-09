//! Meta-search: dependable answers from several engines.
//!
//! - the encyclopedias (Wikipedia, Grokipedia) each add their one best page;
//! - the web engines are asked free-soonest and healthiest first (each
//!   keeps a moving record of how often it answers, how fast and how much:
//!   [`Health`]), with a random spread so engines of similar health share
//!   the searches and none sees every query;
//! - as many at once as their usual yield says the limit needs, at least
//!   two as a hedge, at most one per result provider (Yahoo, DuckDuckGo and
//!   Bing all serve Bing's index), and no more once there are enough
//!   results;
//! - results are merged by URL and scored: a vote from each engine that
//!   found one, worth less the lower it ranked it, plus how much of the
//!   query its title, snippet and URL answer ([`fuse`]).
//!
//! Each engine has its own circuit breaker, so one that refuses is left alone
//! while the others carry on, and its own pace: searches made at the same time go first to the
//! engines free soonest, rather than queueing one behind another. Only the
//! web engines make an answer: when none answered, the encyclopedias' one page
//! each is not passed off as a search, so the sources after this one get
//! their turn. Refused only when every engine asked refused.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::join_all;

use super::{Engine, EngineError, Hit, Report, ReportFuture, SearchFuture, SearchRequest};
use crate::polite::{CircuitBreaker, Health, RateLimiter, asking_order, engines_needed};

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

/// A member with its breaker, its pace and its recent record.
struct Seat {
    member: Member,
    breaker: CircuitBreaker,
    limiter: RateLimiter,
    health: Health,
}

pub struct Meta {
    seats: Vec<Seat>,
}

impl Meta {
    /// `interval` spaces the requests to each engine; `cooldown` rests one that refused.
    pub fn new(members: Vec<Member>, cooldown: Duration, interval: Duration) -> Self {
        let seat = |member| Seat {
            member,
            breaker: CircuitBreaker::new(cooldown),
            limiter: RateLimiter::new(interval),
            health: Health::default(),
        };
        Self { seats: members.into_iter().map(seat).collect() }
    }
}

/// The URL as a de-duplication key: scheme-less, no trailing slash, lower-case host.
pub(crate) fn url_key(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => format!(
            "{}{}{}",
            parsed.host_str().unwrap_or("").trim_start_matches("www.").to_lowercase(),
            parsed.path().trim_end_matches('/'),
            parsed.query().map(|q| format!("?{q}")).unwrap_or_default()
        ),
        Err(_) => url.to_string(),
    }
}

/// How fast an engine's vote fades down its list: its first result counts 1,
/// its sixth half that. Gentle, because engines disagree on order far more
/// than on what is relevant.
const POSITION_DAMPING: f64 = 5.0;

/// What covering the whole query is worth, in engine votes: two, so a result
/// one engine ranked low but that answers every word beats one an engine
/// ranked first that answers none, while results several engines agree on
/// still lead.
const COVERAGE_WEIGHT: f64 = 2.0;

/// Words too common to tell results apart.
const COMMON_WORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "what", "how", "who", "why", "when", "where", "which", "are", "was", "were",
    "this", "that", "into", "about",
];

/// The query's distinct words worth matching, lower-case: three letters or
/// more, any word with a digit (years, versions, scores like "2nd"), and
/// any word in a script where one or two characters make a word (Chinese,
/// Japanese, Korean: 東京 is "Tokyo").
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        let telling = word.chars().count() >= 3 || word.chars().any(|c| c.is_ascii_digit() || c >= '\u{2E80}');
        if telling && !COMMON_WORDS.contains(&word) && !terms.iter().any(|t| t == word) {
            terms.push(word.to_string());
        }
    }
    terms
}

/// How much of the query a hit answers, from 0 to 1: the share of its terms
/// in the title counts double (a title is what a page is about), in the
/// snippet and in the URL once each.
fn coverage(hit: &Hit, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let share = |text: &str| {
        let text = text.to_lowercase();
        terms.iter().filter(|t| text.contains(t.as_str())).count() as f64 / terms.len() as f64
    };
    (2.0 * share(&hit.title) + share(&hit.snippet) + share(&hit.url)) / 4.0
}

/// What a page from an idea-board or short-video site costs, in engine votes.
/// They rank well on engines for nearly any query (a Pinterest board was the
/// top answer for "height of mount everest") and seldom answer one; set
/// against the comparison's useful results they were all six off-topic hits
/// in a top five. Not a ban: one that is all a search finds still shows.
const LOW_VALUE_PENALTY: f64 = 1.5;

/// Pinterest (any country's) and TikTok: pages that point at answers or
/// show them in a video a model cannot watch.
fn is_low_value(hit: &Hit) -> bool {
    let host = url::Url::parse(&hit.url).ok().and_then(|u| u.host_str().map(str::to_lowercase)).unwrap_or_default();
    let host = host.trim_start_matches("www.");
    host.split('.').any(|label| label == "pinterest") || host == "tiktok.com" || host.ends_with(".tiktok.com")
}

/// A wiki's category listing: a page of links to pages, never the answer.
fn is_category_listing(hit: &Hit) -> bool {
    hit.url.contains("wikimedia.org") && (hit.title.contains("Category:") || hit.url.contains("Category:"))
}

/// The engines' answers as one list, best first. Each URL appears once and
/// collects a vote from every engine that returned it, worth less the further
/// down that engine's list it was; the votes plus how much of the query the
/// hit answers make its score. Ties keep the order the answers came in. A
/// duplicate's snippet fills in an empty one, so a page is not shown bare
/// because the engine that answered first gave no summary.
pub fn fuse(lists: Vec<Vec<Hit>>, query: &str) -> Vec<Hit> {
    fuse_counted(lists, query).0
}

/// [`fuse`], and how many results another engine had already returned
/// (merged into one).
fn fuse_counted(lists: Vec<Vec<Hit>>, query: &str) -> (Vec<Hit>, usize) {
    let mut merged = 0;
    struct Entry {
        hit: Hit,
        votes: f64,
    }
    let terms = query_terms(query);
    let mut entries: Vec<Entry> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for list in lists {
        let mut in_this_list = HashSet::new();
        for (position, hit) in list.into_iter().enumerate() {
            let key = url_key(&hit.url);
            if is_category_listing(&hit) || !in_this_list.insert(key.clone()) {
                continue;
            }
            let vote = POSITION_DAMPING / (POSITION_DAMPING + position as f64);
            match by_key.get(&key) {
                Some(&i) => {
                    merged += 1;
                    let entry = &mut entries[i];
                    entry.votes += vote;
                    if entry.hit.snippet.trim().is_empty() {
                        entry.hit.snippet = hit.snippet;
                    }
                }
                None => {
                    by_key.insert(key, entries.len());
                    entries.push(Entry { hit, votes: vote });
                }
            }
        }
    }
    let mut scored: Vec<(f64, usize, Hit)> = entries
        .into_iter()
        .enumerate()
        .map(|(order, e)| {
            let penalty = if is_low_value(&e.hit) { LOW_VALUE_PENALTY } else { 0.0 };
            (e.votes + COVERAGE_WEIGHT * coverage(&e.hit, &terms) - penalty, order, e.hit)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    (scored.into_iter().map(|(_, _, hit)| hit).collect(), merged)
}

#[derive(Default)]
struct Answers {
    lists: Vec<Vec<Hit>>,
    /// The engine behind each list, in the same order.
    sources: Vec<&'static str>,
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
    async fn ask(&self, batch: &[&Seat], request: &SearchRequest, answers: &mut Answers) -> Vec<&'static str> {
        let one = request.with_limit(1);
        let replies = join_all(batch.iter().map(|seat| {
            let asked = if seat.member.encyclopedia { &one } else { request };
            async move {
                seat.limiter.wait_turn().await;
                let started = Instant::now();
                let reply = seat.member.engine.search_request(asked).await;
                seat.health.record(reply.as_ref().ok().map(Vec::len), started.elapsed());
                reply
            }
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
                        answers.sources.push(name);
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

    /// The meta-search itself: its results, the engines that gave them, and
    /// how many it merged.
    async fn report(&self, request: &SearchRequest) -> Result<Report, EngineError> {
        let (query, limit) = (request.query.as_str(), request.limit);
        let ready = |encyclopedia: bool| -> Vec<&Seat> {
            self.seats
                .iter()
                .filter(|s| s.member.encyclopedia == encyclopedia && s.breaker.open_for().is_none())
                .collect()
        };
        let web = asking_order(ready(false), |seat| (&seat.limiter, &seat.health));
        // An encyclopedia adds one page: not worth waiting for its turn.
        // Nor for a later page or a recent-only search: its one best page
        // answers neither.
        let encyclopedias: Vec<&Seat> = if request.is_plain() {
            ready(true).into_iter().filter(|s| s.limiter.ready_in().is_zero()).collect()
        } else {
            Vec::new()
        };
        if web.is_empty() {
            return Err(EngineError::Refused("every web engine is cooling down".into()));
        }
        let providers: HashSet<&str> = web.iter().map(|s| s.member.provider).collect();
        let usual_yield = web.iter().map(|s| s.health.usual_yield()).sum::<f64>() / web.len() as f64;
        let workers = engines_needed(limit, usual_yield, providers.len());
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
            while taken < workers
                && let Some(member) = queue.pop_front()
            {
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
            answered.extend(self.ask(&batch, request, &mut answers).await);
            batch = Vec::new();
            if answers.web_found.len() >= limit {
                break;
            }
        }
        if !answers.web_found.is_empty() {
            let (mut hits, merged) = fuse_counted(std::mem::take(&mut answers.lists), query);
            hits.truncate(limit);
            return Ok(Report { hits, sources: answers.sources, merged });
        }
        if answers.web_refusals == answers.web_asked {
            return Err(EngineError::Refused(answers.errors.join("; ")));
        }
        if answers.errors.is_empty() {
            Ok(Report::default())
        } else {
            Err(EngineError::Transport(answers.errors.join("; ")))
        }
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
        Box::pin(async move { self.search_request(&SearchRequest::new(query, limit).country(country)).await })
    }

    fn search_request<'a>(&'a self, request: &'a SearchRequest) -> SearchFuture<'a> {
        Box::pin(async move { self.report(request).await.map(|report| report.hits) })
    }

    fn search_report<'a>(&'a self, request: &'a SearchRequest) -> ReportFuture<'a> {
        Box::pin(self.report(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn hit(url: &str) -> Hit {
        Hit { title: url.into(), url: url.into(), snippet: String::new() }
    }

    fn full(url: &str, title: &str, snippet: &str) -> Hit {
        Hit { title: title.into(), url: url.into(), snippet: snippet.into() }
    }

    fn urls(hits: Vec<Hit>) -> Vec<String> {
        hits.into_iter().map(|h| h.url).collect()
    }

    #[test]
    fn pages_several_engines_found_lead_and_appear_once() {
        // No query word matches, so only the votes decide: rust-lang.org is
        // second and first (0.83 + 1), the book third twice (0.71 + 0.71),
        // a.example first once (1), b.example second once (0.83).
        let yahoo =
            vec![hit("https://a.example/"), hit("https://rust-lang.org/"), hit("https://doc.rust-lang.org/book")];
        let yandex =
            vec![hit("https://www.rust-lang.org"), hit("https://b.example/x"), hit("https://doc.rust-lang.org/book/")];
        assert_eq!(
            urls(fuse(vec![yahoo, yandex], "zzz")),
            ["https://rust-lang.org/", "https://doc.rust-lang.org/book", "https://a.example/", "https://b.example/x"]
        );
    }

    #[test]
    fn answering_the_query_outranks_position() {
        let web = vec![
            full("https://n.example/", "Nothing", "else"),
            full("https://s.example/", "Other", "about tokio"),
            full("https://t.example/", "Tokio guide", "x"),
            full("https://b.example/", "Tokio select", "the select macro"),
            full("https://commons.wikimedia.org/c", "Category:Tokio - Wikimedia Commons", ""),
        ];
        let encyclopedia = vec![full("https://en.wikipedia.org/wiki/Tokio", "Tokio", "")];
        assert_eq!(
            urls(fuse(vec![web, encyclopedia], "tokio select")),
            [
                "https://b.example/",
                "https://en.wikipedia.org/wiki/Tokio",
                "https://t.example/",
                "https://s.example/",
                "https://n.example/"
            ],
            "the full match first, the encyclopedia's half match next, the category listing dropped"
        );
    }

    /// Regression: a Pinterest board was the top result for "height of mount
    /// everest", and idea boards and TikTok pages took six top-five places
    /// in twenty-five searches.
    #[test]
    fn idea_boards_and_short_videos_rank_below_answers() {
        let web = vec![
            full("https://www.pinterest.com/pin/123/", "Height of Mount Everest", "everest height diagram"),
            full("https://ru.pinterest.com/pin/9/", "Mount Everest height", "pins"),
            full("https://www.tiktok.com/@x/video/1", "Mount Everest height explained", "everest"),
            full("https://a.example/everest", "Mount Everest", "Its height is 8,849 m"),
        ];
        let ranked = urls(fuse(vec![web], "height of mount everest"));
        assert_eq!(ranked[0], "https://a.example/everest", "the answer leads: {ranked:?}");
        assert_eq!(ranked.len(), 4, "the boards and videos stay, below it");
        assert!(!is_low_value(&full("https://notpinterest.example/", "", "")));
    }

    #[test]
    fn a_duplicate_fills_in_a_missing_snippet() {
        let first = vec![full("https://x.example/", "X", "")];
        let second = vec![full("https://www.x.example", "X", "What x is.")];
        let hits = fuse(vec![first, second], "x");
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].url.as_str(), hits[0].snippet.as_str()), ("https://x.example/", "What x is."));
    }

    #[test]
    fn query_terms_keep_numbers_and_drop_filler() {
        assert_eq!(query_terms("India vs NZ 2nd ODI in 2026, the India"), ["india", "2nd", "odi", "2026"]);
        assert!(query_terms("a of in").is_empty());
        // Regression: two-character words were dropped, so a Chinese or
        // Japanese query had no terms and ranked by votes alone.
        assert_eq!(query_terms("東京 天気"), ["東京", "天気"]);
    }

    #[test]
    fn as_many_engines_as_their_yield_needs_and_two_to_hedge() {
        assert_eq!(engines_needed(5, 10.0, 3), 2, "one would do; the second hedges");
        assert_eq!(engines_needed(25, 10.0, 5), 4);
        assert_eq!(engines_needed(25, 10.0, 3), 3, "never more than the providers");
        assert_eq!(engines_needed(10, 3.0, 9), 5, "engines that give few results: more of them");
        assert_eq!(engines_needed(5, 10.0, 1), 1);
        assert_eq!(engines_needed(5, 10.0, 0), 1);
    }

    #[test]
    fn healthy_engines_are_asked_first_and_similar_ones_share_the_lead() {
        let failing = Health::default();
        for _ in 0..5 {
            failing.record(None, Duration::from_secs(5));
        }
        let free = || RateLimiter::new(Duration::ZERO);
        let seats = [("failing", free(), failing), ("a", free(), Health::default()), ("b", free(), Health::default())];
        let mut leaders = HashSet::new();
        for _ in 0..60 {
            let order = asking_order(seats.iter().collect(), |seat| (&seat.1, &seat.2));
            assert_eq!(order[2].0, "failing");
            leaders.insert(order[0].0);
        }
        assert_eq!(leaders.len(), 2, "neither healthy engine sees every query");
    }

    /// Regression: an engine that answered fast once outscored one never
    /// asked, so with one engine per provider the untried one (DuckDuckGo
    /// behind Bing) was never asked at all.
    #[test]
    fn an_untried_engine_goes_before_a_proven_one() {
        let proven = Health::default();
        proven.record(Some(10), Duration::from_millis(5));
        let free = || RateLimiter::new(Duration::ZERO);
        let seats = [("proven", free(), proven), ("untried", free(), Health::default())];
        for _ in 0..20 {
            let order = asking_order(seats.iter().collect(), |seat| (&seat.1, &seat.2));
            assert_eq!(order[0].0, "untried");
        }
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
    async fn the_report_names_the_engines_and_counts_what_they_shared() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let pages = |prefix: &str| (0..2).map(|i| hit(&format!("https://{prefix}.example/{i}"))).collect::<Vec<_>>();
        let alpha: Vec<Hit> = pages("both").into_iter().chain(pages("a")).collect();
        let beta: Vec<Hit> = pages("both").into_iter().chain(pages("b")).collect();
        let meta = Meta::new(
            vec![Member::web(fake("alpha", Ok(alpha)), "alpha"), Member::web(fake("beta", Ok(beta)), "beta")],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        let report = meta.search_report(&SearchRequest::new("x", 10)).await.unwrap();
        let mut sources = report.sources.clone();
        sources.sort();
        assert_eq!(sources, ["alpha", "beta"]);
        assert_eq!((report.hits.len(), report.merged), (6, 2), "the two pages both returned are merged");
        assert_eq!(meta.search_request(&SearchRequest::new("x", 10)).await.unwrap().len(), 6, "the same hits");
    }

    #[tokio::test]
    async fn enough_results_stop_the_search_and_one_answer_per_provider_is_enough() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(
            vec![
                Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
                Member::web(fake("yahoo", Ok(web_hits("y"))), "bing"),
                Member::web(fake("duckduckgo", Ok(web_hits("d"))), "bing"),
                Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        let hits = meta.search("x", 5).await.unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].url, "https://en.wikipedia.org/wiki/X");
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 3, "the encyclopedia and two web engines, one batch: {asked:?}");
        assert!(asked.contains(&("wikipedia", 1)), "an encyclopedia gives one page");
        assert!(asked.contains(&("yandex", 5)));
        assert!(
            asked.iter().filter(|(n, _)| *n == "yahoo" || *n == "duckduckgo").count() == 1,
            "one Bing engine: {asked:?}"
        );
    }

    /// Regression: with every web engine resting, the encyclopedias' one page
    /// each came back as the answer, and the sources after the meta-search
    /// (the keyed APIs, Wikipedia's full search) were never asked.
    #[tokio::test]
    async fn encyclopedias_alone_are_no_answer() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fake = |name, reply| Arc::new(Fake { name, reply, asked: asked.clone() }) as Arc<dyn Engine>;
        let meta = Meta::new(
            vec![
                Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
                Member::web(fake("yahoo", Ok(vec![])), "bing"),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert_eq!(meta.search("x", 5).await.unwrap(), vec![]);
        let resting = Meta::new(
            vec![
                Member::encyclopedia(fake("wikipedia", Ok(vec![hit("https://en.wikipedia.org/wiki/X")]))),
                Member::web(fake("duckduckgo", Err(EngineError::Refused("challenge".into()))), "bing"),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
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
        let meta = Meta::new(
            vec![
                Member::web(fake("yahoo", Ok(same.clone())), "bing"),
                Member::web(fake("yandex", Ok(same)), "yandex"),
                Member::web(fake("other", Ok(web_hits("o"))), "other"),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
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
        let meta = Meta::new(
            vec![
                Member::web(fake("yahoo", Ok(web_hits("y"))), "bing"),
                Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
                Member::web(fake("other", Ok(web_hits("o"))), "other"),
            ],
            Duration::from_secs(60),
            Duration::from_secs(30),
        );
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
        let meta = Meta::new(
            vec![
                Member::web(fake("duckduckgo", Err(EngineError::Refused("challenge".into()))), "bing"),
                Member::web(fake("yandex", Ok(web_hits("x"))), "yandex"),
            ],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert_eq!(meta.search("x", 5).await.unwrap().len(), 5);
        assert_eq!(meta.search("y", 5).await.unwrap().len(), 5);
        let ddg_asks = asked.lock().unwrap().iter().filter(|(n, _)| *n == "duckduckgo").count();
        assert_eq!(ddg_asks, 1, "a refusing engine rests");

        let all_refuse = Meta::new(
            vec![Member::web(fake("yahoo", Err(EngineError::Refused("throttled".into()))), "bing")],
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert!(matches!(all_refuse.search("x", 5).await, Err(EngineError::Refused(_))));
        assert!(
            matches!(all_refuse.search("x", 5).await, Err(EngineError::Refused(why)) if why.contains("cooling down"))
        );
    }
}

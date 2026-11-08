//! Ratatoskr — the squirrel of Yggdrasil who runs messages between its roots
//! and branches — carries questions out to the web and brings the
//! answers back.
//!
//! [`Ratatoskr::search`] asks sources in order and returns the first useful
//! answer: its own meta-search first (Wikipedia's and Grokipedia's best page,
//! and Yahoo, DuckDuckGo, Bing and Yandex, healthiest first, two or more at a time, each
//! search from a fresh browser, merged and ranked; see [`engine::meta`]),
//! then an external search server if configured, Brave's and Google's
//! official APIs when keyed,
//! and Wikipedia last — an encyclopedia, not web search, kept only for plain
//! facts when nothing else answers. [`Ratatoskr::fetch`] reads a page as text.
//!
//! Search is deliberately polite: a rate limit per engine, a cache so
//! repeated questions never leave the machine, and a circuit breaker that
//! stops asking an engine the moment it signals "no" (DuckDuckGo's 202
//! anomaly page, 403, 429) and falls through to the next. A fetch reads one
//! page a person asked for, as a browser would (see [`fetch`]), and can never
//! reach loopback, private or link-local addresses.

pub mod engine;
pub mod fetch;
pub mod format;
pub mod mcp;
pub mod polite;
pub mod request;
pub mod text;
pub mod vertical;

use std::sync::Arc;
use std::time::Duration;

use engine::{Engine, Hit};
use polite::{Cache, CircuitBreaker, RateLimiter};
pub use request::{SafeSearch, SearchRequest, TimeLimit};

/// The User-Agent of the API engines (Wikipedia, Grokipedia, Brave's and
/// Google's APIs, a search server): who is asking, not a browser. The scraped
/// engines and page fetches present as a browser instead ([`engine::browser`]).
///
/// An API operator who sees this traffic should be able to reach whoever runs
/// it, so `RATATOSKR_CONTACT` (a URL or email of the deployment's owner) is
/// appended when set. Read once; nothing about a particular install is baked in.
pub fn user_agent() -> &'static str {
    static AGENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| agent_for(std::env::var("RATATOSKR_CONTACT").ok().as_deref()))
}

fn agent_for(contact: Option<&str>) -> String {
    let base = concat!("Mozilla/5.0 (compatible; Ratatoskr/", env!("CARGO_PKG_VERSION"));
    match contact.map(str::trim).filter(|c| !c.is_empty()) {
        Some(contact) => format!("{base}; +{contact})"),
        None => format!("{base})"),
    }
}

/// What a search produced, and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// The engine that answered.
    pub engine: &'static str,
    pub from_cache: bool,
}

/// Why no engine could answer.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchFailure {
    /// Each engine tried (or skipped), with why it gave nothing.
    pub attempts: Vec<(&'static str, String)>,
}

impl std::fmt::Display for SearchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.attempts.iter().map(|(e, why)| format!("{e}: {why}")).collect();
        write!(f, "search unavailable ({})", parts.join("; "))
    }
}

struct Slot {
    engine: Arc<dyn Engine>,
    limiter: RateLimiter,
    breaker: CircuitBreaker,
}

/// Tunables; [`Settings::default`] is what production uses.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Minimum spacing between requests to one engine.
    pub min_interval: Duration,
    /// How long a tripped engine is skipped.
    pub cooldown: Duration,
    /// How long a search result is reused.
    pub cache_ttl: Duration,
    pub cache_entries: usize,
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_secs(2),
            cooldown: Duration::from_secs(600),
            cache_ttl: Duration::from_secs(900),
            cache_entries: 256,
            timeout: Duration::from_secs(15),
        }
    }
}

/// Which optional sources are configured. The meta-search (first) and
/// Wikipedia (last) need nothing and are always present.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// An external search server ([`engine::search_api`]), e.g. `http://127.0.0.1:8765`.
    pub search_api_url: Option<String>,
    pub brave_key: Option<String>,
    pub google_key: Option<String>,
    /// Google's Programmable Search Engine id.
    pub google_cx: Option<String>,
}

impl Sources {
    /// From SEARCH_API_URL, BRAVE_SEARCH_API_KEY, GOOGLE_CSE_KEY, GOOGLE_CSE_ID.
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        Self {
            search_api_url: var("SEARCH_API_URL"),
            brave_key: var("BRAVE_SEARCH_API_KEY"),
            google_key: var("GOOGLE_CSE_KEY"),
            google_cx: var("GOOGLE_CSE_ID"),
        }
    }
}

/// The results of a news, images, videos or books search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found<T> {
    pub results: Vec<T>,
    /// The sources that answered.
    pub sources: Vec<&'static str>,
    pub from_cache: bool,
}

/// One cache and one pool of sources for a kind of result.
struct Kind<T> {
    pool: vertical::Pool<T>,
    cache: Cache<(Vec<T>, Vec<&'static str>)>,
}

impl<T: vertical::Keyed + Clone + Send + 'static> Kind<T> {
    fn new(sources: Vec<Arc<dyn vertical::Source<T>>>, settings: &Settings) -> Self {
        Self {
            pool: vertical::Pool::new(sources, settings.cooldown, settings.min_interval),
            cache: Cache::new(settings.cache_ttl, settings.cache_entries),
        }
    }

    async fn search(&self, label: &'static str, request: &SearchRequest) -> Result<Found<T>, SearchFailure> {
        let key = request.cache_key(label);
        if let Some((_, (results, sources))) = self.cache.get(&key) {
            return Ok(Found { results, sources, from_cache: true });
        }
        match self.pool.search(request).await {
            Ok((results, sources)) if !results.is_empty() => {
                self.cache.put(key, label, (results.clone(), sources.clone()));
                Ok(Found { results, sources, from_cache: false })
            }
            Ok(_) => Err(SearchFailure { attempts: vec![(label, "no results".into())] }),
            Err(error) => Err(SearchFailure { attempts: vec![(label, error.to_string())] }),
        }
    }
}

/// The sources of the news, images, videos and books searches.
pub struct Verticals {
    pub news: Vec<Arc<dyn vertical::Source<vertical::NewsResult>>>,
    pub images: Vec<Arc<dyn vertical::Source<vertical::ImageResult>>>,
    pub videos: Vec<Arc<dyn vertical::Source<vertical::VideoResult>>>,
    pub books: Vec<Arc<dyn vertical::Source<vertical::BookResult>>>,
}

impl Verticals {
    /// None: [`Ratatoskr::new`]'s default, for a web-search-only instance.
    pub fn none() -> Self {
        Self { news: Vec::new(), images: Vec::new(), videos: Vec::new(), books: Vec::new() }
    }

    /// The production sources:
    /// - news: DuckDuckGo and Bing (Bing's index, so one of them a search) and
    ///   Yahoo (its own);
    /// - images: Bing, and DuckDuckGo when it does not ask its proof of work;
    /// - videos: DuckDuckGo;
    /// - books: Open Library's official API.
    pub fn standard(settings: &Settings) -> Self {
        use vertical::{bing, duckduckgo, openlibrary, yahoo};
        let t = settings.timeout;
        Self {
            news: vec![
                Arc::new(duckduckgo::DuckDuckGo::new(t).news()),
                Arc::new(bing::Bing::new(t).news()),
                Arc::new(yahoo::News(yahoo::Yahoo::new(t))),
            ],
            images: vec![Arc::new(bing::Bing::new(t).images()), Arc::new(duckduckgo::DuckDuckGo::new(t).images())],
            videos: vec![Arc::new(duckduckgo::DuckDuckGo::new(t).videos())],
            books: vec![Arc::new(openlibrary::OpenLibrary::new(engine::http_client(t)))],
        }
    }
}

pub struct Ratatoskr {
    slots: Vec<Slot>,
    cache: Cache,
    fetcher: fetch::Fetcher,
    news: Kind<vertical::NewsResult>,
    images: Kind<vertical::ImageResult>,
    videos: Kind<vertical::VideoResult>,
    books: Kind<vertical::BookResult>,
}

impl Ratatoskr {
    /// The given engines, tried in order.
    pub fn new(engines: Vec<Arc<dyn Engine>>, settings: Settings) -> Self {
        let slots = engines
            .into_iter()
            .map(|engine| Slot {
                engine,
                limiter: RateLimiter::new(settings.min_interval),
                breaker: CircuitBreaker::new(settings.cooldown),
            })
            .collect();
        let none = Verticals::none();
        Self {
            slots,
            cache: Cache::new(settings.cache_ttl, settings.cache_entries),
            fetcher: fetch::Fetcher::new(settings.timeout),
            news: Kind::new(none.news, &settings),
            images: Kind::new(none.images, &settings),
            videos: Kind::new(none.videos, &settings),
            books: Kind::new(none.books, &settings),
        }
    }

    /// With these news, images, videos and books sources.
    pub fn with_verticals(mut self, verticals: Verticals, settings: &Settings) -> Self {
        self.news = Kind::new(verticals.news, settings);
        self.images = Kind::new(verticals.images, settings);
        self.videos = Kind::new(verticals.videos, settings);
        self.books = Kind::new(verticals.books, settings);
        self
    }

    /// News articles, with dates as ISO 8601 where the source gives one.
    pub async fn news(&self, request: &SearchRequest) -> Result<Found<vertical::NewsResult>, SearchFailure> {
        self.news.search("news", request).await
    }

    /// Images, with their page, thumbnail and size where known.
    pub async fn images(&self, request: &SearchRequest) -> Result<Found<vertical::ImageResult>, SearchFailure> {
        self.images.search("images", request).await
    }

    /// Videos, with duration, publisher and views where known.
    pub async fn videos(&self, request: &SearchRequest) -> Result<Found<vertical::VideoResult>, SearchFailure> {
        self.videos.search("videos", request).await
    }

    /// Books, with authors, year and cover where known.
    pub async fn books(&self, request: &SearchRequest) -> Result<Found<vertical::BookResult>, SearchFailure> {
        self.books.search("books", request).await
    }

    /// With a given fetcher (tests use [`fetch::Fetcher::allowing_private_for_tests`]).
    pub fn with_fetcher(mut self, fetcher: fetch::Fetcher) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// The production set, in order, from what is configured (see [`Sources`]).
    pub fn standard(sources: &Sources) -> Self {
        let settings = Settings::default();
        let http = engine::http_client(settings.timeout);
        // The engines that answer a browser. Brave's, Google's, Mojeek's and
        // Startpage's result pages are left out: tried from two networks,
        // Brave and Mojeek answered with a captcha, Google with its JavaScript
        // wall and Startpage with Anubis's proof of work, and challenges are
        // never solved here.
        use engine::meta::Member;
        let meta = vec![
            Member::encyclopedia(Arc::new(engine::wikipedia::Wikipedia::best_page(http.clone(), "en"))),
            Member::encyclopedia(Arc::new(engine::grokipedia::Grokipedia::new(http.clone()))),
            Member::web(Arc::new(engine::yahoo::Yahoo::new(settings.timeout)), "bing"),
            Member::web(Arc::new(engine::duckduckgo::DuckDuckGo::new(settings.timeout)), "bing"),
            Member::web(Arc::new(engine::bing::Bing::new(settings.timeout)), "bing"),
            Member::web(Arc::new(engine::yandex::Yandex::new(settings.timeout)), "yandex"),
        ];
        let mut engines: Vec<Arc<dyn Engine>> =
            vec![Arc::new(engine::meta::Meta::new(meta, settings.cooldown, settings.min_interval))];
        if let Some(base) = sources.search_api_url.as_deref().filter(|u| !u.trim().is_empty()) {
            engines.push(Arc::new(engine::search_api::SearchApi::new(http.clone(), base.trim())));
        }
        if let Some(key) = sources.brave_key.as_deref().filter(|k| !k.trim().is_empty()) {
            engines.push(Arc::new(engine::brave::Brave::new(http.clone(), key.trim())));
        }
        if let (Some(key), Some(cx)) = (sources.google_key.as_deref(), sources.google_cx.as_deref())
            && !key.trim().is_empty()
            && !cx.trim().is_empty()
        {
            engines.push(Arc::new(engine::google::Google::new(http.clone(), key.trim(), cx.trim())));
        }
        engines.push(Arc::new(engine::wikipedia::Wikipedia::new(http, "en")));
        Self::new(engines, settings.clone()).with_verticals(Verticals::standard(&settings), &settings)
    }

    /// Search: the cache first, then each engine in order that is not cooling
    /// down, waiting out its rate limit. An engine that refuses trips its
    /// breaker; an empty answer falls through without tripping it.
    pub async fn search(&self, query: &str, limit: usize) -> Result<SearchOutcome, SearchFailure> {
        self.search_in(query, limit, None).await
    }

    /// [`Ratatoskr::search`] localized to `country` (ISO 3166-1 alpha-2).
    pub async fn search_in(
        &self,
        query: &str,
        limit: usize,
        country: Option<&str>,
    ) -> Result<SearchOutcome, SearchFailure> {
        self.search_with(&SearchRequest::new(query, limit).country(country)).await
    }

    /// [`Ratatoskr::search`] with every option: country, time limit, safe
    /// search, page ([`SearchRequest`]).
    pub async fn search_with(&self, request: &SearchRequest) -> Result<SearchOutcome, SearchFailure> {
        let key = request.cache_key("text");
        if let Some((engine, hits)) = self.cache.get(&key) {
            return Ok(SearchOutcome { hits, engine, from_cache: true });
        }
        let mut attempts = Vec::new();
        for slot in &self.slots {
            let name = slot.engine.name();
            if let Some(left) = slot.breaker.open_for() {
                attempts.push((name, format!("cooling down for {}s", left.as_secs())));
                continue;
            }
            if !slot.engine.paces_itself() {
                slot.limiter.wait_turn().await;
            }
            match slot.engine.search_request(request).await {
                Ok(hits) if !hits.is_empty() => {
                    slot.breaker.record_success();
                    self.cache.put(key, name, hits.clone());
                    return Ok(SearchOutcome { hits, engine: name, from_cache: false });
                }
                Ok(_) => attempts.push((name, "no results".into())),
                Err(error) => {
                    if error.is_refusal() {
                        slot.breaker.trip();
                        tracing::warn!("ratatoskr: {name} refused ({error}); skipping it for a while");
                    }
                    attempts.push((name, error.to_string()));
                }
            }
        }
        Err(SearchFailure { attempts })
    }

    /// Read `url` as text (see [`fetch::Fetcher::fetch`]).
    pub async fn fetch(&self, url: &str) -> Result<fetch::Page, fetch::FetchError> {
        self.fetcher.fetch(url).await
    }

    /// Per-engine health, for logs and /stats.
    pub fn status(&self) -> Vec<(&'static str, Option<Duration>)> {
        self.slots.iter().map(|s| (s.engine.name(), s.breaker.open_for())).collect()
    }

    /// Each news, images, videos and books source, by kind, with how long it
    /// is still resting, if it is.
    pub fn vertical_status(&self) -> Vec<(&'static str, &'static str, Option<Duration>)> {
        let tag = |kind, list: Vec<(&'static str, Option<Duration>)>| list.into_iter().map(move |(n, r)| (kind, n, r));
        tag("news", self.news.pool.status())
            .chain(tag("images", self.images.pool.status()))
            .chain(tag("videos", self.videos.pool.status()))
            .chain(tag("books", self.books.pool.status()))
            .collect()
    }
}

#[cfg(test)]
mod agent_tests {
    use super::agent_for;

    #[test]
    fn the_contact_is_optional_and_never_built_in() {
        let bare = agent_for(None);
        assert!(bare.starts_with("Mozilla/5.0 (compatible; Ratatoskr/") && bare.ends_with(')'));
        assert!(!bare.contains("+"));
        assert_eq!(agent_for(Some("  ")), bare);
        assert!(agent_for(Some("https://example.org/bot")).ends_with("; +https://example.org/bot)"));
    }
}

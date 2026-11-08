//! Search engines behind one interface.

pub mod brave;
pub mod browser;
pub mod ddgs;
pub mod duckduckgo;
pub mod google;
pub mod grokipedia;
pub mod meta;
pub mod wikipedia;
pub mod yahoo;
pub mod yandex;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Why an engine gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// The engine signalled "slow down" or "go away" (rate limit, bot
    /// challenge, forbidden). The breaker trips: we stop asking.
    Refused(String),
    /// Any other HTTP failure.
    Http(u16),
    /// The request did not complete.
    Transport(String),
    /// The page did not have the expected shape (layout change).
    Parse(String),
}

impl EngineError {
    /// Whether this is the engine saying no, as opposed to a fault.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Transport(why) => write!(f, "{why}"),
            Self::Parse(why) => write!(f, "unexpected page: {why}"),
        }
    }
}

pub type SearchFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Hit>, EngineError>> + Send + 'a>>;

pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a>;

    /// Results for `country` (ISO 3166-1 alpha-2, web_search's `user_location`).
    /// Engines that cannot localize ignore it.
    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        let _ = country;
        self.search(query, limit)
    }
    /// Whether the engine spaces its own requests (the meta-search paces each
    /// of its engines), so the caller should not add its own wait.
    fn paces_itself(&self) -> bool {
        false
    }
}

/// The HTTP client engines share: our User-Agent, compression, a timeout.
pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder().user_agent(crate::user_agent()).timeout(timeout).build().expect("http client")
}

/// Map an HTTP status to an engine error: 403/429 (and 202 for engines that
/// use it as a challenge) are refusals.
pub(crate) fn status_error(status: u16) -> EngineError {
    match status {
        403 | 429 | 503 => EngineError::Refused(format!("HTTP {status}")),
        code => EngineError::Http(code),
    }
}

/// A transport failure's text, without the request's URL: an engine's URL
/// can carry its API key (Google's `key=`), and this text is logged and
/// handed to the caller (and from there, often, to a user).
pub(crate) fn transport(error: reqwest::Error) -> EngineError {
    EngineError::Transport(if error.is_timeout() { "timed out".into() } else { error.without_url().to_string() })
}

/// [`transport`] for the browser clients, whose URLs carry no key.
pub(crate) fn browser_transport(error: wreq::Error) -> EngineError {
    EngineError::Transport(if error.is_timeout() { "timed out".into() } else { error.to_string() })
}

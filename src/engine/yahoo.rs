//! Yahoo Search (search.yahoo.com, Bing-backed): `p=<query>` on a URL with random `_ylt`/`_ylu` tokens, results in
//! `div.relsrch`, links wrapped in r.search.yahoo.com redirects whose `RU=`
//! segment carries the destination. A browser client is enough: Yahoo's first
//! answer is a consent redirect that sets a cookie, then the results.

use scraper::{ElementRef, Html, Selector};

use super::browser;
use super::{Engine, EngineError, Hit, SearchFuture, SearchRequest};

pub struct Yahoo {
    sessions: browser::Sessions,
    base: String,
}

impl Yahoo {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), base: "https://search.yahoo.com".into() }
    }
    pub fn with_base(http: wreq::Client, base: &str) -> Self {
        Self {
            sessions: browser::Sessions::Fixed(http, browser::Family::Chromium),
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl Engine for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move { self.search_request(&SearchRequest::new(query, limit)).await })
    }

    fn search_request<'a>(&'a self, req: &'a SearchRequest) -> SearchFuture<'a> {
        Box::pin(async move {
            let (query, limit) = (req.query.as_str(), req.limit);
            let url = format!("{}/search;_ylt={};_ylu={}", self.base, browser::token(24), browser::token(47));
            // A typed search URL: no Referer, no same-origin claim. Each
            // search is a fresh browser with no Yahoo cookies, and a visit
            // "from Yahoo's own page" without them is a mismatch.
            let (http, _) = self.sessions.open();
            let mut params = vec![("p", query.to_string())];
            // The results page's own controls: a date filter `btf`, and the
            // first result `b` of the next page (7 a page).
            if let Some(time) = req.time {
                params.push(("btf", time.letter().into()));
            }
            if req.page > 1 {
                params.push(("b", ((req.page - 1) * 7 + 1).to_string()));
            }
            let body = browser::read_page(http.get(&url).query(&params), "relsrch").await?;
            if is_throttled(&body) {
                return Err(EngineError::Refused("temporary problems page (throttled)".into()));
            }
            parse(&body, limit)
        })
    }
}

/// Yahoo's soft block: a results page whose only result is "We had temporary
/// problems searching for web pages", served with 200 to a session it is
/// throttling (about half the searches from one client on 2026-11-08). A
/// refusal, not a layout change: the breaker rests the engine.
pub fn is_throttled(page: &str) -> bool {
    page.contains("We had temporary problems searching") && !page.contains("relsrch")
}

fn has_class_containing(element: &ElementRef, needle: &str) -> bool {
    element.value().attr("class").is_some_and(|c| c.contains(needle))
}

fn text_of(element: ElementRef) -> String {
    crate::text::squash(&element.text().collect::<String>())
}

/// Results from a Yahoo results page; an unrecognised page is a Parse error.
pub fn parse(page: &str, limit: usize) -> Result<Vec<Hit>, EngineError> {
    let document = Html::parse_document(page);
    let div = Selector::parse("div").expect("selector");
    let h3 = Selector::parse("h3").expect("selector");
    let a = Selector::parse("a[href]").expect("selector");
    let mut hits = Vec::new();
    for item in document.select(&div).filter(|d| d.value().classes().any(|c| c == "relsrch")) {
        let Some(title_box) = item.select(&div).find(|d| has_class_containing(d, "Title")) else { continue };
        let title = title_box.select(&h3).next().map(text_of).unwrap_or_default();
        let Some(href) = title_box.select(&a).next().and_then(|link| link.value().attr("href")) else { continue };
        let Some(url) = real_url(href) else { continue };
        let snippet = item.select(&div).find(|d| has_class_containing(d, "Text")).map(text_of).unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        hits.push(Hit { title, url, snippet });
        if hits.len() >= limit {
            break;
        }
    }
    if hits.is_empty() && !page.contains("relsrch") && !page.contains("We did not find results") {
        return Err(EngineError::Parse("no result blocks".into()));
    }
    Ok(hits)
}

/// The destination of a result link (web or news): Yahoo's redirect unwrapped
/// (`/RU=<url>/RK=`), ad click-throughs dropped.
pub(crate) fn real_url(href: &str) -> Option<String> {
    if href.contains("bing.com/aclick") || href.contains("/cbclk") {
        return None;
    }
    let target = match href.find("/RU=") {
        Some(at) => {
            let rest = &href[at + 4..];
            let end = rest.find("/RK=").or_else(|| rest.find("/RS=")).unwrap_or(rest.len());
            url::form_urlencoded::parse(format!("u={}", &rest[..end]).as_bytes()).next().map(|(_, v)| v.into_owned())?
        }
        None => href.to_string(),
    };
    let parsed = url::Url::parse(&target).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../../tests/fixtures/meta/yahoo_results.html");
    const THROTTLED: &str = include_str!("../../tests/fixtures/meta/yahoo_temporary_problems.html");

    /// Regression: the throttled page read as a layout change ("no result
    /// blocks") on every search from a throttled host, so the engine was never rested.
    #[test]
    fn the_temporary_problems_page_is_a_throttle_not_a_layout() {
        assert!(is_throttled(THROTTLED));
        assert!(!is_throttled(RESULTS));
    }

    #[test]
    fn a_real_results_page_parses_with_unwrapped_links() {
        let hits = parse(RESULTS, 10).unwrap();
        assert!(hits.len() >= 5, "{}", hits.len());
        assert!(hits.iter().all(|h| h.url.starts_with("http") && !h.url.contains("r.search.yahoo.com")), "{hits:?}");
        let tokio = hits.iter().find(|h| h.url.starts_with("https://tokio.rs/")).expect("tokio.rs result");
        assert!(tokio.title.contains("Tokio"));
        assert!(tokio.snippet.contains("runtime"));
        assert_eq!(parse(RESULTS, 2).unwrap().len(), 2);
    }

    #[test]
    fn redirects_unwrap_and_ads_drop() {
        assert_eq!(real_url("https://r.search.yahoo.com/_ylt=A;_ylu=B/RV=2/RE=1/RO=10/RU=https%3a%2f%2ftokio.rs%2ftokio%2ftutorial/RK=2/RS=x").as_deref(),
                   Some("https://tokio.rs/tokio/tutorial"));
        assert_eq!(real_url("https://www.bing.com/aclick?ld=x"), None);
        assert_eq!(real_url("https://example.com/direct").as_deref(), Some("https://example.com/direct"));
        assert!(matches!(parse("<html><body>new layout</body></html>", 5), Err(EngineError::Parse(_))));
    }
}

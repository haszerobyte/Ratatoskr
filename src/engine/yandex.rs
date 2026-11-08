//! Yandex's site-search page (yandex.com/search/site): a GET with the payload
//! its search widget sends (`text`, `web=1`, a random seven-digit `searchid`)
//! from a fresh [`super::browser`] client, results in `li.b-serp-item`. Its own index, not Bing's: it kept answering a host
//! that DuckDuckGo challenged and Yahoo throttled.

use scraper::{Html, Selector};

use crate::text::squash;

use super::{Engine, EngineError, Hit, SearchFuture, SearchRequest, browser};

pub struct Yandex {
    sessions: browser::Sessions,
    endpoint: String,
}

impl Yandex {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), endpoint: "https://yandex.com/search/site/".into() }
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: wreq::Client, endpoint: &str) -> Self {
        Self { sessions: browser::Sessions::Fixed(http, browser::Family::Chromium), endpoint: endpoint.to_string() }
    }
}

impl Engine for Yandex {
    fn name(&self) -> &'static str {
        "yandex"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move { self.search_request(&SearchRequest::new(query, limit)).await })
    }

    /// Paging only: the site search has no date filter.
    fn search_request<'a>(&'a self, req: &'a SearchRequest) -> SearchFuture<'a> {
        Box::pin(async move {
            let (query, limit) = (req.query.as_str(), req.limit);
            let page = (req.page - 1).to_string();
            let search_id = (1_000_000 + browser::random_u64() % 9_000_000).to_string();
            let (http, _) = self.sessions.open();
            let mut params = vec![("text", query), ("web", "1"), ("searchid", search_id.as_str())];
            if req.page > 1 {
                params.push(("p", page.as_str()));
            }
            let request = http.get(&self.endpoint).query(&params);
            let body = browser::read_page(request, "b-serp-item").await?;
            parse(&body, limit)
        })
    }
}

/// Results from a Yandex site-search page. Its title reads "There are no
/// search results" even above results, so only a page with neither result
/// items nor that notice is an unknown layout.
pub fn parse(page: &str, limit: usize) -> Result<Vec<Hit>, EngineError> {
    let document = Html::parse_document(page);
    let item = Selector::parse("li.b-serp-item").expect("selector");
    let link = Selector::parse("h3 a[href]").expect("selector");
    let text = Selector::parse(".b-serp-item__text").expect("selector");
    let mut hits = Vec::new();
    for block in document.select(&item) {
        let Some(anchor) = block.select(&link).next() else { continue };
        let Some(url) = anchor.value().attr("href").filter(|u| u.starts_with("http")) else { continue };
        hits.push(Hit {
            title: squash(&anchor.text().collect::<String>()),
            url: url.to_string(),
            snippet: block.select(&text).next().map(|t| squash(&t.text().collect::<String>())).unwrap_or_default(),
        });
        if hits.len() >= limit {
            break;
        }
    }
    if hits.is_empty() && !page.contains("There are no search results") {
        return Err(EngineError::Parse("no result items".into()));
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../../tests/fixtures/meta/yandex_results.html");

    #[test]
    fn a_real_results_page_parses() {
        let hits = parse(RESULTS, 10).unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].url, "https://www.raspberrypi.com/software/operating-systems/");
        assert_eq!(hits[0].title, "Raspberry Pi OS downloads – Raspberry Pi");
        assert!(hits[0].snippet.starts_with("Raspberry Pi OS (previously called Raspbian)"), "{}", hits[0].snippet);
        assert_eq!(parse(RESULTS, 2).unwrap().len(), 2);
    }

    #[test]
    fn layout_changes_are_errors_and_no_results_is_empty() {
        assert!(matches!(parse("<html><body>new layout</body></html>", 5), Err(EngineError::Parse(_))));
        let none =
            "<html><head><title>There are no search results for zzqx — on Yandex</title></head><body></body></html>";
        assert_eq!(parse(none, 5).unwrap(), vec![]);
    }
}

//! Yahoo's news search (news.search.yahoo.com): results in `div#web li`, each
//! with its title link in an `h4`, a summary `p`, an age ("20 hours ago") and
//! a source; links are r.search.yahoo.com redirects whose `RU=` segment
//! carries the article. A fresh [`browser`] session per search. Its own
//! index (provider "yahoo").

use scraper::{Html, Selector};

use super::{NewsResult, Source, SourceFuture, date_from_age, now_unix};
use crate::engine::yahoo::real_url;
use crate::engine::{EngineError, SearchRequest, browser};
use crate::text::squash;

pub struct Yahoo {
    sessions: browser::Sessions,
    endpoint: String,
}

impl Yahoo {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), endpoint: "https://news.search.yahoo.com/search".into() }
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: wreq::Client, endpoint: &str) -> Self {
        Self { sessions: browser::Sessions::Fixed(http, browser::Family::Chromium), endpoint: endpoint.into() }
    }
}

/// The news page's query: `p`, a date filter `btf` (d/w/m/y) and the next
/// page's first result `b` (10 a page).
pub fn params(req: &SearchRequest) -> Vec<(&'static str, String)> {
    let mut params = vec![("p", req.query.clone())];
    if let Some(time) = req.time {
        params.push(("btf", time.letter().into()));
    }
    if req.page > 1 {
        params.push(("b", ((req.page - 1) * 10 + 1).to_string()));
    }
    params
}

/// News from a Yahoo news results page.
pub fn parse(page: &str, limit: usize, now: i64) -> Result<Vec<NewsResult>, EngineError> {
    let document = Html::parse_document(page);
    let item = Selector::parse("div#web li").expect("selector");
    let title = Selector::parse("h4 a[href]").expect("selector");
    let summary = Selector::parse("p").expect("selector");
    let age = Selector::parse("span.s-time, span[class*=time]").expect("selector");
    let source = Selector::parse("span.s-source, span[class*=source]").expect("selector");
    let image = Selector::parse("img").expect("selector");
    let mut news = Vec::new();
    // An article's `li` holds another `li` with the same link: one result.
    let mut seen = std::collections::HashSet::new();
    for block in document.select(&item) {
        let Some(anchor) = block.select(&title).next() else { continue };
        let Some(url) = anchor.value().attr("href").and_then(real_url) else { continue };
        if !seen.insert(url.clone()) {
            continue;
        }
        let text_of =
            |selector: &Selector| block.select(selector).next().map(|e| squash(&e.text().collect::<String>()));
        let image = block
            .select(&image)
            .next()
            .and_then(|img| img.value().attr("data-src").or_else(|| img.value().attr("src")))
            // Yahoo's resizer wraps the original: …/-/https://original
            .map(|src| src.split_once("-/").map_or(src, |(_, original)| original).to_string())
            .unwrap_or_default();
        news.push(NewsResult {
            title: squash(&anchor.text().collect::<String>()),
            url,
            body: text_of(&summary).unwrap_or_default(),
            date: text_of(&age).map(|a| date_from_age(a.trim_end_matches(['·', ' ']), now)).unwrap_or_default(),
            source: text_of(&source)
                .map(|s| s.split(" · via Yahoo").next().unwrap_or(&s).trim().to_string())
                .unwrap_or_default(),
            image,
        });
        if news.len() >= limit {
            break;
        }
    }
    if news.is_empty() && !page.contains("id=\"web\"") {
        return Err(EngineError::Parse("no news results".into()));
    }
    Ok(news)
}

/// Yahoo's news, as a [`Source`].
pub struct News(pub Yahoo);

impl Source<NewsResult> for News {
    fn name(&self) -> &'static str {
        "yahoo"
    }
    fn search<'a>(&'a self, request: &'a SearchRequest) -> SourceFuture<'a, NewsResult> {
        Box::pin(async move {
            let (http, _) = self.0.sessions.open();
            let page = browser::read_page(http.get(&self.0.endpoint).query(&params(request)), "id=\"web\"").await?;
            if crate::engine::yahoo::is_throttled(&page) {
                return Err(EngineError::Refused("temporary problems page (throttled)".into()));
            }
            parse(&page, request.limit, now_unix())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEWS: &str = include_str!("../../tests/fixtures/verticals/yahoo_news.html");

    #[test]
    fn a_real_news_page_parses() {
        let now = 1_794_092_400;
        let news = parse(NEWS, 10, now).unwrap();
        assert_eq!(news.len(), 5, "{news:#?}");
        assert!(
            news[0].url.starts_with("https://tech.yahoo.com/computing/articles/raspberry-pi-5-price-hike"),
            "{}",
            news[0].url
        );
        assert!(news[0].title.starts_with("Raspberry Pi 5 price hike"));
        assert_eq!(news[0].date, super::super::iso_from_unix(now - 20 * 3600), "\"20 hours ago\"");
        assert!(!news[0].body.is_empty());
        assert!(news[0].image.starts_with("https://"), "{}", news[0].image);
        assert!(matches!(parse("<html><body>new layout</body></html>", 5, now), Err(EngineError::Parse(_))));
        let req = SearchRequest::new("x", 5).page(3).time(Some(crate::engine::TimeLimit::Day));
        assert_eq!(params(&req), vec![("p", "x".into()), ("btf", "d".into()), ("b", "21".into())]);
    }
}

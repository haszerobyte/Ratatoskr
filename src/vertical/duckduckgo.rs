//! DuckDuckGo's news (`news.js`), videos (`v.js`) and images (`i.js`): JSON
//! endpoints its own result pages call. Each needs the `vqd` token of a
//! results page for the query, so a search is two requests from one fresh
//! [`browser`] session: the page, then the JSON with the headers the page's
//! script sends. Results come from Bing's index (provider "bing").
//!
//! When DuckDuckGo distrusts a client it answers the JSON with HTTP 202 and a
//! `botnet_challenge` (a proof of work for the page's script): a refusal.

use serde_json::Value;

use super::{ImageResult, NewsResult, Source, SourceFuture, VideoResult, iso_from_unix};
use crate::engine::{EngineError, SafeSearch, SearchRequest, TimeLimit, browser, browser_transport, status_error};
use crate::text::fragment_text;

const SITE: &str = "https://duckduckgo.com";

/// Which of the three endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    News,
    Videos,
    Images,
}

pub struct DuckDuckGo {
    sessions: browser::Sessions,
    site: String,
}

impl DuckDuckGo {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), site: SITE.into() }
    }

    /// Tests point this at a fake.
    pub fn with_site(http: wreq::Client, site: &str) -> Self {
        Self {
            sessions: browser::Sessions::Fixed(http, browser::Family::Chromium),
            site: site.trim_end_matches('/').into(),
        }
    }

    /// The JSON for `kind`, after the results page that hands out its `vqd`.
    async fn json(&self, kind: Kind, req: &SearchRequest) -> Result<Value, EngineError> {
        let (http, _) = self.sessions.open();
        let page = http.get(format!("{}/", self.site)).query(&[("q", req.query.as_str())]);
        let page = page.send().await.map_err(browser_transport)?;
        if !page.status().is_success() {
            return Err(status_error(page.status().as_u16()));
        }
        let page = page.text().await.map_err(browser_transport)?;
        let vqd = vqd(&page).ok_or_else(|| EngineError::Parse("no vqd on the results page".into()))?;
        let path = match kind {
            Kind::News => "news.js",
            Kind::Videos => "v.js",
            Kind::Images => "i.js",
        };
        let reply = http
            .get(format!("{}/{path}", self.site))
            .query(&params(kind, req, &vqd))
            .header("accept", "application/json, text/javascript, */*; q=0.01")
            .header("referer", format!("{}/", self.site))
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-origin")
            .send()
            .await
            .map_err(browser_transport)?;
        let status = reply.status().as_u16();
        let body = reply.text().await.map_err(browser_transport)?;
        if status == 202 || body.contains("botnet_challenge") {
            return Err(EngineError::Refused("bot challenge".into()));
        }
        if !(200..300).contains(&status) {
            return Err(status_error(status));
        }
        serde_json::from_str(&body).map_err(|e| EngineError::Parse(e.to_string()))
    }
}

/// The `vqd` token in a results page: `vqd="…"`, `vqd='…'` or `vqd=…&`.
pub fn vqd(page: &str) -> Option<String> {
    for (open, close) in [("vqd=\"", '"'), ("vqd='", '\''), ("vqd=", '&')] {
        if let Some(rest) = page.split_once(open).map(|(_, rest)| rest)
            && let Some(token) = rest.split(close).next().filter(|t| !t.is_empty() && t.len() < 100)
        {
            return Some(token.to_string());
        }
    }
    None
}

/// The endpoint's query: its region, safe search, filters and offset, as the
/// page's script sends them.
fn params(kind: Kind, req: &SearchRequest, vqd: &str) -> Vec<(&'static str, String)> {
    let region = crate::engine::duckduckgo::region(req.country.as_deref()).to_string();
    let mut params = vec![("o", "json".into()), ("q", req.query.clone()), ("l", region), ("vqd", vqd.into())];
    let safe = |on: &str, moderate: &str, off: &str| match req.safesearch {
        SafeSearch::On => on.to_string(),
        SafeSearch::Moderate => moderate.to_string(),
        SafeSearch::Off => off.to_string(),
    };
    let page = u64::from(req.page.saturating_sub(1));
    match kind {
        Kind::News => {
            params.push(("noamp", "1".into()));
            params.push(("p", safe("1", "-1", "-2")));
            if let Some(time) = req.time {
                params.push(("df", time.letter().into()));
            }
            if page > 0 {
                params.push(("s", (page * 30).to_string()));
            }
        }
        Kind::Videos => {
            params.push(("p", safe("1", "-1", "-2")));
            let after = req.time.map(|t| format!("publishedAfter:{}", t.letter())).unwrap_or_default();
            params.push(("f", format!("{after},,,")));
            if page > 0 {
                params.push(("s", (page * 60).to_string()));
            }
        }
        Kind::Images => {
            params.push(("p", safe("1", "1", "-1")));
            params.push(("ct", "AT".into()));
            if let Some(time) = req.time {
                let word = match time {
                    TimeLimit::Day => "Day",
                    TimeLimit::Week => "Week",
                    TimeLimit::Month => "Month",
                    TimeLimit::Year => "Year",
                };
                params.push(("f", format!("time:{word},,,,,")));
            }
            if page > 0 {
                params.push(("s", (page * 100).to_string()));
            }
        }
    }
    params
}

fn results(body: &Value) -> Result<&Vec<Value>, EngineError> {
    body.get("results").and_then(Value::as_array).ok_or_else(|| EngineError::Parse("no results field".into()))
}

fn text(item: &Value, key: &str) -> String {
    item.get(key).and_then(Value::as_str).map(fragment_text).unwrap_or_default()
}

/// News from a `news.js` answer; `date` is Unix seconds.
pub fn parse_news(body: &Value, limit: usize) -> Result<Vec<NewsResult>, EngineError> {
    Ok(results(body)?
        .iter()
        .filter(|item| text(item, "url").starts_with("http"))
        .take(limit)
        .map(|item| NewsResult {
            title: text(item, "title"),
            url: text(item, "url"),
            body: text(item, "excerpt"),
            date: item.get("date").and_then(Value::as_i64).map(iso_from_unix).unwrap_or_default(),
            source: text(item, "source"),
            image: text(item, "image"),
        })
        .collect())
}

/// Videos from a `v.js` answer.
pub fn parse_videos(body: &Value, limit: usize) -> Result<Vec<VideoResult>, EngineError> {
    Ok(results(body)?
        .iter()
        .take(limit)
        .map(|item| VideoResult {
            title: text(item, "title"),
            url: text(item, "content"),
            description: text(item, "description"),
            duration: text(item, "duration"),
            embed_url: text(item, "embed_url"),
            thumbnail: item.pointer("/images/medium").and_then(Value::as_str).unwrap_or_default().to_string(),
            publisher: text(item, "publisher"),
            uploader: text(item, "uploader"),
            published: text(item, "published"),
            views: item.pointer("/statistics/viewCount").and_then(Value::as_u64),
        })
        .filter(|video| video.url.starts_with("http") || video.embed_url.starts_with("http"))
        .collect())
}

/// Images from an `i.js` answer.
pub fn parse_images(body: &Value, limit: usize) -> Result<Vec<ImageResult>, EngineError> {
    let number = |item: &Value, key: &str| item.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Ok(results(body)?
        .iter()
        .filter(|item| text(item, "image").starts_with("http"))
        .take(limit)
        .map(|item| ImageResult {
            title: text(item, "title"),
            image: text(item, "image"),
            thumbnail: text(item, "thumbnail"),
            url: text(item, "url"),
            width: number(item, "width"),
            height: number(item, "height"),
            source: text(item, "source"),
        })
        .collect())
}

/// One of DuckDuckGo's result kinds as a [`Source`]: which endpoint, and its parser.
pub struct Feed<T> {
    site: DuckDuckGo,
    kind: Kind,
    parse: fn(&Value, usize) -> Result<Vec<T>, EngineError>,
}

impl DuckDuckGo {
    /// Its news (`news.js`).
    pub fn news(self) -> Feed<NewsResult> {
        Feed { site: self, kind: Kind::News, parse: parse_news }
    }

    /// Its videos (`v.js`).
    pub fn videos(self) -> Feed<VideoResult> {
        Feed { site: self, kind: Kind::Videos, parse: parse_videos }
    }

    /// Its images (`i.js`).
    pub fn images(self) -> Feed<ImageResult> {
        Feed { site: self, kind: Kind::Images, parse: parse_images }
    }
}

impl<T: Send + 'static> Source<T> for Feed<T> {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }
    fn provider(&self) -> &'static str {
        "bing"
    }
    fn search<'a>(&'a self, request: &'a SearchRequest) -> SourceFuture<'a, T> {
        Box::pin(async move { (self.parse)(&self.site.json(self.kind, request).await?, request.limit) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/verticals/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn real_news_and_videos_parse() {
        let news = parse_news(&fixture("ddg_news.json"), 10).unwrap();
        assert_eq!(news.len(), 5);
        assert!(news[0].url.starts_with("https://") && !news[0].title.is_empty());
        assert!(news[0].date.ends_with('Z') && news[0].date.starts_with("2026-"), "{}", news[0].date);
        assert!(!news[0].body.contains("<b>"), "highlighting is stripped: {}", news[0].body);
        let videos = parse_videos(&fixture("ddg_videos.json"), 10).unwrap();
        assert_eq!(videos[0].url, "https://www.youtube.com/watch?v=WBLpaS86Z9c");
        assert_eq!((videos[0].duration.as_str(), videos[0].views), ("9:20", Some(136_355)));
        assert!(videos[0].thumbnail.starts_with("https://"));
        assert_eq!(parse_news(&fixture("ddg_news.json"), 2).unwrap().len(), 2);
        assert!(matches!(parse_images(&serde_json::json!({}), 5), Err(EngineError::Parse(_))));
    }

    #[test]
    fn the_token_and_parameters_are_the_pages_own() {
        assert_eq!(vqd(r#"<script>vqd="4-1234567890";</script>"#).as_deref(), Some("4-1234567890"));
        assert_eq!(vqd("x&vqd=4-99&y").as_deref(), Some("4-99"));
        assert_eq!(vqd("nothing"), None);
        let req = SearchRequest::new("rust", 5).country(Some("IN")).time(Some(TimeLimit::Week)).page(2);
        let get = |kind, key: &str| params(kind, &req, "4-1").into_iter().find(|(k, _)| *k == key).map(|(_, v)| v);
        assert_eq!(get(Kind::News, "l").as_deref(), Some("in-en"));
        assert_eq!(get(Kind::News, "df").as_deref(), Some("w"));
        assert_eq!(get(Kind::News, "s").as_deref(), Some("30"));
        assert_eq!(get(Kind::Videos, "f").as_deref(), Some("publishedAfter:w,,,"));
        assert_eq!(get(Kind::Images, "f").as_deref(), Some("time:Week,,,,,"));
        assert_eq!(get(Kind::Images, "s").as_deref(), Some("100"));
    }
}

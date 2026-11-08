//! Bing's news (`/news/infinitescrollajax`) and images (`/images/async`): the
//! HTML fragments its result pages load as they scroll, from a fresh
//! [`browser`] session.
//! - A news card is a `div.newsitem` whose attributes carry the article's
//!   `url`, `data-title` and `data-author`; its age is a span's `aria-label`.
//! - An image's metadata is the JSON in its `a.iusc` link's `m` attribute
//!   (`t` title, `murl` image, `turl` thumbnail, `purl` page).

use scraper::{Html, Selector};
use serde_json::Value;

use super::{ImageResult, NewsResult, Source, SourceFuture, date_from_age, now_unix};
use crate::engine::{EngineError, SearchRequest, TimeLimit, browser};
use crate::text::squash;

const SITE: &str = "https://www.bing.com";

pub struct Bing {
    sessions: browser::Sessions,
    site: String,
}

impl Bing {
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

    async fn page(&self, path: &str, params: &[(&'static str, String)], marker: &str) -> Result<String, EngineError> {
        let (http, _) = self.sessions.open();
        browser::read_page(http.get(format!("{}{path}", self.site)).query(params), marker).await
    }
}

/// The news fragment's query: the query, the page (`first`, `SFX`), the
/// country `cc`, and a date filter `qft` (`interval="7"` past day, `"8"` past
/// week, `"9"` past month; Bing has no year filter, so a year is a month).
pub fn news_params(req: &SearchRequest) -> Vec<(&'static str, String)> {
    let page = req.page;
    let mut params = vec![
        ("q", req.query.clone()),
        ("InfiniteScroll", "1".into()),
        ("first", ((page - 1) * 10 + 1).to_string()),
        ("SFX", page.to_string()),
    ];
    if let Some(country) = &req.country {
        params.push(("cc", country.to_ascii_lowercase()));
    }
    if let Some(time) = req.time {
        let interval = match time {
            TimeLimit::Day => "7",
            TimeLimit::Week => "8",
            TimeLimit::Month | TimeLimit::Year => "9",
        };
        params.push(("qft", format!("interval=\"{interval}\"")));
    }
    params
}

/// The images fragment's query: the query, `count` per page and the page's
/// `first`, and an age filter `qft` in minutes.
pub fn images_params(req: &SearchRequest) -> Vec<(&'static str, String)> {
    let count = req.limit.max(35);
    let mut params = vec![
        ("q", req.query.clone()),
        ("async", "1".into()),
        ("first", ((req.page as usize - 1) * count + 1).to_string()),
        ("count", count.to_string()),
    ];
    if let Some(time) = req.time {
        let minutes = match time {
            TimeLimit::Day => 1_440,
            TimeLimit::Week => 10_080,
            TimeLimit::Month => 44_640,
            TimeLimit::Year => 525_600,
        };
        params.push(("qft", format!("+filterui:age-lt{minutes}")));
    }
    params
}

/// News cards from Bing's news fragment.
pub fn parse_news(page: &str, limit: usize, now: i64) -> Result<Vec<NewsResult>, EngineError> {
    let document = Html::parse_document(page);
    let card = Selector::parse("div.newsitem").expect("selector");
    let snippet = Selector::parse(".snippet").expect("selector");
    let age = Selector::parse("span[aria-label]").expect("selector");
    let image = Selector::parse("a[class*=image] img").expect("selector");
    let mut news = Vec::new();
    for item in document.select(&card) {
        let attr = |name: &str| item.value().attr(name).map(str::trim).unwrap_or("").to_string();
        let url = attr("url");
        if !url.starts_with("http") {
            continue;
        }
        let image = item
            .select(&image)
            .next()
            // A lazy-loaded image's address is in data-src; src is a placeholder.
            .and_then(|img| {
                let value = img.value();
                value.attr("data-src").or(value.attr("src")).filter(|src| !src.starts_with("data:"))
            })
            .map(|src| {
                if src.starts_with('/') {
                    format!("{SITE}{}", src.split('&').next().unwrap_or(src))
                } else {
                    src.to_string()
                }
            })
            .unwrap_or_default();
        news.push(NewsResult {
            title: attr("data-title"),
            url,
            body: item.select(&snippet).next().map(|s| squash(&s.text().collect::<String>())).unwrap_or_default(),
            date: item
                .select(&age)
                .next()
                .and_then(|s| s.value().attr("aria-label"))
                .map(|a| date_from_age(a, now))
                .unwrap_or_default(),
            source: attr("data-author"),
            image,
        });
        if news.len() >= limit {
            break;
        }
    }
    if news.is_empty() && !page.contains("newsitem") && !page.trim().is_empty() && !page.contains("<body></body>") {
        return Err(EngineError::Parse("no news cards".into()));
    }
    Ok(news)
}

/// Images from Bing's images fragment.
pub fn parse_images(page: &str, limit: usize) -> Result<Vec<ImageResult>, EngineError> {
    let document = Html::parse_document(page);
    let link = Selector::parse("a.iusc[m]").expect("selector");
    let mut images = Vec::new();
    for anchor in document.select(&link) {
        let Some(meta) = anchor.value().attr("m").and_then(|m| serde_json::from_str::<Value>(m).ok()) else { continue };
        let field = |key: &str| meta.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let image = field("murl");
        if !image.starts_with("http") {
            continue;
        }
        let page_url = field("purl");
        let source = url::Url::parse(&page_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()));
        // The full size is in the link's own href: `expw` and `exph`.
        let size = |key: &str| {
            anchor
                .value()
                .attr("href")
                .and_then(|href| url::Url::parse(&format!("{SITE}{href}")).ok())
                .and_then(|u| u.query_pairs().find(|(k, _)| k == key).and_then(|(_, v)| v.parse::<u32>().ok()))
        };
        images.push(ImageResult {
            title: field("t"),
            image,
            thumbnail: field("turl"),
            url: page_url,
            width: size("expw"),
            height: size("exph"),
            source: source.unwrap_or_default(),
        });
        if images.len() >= limit {
            break;
        }
    }
    if images.is_empty() && !page.contains("iusc") && page.len() > 2_000 {
        return Err(EngineError::Parse("no image results".into()));
    }
    Ok(images)
}

/// One of Bing's result kinds as a [`Source`]: which fragment, how to ask
/// for it, and its parser (given the time now, for ages).
pub struct Feed<T> {
    site: Bing,
    path: &'static str,
    marker: &'static str,
    params: fn(&SearchRequest) -> Vec<(&'static str, String)>,
    parse: fn(&str, usize, i64) -> Result<Vec<T>, EngineError>,
}

impl Bing {
    /// Its news fragment.
    pub fn news(self) -> Feed<NewsResult> {
        Feed {
            site: self,
            path: "/news/infinitescrollajax",
            marker: "newsitem",
            params: news_params,
            parse: parse_news,
        }
    }

    /// Its images fragment.
    pub fn images(self) -> Feed<ImageResult> {
        Feed {
            site: self,
            path: "/images/async",
            marker: "iusc",
            params: images_params,
            parse: |page, limit, _now| parse_images(page, limit),
        }
    }
}

impl<T: Send + 'static> Source<T> for Feed<T> {
    fn name(&self) -> &'static str {
        "bing"
    }
    fn search<'a>(&'a self, request: &'a SearchRequest) -> SourceFuture<'a, T> {
        Box::pin(async move {
            let page = self.site.page(self.path, &(self.params)(request), self.marker).await?;
            (self.parse)(&page, request.limit, now_unix())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEWS: &str = include_str!("../../tests/fixtures/verticals/bing_news.html");
    const IMAGES: &str = include_str!("../../tests/fixtures/verticals/bing_images.html");

    #[test]
    fn real_news_cards_parse() {
        let now = 1_794_092_400;
        let news = parse_news(NEWS, 10, now).unwrap();
        assert_eq!(news.len(), 5);
        assert!(news[0].url.starts_with("https://www.msn.com/"), "{}", news[0].url);
        assert_eq!(news[0].title, "Stop trusting hotel Wi-Fi: Build this Raspberry Pi travel router instead");
        assert_eq!(news[0].date, super::super::iso_from_unix(now - 3 * 86_400), "aria-label \"3 days ago\"");
        assert!(news[0].image.starts_with("https://www.bing.com/th?id="), "{}", news[0].image);
        assert!(!news[0].body.is_empty() && !news[0].source.is_empty(), "{:?}", news[0]);
    }

    #[test]
    fn real_images_parse_with_their_size() {
        let images = parse_images(IMAGES, 10).unwrap();
        assert_eq!(images.len(), 6);
        assert_eq!(images[0].image, "https://www.flatpanelshd.com/pictures/raspberrypi5_1_large.jpg");
        assert_eq!(images[0].source, "flatpanelshd.com");
        assert_eq!(images[0].title, "Raspberry Pi 5");
        assert!(images[0].thumbnail.starts_with("https://") && images[0].width.is_some(), "{:?}", images[0]);
        assert!(matches!(parse_images(&"<p>x</p>".repeat(500), 5), Err(EngineError::Parse(_))));
    }

    #[test]
    fn options_become_bings_parameters() {
        let req = SearchRequest::new("rust", 5).country(Some("gb")).time(Some(TimeLimit::Week)).page(2);
        let news = news_params(&req);
        assert!(news.contains(&("first", "11".into())) && news.contains(&("cc", "gb".into())));
        assert!(news.contains(&("qft", "interval=\"8\"".into())));
        let images = images_params(&req);
        assert!(images.contains(&("first", "36".into())) && images.contains(&("qft", "+filterui:age-lt10080".into())));
    }
}

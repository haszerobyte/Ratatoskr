//! Bing's web results (www.bing.com/search): a GET with the query, the
//! country `cc` and the options its own controls set, from a fresh
//! [`super::browser`] client; results in `li.b_algo`. Each link is a
//! `bing.com/ck/a?…&u=a1<base64url>` click-through whose `u` carries the
//! destination, decoded here.

use scraper::{Html, Selector};

use super::{Engine, EngineError, Hit, SafeSearch, SearchFuture, SearchRequest, TimeLimit, browser};
use crate::text::squash;

pub struct Bing {
    sessions: browser::Sessions,
    endpoint: String,
}

impl Bing {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), endpoint: "https://www.bing.com/search".into() }
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: wreq::Client, endpoint: &str) -> Self {
        Self { sessions: browser::Sessions::Fixed(http, browser::Family::Chromium), endpoint: endpoint.to_string() }
    }
}

impl Engine for Bing {
    fn name(&self) -> &'static str {
        "bing"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move { self.search_request(&SearchRequest::new(query, limit)).await })
    }

    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        Box::pin(async move { self.search_request(&SearchRequest::new(query, limit).country(country)).await })
    }

    fn search_request<'a>(&'a self, req: &'a SearchRequest) -> SearchFuture<'a> {
        Box::pin(async move {
            let (http, family) = self.sessions.open();
            let mut request = http.get(&self.endpoint).query(&params(req, today()));
            if let Some(language) = req.country.as_deref().and_then(|c| browser::accept_language(c, family)) {
                request = request.header("accept-language", language);
            }
            let body = browser::read_page(request, "b_algo").await?;
            parse(&body, req.limit)
        })
    }
}

/// Days since the Unix epoch: Bing's year filter is a range of days.
fn today() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0)
}

/// The query string Bing's own search box and filters send: the query `q`
/// (and `pq`), the country `cc`, safe search `adlt`, a date filter `filters`
/// (`ez1` past day, `ez2` week, `ez3` month, `ez5_<from>_<to>` a range of
/// days), and the next page's first result `first`. Bing personalises results
/// by location and shifts them between any two cookieless requests, so
/// whether it honours the date filter and the page from a fresh session could
/// not be confirmed; they are what its own page sends.
pub fn params(req: &SearchRequest, today: u64) -> Vec<(&'static str, String)> {
    let mut params = vec![("q", req.query.clone()), ("pq", req.query.clone())];
    if let Some(country) = &req.country {
        params.push(("cc", country.to_ascii_lowercase()));
    }
    params.push((
        "adlt",
        match req.safesearch {
            SafeSearch::On => "strict",
            SafeSearch::Moderate => "moderate",
            SafeSearch::Off => "off",
        }
        .into(),
    ));
    if let Some(time) = req.time {
        let code = match time {
            TimeLimit::Day => "ez1".to_string(),
            TimeLimit::Week => "ez2".to_string(),
            TimeLimit::Month => "ez3".to_string(),
            TimeLimit::Year => format!("ez5_{}_{today}", today.saturating_sub(365)),
        };
        params.push(("filters", format!("ex1:\"{code}\"")));
    }
    if req.page > 1 {
        params.push(("first", ((req.page - 1) * 10 + 1).to_string()));
    }
    params
}

/// Results from a Bing results page; an unrecognised page is a Parse error.
pub fn parse(page: &str, limit: usize) -> Result<Vec<Hit>, EngineError> {
    let document = Html::parse_document(page);
    let item = Selector::parse("li.b_algo").expect("selector");
    let link = Selector::parse("h2 a[href]").expect("selector");
    let snippet = Selector::parse("p").expect("selector");
    let mut hits = Vec::new();
    for block in document.select(&item) {
        let Some(anchor) = block.select(&link).next() else { continue };
        let Some(url) = anchor.value().attr("href").and_then(real_url) else { continue };
        hits.push(Hit {
            title: squash(&anchor.text().collect::<String>()),
            url,
            snippet: block.select(&snippet).next().map(|p| squash(&p.text().collect::<String>())).unwrap_or_default(),
        });
        if hits.len() >= limit {
            break;
        }
    }
    if hits.is_empty() && !page.contains("b_no") && !page.contains("There are no results") {
        return Err(EngineError::Parse("no result items".into()));
    }
    Ok(hits)
}

/// A result's destination: Bing's `ck/a` click-through unwrapped (its `u` is
/// `a1` and the URL in base64url), ads dropped, anything not http(s) dropped.
fn real_url(href: &str) -> Option<String> {
    let parsed = url::Url::parse(href).ok()?;
    let host = parsed.host_str().unwrap_or("");
    if host.ends_with("bing.com") && parsed.path().starts_with("/aclick") {
        return None;
    }
    let target = if host.ends_with("bing.com") && parsed.path().starts_with("/ck/a") {
        let u = parsed.query_pairs().find(|(k, _)| k == "u").map(|(_, v)| v.into_owned())?;
        String::from_utf8(base64url(u.strip_prefix("a1")?)?).ok()?
    } else {
        href.to_string()
    };
    let target = url::Url::parse(&target).ok()?;
    matches!(target.scheme(), "http" | "https").then(|| target.to_string())
}

/// Decode unpadded base64url; None on a character outside its alphabet.
fn base64url(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|&c| c != b'=') {
        buffer = (buffer << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../../tests/fixtures/meta/bing_results.html");

    #[test]
    fn a_real_results_page_parses_with_unwrapped_links() {
        let hits = parse(RESULTS, 10).unwrap();
        assert!(hits.len() >= 5, "{}", hits.len());
        assert!(hits.iter().all(|h| h.url.starts_with("http") && !h.url.contains("bing.com/ck/")), "{hits:?}");
        assert_eq!(hits[0].url, "https://www.raspberrypi.com/");
        assert!(!hits[0].title.is_empty() && !hits[0].snippet.is_empty(), "{:?}", hits[0]);
        assert_eq!(parse(RESULTS, 2).unwrap().len(), 2);
        assert!(matches!(parse("<html><body>new layout</body></html>", 5), Err(EngineError::Parse(_))));
    }

    #[test]
    fn links_unwrap_and_ads_drop() {
        assert_eq!(
            real_url("https://www.bing.com/ck/a?!&p=x&u=a1aHR0cHM6Ly90b2tpby5ycy8&ntb=1").as_deref(),
            Some("https://tokio.rs/")
        );
        assert_eq!(real_url("https://www.bing.com/aclick?ld=x"), None);
        assert_eq!(real_url("https://www.bing.com/ck/a?u=a1amF2YXNjcmlwdDp4"), None, "javascript: is dropped");
        assert_eq!(real_url("https://example.com/direct").as_deref(), Some("https://example.com/direct"));
        assert_eq!(base64url("aGk").as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn options_become_bings_own_parameters() {
        let req = SearchRequest::new("rust", 5)
            .country(Some("in"))
            .time(Some(TimeLimit::Year))
            .page(3)
            .safesearch(SafeSearch::On);
        let params = params(&req, 20_000);
        let get = |key: &str| params.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        assert_eq!(get("cc"), Some("in"));
        assert_eq!(get("adlt"), Some("strict"));
        assert_eq!(get("filters"), Some("ex1:\"ez5_19635_20000\""));
        assert_eq!(get("first"), Some("21"));
        let plain = params_of_plain();
        assert!(!plain.iter().any(|(k, _)| *k == "filters" || *k == "first" || *k == "cc"), "{plain:?}");
    }

    fn params_of_plain() -> Vec<(&'static str, String)> {
        params(&SearchRequest::new("rust", 5), 1)
    }
}

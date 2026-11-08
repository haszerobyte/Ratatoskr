//! DuckDuckGo's HTML endpoint (html.duckduckgo.com/html), ddgs's "duckduckgo"
//! engine: one POST with ddgs's payload (`q`, `b`, `l`) from a
//! [`super::browser`] client, parsed from the server-rendered results; ads
//! are skipped.
//!
//! DuckDuckGo answers automated traffic it dislikes with HTTP 202 and an
//! "anomaly" challenge page instead of results. That is a refusal: we do not
//! try to get past it, we stop asking for a while.

use scraper::{Html, Selector};

use super::{Engine, EngineError, Hit, SearchFuture, browser};
use crate::text::squash;

pub struct DuckDuckGo {
    sessions: browser::Sessions,
    endpoint: String,
}

impl DuckDuckGo {
    /// A fresh browser for every search ([`browser::Sessions`]).
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { sessions: browser::Sessions::Fresh(timeout), endpoint: "https://html.duckduckgo.com/html/".into() }
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: wreq::Client, endpoint: &str) -> Self {
        Self { sessions: browser::Sessions::Fixed(http, browser::Family::Chromium), endpoint: endpoint.to_string() }
    }
}

impl Engine for DuckDuckGo {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        self.search_in(query, limit, None)
    }

    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        Box::pin(async move {
            // The search box on the same page posts this form; only Chrome
            // adds cache-control to a form submit.
            let (http, family) = self.sessions.open();
            let mut form = http.post(&self.endpoint);
            if family == browser::Family::Chromium {
                form = form.header("cache-control", "max-age=0");
            }
            if let Some(language) = country.and_then(|c| browser::accept_language(c, family)) {
                form = form.header("accept-language", language);
            }
            let request =
                browser::same_site(form, &self.endpoint).form(&[("q", query), ("b", ""), ("l", region(country))]);
            let body = browser::read_page(request, "result__a").await?;
            parse(&body, limit)
        })
    }
}

/// DuckDuckGo's region for a country: its own codes (GB is "uk"), one
/// language per country. A country it has no region for searches worldwide.
pub fn region(country: Option<&str>) -> &'static str {
    const REGIONS: &[&str] = &[
        "ar-es", "at-de", "au-en", "be-nl", "bg-bg", "br-pt", "ca-en", "ch-de", "cl-es", "cn-zh", "co-es", "cz-cs",
        "de-de", "dk-da", "ee-et", "es-es", "fi-fi", "fr-fr", "gr-el", "hk-tzh", "hr-hr", "hu-hu", "id-en", "ie-en",
        "il-he", "in-en", "it-it", "jp-jp", "kr-kr", "lt-lt", "lv-lv", "mx-es", "my-en", "nl-nl", "no-no", "nz-en",
        "pe-es", "ph-en", "pl-pl", "pt-pt", "ro-ro", "ru-ru", "se-sv", "sg-en", "sk-sk", "th-th", "tr-tr", "tw-tzh",
        "ua-uk", "uk-en", "us-en", "ve-es", "vn-vi", "za-en",
    ];
    let Some(country) = country else { return "us-en" };
    let code = match country.to_ascii_lowercase().as_str() {
        "gb" => "uk".to_string(),
        other => other.to_string(),
    };
    REGIONS.iter().find(|r| r.split('-').next() == Some(code.as_str())).copied().unwrap_or("wt-wt")
}

/// Results from an html.duckduckgo.com page. A page with neither results nor
/// the "No results" notice is a layout we don't understand: an error, so a
/// silent layout change shows up instead of reading as "nothing found".
pub fn parse(page: &str, limit: usize) -> Result<Vec<Hit>, EngineError> {
    let document = Html::parse_document(page);
    let result = Selector::parse("div.result").expect("selector");
    let title = Selector::parse("a.result__a").expect("selector");
    let snippet = Selector::parse(".result__snippet").expect("selector");
    let mut hits = Vec::new();
    for block in document.select(&result) {
        if block.value().classes().any(|c| c == "result--ad") {
            continue;
        }
        let Some(link) = block.select(&title).next() else { continue };
        let Some(url) = link.value().attr("href").and_then(real_url) else { continue };
        hits.push(Hit {
            title: squash(&link.text().collect::<String>()),
            url,
            snippet: block.select(&snippet).next().map(|s| squash(&s.text().collect::<String>())).unwrap_or_default(),
        });
        if hits.len() >= limit {
            break;
        }
    }
    if hits.is_empty() && !page.contains("No results") && !page.contains("no-results") {
        return Err(EngineError::Parse("no result blocks".into()));
    }
    Ok(hits)
}

/// A result's destination: direct links as-is; the older `/l/?uddg=` redirect
/// form unwrapped. Anything that is not http(s) is dropped.
fn real_url(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    let parsed = url::Url::parse(&absolute).ok()?;
    if parsed.host_str().is_some_and(|h| h.ends_with("duckduckgo.com")) && parsed.path().starts_with("/l/") {
        let target = parsed.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned())?;
        return real_url(&target);
    }
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../../tests/fixtures/ddg_html_results.html");
    const ANOMALY: &str = include_str!("../../tests/fixtures/ddg_anomaly_202.html");

    #[test]
    fn a_real_results_page_parses() {
        let hits = parse(RESULTS, 10).unwrap();
        assert_eq!(hits.len(), 10);
        assert_eq!(
            hits[0],
            Hit {
                title: "Rust Programming Language".into(),
                url: "https://rust-lang.org/".into(),
                snippet: "A language empowering everyone to build reliable and efficient software.".into(),
            }
        );
        assert!(hits.iter().all(|h| h.url.starts_with("http")));
        assert_eq!(parse(RESULTS, 3).unwrap().len(), 3);
    }

    #[test]
    fn countries_map_to_duckduckgos_regions() {
        assert_eq!(region(None), "us-en");
        assert_eq!(region(Some("IN")), "in-en");
        assert_eq!(region(Some("gb")), "uk-en");
        assert_eq!(region(Some("KE")), "wt-wt", "no region: worldwide");
    }

    #[test]
    fn a_country_sets_the_browsers_language_in_its_familys_format() {
        use browser::{Family, accept_language};
        assert_eq!(accept_language("IN", Family::Chromium).as_deref(), Some("en-IN,en;q=0.9"));
        assert_eq!(accept_language("de", Family::Chromium).as_deref(), Some("de-DE,de;q=0.9,en-US;q=0.8,en;q=0.7"));
        assert_eq!(accept_language("JP", Family::Firefox).as_deref(), Some("ja-JP,ja;q=0.8,en-US;q=0.5,en;q=0.3"));
        assert_eq!(accept_language("US", Family::Chromium), None, "the profile's own default");
        assert_eq!(accept_language("KE", Family::Chromium), None);
    }

    #[test]
    fn the_real_anomaly_page_is_recognised_as_a_challenge() {
        assert!(browser::is_challenge(ANOMALY));
        assert!(!browser::is_challenge(RESULTS));
    }

    #[test]
    fn redirects_are_unwrapped_and_odd_links_dropped() {
        assert_eq!(
            real_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&rut=x").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(real_url("javascript:alert(1)"), None);
        assert_eq!(real_url("https://example.org/x").as_deref(), Some("https://example.org/x"));
    }

    #[test]
    fn ads_are_skipped_and_layout_changes_are_errors() {
        let page = r#"<div class="result result--ad"><a class="result__a" href="https://ad.example/">Ad</a></div>
                      <div class="result"><a class="result__a" href="https://real.example/">Real</a></div>"#;
        let hits = parse(page, 10).unwrap();
        assert_eq!(hits.iter().map(|h| h.url.as_str()).collect::<Vec<_>>(), ["https://real.example/"]);
        assert!(matches!(parse("<html><body>redesigned</body></html>", 10), Err(EngineError::Parse(_))));
        assert_eq!(parse("<div class='no-results'>No results.</div>", 10), Ok(vec![]));
    }
}

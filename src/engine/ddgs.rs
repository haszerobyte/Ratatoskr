//! The ddgs package's own API server (`ddgs api`, ddgs/api_server/api.py),
//! running as a local service. ddgs is what makes search dependable: it
//! spreads queries across many engines (bing, brave, duckduckgo, google,
//! mojeek, startpage, yahoo, yandex, …) with `backend="auto"` and its parsers
//! are maintained upstream. Ratatoskr talks to it over loopback only.
//!
//! Start it bound to loopback — its default host is 0.0.0.0, which would put
//! an unauthenticated search relay on the LAN:
//! `ddgs api --host 127.0.0.1 --port 8765`.

use serde_json::{Value, json};

use super::{Engine, EngineError, Hit, SearchFuture, status_error, transport};

pub struct Ddgs {
    http: reqwest::Client,
    base: String,
}

impl Ddgs {
    /// `base` like `http://127.0.0.1:8765`.
    pub fn new(http: reqwest::Client, base: &str) -> Self {
        Self { http, base: base.trim_end_matches('/').to_string() }
    }
}

impl Engine for Ddgs {
    fn name(&self) -> &'static str {
        "ddgs"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move {
            let reply = self
                .http
                .post(format!("{}/search/text", self.base))
                .json(&json!({"query": query, "max_results": limit.clamp(1, 25), "backend": "auto",
                               "region": "wt-wt", "safesearch": "moderate"}))
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                // ddgs reports "no results" from every backend as an error status.
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(transport)?;
            parse(&body)
        })
    }
}

pub fn parse(body: &Value) -> Result<Vec<Hit>, EngineError> {
    let results = body.get("results").and_then(Value::as_array).ok_or_else(|| EngineError::Parse("no results field".into()))?;
    Ok(results
        .iter()
        .filter_map(|item| {
            let url = item.get("href").or_else(|| item.get("url"))?.as_str()?.to_string();
            Some(Hit {
                title: item.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                snippet: item.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
                url,
            })
        })
        .filter(|hit| hit.url.starts_with("http") && !is_ad_redirect(&hit.url))
        .collect())
}

/// Search-engine ad click-throughs ddgs can pass on as results: Bing's
/// `/aclick`, DuckDuckGo's `y.js`, Google's `/aclk`.
fn is_ad_redirect(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return true };
    let host = parsed.host_str().unwrap_or("");
    let path = parsed.path();
    (host.ends_with("bing.com") && path.starts_with("/aclick"))
        || (host.ends_with("duckduckgo.com") && path.starts_with("/y.js"))
        || (host.ends_with("google.com") && path.starts_with("/aclk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddgs_text_results_map_to_hits() {
        let body = json!({"results": [
            {"title": "Rust", "href": "https://rust-lang.org/", "body": "A language"},
            {"title": "odd", "href": "javascript:x", "body": ""},
            {"title": "Ad", "href": "https://www.bing.com/aclick?ld=e8_KUCT&u=x", "body": "ad"}]});
        assert_eq!(parse(&body).unwrap(), vec![Hit { title: "Rust".into(), url: "https://rust-lang.org/".into(), snippet: "A language".into() }]);
        assert!(matches!(parse(&json!({"detail": "x"})), Err(EngineError::Parse(_))));
    }
}

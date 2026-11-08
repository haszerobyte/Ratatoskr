//! Grokipedia's typeahead API: the one best encyclopedia page for a query,
//! added to every search next to Wikipedia's. JSON, so the plain client (our User-Agent) is enough.

use serde_json::Value;

use super::{Engine, EngineError, Hit, SearchFuture, status_error, transport};

pub struct Grokipedia {
    http: reqwest::Client,
    endpoint: String,
}

impl Grokipedia {
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_endpoint(http, "https://grokipedia.com/api/typeahead")
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: reqwest::Client, endpoint: &str) -> Self {
        Self { http, endpoint: endpoint.to_string() }
    }
}

impl Engine for Grokipedia {
    fn name(&self) -> &'static str {
        "grokipedia"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move {
            let reply = self
                .http
                .get(&self.endpoint)
                .query(&[("query", query), ("limit", &limit.clamp(1, 10).to_string())])
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(|e| EngineError::Parse(e.to_string()))?;
            parse(&body, limit)
        })
    }
}

/// Hits from a typeahead answer: the page at `/page/<slug>`, the snippet's
/// first paragraph dropped when there are more (it repeats the title).
pub fn parse(body: &Value, limit: usize) -> Result<Vec<Hit>, EngineError> {
    let results =
        body.get("results").and_then(Value::as_array).ok_or_else(|| EngineError::Parse("no results field".into()))?;
    Ok(results
        .iter()
        .filter_map(|item| {
            let slug = item.get("slug")?.as_str()?;
            let snippet = item.get("snippet").and_then(Value::as_str).unwrap_or("");
            Some(Hit {
                title: item.get("title").and_then(Value::as_str).unwrap_or(slug).trim_matches('_').to_string(),
                url: format!("https://grokipedia.com/page/{slug}"),
                snippet: snippet.split_once("\n\n").map_or(snippet, |(_, rest)| rest).to_string(),
            })
        })
        .take(limit)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_answer_parses() {
        let body: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/meta/grokipedia_typeahead.json")).unwrap();
        let hits = parse(&body, 5).unwrap();
        assert_eq!(hits[0].url, "https://grokipedia.com/page/Raspberry_Pi_OS");
        assert_eq!(hits[0].title, "Raspberry Pi OS");
        assert!(hits[0].snippet.starts_with("Raspberry Pi OS is a free"));
        assert!(matches!(parse(&serde_json::json!({}), 5), Err(EngineError::Parse(_))));
        let two = serde_json::json!({"results": [{"slug": "A_", "title": "_A_", "snippet": "A\n\nAbout A."}]});
        assert_eq!(parse(&two, 5).unwrap()[0].snippet, "About A.");
    }
}

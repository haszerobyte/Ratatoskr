//! Wikipedia's official search API (MediaWiki `action=query&list=search`):
//! a sanctioned source, good for factual questions, and the fallback when
//! DuckDuckGo is cooling down.

use serde_json::Value;

use super::{Engine, EngineError, Hit, SearchFuture, status_error, transport};

pub struct Wikipedia {
    http: reqwest::Client,
    api: String,
    site: String,
}

impl Wikipedia {
    /// `lang` selects the wiki, e.g. "en".
    pub fn new(http: reqwest::Client, lang: &str) -> Self {
        let site = format!("https://{lang}.wikipedia.org");
        Self { http, api: format!("{site}/w/api.php"), site }
    }

    /// Tests point this at a fake.
    pub fn with_api(http: reqwest::Client, api: &str, site: &str) -> Self {
        Self { http, api: api.to_string(), site: site.to_string() }
    }
}

impl Engine for Wikipedia {
    fn name(&self) -> &'static str {
        "wikipedia"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move {
            let limit = limit.clamp(1, 20).to_string();
            let reply = self
                .http
                .get(&self.api)
                .query(&[("action", "query"), ("list", "search"), ("srsearch", query), ("srlimit", &limit),
                         ("format", "json"), ("formatversion", "2"), ("srprop", "snippet")])
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(transport)?;
            parse(&body, &self.site)
        })
    }
}

pub fn parse(body: &Value, site: &str) -> Result<Vec<Hit>, EngineError> {
    if let Some(error) = body.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
        return Err(if code == "ratelimited" { EngineError::Refused(code.into()) } else { EngineError::Parse(code.into()) });
    }
    let results = body.pointer("/query/search").and_then(Value::as_array).ok_or_else(|| EngineError::Parse("no query.search".into()))?;
    Ok(results
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let page = title.replace(' ', "_");
            // Snippets carry <span class="searchmatch"> highlighting and entities.
            let snippet = item.get("snippet").and_then(Value::as_str).map(crate::text::fragment_text).unwrap_or_default();
            Some(Hit { url: format!("{site}/wiki/{}", url_path(&page)), title, snippet })
        })
        .collect())
}

fn url_path(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect::<String>().replace('+', "_")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn results_become_article_links_with_clean_snippets() {
        let body = json!({"query": {"search": [
            {"title": "Rust (programming language)", "snippet": "<span class=\"searchmatch\">Rust</span> is a general-purpose &amp; fast"}]}});
        assert_eq!(parse(&body, "https://en.wikipedia.org").unwrap(), vec![Hit {
            title: "Rust (programming language)".into(),
            url: "https://en.wikipedia.org/wiki/Rust_%28programming_language%29".into(),
            snippet: "Rust is a general-purpose & fast".into(),
        }]);
        assert_eq!(parse(&json!({"query": {"search": []}}), "s").unwrap(), vec![]);
        assert!(parse(&json!({"error": {"code": "ratelimited"}}), "s").unwrap_err().is_refusal());
        assert!(matches!(parse(&json!({}), "s"), Err(EngineError::Parse(_))));
    }
}

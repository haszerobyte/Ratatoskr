//! Wikipedia's official search API, in two modes:
//! - full-text search (`action=query&list=search`): a sanctioned source, good
//!   for factual questions, and the last fallback of a search;
//! - the best page ([`Wikipedia::best_page`]), the meta-search's encyclopedia
//!   slot: the article whose title
//!   matches the query (`action=opensearch`, fuzzy), with its intro as the
//!   snippet, and nothing for a disambiguation page. Full-text search there put
//!   any article that mentions the words first: "tokio select macro" came back
//!   as an artist's biography that mentions Tokio.

use serde_json::Value;

use super::{Engine, EngineError, Hit, SearchFuture, status_error, transport};

pub struct Wikipedia {
    http: reqwest::Client,
    api: String,
    site: String,
    best_page: bool,
}

impl Wikipedia {
    /// `lang` selects the wiki, e.g. "en".
    pub fn new(http: reqwest::Client, lang: &str) -> Self {
        let site = format!("https://{lang}.wikipedia.org");
        Self { http, api: format!("{site}/w/api.php"), site, best_page: false }
    }

    /// The one article whose title matches a query, or none (see the module docs).
    pub fn best_page(http: reqwest::Client, lang: &str) -> Self {
        Self { best_page: true, ..Self::new(http, lang) }
    }

    /// Tests point this at a fake.
    pub fn with_api(http: reqwest::Client, api: &str, site: &str) -> Self {
        Self { http, api: api.to_string(), site: site.to_string(), best_page: false }
    }

    /// [`Wikipedia::best_page`] against a fake.
    pub fn best_page_with_api(http: reqwest::Client, api: &str) -> Self {
        Self { best_page: true, ..Self::with_api(http, api, "") }
    }

    async fn get(&self, query: &[(&str, &str)]) -> Result<Value, EngineError> {
        let reply = self.http.get(&self.api).query(query).send().await.map_err(transport)?;
        if !reply.status().is_success() {
            return Err(status_error(reply.status().as_u16()));
        }
        reply.json().await.map_err(transport)
    }

    async fn find_best_page(&self, query: &str) -> Result<Vec<Hit>, EngineError> {
        let found = self
            .get(&[
                ("action", "opensearch"),
                ("profile", "fuzzy"),
                ("limit", "1"),
                ("search", query),
                ("format", "json"),
            ])
            .await?;
        let Some((title, url)) = opensearch_first(&found) else { return Ok(Vec::new()) };
        let extract = self
            .get(&[
                ("action", "query"),
                ("prop", "extracts"),
                ("exintro", "1"),
                ("explaintext", "1"),
                ("redirects", "1"),
                ("titles", title.as_str()),
                ("format", "json"),
                ("formatversion", "2"),
            ])
            .await?;
        let intro = extract.pointer("/query/pages/0/extract").and_then(Value::as_str).unwrap_or("");
        Ok(best_page_hit(title, url, intro).into_iter().collect())
    }
}

/// The first title and URL of an opensearch answer: `[query, [titles], [descriptions], [urls]]`.
pub fn opensearch_first(body: &Value) -> Option<(String, String)> {
    let title = body.get(1)?.get(0)?.as_str()?.to_string();
    let url = body.get(3)?.get(0)?.as_str()?.to_string();
    Some((title, url))
}

/// The hit for an article and its intro, or none for a disambiguation page.
/// The snippet is the intro's first 400 characters, cut at a word.
pub fn best_page_hit(title: String, url: String, intro: &str) -> Option<Hit> {
    if intro.contains("may refer to:") {
        return None;
    }
    let intro = crate::text::squash(intro);
    let snippet = if intro.chars().count() <= 400 {
        intro
    } else {
        let cut: String = intro.chars().take(400).collect();
        format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head))
    };
    Some(Hit { title, url, snippet })
}

impl Engine for Wikipedia {
    fn name(&self) -> &'static str {
        "wikipedia"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        Box::pin(async move {
            if self.best_page {
                return self.find_best_page(query).await;
            }
            let limit = limit.clamp(1, 20).to_string();
            let reply = self
                .http
                .get(&self.api)
                .query(&[
                    ("action", "query"),
                    ("list", "search"),
                    ("srsearch", query),
                    ("srlimit", &limit),
                    ("format", "json"),
                    ("formatversion", "2"),
                    ("srprop", "snippet"),
                ])
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
        return Err(if code == "ratelimited" {
            EngineError::Refused(code.into())
        } else {
            EngineError::Parse(code.into())
        });
    }
    let results = body
        .pointer("/query/search")
        .and_then(Value::as_array)
        .ok_or_else(|| EngineError::Parse("no query.search".into()))?;
    Ok(results
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let page = title.replace(' ', "_");
            // Snippets carry <span class="searchmatch"> highlighting and entities.
            let snippet =
                item.get("snippet").and_then(Value::as_str).map(crate::text::fragment_text).unwrap_or_default();
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
        assert_eq!(
            parse(&body, "https://en.wikipedia.org").unwrap(),
            vec![Hit {
                title: "Rust (programming language)".into(),
                url: "https://en.wikipedia.org/wiki/Rust_%28programming_language%29".into(),
                snippet: "Rust is a general-purpose & fast".into(),
            }]
        );
        assert_eq!(parse(&json!({"query": {"search": []}}), "s").unwrap(), vec![]);
        assert!(parse(&json!({"error": {"code": "ratelimited"}}), "s").unwrap_err().is_refusal());
        assert!(matches!(parse(&json!({}), "s"), Err(EngineError::Parse(_))));
    }
}

#[cfg(test)]
mod best_page_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_title_match_with_its_intro_and_never_a_disambiguation_page() {
        assert_eq!(opensearch_first(&json!(["tokio select macro", [], [], []])), None);
        let found =
            json!(["Raspberry Pi OS", ["Raspberry Pi OS"], [""], ["https://en.wikipedia.org/wiki/Raspberry_Pi_OS"]]);
        let (title, url) = opensearch_first(&found).unwrap();
        let hit = best_page_hit(title, url, "Raspberry Pi OS is a Unix-like\n operating system.").unwrap();
        assert_eq!(hit.url, "https://en.wikipedia.org/wiki/Raspberry_Pi_OS");
        assert_eq!(hit.snippet, "Raspberry Pi OS is a Unix-like operating system.");
        assert_eq!(best_page_hit("Mercury".into(), "u".into(), "Mercury may refer to:"), None);
        let long = "word ".repeat(200);
        let snippet = best_page_hit("T".into(), "u".into(), &long).unwrap().snippet;
        assert!(snippet.chars().count() <= 401 && snippet.ends_with("word…"), "{snippet}");
    }
}

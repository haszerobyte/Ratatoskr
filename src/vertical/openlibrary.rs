//! Books from Open Library's search API (openlibrary.org/search.json): the
//! Internet Archive's open catalogue, with an official, keyless JSON API, so
//! the plain client and ratatoskr's own User-Agent. Covers come from
//! covers.openlibrary.org.

use serde_json::Value;

use super::{BookResult, Source, SourceFuture};
use crate::engine::{EngineError, SearchRequest, status_error, transport};

pub struct OpenLibrary {
    http: reqwest::Client,
    endpoint: String,
    site: String,
}

impl OpenLibrary {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, endpoint: "https://openlibrary.org/search.json".into(), site: "https://openlibrary.org".into() }
    }

    /// Tests point this at a fake.
    pub fn with_endpoint(http: reqwest::Client, endpoint: &str) -> Self {
        Self { http, endpoint: endpoint.into(), site: "https://openlibrary.org".into() }
    }
}

/// Authors trimmed, each once, in order: the catalogue repeats a co-author
/// and pads some names.
fn authors(names: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    names.into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty() && seen.insert(n.clone())).collect()
}

/// Books from a search.json answer.
pub fn parse(body: &Value, site: &str, limit: usize) -> Result<Vec<BookResult>, EngineError> {
    let docs = body.get("docs").and_then(Value::as_array).ok_or_else(|| EngineError::Parse("no docs field".into()))?;
    let strings = |item: &Value, key: &str| -> Vec<String> {
        item.get(key)
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    Ok(docs
        .iter()
        .filter_map(|item| {
            let key = item.get("key")?.as_str()?;
            let editions = item.get("edition_count").and_then(Value::as_u64);
            let languages = strings(item, "language");
            let mut info = Vec::new();
            if let Some(n) = editions {
                info.push(format!("{n} edition{}", if n == 1 { "" } else { "s" }));
            }
            if !languages.is_empty() {
                info.push(languages.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
            }
            Some(BookResult {
                title: item.get("title")?.as_str()?.to_string(),
                authors: authors(strings(item, "author_name")),
                publisher: strings(item, "publisher").into_iter().next().unwrap_or_default(),
                year: item.get("first_publish_year").and_then(Value::as_i64).and_then(|y| i32::try_from(y).ok()),
                url: format!("{site}{key}"),
                thumbnail: item
                    .get("cover_i")
                    .and_then(Value::as_i64)
                    .map(|id| format!("https://covers.openlibrary.org/b/id/{id}-M.jpg"))
                    .unwrap_or_default(),
                info: info.join("; "),
            })
        })
        .take(limit)
        .collect())
}

impl Source<BookResult> for OpenLibrary {
    fn name(&self) -> &'static str {
        "openlibrary"
    }
    fn search<'a>(&'a self, request: &'a SearchRequest) -> SourceFuture<'a, BookResult> {
        Box::pin(async move {
            let limit = request.limit.clamp(1, 100).to_string();
            let page = request.page.to_string();
            let fields = "key,title,author_name,publisher,first_publish_year,cover_i,edition_count,language";
            let reply = self
                .http
                .get(&self.endpoint)
                .query(&[("q", request.query.as_str()), ("limit", &limit), ("page", &page), ("fields", fields)])
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(transport)?;
            parse(&body, &self.site, request.limit)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_answer_parses() {
        let body: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/verticals/openlibrary.json")).unwrap();
        let books = parse(&body, "https://openlibrary.org", 10).unwrap();
        assert_eq!(books.len(), 4);
        assert!(books[0].url.starts_with("https://openlibrary.org/works/"), "{}", books[0].url);
        assert!(!books[0].title.is_empty() && !books[0].authors.is_empty(), "{:?}", books[0]);
        assert!(books.iter().any(|b| b.thumbnail.starts_with("https://covers.openlibrary.org/b/id/")));
        assert!(books.iter().any(|b| b.year.is_some()));
        assert!(matches!(parse(&serde_json::json!({}), "s", 5), Err(EngineError::Parse(_))));
        assert_eq!(authors(vec!["A".into(), " B".into(), "A".into(), " ".into()]), ["A", "B"]);
    }
}

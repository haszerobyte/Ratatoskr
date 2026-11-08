//! Brave's Search API (api.search.brave.com, `X-Subscription-Token`): a keyed
//! API built for programmatic use. Used first when a key is configured.

use serde_json::Value;

use super::{Engine, Hit, SearchFuture, status_error, transport};

pub struct Brave {
    http: reqwest::Client,
    key: String,
    endpoint: String,
}

impl Brave {
    pub fn new(http: reqwest::Client, key: &str) -> Self {
        Self::with_endpoint(http, key, "https://api.search.brave.com/res/v1/web/search")
    }
    pub fn with_endpoint(http: reqwest::Client, key: &str, endpoint: &str) -> Self {
        Self { http, key: key.to_string(), endpoint: endpoint.to_string() }
    }
}

impl Engine for Brave {
    fn name(&self) -> &'static str {
        "brave"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        self.search_in(query, limit, None)
    }

    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        Box::pin(async move {
            let count = limit.clamp(1, 20).to_string();
            let mut params = vec![("q", query), ("count", count.as_str())];
            params.extend(country.map(|c| ("country", c)));
            let reply = self
                .http
                .get(&self.endpoint)
                .header("X-Subscription-Token", &self.key)
                .header("Accept", "application/json")
                .query(&params)
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(transport)?;
            Ok(parse(&body))
        })
    }
}

pub fn parse(body: &Value) -> Vec<Hit> {
    body.pointer("/web/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|item| {
                    Some(Hit {
                        title: item.get("title")?.as_str()?.to_string(),
                        url: item.get("url")?.as_str()?.to_string(),
                        // Descriptions carry <strong> highlighting.
                        snippet: item
                            .get("description")
                            .and_then(Value::as_str)
                            .map(crate::text::fragment_text)
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

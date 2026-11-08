//! Google's Custom Search JSON API (customsearch/v1): Google's sanctioned way
//! to query it programmatically. Needs an API key and a search-engine id (cx);
//! the free tier is 100 queries a day, and a spent quota is a refusal.

use serde_json::Value;

use super::{Engine, EngineError, Hit, SearchFuture, status_error, transport};

pub struct Google {
    http: reqwest::Client,
    key: String,
    cx: String,
    endpoint: String,
}

impl Google {
    pub fn new(http: reqwest::Client, key: &str, cx: &str) -> Self {
        Self::with_endpoint(http, key, cx, "https://www.googleapis.com/customsearch/v1")
    }
    pub fn with_endpoint(http: reqwest::Client, key: &str, cx: &str, endpoint: &str) -> Self {
        Self { http, key: key.to_string(), cx: cx.to_string(), endpoint: endpoint.to_string() }
    }
}

impl Engine for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
        self.search_in(query, limit, None)
    }

    fn search_in<'a>(&'a self, query: &'a str, limit: usize, country: Option<&'a str>) -> SearchFuture<'a> {
        Box::pin(async move {
            let num = limit.clamp(1, 10).to_string();
            let mut params = vec![("key", self.key.as_str()), ("cx", self.cx.as_str()), ("q", query), ("num", num.as_str())];
            params.extend(country.map(|c| ("gl", c)));
            let reply = self
                .http
                .get(&self.endpoint)
                .query(&params)
                .send()
                .await
                .map_err(transport)?;
            if !reply.status().is_success() {
                return Err(status_error(reply.status().as_u16()));
            }
            let body: Value = reply.json().await.map_err(transport)?;
            parse(&body)
        })
    }
}

pub fn parse(body: &Value) -> Result<Vec<Hit>, EngineError> {
    if let Some(error) = body.get("error") {
        let reason = error.pointer("/errors/0/reason").and_then(Value::as_str).unwrap_or("error");
        return Err(if reason.contains("Exceeded") || reason.contains("rateLimit") {
            EngineError::Refused(reason.into())
        } else {
            EngineError::Parse(reason.into())
        });
    }
    // No "items" means no results (Google omits the field).
    Ok(body
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(Hit {
                        title: item.get("title")?.as_str()?.to_string(),
                        url: item.get("link")?.as_str()?.to_string(),
                        snippet: item.get("snippet").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn items_quota_and_empty() {
        let body = json!({"items": [{"title": "T", "link": "https://t.example/", "snippet": "s"}]});
        assert_eq!(parse(&body).unwrap()[0].url, "https://t.example/");
        assert_eq!(parse(&json!({"searchInformation": {}})).unwrap(), vec![]);
        let quota = json!({"error": {"errors": [{"reason": "dailyLimitExceeded"}]}});
        assert!(parse(&quota).unwrap_err().is_refusal());
    }
}

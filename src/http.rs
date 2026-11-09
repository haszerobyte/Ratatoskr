//! A local HTTP API: the MCP tools' search and fetch for anything that can
//! make an HTTP request (a script in any language, n8n, cron, an agent
//! framework without MCP), served by one long-running process whose cache,
//! engine health and pacing every caller shares. `ratatoskr serve` runs it.
//!
//! - `GET /health`: `{"status": "ok", "name", "version"}`, never behind the token.
//! - `POST /search`: `{"query", "type", "limit", "country", "time", "safesearch",
//!   "page"}` as the `web_search` tool takes them; answers `{"type", "query",
//!   "engine" or "sources", "from_cache", "results": [...]}`.
//! - `POST /fetch`: `{"url"}`; answers `{"url", "title", "content_type",
//!   "truncated", "text"}`, or `{"error", "kind"}` with a 4xx/5xx status.
//! - `POST /search/text`: the request and answer shape of a "search server"
//!   (`{"query", "max_results", "region", "safesearch", "timelimit", "page"}` to
//!   `{"results": [{"title", "href", "body"}]}`), so this server stands in for
//!   one wherever such a server is expected, ratatoskr's own SEARCH_API_URL
//!   included.
//!
//! It listens on loopback by default. Any other address needs a token (see
//! [`needs_token`]), sent as `Authorization: Bearer <token>`: a fetch can never
//! reach the local network, but an open search relay hands the host's address
//! and its standing with the search engines to whoever finds it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::engine::Hit;
use crate::fetch::FetchError;
use crate::mcp::{DEFAULT_RESULTS, MAX_RESULTS};
use crate::request::SearchRequest;
use crate::vertical::{BookResult, ImageResult, NewsResult, VideoResult};
use crate::{Found, Ratatoskr, SearchFailure};

/// The largest request body taken: a search or a URL is a few hundred bytes.
const MAX_BODY: usize = 64 * 1024;
/// A search server's default and largest result counts.
const SEARCH_TEXT_DEFAULT: u64 = 10;
const SEARCH_TEXT_MAX: u64 = 25;

struct App {
    rt: Arc<Ratatoskr>,
    token: Option<String>,
}

/// Whether listening on `address` needs a token: anything but loopback.
pub fn needs_token(address: &SocketAddr) -> bool {
    !address.ip().is_loopback()
}

/// The API's routes. `token`, when given (and not blank), is required on
/// every route but `/health`.
pub fn router(rt: Arc<Ratatoskr>, token: Option<String>) -> Router {
    let app = Arc::new(App { rt, token: token.filter(|t| !t.trim().is_empty()) });
    Router::new()
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/fetch", post(fetch))
        .route("/search/text", post(search_text))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

/// Serve the API on `listener` until the process ends.
pub async fn serve(
    rt: Arc<Ratatoskr>,
    listener: tokio::net::TcpListener,
    token: Option<String>,
) -> std::io::Result<()> {
    axum::serve(listener, router(rt, token)).await
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    reply(status, json!({"error": message.into()}))
}

/// The token checked in constant time, so its bytes cannot be guessed one by
/// one from how long a refusal takes.
fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(token) = &app.token else { return true };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .as_bytes();
    given.len() == token.len() && given.iter().zip(token.as_bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// The request's JSON object, or the reply refusing it (boxed: a response is
/// large next to the object).
fn arguments(app: &App, headers: &HeaderMap, body: &Bytes) -> Result<Value, Box<Response>> {
    if !authorized(app, headers) {
        return Err(Box::new(error(StatusCode::UNAUTHORIZED, "a valid Authorization: Bearer token is needed")));
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| Box::new(error(StatusCode::BAD_REQUEST, "the body must be a JSON object")))
}

async fn health() -> Response {
    reply(StatusCode::OK, json!({"status": "ok", "name": "ratatoskr", "version": env!("CARGO_PKG_VERSION")}))
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "no such endpoint: GET /health, POST /search, /fetch or /search/text")
}

async fn search(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let args = match arguments(&app, &headers, &body) {
        Ok(args) => args,
        Err(refused) => return *refused,
    };
    let request = match SearchRequest::from_json(&args, DEFAULT_RESULTS, MAX_RESULTS) {
        Ok(request) => request,
        Err(bad) => return error(StatusCode::BAD_REQUEST, bad.to_string()),
    };
    let kind = args.get("type").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).unwrap_or("web");
    let rt = &app.rt;
    match kind {
        "web" => match rt.search_with(&request).await {
            Ok(outcome) => reply(
                StatusCode::OK,
                json!({"type": "web", "query": request.query, "engine": outcome.engine,
                       "from_cache": outcome.from_cache, "results": outcome.hits.iter().map(hit).collect::<Vec<_>>()}),
            ),
            Err(failure) => unavailable(&failure),
        },
        "news" => found(kind, &request, rt.news(&request).await, news),
        "images" => found(kind, &request, rt.images(&request).await, image),
        "videos" => found(kind, &request, rt.videos(&request).await, video),
        "books" => found(kind, &request, rt.books(&request).await, book),
        other => error(
            StatusCode::BAD_REQUEST,
            format!("type must be one of web, news, images, videos, books, not {other:?}"),
        ),
    }
}

fn found<T>(
    kind: &str,
    request: &SearchRequest,
    found: Result<Found<T>, SearchFailure>,
    to_json: fn(&T) -> Value,
) -> Response {
    match found {
        Ok(found) => reply(
            StatusCode::OK,
            json!({"type": kind, "query": request.query, "sources": found.sources, "from_cache": found.from_cache,
                   "results": found.results.iter().map(to_json).collect::<Vec<_>>()}),
        ),
        Err(failure) => unavailable(&failure),
    }
}

/// Every engine failed: the search is unavailable, not the request wrong.
fn unavailable(failure: &SearchFailure) -> Response {
    error(StatusCode::BAD_GATEWAY, failure.to_string())
}

async fn fetch(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let args = match arguments(&app, &headers, &body) {
        Ok(args) => args,
        Err(refused) => return *refused,
    };
    let Some(url) = args.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "a url is needed");
    };
    match app.rt.fetch(url).await {
        Ok(page) => reply(
            StatusCode::OK,
            json!({"url": page.url, "title": page.title, "content_type": page.content_type,
                   "truncated": page.truncated, "text": page.text}),
        ),
        Err(failure) => {
            // The caller's mistake is a 4xx; the site's refusal or the network a 502.
            let (status, kind) = match &failure {
                FetchError::BadUrl(_) => (StatusCode::BAD_REQUEST, "bad_url"),
                FetchError::Blocked(_) => (StatusCode::FORBIDDEN, "blocked"),
                FetchError::Http(_) => (StatusCode::BAD_GATEWAY, "http"),
                FetchError::Challenge(_) => (StatusCode::BAD_GATEWAY, "challenge"),
                FetchError::Unsupported(_) => (StatusCode::BAD_GATEWAY, "unsupported"),
                FetchError::TooLarge(_) => (StatusCode::BAD_GATEWAY, "too_large"),
                FetchError::Transport(_) => (StatusCode::BAD_GATEWAY, "transport"),
            };
            reply(status, json!({"error": failure.to_string(), "kind": kind}))
        }
    }
}

/// A search server's `/search/text`: its field names mapped onto a search.
/// `region` is `<country>-<language>` (`us-en`); `wt-wt` means anywhere.
async fn search_text(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let args = match arguments(&app, &headers, &body) {
        Ok(args) => args,
        Err(refused) => return *refused,
    };
    let country = args
        .get("region")
        .and_then(Value::as_str)
        .and_then(|region| region.split('-').next())
        .filter(|c| !c.is_empty() && *c != "wt");
    let mapped = json!({
        "query": args.get("query"),
        "limit": args.get("max_results").and_then(Value::as_u64).unwrap_or(SEARCH_TEXT_DEFAULT),
        "country": country,
        "time": args.get("timelimit"),
        "safesearch": args.get("safesearch"),
        "page": args.get("page"),
    });
    let request = match SearchRequest::from_json(&mapped, SEARCH_TEXT_DEFAULT, SEARCH_TEXT_MAX) {
        Ok(request) => request,
        Err(bad) => return error(StatusCode::BAD_REQUEST, bad.to_string()),
    };
    match app.rt.search_with(&request).await {
        Ok(outcome) => {
            let results: Vec<Value> =
                outcome.hits.iter().map(|h| json!({"title": h.title, "href": h.url, "body": h.snippet})).collect();
            reply(StatusCode::OK, json!({"results": results}))
        }
        Err(failure) => unavailable(&failure),
    }
}

fn hit(h: &Hit) -> Value {
    json!({"title": h.title, "url": h.url, "snippet": h.snippet})
}

fn news(n: &NewsResult) -> Value {
    json!({"title": n.title, "url": n.url, "body": n.body, "date": n.date, "source": n.source, "image": n.image})
}

fn image(i: &ImageResult) -> Value {
    json!({"title": i.title, "image": i.image, "thumbnail": i.thumbnail, "url": i.url,
           "width": i.width, "height": i.height, "source": i.source})
}

fn video(v: &VideoResult) -> Value {
    json!({"title": v.title, "url": v.url, "description": v.description, "duration": v.duration,
           "embed_url": v.embed_url, "thumbnail": v.thumbnail, "publisher": v.publisher,
           "uploader": v.uploader, "published": v.published, "views": v.views})
}

fn book(b: &BookResult) -> Value {
    json!({"title": b.title, "authors": b.authors, "publisher": b.publisher, "year": b.year,
           "url": b.url, "thumbnail": b.thumbnail, "info": b.info})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;
    use crate::engine::{Engine, SearchFuture};

    struct Fixed;

    impl Engine for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
            let hits = (1..=limit)
                .map(|i| Hit {
                    title: format!("{query} {i}"),
                    url: format!("https://e.example/{i}"),
                    snippet: "s".into(),
                })
                .collect();
            Box::pin(async move { Ok(hits) })
        }
    }

    /// A server on a free loopback port, and its base URL.
    async fn start(token: Option<&str>) -> String {
        let rt = Arc::new(Ratatoskr::new(vec![Arc::new(Fixed)], Settings::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let token = token.map(str::to_owned);
        tokio::spawn(async move { serve(rt, listener, token).await.unwrap() });
        base
    }

    async fn post(base: &str, path: &str, body: Value, token: Option<&str>) -> (u16, Value) {
        let mut request = reqwest::Client::new().post(format!("{base}{path}")).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let reply = request.send().await.unwrap();
        (reply.status().as_u16(), reply.json().await.unwrap())
    }

    #[tokio::test]
    async fn search_fetch_and_health_answer_as_json() {
        let base = start(None).await;
        let health: Value = reqwest::get(format!("{base}/health")).await.unwrap().json().await.unwrap();
        assert_eq!((health["status"].as_str(), health["name"].as_str()), (Some("ok"), Some("ratatoskr")));

        let (status, found) = post(&base, "/search", json!({"query": "rust", "limit": 3}), None).await;
        assert_eq!(status, 200);
        assert_eq!((found["type"].as_str(), found["engine"].as_str()), (Some("web"), Some("fixed")));
        assert_eq!(found["results"].as_array().unwrap().len(), 3);
        assert_eq!(found["results"][0], json!({"title": "rust 1", "url": "https://e.example/1", "snippet": "s"}));

        assert_eq!(post(&base, "/search", json!({}), None).await.0, 400, "no query");
        assert_eq!(post(&base, "/search", json!({"query": "x", "time": "fortnight"}), None).await.0, 400);
        assert_eq!(post(&base, "/search", json!({"query": "x", "type": "podcasts"}), None).await.0, 400);

        let (status, refused) = post(&base, "/fetch", json!({"url": "http://127.0.0.1/"}), None).await;
        assert_eq!((status, refused["kind"].as_str()), (403, Some("blocked")), "the local network stays out of reach");
        assert_eq!(post(&base, "/fetch", json!({}), None).await.0, 400);

        let reply = reqwest::Client::new().post(format!("{base}/search")).body("not json").send().await.unwrap();
        assert_eq!(reply.status().as_u16(), 400);
        assert_eq!(reqwest::get(format!("{base}/nope")).await.unwrap().status().as_u16(), 404);
    }

    /// The search-server shape: ratatoskr's own SEARCH_API_URL client reads
    /// this server's answers.
    #[tokio::test]
    async fn search_text_serves_a_search_server_client() {
        let base = start(None).await;
        let (status, body) =
            post(&base, "/search/text", json!({"query": "tokio", "max_results": 2, "region": "us-en"}), None).await;
        assert_eq!(status, 200);
        assert_eq!(body["results"][0], json!({"title": "tokio 1", "href": "https://e.example/1", "body": "s"}));
        let client = crate::engine::search_api::SearchApi::new(reqwest::Client::new(), &base);
        let hits = client.search("tokio", 4).await.unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].url, "https://e.example/1");
    }

    #[tokio::test]
    async fn a_token_guards_everything_but_health() {
        let base = start(Some("s3cret")).await;
        assert_eq!(post(&base, "/search", json!({"query": "x"}), None).await.0, 401);
        assert_eq!(post(&base, "/search", json!({"query": "x"}), Some("wrong")).await.0, 401);
        assert_eq!(post(&base, "/search", json!({"query": "x"}), Some("s3cret")).await.0, 200);
        assert_eq!(reqwest::get(format!("{base}/health")).await.unwrap().status().as_u16(), 200);
    }

    #[test]
    fn only_loopback_listens_without_a_token() {
        assert!(!needs_token(&"127.0.0.1:8765".parse().unwrap()));
        assert!(!needs_token(&"[::1]:8765".parse().unwrap()));
        assert!(needs_token(&"0.0.0.0:8765".parse().unwrap()));
        assert!(needs_token(&"10.0.0.20:8765".parse().unwrap()));
    }
}

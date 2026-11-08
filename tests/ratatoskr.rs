//! Ratatoskr against fake engines and a fake website: fall-through, the
//! circuit breaker, the cache, and fetch (text extraction, identity, caps).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use ratatoskr::engine::duckduckgo::DuckDuckGo;
use ratatoskr::engine::google::Google;
use ratatoskr::engine::grokipedia::Grokipedia;
use ratatoskr::engine::wikipedia::Wikipedia;
use ratatoskr::engine::yahoo::Yahoo;
use ratatoskr::engine::yandex::Yandex;
use ratatoskr::engine::{Engine, browser, http_client};
use ratatoskr::fetch::{FetchError, Fetcher};
use ratatoskr::{Ratatoskr, Settings};
use serde_json::json;

const RESULTS: &str = include_str!("fixtures/ddg_html_results.html");
const ANOMALY: &str = include_str!("fixtures/ddg_anomaly_202.html");

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

fn settings() -> Settings {
    Settings {
        min_interval: Duration::from_millis(1),
        cooldown: Duration::from_secs(60),
        cache_ttl: Duration::from_secs(60),
        cache_entries: 16,
        timeout: Duration::from_secs(5),
    }
}

/// DuckDuckGo that answers with the real anomaly page (202), counting calls.
async fn refusing_ddg(calls: Arc<AtomicUsize>) -> String {
    serve(Router::new().route(
        "/html/",
        post(move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                (StatusCode::ACCEPTED, ANOMALY)
            }
        }),
    ))
    .await
}

async fn wikipedia() -> String {
    serve(Router::new().route(
        "/w/api.php",
        get(|| async {
            axum::Json(
                json!({"query": {"search": [{"title": "Rust (programming language)", "snippet": "a language"}]}}),
            )
        }),
    ))
    .await
}

#[tokio::test]
async fn a_refusing_engine_trips_and_the_next_one_answers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let ddg = refusing_ddg(calls.clone()).await;
    let wiki = wikipedia().await;
    let http = http_client(Duration::from_secs(5));
    let engines: Vec<Arc<dyn Engine>> = vec![
        Arc::new(DuckDuckGo::with_endpoint(browser::client(Duration::from_secs(5)), &format!("{ddg}/html/"))),
        Arc::new(Wikipedia::with_api(http, &format!("{wiki}/w/api.php"), "https://en.wikipedia.org")),
    ];
    let rt = Ratatoskr::new(engines, settings());

    let first = rt.search("rust language", 5).await.unwrap();
    assert_eq!((first.engine, first.from_cache), ("wikipedia", false));
    assert_eq!(first.hits[0].url, "https://en.wikipedia.org/wiki/Rust_%28programming_language%29");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(rt.status()[0].1.is_some(), "duckduckgo's breaker is open after the challenge");

    // A different query: DuckDuckGo is cooling down, so it is not asked again.
    let second = rt.search("ferris crab", 5).await.unwrap();
    assert_eq!(second.engine, "wikipedia");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a refusing engine is left alone");

    // The same query again: served from the cache, nobody asked.
    let cached = rt.search("  Rust Language ", 5).await.unwrap();
    assert!(cached.from_cache);
    assert_eq!(cached.hits, first.hits);
}

#[tokio::test]
async fn results_from_the_real_page_come_through_end_to_end() {
    let ddg = serve(Router::new().route("/html/", post(|| async { RESULTS }))).await;
    let http = browser::client(Duration::from_secs(5));
    let rt = Ratatoskr::new(vec![Arc::new(DuckDuckGo::with_endpoint(http, &format!("{ddg}/html/")))], settings());
    let outcome = rt.search("rust programming language", 3).await.unwrap();
    assert_eq!(outcome.engine, "duckduckgo");
    assert_eq!(outcome.hits.len(), 3);
    assert_eq!(outcome.hits[0].url, "https://rust-lang.org/");
}

#[tokio::test]
async fn every_engine_failing_is_a_clear_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let ddg = refusing_ddg(calls).await;
    let http = browser::client(Duration::from_secs(5));
    let rt = Ratatoskr::new(vec![Arc::new(DuckDuckGo::with_endpoint(http, &format!("{ddg}/html/")))], settings());
    let failure = rt.search("q", 5).await.unwrap_err();
    assert!(failure.to_string().contains("duckduckgo: refused: bot challenge"), "{failure}");
    let again = rt.search("q2", 5).await.unwrap_err();
    assert!(again.to_string().contains("cooling down"), "{again}");
}

async fn website() -> String {
    let page = "<html><head><title>Ferris</title></head><body><nav>menu</nav>\
                <article><h2>About</h2><p>Ferris is the <em>crab</em>.</p></article></body></html>";
    let big = "x".repeat(3 * 1024 * 1024);
    serve(
        Router::new()
            // Cricbuzz's: every agent but Googlebot disallowed.
            .route("/robots.txt", get(|| async { "User-agent: Googlebot\nAllow: /\n\nUser-agent: *\nDisallow: /\n" }))
            .route("/page", get(move || async move { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], page) }))
            .route(
                "/user-agent",
                get(|headers: HeaderMap| async move {
                    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
                }),
            )
            .route("/data.json", get(|| async { ([(header::CONTENT_TYPE, "application/json")], r#"{"a":1}"#) }))
            .route("/image.png", get(|| async { ([(header::CONTENT_TYPE, "image/png")], "\u{89}PNG") }))
            .route("/huge", get(move || async move { ([(header::CONTENT_TYPE, "text/plain")], big.clone()) }))
            .route("/missing", get(|| async { StatusCode::NOT_FOUND.into_response() })),
    )
    .await
}

#[tokio::test]
async fn fetch_reads_text_types_and_caps() {
    let site = website().await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));
    let page = fetcher.fetch(&format!("{site}/page")).await.unwrap();
    assert_eq!(page.title.as_deref(), Some("Ferris"));
    assert_eq!(page.text, "## About\nFerris is the crab.");
    assert!(!page.truncated);

    assert_eq!(fetcher.fetch(&format!("{site}/data.json")).await.unwrap().text, r#"{"a":1}"#);
    assert_eq!(fetcher.fetch(&format!("{site}/image.png")).await, Err(FetchError::Unsupported("image/png".into())));
    assert_eq!(fetcher.fetch(&format!("{site}/missing")).await, Err(FetchError::Http(404)));
    let huge = fetcher.fetch(&format!("{site}/huge")).await.unwrap();
    assert!(huge.truncated);
    assert!(huge.text.len() <= 100_000);
}

/// Cricbuzz disallows every agent but Googlebot in robots.txt, so a page the
/// user asked for was refused before any request. A fetch is one browser
/// visit on their behalf, not a crawl: the page is read.
#[tokio::test]
async fn a_page_robots_txt_disallows_is_still_read() {
    let site = website().await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));
    assert_eq!(fetcher.fetch(&format!("{site}/page")).await.unwrap().title.as_deref(), Some("Ferris"));
}

/// ESPNcricinfo answers a truthful User-Agent and TLS fingerprint 403, so a
/// fetch presents as Chrome.
#[tokio::test]
async fn a_fetch_presents_as_chrome() {
    let site = website().await;
    let agent = Fetcher::allowing_private_for_tests(Duration::from_secs(5))
        .fetch(&format!("{site}/user-agent"))
        .await
        .unwrap()
        .text;
    assert!(agent.contains("Chrome/") && agent.contains("Linux") && !agent.contains("Ratatoskr"), "{agent}");
}

#[tokio::test]
async fn grokipedia_answers_its_one_page_from_the_typeahead() {
    let site = serve(Router::new().route(
        "/api/typeahead",
        get(|axum::extract::RawQuery(query): axum::extract::RawQuery| async move {
            assert_eq!(query.as_deref(), Some("query=raspberry+pi&limit=1"));
            ([(header::CONTENT_TYPE, "application/json")], include_str!("fixtures/meta/grokipedia_typeahead.json"))
        }),
    ))
    .await;
    let engine = Grokipedia::with_endpoint(http_client(Duration::from_secs(5)), &format!("{site}/api/typeahead"));
    let hits = engine.search("raspberry pi", 1).await.unwrap();
    assert_eq!(hits[0].url, "https://grokipedia.com/page/Raspberry_Pi_OS");
}

/// Regression: Wikipedia's short rate limit (429) failed web_fetch at once on
/// a busy host though it cleared within seconds: a short
/// Retry-After is waited out once; a long one is reported.
#[tokio::test]
async fn a_short_429_is_waited_out_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let site = serve(
        Router::new()
            .route(
                "/busy",
                get(move || {
                    let n = counted.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n == 0 {
                            (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")], "slow down").into_response()
                        } else {
                            ([(header::CONTENT_TYPE, "text/plain")], "here").into_response()
                        }
                    }
                }),
            )
            .route(
                "/long",
                get(|| async {
                    (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "3600")], "later").into_response()
                }),
            ),
    )
    .await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));
    let started = std::time::Instant::now();
    assert_eq!(fetcher.fetch(&format!("{site}/busy")).await.unwrap().text, "here");
    assert!(started.elapsed() >= Duration::from_secs(1), "the Retry-After was waited out");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let started = std::time::Instant::now();
    assert_eq!(fetcher.fetch(&format!("{site}/long")).await, Err(FetchError::Http(429)));
    assert!(started.elapsed() < Duration::from_secs(2), "an hour is not waited");
}

#[tokio::test]
async fn redirects_are_followed_up_to_five() {
    let site = serve(Router::new().route(
        "/hop/{n}",
        get(|axum::extract::Path(n): axum::extract::Path<u32>| async move {
            if n == 0 {
                "landed".into_response()
            } else {
                axum::response::Redirect::temporary(&format!("/hop/{}", n - 1)).into_response()
            }
        }),
    ))
    .await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));
    let page = fetcher.fetch(&format!("{site}/hop/5")).await.unwrap();
    assert_eq!((page.text.as_str(), page.url.ends_with("/hop/0")), ("landed", true));
    assert!(matches!(fetcher.fetch(&format!("{site}/hop/6")).await, Err(FetchError::Transport(_))));
}

#[tokio::test]
async fn the_production_fetcher_refuses_the_local_fake_site() {
    // Without the test escape hatch, the same local server is unreachable.
    let site = website().await;
    let error = Fetcher::new(Duration::from_secs(5)).fetch(&format!("{site}/page")).await.unwrap_err();
    assert!(matches!(error, FetchError::Blocked(_)), "{error:?}");
}

/// Security MUST-3: a transport or decode failure's text carried the
/// request URL, and Google's has the API key in its query (key=), so the
/// key reached the log and the model's tool result. Neither failure
/// names the key now.
#[tokio::test]
async fn an_engine_failure_never_carries_its_api_key() {
    const KEY: &str = "AIzaSECRETKEY";
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/customsearch/v1", listener.local_addr().unwrap())
    };
    let garbled = serve(Router::new().route("/customsearch/v1", get(|| async { "not json" }))).await;
    for endpoint in [closed, format!("{garbled}/customsearch/v1")] {
        let google = Google::with_endpoint(http_client(Duration::from_secs(5)), KEY, "cx1", &endpoint);
        let rt = Ratatoskr::new(vec![Arc::new(google)], settings());
        let failure = rt.search("anything", 3).await.expect_err("the engine fails");
        let text = format!("{failure} {failure:?}");
        assert!(!text.contains(KEY) && !text.contains("key="), "{endpoint}: {text}");
    }
}

/// Prints what a fingerprinting service sees, to check after bumping wreq.
/// Network: run with `--ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn show_live_fingerprint() {
    use browser::{BrowserPlatform, BrowserProfile};
    for (name, profile, platform) in [
        ("chrome", BrowserProfile::Chrome149, BrowserPlatform::Linux),
        ("firefox", BrowserProfile::Firefox151, BrowserPlatform::Windows),
    ] {
        let http = browser::builder(profile, platform).build().unwrap();
        let text = http.get("https://tls.peet.ws/api/all").send().await.unwrap().text().await.unwrap();
        println!("FINGERPRINT {name}:\n{text}\nEND");
    }
}

/// Missing navigation headers, wreq's own Accept-Encoding and a library
/// header order all told a bot check this was not Chrome.
#[tokio::test]
async fn a_fetch_sends_chromes_navigation_headers_in_chromes_order() {
    let site = serve(Router::new().route(
        "/h",
        get(|headers: HeaderMap| async move {
            headers.iter().map(|(k, v)| format!("{k}: {}", v.to_str().unwrap_or("?"))).collect::<Vec<_>>().join("\n")
        }),
    ))
    .await;
    let sent =
        Fetcher::allowing_private_for_tests(Duration::from_secs(5)).fetch(&format!("{site}/h")).await.unwrap().text;
    let names: Vec<&str> =
        sent.lines().filter_map(|l| l.split_once(": ").map(|(k, _)| k)).filter(|k| *k != "host").collect();
    assert_eq!(
        names,
        [
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "upgrade-insecure-requests",
            "user-agent",
            "accept",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-user",
            "sec-fetch-dest",
            "accept-encoding",
            "accept-language",
            "priority"
        ],
        "{sent}"
    );
    assert!(sent.contains("accept-encoding: gzip, deflate, br, zstd"), "{sent}");
}

/// Without a jar, a set-cookie-and-redirect gate refused every fetch.
#[tokio::test]
async fn a_cookie_set_on_the_way_is_sent_back_within_that_fetch_only() {
    let site = serve(
        Router::new()
            .route(
                "/gate",
                get(|| async {
                    ([(header::SET_COOKIE, "gate=passed; Path=/"), (header::LOCATION, "/inside")], StatusCode::FOUND)
                        .into_response()
                }),
            )
            .route(
                "/inside",
                get(|headers: HeaderMap| async move {
                    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");
                    if cookie.contains("gate=passed") {
                        "welcome".into_response()
                    } else {
                        StatusCode::NOT_FOUND.into_response()
                    }
                }),
            ),
    )
    .await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));
    assert_eq!(fetcher.fetch(&format!("{site}/gate")).await.unwrap().text, "welcome");
    assert_eq!(fetcher.fetch(&format!("{site}/inside")).await, Err(FetchError::Http(404)));
}

/// A challenge used to read as a bare 403, and nothing was retried.
#[tokio::test]
async fn refusals_are_retried_once_as_firefox_and_challenges_are_named() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (h1, h2, h3) = (hits.clone(), hits.clone(), hits.clone());
    let site = serve(
        Router::new()
            .route(
                "/picky",
                get(move |headers: HeaderMap| {
                    h1.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let agent =
                            headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                        if agent.contains("Firefox/") {
                            "read".into_response()
                        } else {
                            StatusCode::FORBIDDEN.into_response()
                        }
                    }
                }),
            )
            .route(
                "/challenge",
                get(move || {
                    h2.fetch_add(1, Ordering::SeqCst);
                    async {
                        (StatusCode::FORBIDDEN, [("cf-mitigated", "challenge")], "<title>Just a moment...</title>")
                            .into_response()
                    }
                }),
            )
            .route(
                "/gone",
                get(move || {
                    h3.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::NOT_FOUND.into_response() }
                }),
            ),
    )
    .await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(5));

    assert_eq!(fetcher.fetch(&format!("{site}/picky")).await.unwrap().text, "read");
    assert_eq!(hits.swap(0, Ordering::SeqCst), 2);

    let error = fetcher.fetch(&format!("{site}/challenge")).await.unwrap_err();
    assert_eq!(error, FetchError::Challenge("Cloudflare".into()));
    assert!(error.to_string().contains("bot challenge (Cloudflare)"), "{error}");
    assert_eq!(hits.swap(0, Ordering::SeqCst), 2);

    assert_eq!(fetcher.fetch(&format!("{site}/gone")).await, Err(FetchError::Http(404)));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// The engines sent a browser User-Agent over rustls, which no browser uses.
#[tokio::test]
async fn an_engine_client_is_one_consistent_browser() {
    let site = serve(Router::new().route(
        "/h",
        get(|headers: HeaderMap| async move {
            let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            format!("{}\n{}\n{}", get("user-agent"), get("sec-ch-ua-platform"), get("sec-fetch-user"))
        }),
    ))
    .await;
    for _ in 0..12 {
        let text = browser::client(Duration::from_secs(5))
            .get(format!("{site}/h"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let mut lines = text.lines();
        let (agent, platform, fetch_user) =
            (lines.next().unwrap(), lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        assert!(!agent.contains("Ratatoskr") && agent.starts_with("Mozilla/5.0"), "{agent}");
        assert_eq!(fetch_user, "?1", "{text}");
        if agent.contains("Firefox/") {
            assert_eq!(platform, "", "Firefox sends no client hints: {text}");
        } else {
            let os = if agent.contains("Windows") {
                "\"Windows\""
            } else if agent.contains("Macintosh") {
                "\"macOS\""
            } else {
                "\"Linux\""
            };
            assert_eq!(platform, os, "client hints agree with the User-Agent: {text}");
        }
    }
}

/// A one-page PDF saying `text`, with a correct cross-reference table.
fn pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (n, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", n + 1));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for offset in offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
    out.into_bytes()
}

/// web_fetch reads PDFs (Anthropic's returns them base64); a PDF was
/// Unsupported. One over the cap is refused whole: a cut PDF is unreadable.
#[tokio::test]
async fn pdfs_come_back_whole_with_their_text() {
    let paper = pdf("Ferris reads PDFs");
    let served = paper.clone();
    let big = vec![b'x'; ratatoskr::fetch::MAX_PDF + 1];
    let pdf_type = || [(header::CONTENT_TYPE, "application/pdf")];
    let site = serve(
        Router::new()
            .route("/paper.pdf", get(move || async move { (pdf_type(), served.clone()) }))
            .route("/broken.pdf", get(move || async move { (pdf_type(), "%PDF-1.4 not really") }))
            .route("/huge.pdf", get(move || async move { (pdf_type(), big.clone()) })),
    )
    .await;
    let fetcher = Fetcher::allowing_private_for_tests(Duration::from_secs(10));

    let page = fetcher.fetch(&format!("{site}/paper.pdf")).await.unwrap();
    assert_eq!((page.content_type.as_str(), page.pdf.as_deref()), ("application/pdf", Some(paper.as_slice())));
    assert!(page.text.contains("Ferris reads PDFs"), "{:?}", page.text);

    let broken = fetcher.fetch(&format!("{site}/broken.pdf")).await.unwrap();
    assert!(broken.pdf.is_some() && broken.text.is_empty());

    assert_eq!(fetcher.fetch(&format!("{site}/huge.pdf")).await, Err(FetchError::TooLarge(ratatoskr::fetch::MAX_PDF)));
}

/// A POST without an Origin, or a results page reached from nowhere, is not
/// what a browser's search box sends. web_search's country picks the region.
#[tokio::test]
async fn searches_look_like_the_sites_own_search_box_and_carry_the_country() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (ddg_seen, yahoo_seen) = (seen.clone(), seen.clone());
    let yahoo_page = include_str!("fixtures/meta/yahoo_results.html");
    let site = serve(
        Router::new()
            .route(
                "/html/",
                post(move |headers: HeaderMap, body: String| {
                    let seen = ddg_seen.clone();
                    async move {
                        let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                        seen.lock().unwrap().push(format!(
                            "ddg {} {} {} {} {body}",
                            get("origin"),
                            get("referer"),
                            get("sec-fetch-site"),
                            get("accept-language")
                        ));
                        RESULTS
                    }
                }),
            )
            .route(
                "/{*rest}",
                get(move |headers: HeaderMap| {
                    let seen = yahoo_seen.clone();
                    async move {
                        let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                        seen.lock().unwrap().push(format!(
                            "yahoo {} {} {}",
                            get("origin"),
                            get("referer"),
                            get("sec-fetch-site")
                        ));
                        yahoo_page
                    }
                }),
            ),
    )
    .await;
    let ddg = DuckDuckGo::with_endpoint(browser::client(Duration::from_secs(5)), &format!("{site}/html/"));
    assert!(!ddg.search_in("rust", 3, Some("IN")).await.unwrap().is_empty());
    let yahoo = Yahoo::with_base(browser::client(Duration::from_secs(5)), &site);
    assert!(!yahoo.search("rust", 3).await.unwrap().is_empty());

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], format!("ddg {site} {site}/html/ same-origin en-IN,en;q=0.9 q=rust&b=&l=in-en"));
    // Yahoo is a typed URL (a fresh browser has none of Yahoo's cookies to
    // have come from its page with).
    assert_eq!(seen[1], "yahoo   none");
}

/// The scraped engines share one page reader: results come through, and a
/// vendor's challenge (here Cloudflare's, by its header) is a refusal.
#[tokio::test]
async fn yandex_reads_results_and_refuses_a_challenge() {
    let site = serve(
        Router::new()
            .route(
                "/search/site/",
                get(|axum::extract::RawQuery(query): axum::extract::RawQuery| async move {
                    let query = query.unwrap_or_default();
                    assert!(query.starts_with("text=raspberry+pi&web=1&searchid="), "{query}");
                    ([(header::CONTENT_TYPE, "text/html")], include_str!("fixtures/meta/yandex_results.html"))
                        .into_response()
                }),
            )
            .route(
                "/blocked/",
                get(|| async {
                    (
                        StatusCode::FORBIDDEN,
                        [("cf-mitigated", "challenge")],
                        "<html><title>Just a moment...</title></html>",
                    )
                        .into_response()
                }),
            ),
    )
    .await;
    let yandex = Yandex::with_endpoint(browser::client(Duration::from_secs(5)), &format!("{site}/search/site/"));
    let hits = yandex.search("raspberry pi", 3).await.unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].url, "https://www.raspberrypi.com/software/operating-systems/");
    let blocked = Yandex::with_endpoint(browser::client(Duration::from_secs(5)), &format!("{site}/blocked/"));
    let error = blocked.search("raspberry pi", 3).await.unwrap_err();
    assert!(error.is_refusal() && error.to_string().contains("Cloudflare"), "{error}");
}

/// Regression: the meta-search's Wikipedia page came from full-text search, so
/// "tokio select macro" was answered first with an artist's biography that
/// mentions Tokio. The best page is a title match, with its intro, or nothing.
#[tokio::test]
async fn wikipedias_best_page_is_a_title_match_or_nothing() {
    let site = serve(Router::new().route(
        "/w/api.php",
        get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            let body = match (q.get("action").map(String::as_str), q.get("search").or(q.get("titles")).map(String::as_str)) {
                (Some("opensearch"), Some("raspberry pi os")) => {
                    json!(["raspberry pi os", ["Raspberry Pi OS"], [""], ["https://en.wikipedia.org/wiki/Raspberry_Pi_OS"]])
                }
                (Some("opensearch"), Some(other)) => json!([other, [], [], []]),
                (Some("query"), Some("Raspberry Pi OS")) => {
                    json!({"query": {"pages": [{"title": "Raspberry Pi OS", "extract": "Raspberry Pi OS is an operating system."}]}})
                }
                _ => json!({}),
            };
            axum::Json(body)
        }),
    ))
    .await;
    let engine = Wikipedia::best_page_with_api(http_client(Duration::from_secs(5)), &format!("{site}/w/api.php"));
    let hits = engine.search("raspberry pi os", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://en.wikipedia.org/wiki/Raspberry_Pi_OS");
    assert_eq!(hits[0].snippet, "Raspberry Pi OS is an operating system.");
    assert_eq!(engine.search("tokio select macro", 5).await.unwrap(), vec![]);
}

/// The external search server fallback (SEARCH_API_URL): the query goes as
/// JSON, and the server's results come back as hits.
#[tokio::test]
async fn an_external_search_server_answers_as_a_source() {
    let site = serve(Router::new().route(
        "/search/text",
        post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
            assert_eq!((body["query"].as_str(), body["max_results"].as_u64()), (Some("rust"), Some(3)));
            axum::Json(json!({"results": [{"title": "Rust", "href": "https://rust-lang.org/", "body": "A language"}]}))
        }),
    ))
    .await;
    let engine =
        ratatoskr::engine::search_api::SearchApi::new(http_client(Duration::from_secs(5)), &format!("{site}/"));
    let hits = engine.search("rust", 3).await.unwrap();
    assert_eq!(hits[0].url, "https://rust-lang.org/");
    assert_eq!(engine.name(), "search-api");
}

//! Fetching a page as text, safely.
//!
//! A service that uses this may be reachable by others, so a fetch must never
//! be a way into the local network (SSRF): every address a hostname resolves to
//! is checked at connect time by our own DNS resolver — a name cannot pass a
//! check and then resolve somewhere else — and IP-literal URLs and every
//! redirect hop are checked too. Only http(s), at most 5 redirects, a size cap
//! and a timeout.
//!
//! A fetch is one page a person asked for, read once — a browser visit made on
//! their behalf, not a crawl — so it goes out as Chrome (TLS and HTTP/2
//! fingerprint, headers) and does not consult robots.txt, which governs
//! crawlers. Some sites refuse anything that does not handshake like a browser
//! (ESPNcricinfo answers a truthful client 403), and Cricbuzz's robots.txt
//! disallows every agent but Googlebot.
//!
//! Cookies set during a fetch are kept for that fetch only: protection layers
//! set one and redirect back expecting it. A 403 or challenge is retried once
//! as Firefox, since some firewalls block a fingerprint, not a visitor, and a
//! 429 once after the wait its Retry-After asks for, when that is short
//! (Wikipedia's limits clear in seconds). Challenges are reported, never solved.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use wreq::dns::{Addrs, Name, Resolve, Resolving};
use wreq_util::{Platform, Profile};

use crate::engine::browser;

use crate::text::html_to_text;

/// A fetched page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// After redirects.
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub content_type: String,
    /// The body or text was cut at a cap.
    pub truncated: bool,
    /// A PDF's bytes, which web_fetch returns as is; `text` is its extracted text.
    pub pdf: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// Not http(s), unparseable, or no host.
    BadUrl(String),
    /// The target is loopback, private, link-local or otherwise not public.
    Blocked(String),
    Http(u16),
    /// A bot challenge instead of the page, by vendor.
    Challenge(String),
    /// Not a type we can turn into text (images, archives, ...).
    Unsupported(String),
    /// A PDF over the size cap.
    TooLarge(usize),
    Transport(String),
}

/// Why one attempt failed, and for a 429 the wait its Retry-After asked for.
struct Failed {
    error: FetchError,
    retry_after: Option<Duration>,
}

impl From<FetchError> for Failed {
    fn from(error: FetchError) -> Self {
        Self { error, retry_after: None }
    }
}

/// The longest Retry-After a fetch waits out: a tool call that waits longer
/// has kept its model waiting more than the page is worth.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

/// A Retry-After in seconds (the HTTP-date form is not worth parsing for a
/// wait this short); 1 s when a 429 gives none.
fn retry_after(headers: &wreq::header::HeaderMap) -> Option<Duration> {
    match headers.get("retry-after") {
        None => Some(Duration::from_secs(1)),
        Some(value) => value.to_str().ok()?.trim().parse::<u64>().ok().map(Duration::from_secs),
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadUrl(why) => write!(f, "invalid URL: {why}"),
            Self::Blocked(why) => write!(f, "blocked: {why}"),
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Challenge(vendor) => write!(f, "the site answered with a bot challenge ({vendor}), not the page"),
            Self::Unsupported(kind) => write!(f, "unsupported content type {kind}"),
            Self::TooLarge(cap) => write!(f, "the PDF is larger than {} MB", cap / (1024 * 1024)),
            Self::Transport(why) => write!(f, "{why}"),
        }
    }
}

/// Whether `ip` is a public, routable unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
                || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18/15
                || (a == 192 && b == 0 && c == 0)) // IETF 192.0.0/24
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (first == 0x2001 && v6.segments()[1] == 0x0db8) // documentation
                || (first == 0x0064 && v6.segments()[1] == 0xff9b)) // NAT64 64:ff9b::/96
        }
    }
}

/// DNS that refuses a name if any of its addresses is not public.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
                return Err(format!("{host} resolves to non-public address {}", bad.ip()).into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

fn check_url(url: &url::Url, allow_private: bool) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::BadUrl(format!("scheme {} not allowed", url.scheme())));
    }
    match url.host() {
        None => Err(FetchError::BadUrl("no host".into())),
        Some(url::Host::Ipv4(ip)) if !allow_private && !is_public(IpAddr::V4(ip)) => {
            Err(FetchError::Blocked(ip.to_string()))
        }
        Some(url::Host::Ipv6(ip)) if !allow_private && !is_public(IpAddr::V6(ip)) => {
            Err(FetchError::Blocked(ip.to_string()))
        }
        Some(url::Host::Domain(d)) if !allow_private && (d == "localhost" || d.ends_with(".localhost")) => {
            Err(FetchError::Blocked(d.to_string()))
        }
        _ => Ok(()),
    }
}

const MAX_BODY: usize = 2 * 1024 * 1024;
/// Anthropic's limit for a PDF in a request.
pub const MAX_PDF: usize = 32 * 1024 * 1024;
const MAX_TEXT: usize = 100_000;
const MAX_REDIRECTS: usize = 5;

pub struct Fetcher {
    /// Chrome first; Firefox for the one retry.
    http: [wreq::Client; 2],
    allow_private: bool,
}

impl Fetcher {
    pub fn new(timeout: Duration) -> Self {
        Self::build(timeout, false)
    }

    /// For tests against a local fake server only: skips the address checks.
    pub fn allowing_private_for_tests(timeout: Duration) -> Self {
        Self::build(timeout, true)
    }

    fn build(timeout: Duration, allow_private: bool) -> Self {
        let redirects = wreq::redirect::Policy::custom(move |attempt| {
            // `previous` starts with the original request, so it holds one
            // more URI than redirects followed so far.
            if attempt.previous.len() > MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match url::Url::parse(&attempt.uri.to_string())
                .map_err(|e| FetchError::BadUrl(e.to_string()))
                .and_then(|url| check_url(&url, allow_private))
            {
                Ok(()) => attempt.follow(),
                Err(error) => attempt.error(error.to_string()),
            }
        });
        let client = |profile, platform| {
            // No HTTP(S)_PROXY from the environment: a proxy resolves the host
            // itself, so PublicOnly would never see the address it connects to.
            let mut builder =
                browser::builder(profile, platform).timeout(timeout).redirect(redirects.clone()).no_proxy();
            if !allow_private {
                builder = builder.dns_resolver(Arc::new(PublicOnly));
            }
            builder.build().expect("http client")
        };
        Self {
            http: [client(Profile::Chrome149, Platform::Linux), client(Profile::Firefox151, Platform::Linux)],
            allow_private,
        }
    }

    /// Fetch `url` as text: HTML is reduced to its readable content; other
    /// text types (plain, JSON, XML, Markdown) pass through; anything else is
    /// Unsupported.
    pub async fn fetch(&self, raw: &str) -> Result<Page, FetchError> {
        let url = url::Url::parse(raw).map_err(|e| FetchError::BadUrl(e.to_string()))?;
        check_url(&url, self.allow_private)?;
        // Shared with the retry, so a cookie the first attempt got goes back.
        let jar = Arc::new(wreq::cookie::Jar::default());
        match self.attempt(&self.http[0], &url, &jar).await {
            Err(Failed { error: first @ (FetchError::Challenge(_) | FetchError::Http(403)), .. }) => {
                match self.attempt(&self.http[1], &url, &jar).await {
                    Ok(page) => Ok(page),
                    Err(Failed { error: FetchError::Challenge(vendor), .. }) => Err(FetchError::Challenge(vendor)),
                    Err(_) => Err(first),
                }
            }
            Err(Failed { error: FetchError::Http(429), retry_after })
                if retry_after.is_none_or(|wait| wait > MAX_RETRY_AFTER) =>
            {
                tracing::info!("ratatoskr fetch {url}: 429 asking to wait {retry_after:?}; not waiting");
                Err(FetchError::Http(429))
            }
            Err(Failed { retry_after: Some(wait), .. }) if wait <= MAX_RETRY_AFTER => {
                tracing::info!("ratatoskr fetch {url}: 429; waiting {wait:?} and trying once more");
                tokio::time::sleep(wait).await;
                self.attempt(&self.http[0], &url, &jar).await.map_err(|failed| failed.error)
            }
            other => other.map_err(|failed| failed.error),
        }
    }

    async fn attempt(&self, http: &wreq::Client, url: &url::Url, jar: &Arc<wreq::cookie::Jar>) -> Result<Page, Failed> {
        let reply =
            http.get(url.as_str()).cookie_provider(jar.clone()).send().await.map_err(|e| transport_or_blocked(&e))?;
        let status = reply.status();
        let headers = reply.headers().clone();
        if !status.is_success() {
            let (body, _) = read_capped(reply, MAX_BODY).await.unwrap_or_default();
            let body = String::from_utf8_lossy(&body);
            let error = match challenge_vendor(status.as_u16(), &headers, &body) {
                Some(vendor) => FetchError::Challenge(vendor.into()),
                None => FetchError::Http(status.as_u16()),
            };
            let retry_after = (error == FetchError::Http(429)).then(|| retry_after(&headers)).flatten();
            return Err(Failed { error, retry_after });
        }
        let final_url = reply.uri().to_string();
        let content_type = reply
            .headers()
            .get(wreq::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = content_type.split(';').next().unwrap_or("").trim().to_string();
        if kind == "application/pdf" {
            return Ok(read_pdf(reply, final_url).await?);
        }
        let textual = kind.is_empty() || kind.starts_with("text/") || kind.contains("json") || kind.contains("xml");
        if !textual {
            return Err(FetchError::Unsupported(kind).into());
        }
        let (body, mut truncated) = read_capped(reply, MAX_BODY).await?;
        let body = String::from_utf8_lossy(&body).into_owned();
        if let Some(vendor) = challenge_vendor(status.as_u16(), &headers, &body) {
            return Err(FetchError::Challenge(vendor.into()).into());
        }
        let (title, text) = if kind.contains("html") || (kind.is_empty() && body.trim_start().starts_with('<')) {
            html_to_text(&body, url::Url::parse(&final_url).ok().as_ref())
        } else {
            (None, body)
        };
        let (text, cut) = cap_text(text);
        truncated |= cut;
        Ok(Page { url: final_url, title, text, content_type: kind, truncated, pdf: None })
    }
}

/// The vendor whose bot challenge this response is, if it is one. A 200 needs
/// a vendor header, a challenge title or a small page: articles mention
/// "captcha", and vendors' scripts run on the pages they protect.
pub fn challenge_vendor(status: u16, headers: &wreq::header::HeaderMap, body: &str) -> Option<&'static str> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if header("cf-mitigated") == "challenge" {
        return Some("Cloudflare");
    }
    if matches!(header("x-amzn-waf-action").as_str(), "captcha" | "challenge") {
        return Some("AWS WAF");
    }
    let head: String = body.chars().take(64 * 1024).collect::<String>().to_ascii_lowercase();
    let title = head
        .split_once("<title")
        .and_then(|(_, rest)| rest.split_once('>'))
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(title, _)| title.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let strong: &[(&str, &'static str)] = &[
        ("captcha-delivery.com", "DataDome"),
        ("_incapsula_resource", "Imperva"),
        ("px-captcha", "HUMAN (PerimeterX)"),
        ("/cdn-cgi/challenge-platform/h/", "Cloudflare"),
    ];
    let by_title: &[(&str, &'static str)] = &[
        ("just a moment...", "Cloudflare"),
        ("attention required! | cloudflare", "Cloudflare"),
        ("pardon our interruption", "Imperva"),
        ("access to this page has been denied", "HUMAN (PerimeterX)"),
    ];
    if let Some((_, vendor)) = by_title.iter().find(|(t, _)| title == *t) {
        return Some(vendor);
    }
    if (status != 200 || body.len() < 20_000)
        && let Some((_, vendor)) = strong.iter().find(|(marker, _)| head.contains(marker))
    {
        return Some(vendor);
    }
    if status != 200 {
        if header("server").contains("akamaighost") && status == 403 {
            return Some("Akamai");
        }
        if ["captcha", "are you a robot", "verify you are human", "unusual traffic"].iter().any(|w| head.contains(w)) {
            return Some("unknown vendor");
        }
    }
    None
}

/// A connect refused by our resolver or redirect policy is Blocked, not a
/// network fault.
fn transport_or_blocked(error: &wreq::Error) -> FetchError {
    let chain = format!("{error:?}");
    if chain.contains("non-public address") || chain.contains("blocked:") {
        return FetchError::Blocked("target resolves to a non-public address".into());
    }
    FetchError::Transport(if error.is_timeout() { "timed out".into() } else { error.to_string() })
}

async fn read_capped(reply: wreq::Response, cap: usize) -> Result<(Vec<u8>, bool), FetchError> {
    let mut body = Vec::new();
    let mut chunks = reply.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|e| transport_or_blocked(&e))?;
        let room = cap.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= cap {
            return Ok((body, true));
        }
    }
    Ok((body, false))
}

/// A cut PDF is unreadable, so one over the cap is refused, not truncated.
async fn read_pdf(reply: wreq::Response, url: String) -> Result<Page, FetchError> {
    let (bytes, cut) = read_capped(reply, MAX_PDF).await?;
    if cut {
        return Err(FetchError::TooLarge(MAX_PDF));
    }
    let text = pdf_text(bytes.clone()).await;
    let (text, truncated) = cap_text(text);
    Ok(Page { url, title: None, text, content_type: "application/pdf".into(), truncated, pdf: Some(bytes) })
}

/// The PDF's text, or none. pdf-extract can panic on malformed files, and
/// parsing is CPU work, so it runs on a blocking thread.
async fn pdf_text(bytes: Vec<u8>) -> String {
    tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes).ok()).ok().flatten()
    })
    .await
    .ok()
    .flatten()
    .map(|text| text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string())
    .unwrap_or_default()
}

fn cap_text(mut text: String) -> (String, bool) {
    if text.len() <= MAX_TEXT {
        return (text, false);
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn only_public_unicast_is_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
        ] {
            assert!(!is_public(ip.parse::<Ipv4Addr>().unwrap().into()), "{ip}");
        }
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(ip.parse::<Ipv6Addr>().unwrap().into()), "{ip}");
        }
        for ip in ["1.1.1.1", "151.101.128.223"] {
            assert!(is_public(ip.parse::<Ipv4Addr>().unwrap().into()), "{ip}");
        }
        assert!(is_public("2606:4700::1111".parse::<Ipv6Addr>().unwrap().into()));
    }

    #[test]
    fn challenges_are_named_and_ordinary_pages_are_not() {
        use wreq::header::{HeaderMap, HeaderValue};
        let none = HeaderMap::new();
        let mut cf = HeaderMap::new();
        cf.insert("cf-mitigated", HeaderValue::from_static("challenge"));
        assert_eq!(challenge_vendor(403, &cf, ""), Some("Cloudflare"));
        let moment = "<html><head><title>Just a moment...</title></head></html>";
        assert_eq!(challenge_vendor(200, &none, moment), Some("Cloudflare"));
        assert_eq!(
            challenge_vendor(403, &none, r#"<script src="https://geo.captcha-delivery.com/x.js">"#),
            Some("DataDome")
        );
        assert_eq!(challenge_vendor(403, &none, "<p>Please verify you are human</p>"), Some("unknown vendor"));
        assert_eq!(challenge_vendor(200, &none, "<title>How CAPTCHA works</title><p>captcha</p>"), None);
        let protected =
            format!("<script src=\"/_Incapsula_Resource?SWJIYLWA=1\"></script>{}", "<p>text</p>".repeat(3000));
        assert_eq!(challenge_vendor(200, &none, &protected), None);
        assert_eq!(challenge_vendor(404, &none, "<title>Not found</title>"), None);
    }

    #[tokio::test]
    async fn local_and_odd_targets_are_refused_before_any_request() {
        let fetcher = Fetcher::new(Duration::from_secs(5));
        for (url, blocked) in [
            ("http://127.0.0.1:8080/admin", true),
            ("http://192.168.0.10:8080/stats", true),
            ("http://[::1]/", true),
            ("http://localhost:3000/", true),
            ("http://169.254.169.254/latest/meta-data", true),
            ("file:///etc/passwd", false),
            ("ftp://example.com/", false),
        ] {
            let error = fetcher.fetch(url).await.unwrap_err();
            if blocked {
                assert!(matches!(error, FetchError::Blocked(_)), "{url}: {error:?}");
            } else {
                assert!(matches!(error, FetchError::BadUrl(_)), "{url}: {error:?}");
            }
        }
    }

    /// `localhost.` (a trailing dot) slips past check_url's name test; the
    /// resolver (wreq's dns_resolver) must still refuse it.
    #[tokio::test]
    async fn a_name_resolving_to_loopback_is_refused_at_connect() {
        let error = Fetcher::new(Duration::from_secs(5)).fetch("http://localhost./").await.unwrap_err();
        assert!(matches!(error, FetchError::Blocked(_)), "{error:?}");
    }
}

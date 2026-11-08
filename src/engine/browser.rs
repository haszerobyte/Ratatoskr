//! A browser client for engines that serve browsers only, with a cookie jar:
//! Yahoo answers a first visit with a consent redirect that sets a cookie.
//!
//! Emulated down to the TLS handshake, not just the User-Agent: a browser's
//! User-Agent over rustls is a mismatch bot checks look for.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wreq_util::{Emulation, Platform, Profile};

pub use wreq_util::{Platform as BrowserPlatform, Profile as BrowserProfile};

/// Only families whose headers [`navigation_headers`] has been checked for,
/// each on a platform it ships on. Mostly Chrome, as real traffic is, and the
/// last three releases of each, as real traffic is: browsers update over
/// weeks, so a pool that is all one version is the outlier.
///
/// Weighted towards Linux and macOS: a host running this is usually Linux, and
/// its TCP/IP stack says so (TTL 64, as macOS's does; Windows starts at 128),
/// which edge WAFs compare with the claimed OS. Windows stays in the pool, a
/// quarter of it, so the traffic is not all one OS either.
const PERSONAS: &[(Profile, Platform)] = &[
    (Profile::Chrome149, Platform::Windows),
    (Profile::Chrome149, Platform::Windows),
    (Profile::Edge148, Platform::Windows),
    (Profile::Firefox151, Platform::Windows),
    (Profile::Chrome149, Platform::MacOS),
    (Profile::Chrome148, Platform::MacOS),
    (Profile::Chrome147, Platform::MacOS),
    (Profile::Edge148, Platform::MacOS),
    (Profile::Firefox151, Platform::MacOS),
    (Profile::Firefox150, Platform::MacOS),
    (Profile::Chrome149, Platform::Linux),
    (Profile::Chrome149, Platform::Linux),
    (Profile::Chrome148, Platform::Linux),
    (Profile::Edge147, Platform::Linux),
    (Profile::Firefox151, Platform::Linux),
    (Profile::Firefox150, Platform::Linux),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Chromium,
    Firefox,
}

pub fn family(profile: Profile) -> Family {
    if format!("{profile:?}").starts_with("Firefox") { Family::Firefox } else { Family::Chromium }
}

/// The navigation headers wreq-util leaves out, and the browser's header
/// order: both are compared by bot checks.
pub fn navigation_headers(family: Family) -> (wreq::header::HeaderMap, wreq::header::OrigHeaderMap) {
    use wreq::header::{HeaderMap, HeaderValue, OrigHeaderMap};
    let mut extra = HeaderMap::new();
    extra.insert("upgrade-insecure-requests", HeaderValue::from_static("1"));
    extra.insert("sec-fetch-user", HeaderValue::from_static("?1"));
    let order: &[&'static str] = match family {
        Family::Chromium => &[
            "content-length",
            "cache-control",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "origin",
            "content-type",
            "upgrade-insecure-requests",
            "user-agent",
            "accept",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-user",
            "sec-fetch-dest",
            "referer",
            "accept-encoding",
            "accept-language",
            "cookie",
            "priority",
        ],
        Family::Firefox => &[
            "user-agent",
            "accept",
            "accept-language",
            "accept-encoding",
            "content-type",
            "content-length",
            "origin",
            "referer",
            "cookie",
            "upgrade-insecure-requests",
            "sec-fetch-dest",
            "sec-fetch-mode",
            "sec-fetch-site",
            "sec-fetch-user",
            "priority",
            "te",
        ],
    };
    let mut orig = OrigHeaderMap::new();
    for name in order {
        orig.insert(*name);
    }
    (extra, orig)
}

/// A small non-cryptographic random number (clock-seeded xorshift), enough to
/// vary the profile and URL tokens.
pub(crate) fn random_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9e37_79b9);
    let mut x = STATE.fetch_add(seed | 1, Ordering::Relaxed) ^ seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// URL-safe random token of `len` characters.
pub(crate) fn token(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    (0..len).map(|_| ALPHABET[(random_u64() % 64) as usize] as char).collect()
}

pub fn builder(profile: Profile, platform: Platform) -> wreq::ClientBuilder {
    let (extra, order) = navigation_headers(family(profile));
    wreq::Client::builder()
        .emulation(Emulation::builder().profile(profile).platform(platform).build())
        .default_headers(extra)
        .orig_headers(order)
}

/// Where a scraped engine's client comes from for each search.
///
/// A new client, with a random browser, for every search is what keeps
/// Yahoo answering: a session it has throttled stays
/// throttled. From one throttled host, five searches over one reused client
/// got no results at all, five over fresh clients got one to three.
pub enum Sessions {
    /// A new browser, its own cookie jar, for every search.
    Fresh(Duration),
    /// One given client: tests' fake servers.
    Fixed(wreq::Client, Family),
}

impl Sessions {
    /// The client for one search, and its browser family.
    pub fn open(&self) -> (wreq::Client, Family) {
        match self {
            Self::Fresh(timeout) => pick(*timeout),
            Self::Fixed(client, family) => (client.clone(), *family),
        }
    }
}

/// One browser per client: a browser does not change between requests.
pub fn client(timeout: Duration) -> wreq::Client {
    pick(timeout).0
}

/// [`client`], and which family it is, for requests whose headers differ.
pub fn pick(timeout: Duration) -> (wreq::Client, Family) {
    let (profile, platform) = PERSONAS[(random_u64() % PERSONAS.len() as u64) as usize];
    (builder(profile, platform).cookie_store(true).timeout(timeout).build().expect("http client"), family(profile))
}

/// The Accept-Language of a browser set up in `country` (ISO 3166-1
/// alpha-2), in its family's format. None for the US, whose default the
/// profiles already send, and for countries without a mapping: an en-US
/// browser asking for Indian results is a mismatch a site can see.
pub fn accept_language(country: &str, family: Family) -> Option<String> {
    const LANGUAGES: &[(&str, &str)] = &[
        ("AR", "es"),
        ("AT", "de"),
        ("AU", "en"),
        ("BE", "nl"),
        ("BG", "bg"),
        ("BR", "pt"),
        ("CA", "en"),
        ("CH", "de"),
        ("CL", "es"),
        ("CN", "zh"),
        ("CO", "es"),
        ("CZ", "cs"),
        ("DE", "de"),
        ("DK", "da"),
        ("EE", "et"),
        ("ES", "es"),
        ("FI", "fi"),
        ("FR", "fr"),
        ("GB", "en"),
        ("GR", "el"),
        ("HK", "zh"),
        ("HR", "hr"),
        ("HU", "hu"),
        ("ID", "id"),
        ("IE", "en"),
        ("IL", "he"),
        ("IN", "en"),
        ("IT", "it"),
        ("JP", "ja"),
        ("KR", "ko"),
        ("LT", "lt"),
        ("LV", "lv"),
        ("MX", "es"),
        ("MY", "en"),
        ("NL", "nl"),
        ("NO", "nb"),
        ("NZ", "en"),
        ("PE", "es"),
        ("PH", "en"),
        ("PL", "pl"),
        ("PT", "pt"),
        ("RO", "ro"),
        ("RU", "ru"),
        ("SE", "sv"),
        ("SG", "en"),
        ("SK", "sk"),
        ("TH", "th"),
        ("TR", "tr"),
        ("TW", "zh"),
        ("UA", "uk"),
        ("VE", "es"),
        ("VN", "vi"),
        ("ZA", "en"),
    ];
    let cc = country.to_ascii_uppercase();
    let lang = LANGUAGES.iter().find(|(c, _)| *c == cc)?.1;
    Some(match (lang, family) {
        ("en", Family::Chromium) => format!("en-{cc},en;q=0.9"),
        ("en", Family::Firefox) => format!("en-{cc},en;q=0.5"),
        (_, Family::Chromium) => format!("{lang}-{cc},{lang};q=0.9,en-US;q=0.8,en;q=0.7"),
        (_, Family::Firefox) => format!("{lang}-{cc},{lang};q=0.8,en-US;q=0.5,en;q=0.3"),
    })
}

/// Headers of a request made from the site's own page (a search box), not a
/// typed URL: a POST without an Origin, or a results page reached from
/// nowhere, is not what a browser sends.
pub fn same_site(request: wreq::RequestBuilder, page: &str) -> wreq::RequestBuilder {
    let origin = url::Url::parse(page).map(|u| u.origin().ascii_serialization()).unwrap_or_default();
    request.header("sec-fetch-site", "same-origin").header("origin", origin).header("referer", page)
}

/// Whether a page is a bot challenge rather than results: the engines' own
/// markers (DuckDuckGo's anomaly page among them).
pub fn is_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "captcha",
        "unusual traffic",
        "are you a robot",
        "verify you are human",
        "anomaly-modal",
        "anomaly.js",
        "challenge-form",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

/// Send a scraped engine's request and read its page, the steps every one of
/// them shares. A page without `results_marker` (the class its results carry)
/// is a refusal when it is a challenge: the engines' markers, a vendor the
/// fetcher knows (Cloudflare, DataDome, ...), or a 202, DuckDuckGo's way of
/// serving its anomaly page. Then a non-2xx status is an error.
pub(crate) async fn read_page(
    request: wreq::RequestBuilder,
    results_marker: &str,
) -> Result<String, super::EngineError> {
    use super::{EngineError, browser_transport, status_error};
    let reply = request.send().await.map_err(browser_transport)?;
    let status = reply.status().as_u16();
    let headers = reply.headers().clone();
    let body = reply.text().await.map_err(browser_transport)?;
    if !body.contains(results_marker) {
        if let Some(vendor) = crate::fetch::challenge_vendor(status, &headers, &body) {
            return Err(EngineError::Refused(format!("bot challenge ({vendor})")));
        }
        if status == 202 || is_challenge(&body) {
            return Err(EngineError::Refused("bot challenge".into()));
        }
    }
    if !(200..300).contains(&status) {
        return Err(status_error(status));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_leans_to_the_hosts_os_without_dropping_windows() {
        let on =
            |platform: Platform| PERSONAS.iter().filter(|(_, p)| format!("{p:?}") == format!("{platform:?}")).count();
        let (windows, total) = (on(Platform::Windows), PERSONAS.len());
        assert!(windows >= 4, "Windows stays in the pool");
        assert!(on(Platform::Linux) + on(Platform::MacOS) >= total * 3 / 4, "TTL-64 systems are most of it");
        let versions: std::collections::HashSet<String> =
            PERSONAS.iter().map(|(profile, _)| format!("{profile:?}")).collect();
        assert!(versions.len() >= 7, "versions are spread: {versions:?}");
        for (profile, platform) in PERSONAS {
            builder(*profile, *platform).build().expect("every persona builds");
        }
    }
}

//! Security SHOULD 17: with HTTP_PROXY in the environment, reqwest sent the
//! fetch to the proxy, which resolved the host itself: the PublicOnly
//! resolver never ran, so a proxy on the host would fetch 127.0.0.1 or
//! 169.254.169.254 for a model. Its own binary: it sets process-wide
//! environment variables.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ratatoskr::fetch::Fetcher;

#[tokio::test]
async fn a_fetch_never_goes_through_an_environment_proxy() {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", proxy.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = connections.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = proxy.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    // SAFETY: this binary's only test, set before any client is built.
    unsafe {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            std::env::set_var(key, &url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let fetcher = Fetcher::new(Duration::from_secs(5));
    for target in ["http://ratatoskr-ssrf-test.invalid/", "https://ratatoskr-ssrf-test.invalid/"] {
        assert!(fetcher.fetch(target).await.is_err(), "{target} does not resolve");
    }
    assert_eq!(connections.load(Ordering::SeqCst), 0, "the fetch went to the environment's proxy");
}

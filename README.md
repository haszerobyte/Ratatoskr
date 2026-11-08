# ratatoskr

Web search and page fetching for LLM tools (an agent's `web_search` and
`web_fetch`), in pure Rust, with no API keys required.

Ratatoskr is the squirrel of Yggdrasil who runs messages between its roots and
branches.

## What it does

**Search.** `Ratatoskr::search` asks sources in order and returns the first
useful answer:

1. A meta-search modelled on [ddgs](https://github.com/deedy5/ddgs)'s `auto`
   backend:
   - Wikipedia's and Grokipedia's best page for the query;
   - Yahoo, DuckDuckGo and Yandex, scraped, asked in a random order, two at a
     time, and only one of the two Bing-backed engines (Yahoo, DuckDuckGo) per
     search;
   - stops once there are enough distinct results;
   - merges by URL, and ranks the way ddgs does.
2. A [ddgs](https://github.com/deedy5/ddgs) API server, if `DDGS_API_URL` is set.
3. Brave's Search API (`BRAVE_SEARCH_API_KEY`) and Google's Programmable Search
   (`GOOGLE_CSE_KEY`, `GOOGLE_CSE_ID`), when keyed.
4. Wikipedia's full-text search, last.

**Fetch.** `Ratatoskr::fetch` reads one page as text:
- HTML becomes Markdown-ish text: headings, lists, table cells separated by
  ` | `, links as `[text](absolute url)`;
- JSON, XML and plain text pass through;
- PDFs are returned as bytes for the caller to hand on.

## Being polite

- **Pacing:** each engine has its own minimum spacing between requests, with
  random jitter. Searches made at the same time go to the engines that are free.
- **Backing off:** an engine that refuses (HTTP 403/429, a bot challenge, a
  throttled page) is skipped by a circuit breaker for 10 minutes, doubling up to
  40 on refusals in a row.
- **Caching:** results are cached for 15 minutes, so a repeated question never
  leaves the machine.
- **Challenges:** CAPTCHAs, Cloudflare's checks and proof-of-work walls are
  reported, never solved.

The scraped engines and page fetches present as a real browser, down to the TLS
and HTTP/2 fingerprint and header order, using
[wreq](https://github.com/0x676e67/wreq). Some sites refuse anything else.
Each search starts a fresh browser profile with its own cookies. The API engines
send an honest `User-Agent: Mozilla/5.0 (compatible; Ratatoskr/x.y)`, with
`RATATOSKR_CONTACT` (a URL or email) appended when set.

Scraping search engines may be against their terms of service. You are
responsible for how you use this.

## Safe fetching

A fetch can never reach the local network:
- every address a hostname resolves to is checked at connect time, by its own
  DNS resolver;
- IP-literal URLs and each redirect hop are checked too;
- only http(s), at most 5 redirects;
- a size cap, a timeout, and no environment proxy.

## Use

```rust
use ratatoskr::{Ratatoskr, Sources};

let rt = Ratatoskr::standard(&Sources::from_env());
let found = rt.search("tokio select macro", 5).await?;
for hit in found.hits {
    println!("{} {}", hit.title, hit.url);
}
let page = rt.fetch("https://docs.rs/tokio/latest/tokio/macro.select.html").await?;
println!("{}", page.text);
```

The `ratatoskr` binary does the same from a shell:
`ratatoskr search <query> [limit] [country]` and `ratatoskr fetch <url>`.

## Building

wreq links BoringSSL, so building needs `cmake` and libclang:
- Debian: `apt install cmake libclang-dev`
- macOS: Homebrew's `cmake`, plus Xcode's command-line tools.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Parts are ported from ddgs (MIT, © 2022 deedy5); see [NOTICE](NOTICE).

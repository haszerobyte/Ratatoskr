<p align="center">
  <img src="assets/logo.svg" width="160" alt="ratatoskr logo: a squirrel carrying a message along a branch">
</p>

<h1 align="center">ratatoskr</h1>

<p align="center">
  Web search and page fetching for LLM tools, in pure Rust, with no API key required.
</p>

<p align="center">
  <a href="https://github.com/haszerobyte/Ratatoskr/actions/workflows/ci.yml"><img src="https://github.com/haszerobyte/Ratatoskr/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/ratatoskr-web"><img src="https://img.shields.io/crates/v/ratatoskr-web" alt="crates.io"></a>
  <a href="https://docs.rs/ratatoskr-web"><img src="https://img.shields.io/docsrs/ratatoskr-web" alt="docs.rs"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0"></a>
</p>

Ratatoskr gives an agent its web tools:
- **Search:** the web, news, images, videos and books, each a meta-search
  over several sources, with fallbacks.
- **Fetch:** reads a page as clean, link-preserving text.
- **Safety:** a fetch can never reach your local network.

It is an [MCP server](#mcp-server) for any agent (Claude Code, Codex, Claude
Desktop, Cursor, VS Code, …), a Rust library, and a small command-line tool.
In Norse myth, Ratatoskr is the
squirrel who runs messages up and down the world tree.

## Contents

- [Features](#features)
- [Install](#install)
- [MCP server](#mcp-server)
- [Command line](#command-line)
- [Library](#library)
- [Configuration](#configuration)
- [How search works](#how-search-works)
- [How fetch works](#how-fetch-works)
- [Performance](#performance)
- [Politeness, presentation and terms](#politeness-presentation-and-terms)
- [Building from source](#building-from-source)
- [Contributing](#contributing)
- [License](#license)

## Features

- **An MCP server out of the box:** `ratatoskr mcp` gives any MCP client
  `web_search` (web pages, news, images, videos or books) and `web_fetch`.
- **Web, news, images, videos and books,** each with typed results, and every
  search with a country, a time limit (past day, week, month, year), safe
  search and paging.
- **No API key needed.** Search works out of the box. Brave's and Google's APIs
  are optional fallbacks.
- **Meta-search:**
  - engines: Yahoo, DuckDuckGo, Bing and Yandex, plus Wikipedia's and
    Grokipedia's best page;
  - merged and de-duplicated by URL;
  - ranked by agreement between engines and by where the query's words appear.
- **Resilient:**
  - each engine has its own pacing and circuit breaker;
  - one engine refusing doesn't stop a search;
  - searches made at the same time spread over the engines that are free.
- **Readable pages:**
  - HTML becomes text with headings, lists, ` | `-separated table cells and
    `[text](absolute url)` links;
  - PDFs are read too.
- **SSRF-safe fetching:** private, loopback and link-local addresses are
  refused, at connect time and on every redirect.
- **Cached:** a repeated question within 15 minutes never leaves the machine.
- **Small and fast:** one 10 MB binary. As an MCP server it uses about 6 MB at
  idle and answers in 0.01 s, against 95 MB and 3.6 s for the reference
  Python fetch server (see [Performance](#performance)).

## Install

As a library, from crates.io. The package is `ratatoskr-web` (the name
`ratatoskr` was taken); in code it is `ratatoskr`:

```sh
cargo add ratatoskr-web
cargo add tokio --features macros,rt-multi-thread
```

```rust
use ratatoskr::{Ratatoskr, Sources};
```

As a command-line tool, either:
- download a binary from the
  [releases](https://github.com/haszerobyte/Ratatoskr/releases) (Linux
  x86_64/aarch64, macOS Intel/Apple Silicon, Windows x86_64), with SHA-256
  checksums in `SHA256SUMS`; or
- install it from crates.io:

```sh
cargo install ratatoskr-web --locked     # installs the `ratatoskr` command
```

Building needs `cmake` and libclang; see [Building from source](#building-from-source).

## MCP server

`ratatoskr mcp` serves two tools to an MCP client over stdio:

| Tool | Arguments | Returns |
|---|---|---|
| `web_search` | `query` (required); `type` (`web`, `news`, `images`, `videos`, `books`; default `web`), `limit` (1–20, default 8), `country` (ISO code), `time` (`day`/`week`/`month`/`year`), `safesearch` (`on`/`moderate`/`off`), `page` | a numbered list. Web: title, URL, snippet. News: source and date (ISO 8601). Images: the image's URL, size, the page it is on. Videos: duration, publisher, views. Books: authors and year. |
| `web_fetch` | `url` (required) | the page's title, URL and text, links as `[text](url)` |

Install the binary first (see [Install](#install)), then add it to your client.
If `ratatoskr` isn't on the client's `PATH`, give its full path instead.

**Claude Code**

```sh
claude mcp add ratatoskr -- ratatoskr mcp
# with an optional fallback key:
claude mcp add ratatoskr -e BRAVE_SEARCH_API_KEY=… -- ratatoskr mcp
```

**Codex** (`~/.codex/config.toml`)

```toml
[mcp_servers.ratatoskr]
command = "ratatoskr"
args = ["mcp"]
# env = { BRAVE_SEARCH_API_KEY = "…" }
```

**Claude Desktop** (`claude_desktop_config.json`), **Cursor** (`~/.cursor/mcp.json`)
and most other clients

```json
{
  "mcpServers": {
    "ratatoskr": { "command": "ratatoskr", "args": ["mcp"] }
  }
}
```

**VS Code** (`.vscode/mcp.json`)

```json
{
  "servers": {
    "ratatoskr": { "type": "stdio", "command": "ratatoskr", "args": ["mcp"] }
  }
}
```

The tools then show up under these names (Claude Code prefixes them, as in
`mcp__ratatoskr__web_search`). Requests are
answered concurrently, and a failed search or fetch comes back as a tool error
the model can read, never as a crash. The protocol revisions it speaks are
`2025-06-18`, `2025-03-26` and `2024-11-05`.

Embedding the server in your own program is one call:
`ratatoskr::mcp::serve_stdio(Arc::new(rt)).await`, or `ratatoskr::mcp::serve`
over any reader and writer.

## Command line

```sh
ratatoskr search <query> [limit] [country] [options]
ratatoskr fetch <url>
ratatoskr mcp                  # the MCP server, on stdin/stdout
```

Options: `--type web|news|images|videos|books` (default `web`),
`--time day|week|month|year`, `--safesearch on|moderate|off`, `--page N`,
`--country XX`.

```console
$ ratatoskr search "tokio select macro" 2
engine: meta (cached: false)
- select in tokio - Rust
  https://docs.rs/tokio/latest/tokio/macro.select.html
  tokio. Macro select.The select! macro must be used inside of async functions, closures, and blocks…
- Select | Tokio - An asynchronous Rust runtime
  https://tokio.rs/tokio/tutorial/select
  The tokio::select! macro allows waiting on multiple async computations and returns when a single…

$ ratatoskr search "raspberry pi 5" 2 --type news --time week
sources: yahoo, bing (cached: false)
1. Raspberry Pi 5 price hike means the 16GB model now costs three times what it did...
   TechRadar · 2026-11-07T23:00:05Z
   https://tech.yahoo.com/computing/articles/raspberry-pi-5-price-hike-230000118.html
   …

$ ratatoskr search "cricket scores" 5 IN      # results localized to India
$ ratatoskr search "dune" 3 --type books
$ ratatoskr fetch https://www.raspberrypi.com/news/
```

`country` is an ISO 3166-1 alpha-2 code such as `US`, `IN` or `DE`. The exit
code is 0 on success, 1 when every source failed, and 2 on a usage error.

## Library

### Search

```rust
use ratatoskr::{Ratatoskr, Sources};

#[tokio::main]
async fn main() {
    // Optional keyed sources come from the environment (see Configuration).
    let rt = Ratatoskr::standard(&Sources::from_env());

    match rt.search("rust async runtime", 5).await {
        Ok(found) => {
            println!("answered by {} (cached: {})", found.engine, found.from_cache);
            for hit in found.hits {
                println!("{}\n  {}\n  {}", hit.title, hit.url, hit.snippet);
            }
        }
        // Every source failed or refused; each attempt is listed with its reason.
        Err(failure) => eprintln!("{failure}"),
    }

    // The same, localized to a country.
    let _ = rt.search_in("weather", 5, Some("GB")).await;
}
```

Every option goes in a `SearchRequest`, which the web search and the news,
images, videos and books searches all take:

```rust
use ratatoskr::{SafeSearch, SearchRequest, TimeLimit};

let request = SearchRequest::new("raspberry pi 5", 5)
    .country(Some("GB"))
    .time(Some(TimeLimit::Week))
    .safesearch(SafeSearch::Moderate)
    .page(1);

let web = rt.search_with(&request).await?;     // web pages: SearchOutcome
let news = rt.news(&request).await?;          // Found<NewsResult>: title, url, body, date, source, image
let images = rt.images(&request).await?;      // Found<ImageResult>: image, thumbnail, page, size, source
let videos = rt.videos(&request).await?;      // Found<VideoResult>: url, duration, publisher, views, …
let books = rt.books(&request).await?;        // Found<BookResult>: authors, year, cover, page
println!("{}", ratatoskr::format::news(&news.results));  // the numbered text the tools return
```

Build a `Ratatoskr` once and share it, for example in an `Arc`. Its rate
limits, circuit breakers and cache only work across calls to the same instance.

### Fetch

```rust
use ratatoskr::fetch::FetchError;

match rt.fetch("https://docs.rs/tokio/latest/tokio/macro.select.html").await {
    Ok(page) => {
        println!("{} ({})", page.title.unwrap_or_default(), page.content_type);
        println!("{}", page.text);          // Markdown-ish text, links kept
        if page.truncated { println!("[cut at a size cap]"); }
        if let Some(pdf) = page.pdf { println!("a PDF of {} bytes", pdf.len()); }
    }
    Err(FetchError::Blocked(why)) => eprintln!("not a public address: {why}"),
    Err(FetchError::Challenge(vendor)) => eprintln!("the site answered with a {vendor} bot check"),
    Err(FetchError::Http(status)) => eprintln!("HTTP {status}"),
    Err(other) => eprintln!("{other}"),
}
```

| `FetchError` | Meaning |
|---|---|
| `BadUrl` | not http(s), unparseable, or no host |
| `Blocked` | the target is loopback, private, link-local or otherwise not public |
| `Http(status)` | the site answered with an error status |
| `Challenge(vendor)` | a bot check (Cloudflare, DataDome, Imperva, …) instead of the page |
| `Unsupported(type)` | not a type that can become text (images, archives, …) |
| `TooLarge(cap)` | a PDF over the 32 MB cap |
| `Transport(why)` | the request did not complete (timeout, DNS, TLS) |

### Your own engines and settings

`Ratatoskr::standard` uses the production set and default settings. To choose
the engines or tune the limits, build one yourself:

```rust
use std::{sync::Arc, time::Duration};
use ratatoskr::{Ratatoskr, Settings};
use ratatoskr::engine::{Engine, http_client, wikipedia::Wikipedia, yandex::Yandex};

let settings = Settings {
    min_interval: Duration::from_secs(3),   // spacing between requests to one engine
    cooldown: Duration::from_secs(900),     // rest after a refusal (doubles, up to 4x)
    ..Settings::default()
};
let engines: Vec<Arc<dyn Engine>> = vec![
    Arc::new(Yandex::new(settings.timeout)),
    Arc::new(Wikipedia::new(http_client(settings.timeout), "en")),
];
let rt = Ratatoskr::new(engines, settings);
```

Any type implementing `ratatoskr::engine::Engine` can be added. It needs a name
and a `search`; a country-aware `search_in` is optional.

`rt.status()` lists each source with how long it is still resting, if it is.

## Configuration

Everything is optional.

| Variable | Effect |
|---|---|
| `SEARCH_API_URL` | an external search server, used after the meta-search: it answers `POST /search/text` (`{"query", "max_results"}`) with `{"results": [{"title", "href", "body"}]}`. Bind it to loopback. |
| `BRAVE_SEARCH_API_KEY` | Brave Search API, as a fallback |
| `GOOGLE_CSE_KEY`, `GOOGLE_CSE_ID` | Google Programmable Search, as a fallback (both needed) |
| `RATATOSKR_CONTACT` | a URL or email appended to the API engines' User-Agent, so their operators can reach you |

Defaults (`Settings::default()`):

| Setting | Default |
|---|---|
| spacing between requests to one engine | 2 s, plus up to 50% random jitter |
| rest after a refusal | 10 min, doubling to at most 40 min |
| cache | 15 min, 256 queries |
| request timeout | 15 s |

## How search works

`search` asks sources in order and returns the first one that gives results:

1. **The meta-search:**
   - Wikipedia and Grokipedia each add the one article whose title matches the
     query, if there is one;
   - the web engines are asked a few at a time, in a random order: Yahoo,
     DuckDuckGo and Bing (all serve Bing's index, so one of them per search)
     and Yandex (its own index);
   - it stops once there are enough distinct results, so a typical search costs
     two web requests, not one per engine;
   - results are de-duplicated by URL;
   - those several engines found come first, then Wikipedia pages, then by
     where the query's words appear (title and snippet, title, snippet).
2. **An external search server** (`SEARCH_API_URL`), if configured.
3. **Brave's and Google's APIs**, if keyed.
4. **Wikipedia's full-text search.**

If no web engine answers, the meta-search doesn't pass off the encyclopedia
pages as an answer: the next source gets its turn. A later page or a
time-limited search leaves the encyclopedias out.

The other searches work the same way, each with its own sources:

| Search | Sources |
|---|---|
| news | DuckDuckGo and Bing (Bing's index, one of them per search), Yahoo News |
| images | Bing; DuckDuckGo when it doesn't ask its proof of work |
| videos | DuckDuckGo |
| books | Open Library's official API |

Dates in news results are ISO 8601, worked out from a timestamp or an age such
as "20 hours ago".

Some engines aren't used:
- Google Search: it answers every client that doesn't run JavaScript with a
  page asking to enable it. Use Google's Programmable Search API instead
  (`GOOGLE_CSE_KEY`, `GOOGLE_CSE_ID`).
- Brave and Mojeek: their result pages ask for a CAPTCHA.
- Startpage: it asks for a proof of work.

## How fetch works

- **Formats:**
  - HTML is reduced to its main content (`<main>`, `<article>`, else `<body>`),
    without scripts, styles or navigation;
  - JSON, XML and plain text pass through;
  - a PDF's text is extracted, and its bytes are returned in `page.pdf`.
- **Caps:** 2 MB of body, 32 MB for a PDF, and 100,000 characters of text.
  `truncated` says whether a cap was hit.
- **Redirects:** followed up to 5, each one checked.
- **Retries:**
  - a `403` or a bot check is tried once more as a different browser;
  - a `429` asking to wait 5 s or less is waited out and tried once more.
- **Cookies:** kept only for the duration of one fetch.
- **Proxies:** environment proxies (`HTTP(S)_PROXY`) are ignored, since a proxy
  would bypass the address checks.

## Performance

ratatoskr compared with the reference MCP fetch server,
[`mcp-server-fetch`](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch)
2026.11.4 (Python).

**Setup:**
- one machine, ratatoskr 0.1.0 release build;
- each tool runs in its own fresh process;
- times are medians over 5 calls.

The numbers are for comparison on the same machine, not absolutes: run
`bench/compare.sh` to measure on yours.

**As an MCP server:**

| | ratatoskr `web_fetch` | mcp-server-fetch `fetch` |
|---|---|---|
| ready to answer | **0.01 s** | 3.58 s |
| memory at idle | **6 MB** | 95 MB |
| memory after the calls | **44 MB** | 143 MB |
| time per call (median) | **0.11 s** | 1.59 s |
| install size | **9.8 MB**, one binary, search included | 83.9 MB environment |

ratatoskr's own search, measured the same way:

| `web_search` | |
|---|---|
| time per call (median) | 1.06 s |
| memory at idle, after 5 searches | 6 MB, 20 MB |
| one-shot command line | 1.13 s, 19 MB peak |

Search time is mostly the search engines' own response time. It varies with
your network and location, and from hour to hour. Memory and startup barely
vary.

Reproduce it with `bench/compare.sh [path/to/ratatoskr]`. It needs `uv` and
`python3`, and installs the Python server into a throwaway environment.

## Politeness, presentation and terms

Ratatoskr is meant for an agent answering one person's questions, not for
crawling:
- each engine is paced, with random jitter;
- an engine that refuses (`403`, `429`, a bot check, a throttled page) is rested;
- answers are cached;
- **bot checks are never solved:** CAPTCHAs, Cloudflare and proof-of-work walls
  are reported as such.

The scraped engines and page fetches present as a real browser, using
[wreq](https://github.com/0x676e67/wreq):
- TLS and HTTP/2 fingerprints, header order and navigation headers match the
  browser;
- the pool covers recent Chrome, Edge and Firefox releases on Linux, macOS and
  Windows;
- each search starts a fresh browser profile with its own cookies.

Many sites refuse anything else.

Some sites get ratatoskr's own, honest User-Agent instead:
- the API engines;
- Wikimedia's sites (Wikipedia, Wikidata, Wiktionary, …). Their
  [User-Agent policy](https://meta.wikimedia.org/wiki/User-Agent_policy) asks
  automated clients to identify themselves, and they rate-limit an
  impersonated browser.

Scraping search engines may be against their terms of service, and some sites'
terms forbid automated access. You are responsible for how you use this
software.

## Building from source

The rust toolchain version is pinned in `rust-toolchain.toml`; rustup picks it
up automatically. wreq links BoringSSL, which needs:

| OS | Install |
|---|---|
| Debian / Ubuntu | `sudo apt install cmake clang libclang-dev` |
| Fedora | `sudo dnf install cmake clang-devel` |
| macOS | `brew install cmake`, plus Xcode's command-line tools |
| Windows | Visual Studio Build Tools, CMake, [NASM](https://nasm.us) and LLVM (`LIBCLANG_PATH`) |

```sh
git clone https://github.com/haszerobyte/Ratatoskr
cd ratatoskr
cargo build --release          # target/release/ratatoskr
cargo test                     # offline: tests use recorded pages and local fake servers
```

CI builds and tests on Linux (x86_64, aarch64), macOS (Intel, Apple Silicon) and
Windows. Releases are published by hand, and only after CI passes (see
[Contributing](#contributing)).

## Contributing

Issues and pull requests are welcome. Before opening one:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

To publish a release:
1. bump `version` in `Cargo.toml` and merge it;
2. run **Actions → Release → Run workflow** with that version.

The workflow then runs in order, stopping at the first failure:
- checks that the version matches `Cargo.toml` and is on neither GitHub nor
  crates.io yet, and that the crate packages (`cargo publish --dry-run`);
- runs the full CI on every platform;
- builds the binaries;
- publishes `ratatoskr-web` to crates.io;
- tags the tested commit `v<version>` and publishes a GitHub release with
  SHA-256 checksums.

It needs one repository secret: `CARGO_REGISTRY_TOKEN`, a crates.io API token
with the publish-new and publish-update scopes.

Search engines change their pages. When an engine stops parsing:
1. capture a fresh results page into `tests/fixtures/`;
2. replace per-request tracking tokens with `TOKEN`;
3. adjust the engine's parser.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE)
for the attributions it requires.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work shall be licensed as above, without any additional
terms or conditions.

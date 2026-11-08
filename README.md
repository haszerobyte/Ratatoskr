<p align="center">
  <img src="assets/logo.svg" width="160" alt="ratatoskr logo: a squirrel carrying a message along a branch">
</p>

<h1 align="center">ratatoskr</h1>

<p align="center">
  Web search and page fetching for AI agents, in pure Rust, with no API key required.
</p>

<p align="center">
  <a href="https://github.com/haszerobyte/Ratatoskr/actions/workflows/ci.yml"><img src="https://github.com/haszerobyte/Ratatoskr/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/ratatoskr-web"><img src="https://img.shields.io/crates/v/ratatoskr-web" alt="crates.io"></a>
  <a href="https://docs.rs/ratatoskr-web"><img src="https://img.shields.io/docsrs/ratatoskr-web" alt="docs.rs"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0"></a>
</p>

Ratatoskr gives an agent two tools:
- **`web_search`:** searches the web, news, images, videos or books, merging
  several engines;
- **`web_fetch`:** reads a page as clean text with its links kept, and never
  reaches your local network.

It comes as an [MCP server](#mcp-server) for any agent (Claude Code, Codex,
Claude Desktop, Cursor, VS Code, …), a [command-line tool](#command-line) and
a [Rust library](#library). In Norse myth, Ratatoskr is the squirrel who runs
messages up and down the world tree.

## Quick start

1. Install the `ratatoskr` command, either:
   - from the [releases](https://github.com/haszerobyte/Ratatoskr/releases),
     with no build tools needed; or
   - with `cargo install ratatoskr-web --locked`, which needs
     [cmake and libclang](#building-from-source).
2. Add it to your agent, for example Claude Code:
   ```sh
   claude mcp add ratatoskr -- ratatoskr mcp
   ```
   For other clients, see [MCP server](#mcp-server).
3. Or try it in a terminal:
   ```sh
   ratatoskr search "tokio select macro"
   ratatoskr fetch https://www.rust-lang.org/
   ```

## Contents

- [Features](#features)
- [Install](#install)
- [MCP server](#mcp-server)
- [Command line](#command-line)
- [Library](#library)
- [Configuration](#configuration)
- [How it works](#how-it-works): [search](#search), [fetch](#fetch),
  [politeness and terms](#politeness-and-terms)
- [Performance](#performance)
- [Building from source](#building-from-source)
- [Contributing](#contributing)
- [Releasing](#releasing)
- [License](#license)

## Features

- **No API key needed.** Search works out of the box. Brave's and Google's
  search APIs, and your own search server, are optional fallbacks.
- **Five kinds of search:** web pages, news, images, videos and books, each
  with typed results. Every search can be localized to a country, limited to
  the past day, week, month or year, filtered by safe search, and paged.
- **A meta-search, not one engine:** Yahoo, DuckDuckGo, Bing and Yandex,
  plus Wikipedia's and Grokipedia's best page. Results are merged and ranked
  by how many engines agree and how well they match the query.
- **Resilient:** every engine has its own pace, health record and circuit
  breaker. One engine refusing never stops a search.
- **Readable pages:** HTML becomes text with headings, lists, tables and
  `[text](url)` links. PDFs are read too.
- **Safe fetching:** private, loopback and link-local addresses are refused,
  at connect time and on every redirect.
- **Small and fast:** one 10 MB binary. As an MCP server it uses about 6 MB
  at idle and is ready in 0.01 s (see [Performance](#performance)).

## Install

**The command (`ratatoskr`):**
- **Prebuilt:** download a binary from the
  [releases](https://github.com/haszerobyte/Ratatoskr/releases) for Linux
  (x86_64, aarch64), macOS (Intel, Apple Silicon) or Windows (x86_64). Check
  it against `SHA256SUMS`.
- **From crates.io:** `cargo install ratatoskr-web --locked`. This needs
  `cmake` and libclang; see [Building from source](#building-from-source).

**The library:** the package is `ratatoskr-web`, because the name `ratatoskr`
was taken on crates.io. In code it's still `ratatoskr`.

```sh
cargo add ratatoskr-web
cargo add tokio --features macros,rt-multi-thread
```

```rust
use ratatoskr::{Ratatoskr, Sources};
```

## MCP server

`ratatoskr mcp` serves two tools to an MCP client over stdio.

**`web_search`**
- `query`, required: what to search for.
- `type`: `web` (the default), `news`, `images`, `videos` or `books`.
- `limit`: 1–20, default 8.
- `country`: an ISO 3166-1 alpha-2 code such as `US`, `IN` or `DE`.
- `time`: `day`, `week`, `month` or `year`. Not used for books.
- `safesearch`: `on`, `moderate` (the default) or `off`.
- `page`: from 1.

It returns a numbered list:

| `type` | Each result has |
|---|---|
| `web` | title, URL, snippet |
| `news` | title, source and date (ISO 8601), URL, summary |
| `images` | title, the image's URL, its size and site, the page it is on |
| `videos` | title, duration, publisher, date and views, URL, description |
| `books` | title, authors and year, the book's page |

**`web_fetch`**
- `url`, required. It returns the page's title, URL and text, with links as
  `[text](url)`.

Add it to your client. If `ratatoskr` isn't on the client's `PATH`, give its
full path instead.

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

**Claude Desktop** (`claude_desktop_config.json`), **Cursor**
(`~/.cursor/mcp.json`) and most other clients

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

The tools keep these names; Claude Code shows them as
`mcp__ratatoskr__web_search` and `mcp__ratatoskr__web_fetch`. A failed search
or fetch comes back as a tool error the model can read, never as a crash, and
requests are answered concurrently. The server speaks protocol revisions
`2025-06-18`, `2025-03-26` and `2024-11-05`.

To embed the server in your own program, call
`ratatoskr::mcp::serve_stdio(Arc::new(rt)).await`, or `ratatoskr::mcp::serve`
over any reader and writer.

## Command line

```sh
ratatoskr search <query> [limit] [country] [options]
ratatoskr fetch <url>
ratatoskr mcp                  # the MCP server, on stdin/stdout
```

Search options:
- `--type web|news|images|videos|books` (default `web`)
- `--time day|week|month|year`
- `--safesearch on|moderate|off`
- `--page N`
- `--country XX`

```console
$ ratatoskr search "tokio select macro" 2
engine: meta (cached: false)
1. select in tokio - Rust
   https://docs.rs/tokio/latest/tokio/macro.select.html
   tokio. Macro select.The select! macro must be used inside of async functions, closures, and blocks…

2. Select | Tokio - An asynchronous Rust runtime
   https://tokio.rs/tokio/tutorial/select
   The tokio::select! macro allows waiting on multiple async computations and returns when a single…

$ ratatoskr search "raspberry pi 5" 2 --type news --time week
sources: bing, yahoo (cached: false)
1. The Raspberry Pi 5 16GB now costs $385, more than triple its launch price
   TechSpot · 2026-11-06T20:04:12Z
   https://www.techspot.com/news/114465-raspberry-pi-5-16gb-now-costs-385-more.html
   …

$ ratatoskr search "cricket scores" 5 IN      # results localized to India
$ ratatoskr search "dune" 3 --type books
$ ratatoskr fetch https://www.rust-lang.org/
```

The exit code is 0 on success, 1 when every source failed, and 2 on a usage
error.

## Library

### Searching

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
}
```

Build a `Ratatoskr` once and share it, for example in an `Arc`. Its pacing,
health records, circuit breakers and cache only work across calls to the
same instance.

### Search options and the other kinds

Options go in a `SearchRequest`. The web search and the news, images, videos
and books searches all take one:

```rust
use ratatoskr::{SafeSearch, SearchRequest, TimeLimit};

let request = SearchRequest::new("raspberry pi 5", 5)
    .country(Some("GB"))
    .time(Some(TimeLimit::Week))
    .safesearch(SafeSearch::Moderate)
    .page(1);

let web = rt.search_with(&request).await?;   // SearchOutcome: hits, engine, from_cache
let news = rt.news(&request).await?;         // Found<NewsResult>: title, url, body, date, source, image
let images = rt.images(&request).await?;     // Found<ImageResult>: image, thumbnail, url, width, height, source
let videos = rt.videos(&request).await?;     // Found<VideoResult>: url, duration, publisher, views, …
let books = rt.books(&request).await?;       // Found<BookResult>: authors, year, publisher, thumbnail, url
println!("{}", ratatoskr::format::news(&news.results));  // the numbered text the tools return
```

`rt.search_in("weather", 5, Some("GB"))` is the short form for a country
alone.

### Fetching a page

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

`Ratatoskr::standard` uses every engine and the default settings. To choose
the engines or change the limits, build one yourself:

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

Any type that implements `ratatoskr::engine::Engine` can be added. An engine
needs:
- a `name` and a `search`;
- optionally, `search_in` to localize results to a country;
- optionally, `search_request` to take the time limit, safe search and page.

The news, images, videos and books sources are added with
`.with_verticals(…)`. Each implements `ratatoskr::vertical::Source`.

`rt.status()` and `rt.vertical_status()` list each source and how long it is
still resting, if it is.

## Configuration

Everything is optional.

| Variable | Effect |
|---|---|
| `SEARCH_API_URL` | your own search server, asked after the meta-search. It answers `POST /search/text` (`{"query", "max_results"}`) with `{"results": [{"title", "href", "body"}]}`. Bind it to loopback. |
| `BRAVE_SEARCH_API_KEY` | Brave's Search API, as a fallback |
| `GOOGLE_CSE_KEY`, `GOOGLE_CSE_ID` | Google's Programmable Search, as a fallback (both needed) |
| `RATATOSKR_CONTACT` | a URL or email added to the API engines' User-Agent, so their operators can reach you |

The defaults (`Settings::default()`):

| Setting | Default |
|---|---|
| spacing between requests to one engine | 2 s, plus up to 50% random jitter |
| rest after a refusal | 10 min, doubling to at most 40 min |
| cache | 15 min, 256 queries |
| request timeout | 15 s |

## How it works

### Search

A web search asks its sources in order, and the first one with results
answers:

1. **The meta-search:**
   - **Engines:** Yahoo, DuckDuckGo and Bing (all three serve Bing's index,
     so only one of them is asked per search) and Yandex (its own index).
   - **Which engines are asked:**
     - every engine keeps a running record of how often it answers, how fast,
       and how many results it gives;
     - an engine not yet tried goes first, then the healthiest, with some
       randomness so no engine sees every query;
     - as many are asked at once as their usual yield needs, at least two, so
       one failing costs no extra round trip.
   - **When it stops:** once there are enough distinct results. A typical
     search costs two web requests, not one per engine.
   - **Encyclopedias:** Wikipedia and Grokipedia each add the one article
     whose title matches the query, if there is one. They're left out for a
     later page or a time-limited search.
   - **Ranking:** results are merged by URL and scored. Every engine that
     found a page gives it a vote, worth less the lower it ranked it. The
     page also earns points for each of the query's words in its title
     (counted double), snippet and URL. A page whose snippet one engine left
     empty gets another engine's.
2. **Your own search server** (`SEARCH_API_URL`), if configured.
3. **Brave's and Google's APIs,** if keyed.
4. **Wikipedia's full-text search.**

If no web engine answers, the meta-search doesn't pass off the encyclopedia
pages as an answer, so the next source gets its turn.

The other kinds of search work the same way, each with its own sources:

| Search | Sources |
|---|---|
| news | DuckDuckGo and Bing (Bing's index, one of them per search), Yahoo News |
| images | Bing; DuckDuckGo when it doesn't ask for its proof of work |
| videos | DuckDuckGo |
| books | Open Library's official API |

Dates in news results are ISO 8601, worked out from a timestamp or from an
age such as "20 hours ago".

Some engines aren't used:
- **Google Search:** it answers every client that doesn't run JavaScript with
  a page asking to enable it. Use Google's Programmable Search API instead
  (`GOOGLE_CSE_KEY`, `GOOGLE_CSE_ID`).
- **Brave and Mojeek:** their result pages ask for a CAPTCHA.
- **Startpage:** it asks for a proof of work.

### Fetch

- **Which part of the page:**
  - all of it that a reader sees, menus, header, sidebars and footer
    included: no guess at the "main content", which on a scoreboard, a
    listing or a search page drops the items it holds; scripts, styles,
    a drop-down's unchosen options and anything hidden (`hidden`,
    `aria-hidden`, inline `display: none`) are left out;
  - a menu the page repeats (header and footer, desktop and mobile) is read
    once;
  - a page rendered by script, with almost no text, adds the text fields of
    its JSON-LD data.
- **Formats:**
  - HTML becomes text with headings, lists, ` | `-separated table cells,
    `[text](absolute url)` links, code blocks fenced and kept as written,
    inline code in backticks, and images as `[image: alt text]`;
  - JSON, XML and plain text pass through;
  - a PDF's text is extracted, and its bytes are returned in `page.pdf`.
- **Size caps:** 2 MB of body, 32 MB for a PDF, and 100,000 characters of
  text. `truncated` says whether a cap was hit.
- **Redirects:** followed up to 5, each one checked against the address
  rules.
- **Retries:**
  - a `403` or a bot check is tried once more as a different browser;
  - a `429` asking to wait 5 s or less is waited out and tried once more.
- **Cookies:** kept only for the duration of one fetch.
- **Proxies:** environment proxies (`HTTP(S)_PROXY`) are ignored, since a
  proxy would bypass the address checks.

### Politeness and terms

Ratatoskr is meant for an agent answering one person's questions, not for
crawling:
- each engine is paced, with random jitter;
- an engine that refuses (`403`, `429`, a bot check, a throttled page) is
  rested;
- answers are cached;
- **bot checks are never solved:** CAPTCHAs, Cloudflare checks and
  proof-of-work walls are reported as such.

Many sites refuse anything but a real browser, so the scraped engines and page
fetches present as one, using [wreq](https://github.com/0x676e67/wreq):
- TLS and HTTP/2 fingerprints, header order and navigation headers match the
  browser;
- the browser profiles cover recent Chrome, Edge and Firefox releases on
  Linux, macOS and Windows;
- each search starts a fresh browser profile with its own cookies.

Some sites get ratatoskr's own, honest User-Agent instead:
- **the API engines;**
- **Wikimedia's sites** (Wikipedia, Wikidata, Wiktionary, …). Their
  [User-Agent policy](https://meta.wikimedia.org/wiki/User-Agent_policy) asks
  automated clients to identify themselves, and they rate-limit an
  impersonated browser.

Scraping search engines may be against their terms of service, and some
sites' terms forbid automated access. You are responsible for how you use this
software.

## Performance

ratatoskr compared with the reference MCP fetch server,
[`mcp-server-fetch`](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch)
2026.11.4 (Python), as MCP servers:

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

**How it was measured:**
- on one machine, with the ratatoskr 0.1.0 release build;
- each tool runs in its own fresh process;
- times are medians over 5 calls.

The numbers are for comparison, not absolutes. Search time is mostly the
search engines' own response time, and it varies with your network, location
and the hour. Memory and startup barely vary.

To measure on your machine, run `bench/compare.sh [path/to/ratatoskr]`. It
needs `uv` and `python3`, and installs the Python server into a throwaway
environment.

## Building from source

The Rust toolchain version is pinned in `rust-toolchain.toml`; rustup picks
it up automatically. wreq links BoringSSL, which needs:

| OS | Install |
|---|---|
| Debian / Ubuntu | `sudo apt install cmake clang libclang-dev` |
| Fedora | `sudo dnf install cmake clang-devel` |
| macOS | `brew install cmake`, plus Xcode's command-line tools |
| Windows | Visual Studio Build Tools, CMake, [NASM](https://nasm.us) and LLVM (`LIBCLANG_PATH`) |

```sh
git clone https://github.com/haszerobyte/Ratatoskr
cd Ratatoskr
cargo build --release          # target/release/ratatoskr
cargo test                     # offline: tests use recorded pages and local fake servers
```

CI builds and tests on Linux (x86_64, aarch64), macOS (Intel, Apple Silicon)
and Windows.

## Contributing

Issues and pull requests are welcome. Before opening one, run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Search engines change their pages. When an engine stops parsing:
1. capture a fresh results page into `tests/fixtures/`;
2. replace its per-request tracking tokens with `TOKEN`;
3. adjust the engine's parser.

## Releasing

Releases are published by the **Release** workflow, and only after CI
passes. Run **Actions → Release → Run workflow** on `main` with the new
version, e.g. `0.1.1`. There's no need to edit `Cargo.toml` first.

The workflow runs these steps in order, stopping at the first failure:
1. checks that the version is a semantic version, isn't below `Cargo.toml`'s,
   and isn't released on GitHub or crates.io yet;
2. writes it into `Cargo.toml` and `Cargo.lock`, and commits that to `main`
   as "Release <version>";
3. checks that the crate packages (`cargo publish --dry-run`);
4. runs the full CI on that commit, on every platform;
5. builds the binaries;
6. drafts the GitHub release with them and their SHA-256 checksums, so a
   GitHub-side failure stops it before anything is published;
7. publishes `ratatoskr-web` to crates.io;
8. tags the commit `v<version>` and publishes the drafted release.

It needs these repository secrets:
- **`CARGO_REGISTRY_TOKEN`** (required): a crates.io API token with the
  publish-new and publish-update scopes.
- **`RELEASE_TOKEN`** (optional): a fine-grained token with Contents read and
  write on this repository only, used for the GitHub release. Without it, the
  workflow's own token is used.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See
[NOTICE](NOTICE) for its notices.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work shall be licensed as above, without any
additional terms or conditions.

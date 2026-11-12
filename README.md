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
  <a href="https://registry.modelcontextprotocol.io/v0/servers?search=io.github.haszerobyte/ratatoskr"><img src="https://img.shields.io/badge/MCP_Registry-io.github.haszerobyte%2Fratatoskr-blue" alt="MCP Registry"></a>
  <a href="https://glama.ai/mcp/servers/haszerobyte/Ratatoskr"><img src="https://glama.ai/mcp/servers/haszerobyte/Ratatoskr/badges/score.svg" alt="Glama score"></a>
  <a href="https://github.com/haszerobyte/Ratatoskr/pkgs/container/ratatoskr"><img src="https://img.shields.io/badge/container-ghcr.io%2Fhaszerobyte%2Fratatoskr-blue?logo=docker&logoColor=white" alt="Container image"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0"></a>
  <a href="https://m8ven.ai/mcp/haszerobyte/ratatoskr?s=readme"><img src="https://m8ven.ai/badge/mcp/haszerobyte/ratatoskr" alt="M8ven Score"></a>
  <a href="https://mcprush.com/to/ratatoskr-mcp?ref=badge"><img src="https://mcprush.com/badge/ratatoskr-mcp/score.svg" alt="Ratatoskr MCP on mcprush"></a>
</p>

Ratatoskr gives an agent two tools:
- **`web_search`:** searches the web, news, images, videos or books, merging
  several engines;
- **`web_fetch`:** reads a page as clean text with its links kept, and never
  reaches your local network.

It comes as an [MCP server](#mcp-server) for any agent (Claude Code, Codex,
Claude Desktop, Cursor, VS Code, …), a [command-line tool](#command-line), a
local [HTTP API](#http-api) for scripts and automation tools, and a
[Rust library](#library). In Norse myth, Ratatoskr is the squirrel who runs
messages up and down the world tree.

## Contents

- [Quick start](#quick-start) · [Features](#features) · [Install](#install)
- [MCP server](#mcp-server) · [Command line](#command-line) · [HTTP API](#http-api) · [Library](#library)
- [Documentation](#documentation): how it works, configuration, performance, development

## Quick start

1. Install the `ratatoskr` command, either:
   - with [`cargo binstall ratatoskr-web`](https://github.com/cargo-bins/cargo-binstall),
     which downloads the prebuilt binary, no build tools needed;
   - from the [releases](https://github.com/haszerobyte/Ratatoskr/releases),
     also prebuilt; or
   - with `cargo install ratatoskr-web --locked`, which builds it and needs
     [cmake and libclang](docs/development.md#building-from-source).
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
  at idle and is ready in 0.01 s (see [Performance](docs/performance.md)).

## Install

**The command (`ratatoskr`):**
- **With cargo-binstall:** `cargo binstall ratatoskr-web` fetches the prebuilt
  binary for your platform from the release, with no build tools.
- **Prebuilt:** download a binary from the
  [releases](https://github.com/haszerobyte/Ratatoskr/releases) for Linux
  (x86_64, aarch64), macOS (Intel, Apple Silicon) or Windows (x86_64). Check
  it against `SHA256SUMS`.
- **From crates.io:** `cargo install ratatoskr-web --locked`. This needs
  `cmake` and libclang; see [Building from source](docs/development.md#building-from-source).
- **As a container** (Linux, amd64 and arm64):
  [`ghcr.io/haszerobyte/ratatoskr`](https://github.com/haszerobyte/Ratatoskr/pkgs/container/ratatoskr),
  tagged with each version and `latest`:
  ```sh
  docker pull ghcr.io/haszerobyte/ratatoskr
  docker run -i --rm ghcr.io/haszerobyte/ratatoskr                          # the MCP server on stdio
  docker run --rm ghcr.io/haszerobyte/ratatoskr search "tokio select"       # any other command
  ```
  [`packaging/docker/Dockerfile`](packaging/docker/Dockerfile) builds the same
  image from source: `docker build -f packaging/docker/Dockerfile .`

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
full path instead. It's also listed in the official
[MCP Registry](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.haszerobyte/ratatoskr)
as `io.github.haszerobyte/ratatoskr`, for clients that install from there.

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

**With Docker instead of an installed binary**, in any of the configurations
above use the container as the command:

```json
{ "command": "docker", "args": ["run", "-i", "--rm", "ghcr.io/haszerobyte/ratatoskr"] }
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
ratatoskr serve                # the HTTP API, on 127.0.0.1:8765 (see below)
```

Search options:
- `--type web|news|images|videos|books` (default `web`)
- `--time day|week|month|year`
- `--safesearch on|moderate|off`
- `--page N`
- `--country XX`

```console
$ ratatoskr search "tokio select macro" 2
engine: meta (bing, yandex; cached: false)
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

## HTTP API

For anything that isn't an MCP client: a script in any language, n8n,
Home Assistant, cron, an agent framework without MCP. One long-running
process serves every caller, so they share its cache, its engine health and
its pacing; the command line starts fresh each time.

```sh
ratatoskr serve                                   # http://127.0.0.1:8765
curl -s localhost:8765/search -d '{"query": "tokio select macro", "limit": 3}'
curl -s localhost:8765/search -d '{"query": "raspberry pi 5", "type": "news", "time": "week"}'
curl -s localhost:8765/fetch  -d '{"url": "https://www.rust-lang.org/"}'
```

| Endpoint | Body | Answer |
|---|---|---|
| `GET /health` | | `{"status": "ok", "name", "version"}` |
| `POST /search` | the [`web_search`](#mcp-server) arguments: `query`, `type`, `limit`, `country`, `time`, `safesearch`, `page` | `{"type", "query", "engine", "sources", "merged", "from_cache", "results": [...]}` (`engine` and `merged` for web only) |
| `POST /fetch` | `{"url"}` | `{"url", "title", "content_type", "truncated", "text"}` |
| `POST /search/text` | `{"query", "max_results", "region", "safesearch", "timelimit", "page"}` | `{"results": [{"title", "href", "body"}]}` |

It listens on this machine only unless given a token, and refuses requests
from web pages. Error codes, the token, and the search-server endpoint
`/search/text`: [docs/http-api.md](docs/http-api.md).

## Library

```rust
use ratatoskr::{Ratatoskr, Sources};

#[tokio::main]
async fn main() {
    // Optional keyed sources come from the environment (docs/configuration.md).
    let rt = Ratatoskr::standard(&Sources::from_env());

    match rt.search("rust async runtime", 5).await {
        Ok(found) => {
            // e.g. "meta (bing, yandex; 2 duplicates merged; cached: false)"
            println!("{}", found.provenance());
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

Search options, the news, images, videos and books searches, fetching, and
your own engines and settings: [docs/library.md](docs/library.md).

## Documentation

- [How it works](docs/how-it-works.md): the meta-search and its ranking, how a page is read, politeness and terms
- [Configuration](docs/configuration.md): environment variables and optional keyed sources
- [HTTP API](docs/http-api.md): endpoints, errors and security
- [Library](docs/library.md): the Rust API
- [Performance](docs/performance.md): measurements, and how to run the benchmark
- [Development](docs/development.md): building from source, repository layout, contributing, releasing

## Contributing

Issues and pull requests are welcome. Building, the checks to run and the
repository layout are in [docs/development.md](docs/development.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See
[NOTICE](NOTICE) for its notices.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work shall be licensed as above, without any
additional terms or conditions.

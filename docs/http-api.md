# HTTP API

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
| `POST /search` | the [`web_search`](../README.md#mcp-server) arguments: `query`, `type`, `limit`, `country`, `time`, `safesearch`, `page` | `{"type", "query", "engine", "sources", "merged", "from_cache", "results": [...]}` (`engine` and `merged` for web only) |
| `POST /fetch` | `{"url"}` | `{"url", "title", "content_type", "truncated", "text"}` |
| `POST /search/text` | `{"query", "max_results", "region", "safesearch", "timelimit", "page"}` | `{"results": [{"title", "href", "body"}]}` |

`/search/text` is the shape search servers commonly expose, so `ratatoskr serve`
can stand in wherever such a server is configured, including another
ratatoskr's `SEARCH_API_URL`.

Errors are JSON `{"error": …}`: 400 for a bad request, 401 without the token,
403 for a URL on a private network (with `"kind": "blocked"`) or a request
refused as below, 413 for a body over 64 KB, 502 when every engine or the site
failed.

**Only loopback by default.** `--listen 0.0.0.0:8765` (any address but
loopback) refuses to start without `RATATOSKR_API_TOKEN`; clients then send
`Authorization: Bearer <token>`. Fetches can never reach the local network
either way, but an open search relay would hand your address and its
standing with the search engines to whoever finds it.

**Not for web pages.** Without a token, only requests addressed to
`localhost`, `127.0.0.1` or `[::1]` and not sent by another site's web page are
served: a page open in your browser could otherwise make the server search
and fetch for it, and read the answers by pointing its own name at 127.0.0.1
(DNS rebinding). To call the server under another name, from a container
for instance, set a token.

In your own program: `ratatoskr::http::serve(Arc::new(rt), listener, token)`,
or mount `ratatoskr::http::router(...)` in an axum app.

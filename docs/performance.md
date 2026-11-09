# Performance

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

To measure on your machine, run [`bench/compare.sh`](../bench/compare.sh) `[path/to/ratatoskr]`. It
needs `uv` and `python3`, and installs the Python server into a throwaway
environment.

# Configuration

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

# How it works

## Search

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
     (counted double), snippet and URL. Idea boards and short videos
     (Pinterest, TikTok) score lower: engines rank them for nearly any query
     and they seldom answer one. A page whose snippet one engine left empty
     gets another engine's.
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

## Fetch

- **Which part of the page:**
  - all of it that a reader sees, menus, header, sidebars and footer
    included: no guess at the "main content", which on a scoreboard, a
    listing or a search page drops the items it holds; scripts, styles,
    a drop-down's options other than the one it shows and anything hidden (`hidden`,
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
  text. A longer page first loses its link URLs (each link keeps its text),
  which often makes it fit, and the text says so; one still too long is cut
  at a line, with a closing note saying the page goes on, and `truncated` is
  set.
- **Redirects:** followed up to 5, each one checked against the address
  rules.
- **Retries:**
  - a `403` or a bot check is tried once more as a different browser;
  - a `429` asking to wait 5 s or less is waited out and tried once more.
- **Cookies:** kept only for the duration of one fetch.
- **Proxies:** environment proxies (`HTTP(S)_PROXY`) are ignored, since a
  proxy would bypass the address checks.

## Politeness and terms

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

# Library

The package is `ratatoskr-web`; in code the library is `ratatoskr`. See also the [API docs on docs.rs](https://docs.rs/ratatoskr-web).

## Searching

```rust
use ratatoskr::{Ratatoskr, Sources};

#[tokio::main]
async fn main() {
    // Optional keyed sources come from the environment (configuration.md).
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

## Search options and the other kinds

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

## Fetching a page

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

## Your own engines and settings

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

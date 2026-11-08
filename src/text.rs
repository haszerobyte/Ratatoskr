//! HTML to readable text for a model: headings, list items and paragraphs on
//! their own lines, table cells separated by ` | `, and links as Markdown
//! `[text](url)` with absolute URLs, as Claude Code's own WebFetch gives its
//! model: without them a page's "Scorecard" or "Next page" could not be
//! followed.
//!
//! Which part of the page: its `<main>` (or its one `<article>`) without the
//! navigation chrome around it, when that element carries most of the page's
//! text, as an article's does. Otherwise the whole body, chrome included:
//! on a listing, a scoreboard, a search or an index page the items live in
//! headers, sidebars and tickers, and a page of several `<article>` cards
//! is all cards. A page that is a script shell with little text gives the
//! model its JSON-LD data instead of nothing.

use scraper::{ElementRef, Html, Node, Selector};

/// Never text a reader sees.
const NEVER_TEXT: &[&str] = &["script", "style", "noscript", "template", "svg"];
/// Chrome around an article: menus, site headers and footers, sidebars, forms.
const CHROME: &[&str] = &["nav", "footer", "header", "aside", "form", "iframe"];
/// Kept from a whole page: its menus and tickers may be the content, its
/// footers and forms are not.
const WHOLE_PAGE_SKIP: &[&str] = &["footer", "form", "iframe"];
/// The main element must hold at least this share of the body's text to
/// stand for the page.
const MAIN_SHARE: f64 = 0.5;
/// Below this much text a page is a script shell, worth its JSON-LD.
const THIN_PAGE: usize = 300;
/// JSON-LD text kept from a script shell, in characters.
const MAX_STRUCTURED: usize = 20_000;
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "br",
    "tr",
    "li",
    "ul",
    "ol",
    "table",
    "pre",
    "blockquote",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "dt",
    "dd",
];

/// A page's title and body text; `base` (the page's URL) makes relative links absolute.
pub fn html_to_text(html: &str, base: Option<&url::Url>) -> (Option<String>, String) {
    let document = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|s| document.select(&s).next())
        .map(|t| squash(&t.text().collect::<String>()))
        .filter(|t| !t.is_empty());
    let select = |css: &str| Selector::parse(css).map(|s| document.select(&s).collect::<Vec<_>>()).unwrap_or_default();
    let body = select("body").into_iter().next().unwrap_or_else(|| document.root_element());
    let articles = select("article");
    let main = select("main").into_iter().next().or(if articles.len() == 1 { Some(articles[0]) } else { None });
    let body_len = visible_len(body);
    let (root, skip) = match main {
        Some(main) if visible_len(main) as f64 >= MAIN_SHARE * body_len as f64 => (main, CHROME),
        _ => (body, WHOLE_PAGE_SKIP),
    };
    let mut out = String::new();
    walk(root, base, skip, &mut out);
    // A heading that held only an image is a bare "#": nothing to read.
    let mut lines: Vec<String> =
        out.lines().map(squash).filter(|l| !l.is_empty() && !l.chars().all(|c| c == '#')).collect();
    if lines.iter().map(|l| l.chars().count()).sum::<usize>() < THIN_PAGE {
        let structured = structured_data(&select("script[type=\"application/ld+json\"]"));
        if !structured.is_empty() {
            lines.push("Structured data on the page:".into());
            lines.extend(structured);
        }
    }
    (title, lines.join("\n"))
}

/// How many characters of text a reader sees in `element`.
fn visible_len(element: ElementRef) -> usize {
    element
        .children()
        .map(|child| match child.value() {
            Node::Text(text) => text.split_whitespace().map(|w| w.chars().count() + 1).sum::<usize>(),
            Node::Element(e) if !NEVER_TEXT.contains(&e.name()) => ElementRef::wrap(child).map_or(0, visible_len),
            _ => 0,
        })
        .sum()
}

/// The text of a page's JSON-LD blocks, one `key: value` line per text or
/// number field, nested objects flattened; links, ids and schema
/// bookkeeping left out.
fn structured_data(scripts: &[ElementRef]) -> Vec<String> {
    fn collect(value: &serde_json::Value, key: &str, lines: &mut Vec<String>, budget: &mut usize) {
        if *budget == 0 {
            return;
        }
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    match k.as_str() {
                        "@context" | "@type" | "@id" => {}
                        // A container (`@graph`, the usual wrapper for a
                        // page's several entities) or a value (`@value`):
                        // its contents, under the enclosing field's name.
                        k if k.starts_with('@') => collect(v, key, lines, budget),
                        _ => collect(v, k, lines, budget),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, key, lines, budget)),
            serde_json::Value::String(s) => {
                let s = squash(s);
                if !s.is_empty() && !s.starts_with("http") && !key.is_empty() {
                    let line = format!("{key}: {s}");
                    *budget = budget.saturating_sub(line.chars().count());
                    lines.push(line);
                }
            }
            serde_json::Value::Number(n) if !key.is_empty() => lines.push(format!("{key}: {n}")),
            _ => {}
        }
    }
    let mut lines = Vec::new();
    let mut budget = MAX_STRUCTURED;
    for script in scripts {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&script.text().collect::<String>()) {
            collect(&value, "", &mut lines, &mut budget);
        }
    }
    lines
}

/// Plain text of an HTML fragment (snippets with highlighting or entities).
pub fn fragment_text(fragment: &str) -> String {
    squash(&Html::parse_fragment(fragment).root_element().text().collect::<String>())
}

/// Where a link goes, absolute; None for one that goes nowhere a reader can
/// follow (an in-page anchor, javascript:, mailto:).
fn link_target(href: &str, base: Option<&url::Url>) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let url = match base {
        Some(base) => base.join(href).ok()?,
        None => url::Url::parse(href).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// `element`'s text into `out`, leaving out what never shows and the `skip` tags.
fn walk(element: ElementRef, base: Option<&url::Url>, skip: &[&str], out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(e) => {
                let tag = e.name();
                if NEVER_TEXT.contains(&tag) || skip.contains(&tag) {
                    continue;
                }
                let Some(child) = ElementRef::wrap(child) else { continue };
                if tag == "a"
                    && let Some(target) = e.attr("href").and_then(|href| link_target(href, base))
                {
                    // A link around a whole card (a result, a match) keeps the
                    // card's lines; the link follows it.
                    let wraps_blocks =
                        child.descendants().filter_map(|d| d.value().as_element()).any(|d| BLOCK.contains(&d.name()));
                    if wraps_blocks {
                        out.push('\n');
                        walk(child, base, skip, out);
                        out.truncate(out.trim_end().len());
                        out.push_str(&format!(" [open]({target})\n"));
                        continue;
                    }
                    let mut text = String::new();
                    walk(child, base, skip, &mut text);
                    let text = squash(&text);
                    // An image link has no text to show; a link inside the
                    // text it names would only repeat it.
                    if !text.is_empty() {
                        out.push_str(&format!(" [{}]({target}) ", text.replace(['[', ']'], "")));
                    }
                    continue;
                }
                if matches!(tag, "td" | "th") && !out.ends_with('\n') && !out.trim_end().is_empty() {
                    out.push_str(" | ");
                }
                let block = BLOCK.contains(&tag);
                if block {
                    out.push('\n');
                }
                if let Some(level) = tag.strip_prefix('h').and_then(|n| n.parse::<usize>().ok()) {
                    out.push_str(&"#".repeat(level.min(6)));
                    out.push(' ');
                } else if tag == "li" {
                    out.push_str("- ");
                }
                walk(child, base, skip, out);
                if block {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
}

/// Runs of whitespace as one space, trimmed.
pub(crate) fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_content_without_chrome() {
        let html = "<html><head><title> Rust  Docs </title><style>x{}</style></head><body>\
            <nav>Home | About</nav><main><h1>Intro</h1><p>Rust is <b>fast</b>.</p>\
            <ul><li>safe</li><li>concurrent</li></ul><script>track()</script></main><footer>(c)</footer></body></html>";
        let (title, text) =
            html_to_text(&html.replace("<h1>Intro</h1>", "<h1><img src=\"logo.png\"></h1><h1>Intro</h1>"), None);
        assert_eq!(title.as_deref(), Some("Rust Docs"));
        assert_eq!(text, "# Intro\nRust is fast.\n- safe\n- concurrent");
        assert_eq!(fragment_text("a <strong>b</strong> &amp; c"), "a b & c");
    }

    /// The links of a fetched page were dropped, so the model could read
    /// "Live Score | Scorecard" on Cricbuzz but not open either.
    #[test]
    fn links_are_markdown_with_absolute_urls_and_cells_are_separated() {
        let base = url::Url::parse("https://www.cricbuzz.com/cricket-match/live-scores").unwrap();
        let html = "<body><p>Day 4: Stumps <a href=\"/live-cricket-scores/1/x\">Live Score</a>|\
            <a href=\"https://www.cricbuzz.com/cricket-scorecard/1/x\">Scorecard</a> \
            <a href=\"#top\">Top</a> <a href=\"javascript:void(0)\">More</a> <a href=\"/x\"><img src=\"i.png\"></a></p>\
            <table><tr><th>Batter</th><th>R</th></tr><tr><td>Kohli</td><td>82</td></tr></table>\
            <a href=\"/m/2\"><div>2nd Test</div><div>WI 356</div></a></body>";
        let (_, text) = html_to_text(html, Some(&base));
        assert_eq!(
            text,
            "Day 4: Stumps [Live Score](https://www.cricbuzz.com/live-cricket-scores/1/x) | \
            [Scorecard](https://www.cricbuzz.com/cricket-scorecard/1/x) Top More\nBatter | R\nKohli | 82\n\
            2nd Test\nWI 356 [open](https://www.cricbuzz.com/m/2)"
        );
    }

    /// A scores page whose matches sat in its header ticker and menus came
    /// back as its near-empty `<main>`: the ticker was dropped as chrome.
    #[test]
    fn a_thin_main_gives_way_to_the_whole_page() {
        let base = url::Url::parse("https://x.example/").unwrap();
        let html = "<body><header><a href=\"/m/1\">RDC vs ARA - ARA opt to bowl</a></header>\
            <nav><a href=\"/live\">Live Scores</a></nav><main><h1>Live</h1></main>\
            <footer>(c) site</footer></body>";
        let (_, text) = html_to_text(html, Some(&base));
        assert_eq!(
            text,
            "[RDC vs ARA - ARA opt to bowl](https://x.example/m/1) [Live Scores](https://x.example/live)\n# Live"
        );
    }

    /// A page of several `<article>` cards was cut to its first card.
    #[test]
    fn several_articles_are_a_listing_not_an_article() {
        let html = "<body><article><h2>One</h2></article><article><h2>Two</h2></article></body>";
        assert_eq!(html_to_text(html, None).1, "## One\n## Two");
    }

    /// A script-rendered page has no text in its HTML, but often describes
    /// itself in JSON-LD: that, not nothing.
    #[test]
    fn a_script_shell_gives_its_structured_data() {
        let html = "<html><head><title>Match</title><script type=\"application/ld+json\">\
            {\"@context\":\"https://schema.org\",\"@type\":\"SportsEvent\",\"name\":\"WI vs BAN, 2nd Test\",\
            \"startDate\":\"2026-11-05\",\"url\":\"https://x.example/m\",\"competitor\":[\
            {\"@type\":\"SportsTeam\",\"name\":\"West Indies\"},{\"@type\":\"SportsTeam\",\"name\":\"Bangladesh\"}]}\
            </script></head><body><div id=\"root\"></div><script>app()</script></body></html>";
        assert_eq!(
            html_to_text(html, None).1,
            "Structured data on the page:\nname: WI vs BAN, 2nd Test\nstartDate: 2026-11-05\n\
            name: West Indies\nname: Bangladesh"
        );
    }

    /// Regression: every `@` key was skipped, `@graph` with it, and that is
    /// where most pages put their entities: they came back with nothing.
    #[test]
    fn structured_data_inside_a_graph_is_read() {
        let html = "<html><head><script type=\"application/ld+json\">\
            {\"@context\":\"https://schema.org\",\"@graph\":[\
            {\"@type\":\"WebSite\",\"@id\":\"https://x.example/#site\",\"name\":\"X News\"},\
            {\"@type\":\"NewsArticle\",\"headline\":\"Pakistan U19 beat India\"}]}\
            </script></head><body><div id=\"app\"></div></body></html>";
        assert_eq!(
            html_to_text(html, None).1,
            "Structured data on the page:\nname: X News\nheadline: Pakistan U19 beat India"
        );
    }
}

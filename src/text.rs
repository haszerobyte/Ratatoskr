//! HTML to readable text for a model: headings, list items and paragraphs on
//! their own lines, table cells separated by ` | `, and links as Markdown
//! `[text](url)` with absolute URLs, as Claude Code's own WebFetch gives its
//! model: without them a page's "Scorecard" or "Next page" could not be
//! followed.
//!
//! The whole page, every part a reader sees: its menus, header, sidebars and
//! footer too. Guessing the "main content" lost what was asked for whenever
//! a page put it elsewhere: a scoreboard's matches in its header ticker, a
//! listing's items in a sidebar, a page of several `<article>` cards cut to
//! its first. The model reads past a menu easily; it cannot read what was
//! dropped. A page that is a script shell with little text gives the model
//! its JSON-LD data instead of nothing.

use scraper::{ElementRef, Html, Node, Selector};

/// Never text a reader sees: code, styles and markup templates. A drop-down
/// is read as the one option it shows ([`chosen_option`]).
const NEVER_TEXT: &[&str] = &["script", "style", "noscript", "template", "svg"];
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
    let mut out = String::new();
    walk(body, base, &mut out);
    let mut lines: Vec<(bool, String)> = Vec::new();
    for line in out.lines() {
        if let Some(code) = line.strip_prefix(VERBATIM) {
            lines.push((true, code.trim_end().to_string()));
            continue;
        }
        let line = squash(line);
        // A heading that held only an image is a bare "#": nothing to read.
        if !line.is_empty() && !line.chars().all(|c| c == '#') {
            lines.push((false, line));
        }
    }
    let mut lines: Vec<String> = drop_repeated_menus(lines);
    if lines.iter().map(|l| l.chars().count()).sum::<usize>() < THIN_PAGE {
        let structured = structured_data(&select("script[type=\"application/ld+json\"]"));
        if !structured.is_empty() {
            lines.push("Structured data on the page:".into());
            lines.extend(structured);
        }
    }
    (title, lines.join("\n"))
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

/// Marks a line of a code block, kept as written: its spacing is its meaning.
const VERBATIM: char = '\u{E000}';

/// Hidden from a reader: the `hidden` attribute, `aria-hidden="true"`, or an
/// inline `display: none` / `visibility: hidden`. A whole page carries a
/// lot of it (a collapsed mobile menu repeating the desktop one, closed
/// dialogs, cookie notices), and none of it is what a reader sees.
fn is_hidden(element: &scraper::node::Element) -> bool {
    if element.attr("hidden").is_some() || element.attr("aria-hidden").is_some_and(|v| v.trim() == "true") {
        return true;
    }
    element.attr("style").is_some_and(|style| {
        let style = style.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        style.contains("display:none") || style.contains("visibility:hidden")
    })
}

/// A code block's language, from a `language-…` or `lang-…` class on it or
/// on its `<code>`.
fn code_language(pre: ElementRef) -> String {
    std::iter::once(pre)
        .chain(pre.children().filter_map(ElementRef::wrap))
        .filter_map(|e| e.value().attr("class"))
        .flat_map(str::split_whitespace)
        .find_map(|class| class.strip_prefix("language-").or_else(|| class.strip_prefix("lang-")))
        .unwrap_or("")
        .to_string()
}

/// Lines of links seen before, dropped when they come again as a menu: a run
/// of three or more such lines, or lines of three or more links each. Sites
/// repeat their navigation (header and footer, desktop and mobile); a page's
/// own content seldom repeats a line of links. Code lines are never dropped.
fn drop_repeated_menus(lines: Vec<(bool, String)>) -> Vec<String> {
    let links = |line: &str| line.matches("](http").count();
    let is_menu_line = |(code, line): &(bool, String)| !code && links(line) > 0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if is_menu_line(&lines[i]) && seen.contains(&lines[i].1) {
            let mut end = i;
            while end < lines.len() && is_menu_line(&lines[end]) && seen.contains(&lines[end].1) {
                end += 1;
            }
            let run = &lines[i..end];
            if run.len() < 3 && !run.iter().all(|(_, line)| links(line) >= 3) {
                kept.extend(run.iter().map(|(_, line)| line.clone()));
            }
            i = end;
            continue;
        }
        if is_menu_line(&lines[i]) {
            seen.insert(lines[i].1.clone());
        }
        kept.push(lines[i].1.clone());
        i += 1;
    }
    kept
}

/// `element`'s text into `out`, leaving out what never shows.
/// The option a drop-down shows: the one marked `selected`, else its first,
/// as a browser does. None when it has no option with text.
fn chosen_option(select: ElementRef) -> Option<String> {
    let options = Selector::parse("option").ok()?;
    let mut all = select.select(&options);
    let first = all.next()?;
    let chosen = std::iter::once(first).chain(all).find(|o| o.value().attr("selected").is_some()).unwrap_or(first);
    Some(squash(&chosen.text().collect::<String>())).filter(|t| !t.is_empty())
}

fn walk(element: ElementRef, base: Option<&url::Url>, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(e) => {
                let tag = e.name();
                if NEVER_TEXT.contains(&tag) || is_hidden(e) {
                    continue;
                }
                let Some(child) = ElementRef::wrap(child) else { continue };
                // A drop-down shows its chosen option, not every option: a
                // country picker would be two hundred lines.
                if tag == "select" {
                    if let Some(option) = chosen_option(child) {
                        out.push_str(&format!(" {option} "));
                    }
                    continue;
                }
                if tag == "pre" {
                    let code = child.text().collect::<String>();
                    let code = code.trim_matches('\n');
                    if !code.trim().is_empty() {
                        out.push_str(&format!("\n{VERBATIM}```{}\n", code_language(child)));
                        for line in code.lines() {
                            out.push_str(&format!("{VERBATIM}{line}\n"));
                        }
                        out.push_str(&format!("{VERBATIM}```\n"));
                    }
                    continue;
                }
                if tag == "code" {
                    let code = squash(&child.text().collect::<String>());
                    if !code.is_empty() {
                        out.push_str(&format!(" `{code}` "));
                    }
                    continue;
                }
                if tag == "img" {
                    // What the image shows, when the page says: a team's
                    // logo, a chart, a product. An image with no alt text
                    // is decoration or a tracker.
                    if let Some(alt) = e.attr("alt").map(squash).filter(|alt| !alt.is_empty()) {
                        out.push_str(&format!(" [image: {alt}] "));
                    }
                    continue;
                }
                if tag == "a"
                    && let Some(target) = e.attr("href").and_then(|href| link_target(href, base))
                {
                    // A link around a whole card (a result, a match) keeps the
                    // card's lines; the link follows it.
                    let wraps_blocks =
                        child.descendants().filter_map(|d| d.value().as_element()).any(|d| BLOCK.contains(&d.name()));
                    if wraps_blocks {
                        out.push('\n');
                        walk(child, base, out);
                        out.truncate(out.trim_end().len());
                        out.push_str(&format!(" [open]({target})\n"));
                        continue;
                    }
                    let mut text = String::new();
                    walk(child, base, &mut text);
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
                walk(child, base, out);
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

    /// The whole page, menus and footer too, as a reader sees it.
    #[test]
    fn the_whole_page_with_its_menus_and_footer() {
        let html = "<html><head><title> Rust  Docs </title><style>x{}</style></head><body>\
            <nav>Home | About</nav><main><h1>Intro</h1><p>Rust is <b>fast</b>.</p>\
            <ul><li>safe</li><li>concurrent</li></ul><script>track()</script></main><footer>(c)</footer></body></html>";
        let (title, text) =
            html_to_text(&html.replace("<h1>Intro</h1>", "<h1><img src=\"logo.png\"></h1><h1>Intro</h1>"), None);
        assert_eq!(title.as_deref(), Some("Rust Docs"));
        assert_eq!(text, "Home | About\n# Intro\nRust is fast.\n- safe\n- concurrent\n(c)");
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
    fn a_ticker_in_the_header_is_kept() {
        let base = url::Url::parse("https://x.example/").unwrap();
        let html = "<body><header><a href=\"/m/1\">RDC vs ARA - ARA opt to bowl</a></header>\
            <nav><a href=\"/live\">Live Scores</a></nav><main><h1>Live</h1></main>\
            <footer>(c) site</footer></body>";
        let (_, text) = html_to_text(html, Some(&base));
        assert_eq!(
            text,
            "[RDC vs ARA - ARA opt to bowl](https://x.example/m/1) [Live Scores](https://x.example/live)\n# Live\n(c) site"
        );
    }

    /// A drop-down shows one choice; its every option was two hundred lines
    /// of countries on a sign-up form. The chosen one is kept: it can be the
    /// answer (the currency or region a page is set to).
    #[test]
    fn a_drop_down_reads_as_its_chosen_option() {
        let html = "<body><p>Currency</p><select><option>USD</option><option selected>INR</option>\
            <option>EUR</option></select></body>";
        assert_eq!(html_to_text(html, None).1, "Currency\nINR");
        let first = "<body><label>Team <select><option>India</option><option>Pakistan</option></select></label></body>";
        assert_eq!(html_to_text(first, None).1, "Team India", "none marked: the first shows");
        assert_eq!(html_to_text("<body><p>x</p><select></select></body>", None).1, "x");
    }

    /// Code came back with its indentation squashed out, line by line.
    #[test]
    fn code_blocks_keep_their_spacing_and_inline_code_its_backticks() {
        let html = "<body><p>Use it:</p><pre><code class=\"language-rust\">fn main() {\n    println!(\"hi\");\n}\
            </code></pre><p>Inline <code>x  + 1</code> works.</p></body>";
        assert_eq!(
            html_to_text(html, None).1,
            "Use it:\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nInline `x + 1` works."
        );
    }

    /// Reading whole pages brought in what no reader sees: hidden menus,
    /// closed dialogs, cookie notices.
    #[test]
    fn hidden_parts_are_left_out() {
        let html = "<body><nav hidden>Mobile menu</nav><div aria-hidden=\"true\">Icon</div>\
            <div style=\"display: none\">Cookie notice</div><div style=\"Visibility : hidden\">Ghost</div>\
            <p>Shown</p></body>";
        assert_eq!(html_to_text(html, None).1, "Shown");
    }

    /// Images were dropped whole, and a team's logo or a chart said something.
    #[test]
    fn images_give_their_alt_text() {
        let base = url::Url::parse("https://x.example/").unwrap();
        let html = "<body><p><img src=\"in.png\" alt=\"India\"> 208 all out</p>\
            <a href=\"/t\"><img alt=\"Team page\"></a><img src=\"px.gif\" alt=\"\"></body>";
        assert_eq!(
            html_to_text(html, Some(&base)).1,
            "[image: India] 208 all out\n[image: Team page](https://x.example/t)"
        );
    }

    /// The header's menu came again in the footer: read once is enough.
    #[test]
    fn a_repeated_menu_is_read_once() {
        let base = url::Url::parse("https://x.example/").unwrap();
        let menu = "<ul><li><a href=\"/a\">A</a></li><li><a href=\"/b\">B</a></li><li><a href=\"/c\">C</a></li></ul>";
        let html = format!("<body>{menu}<p>Story</p>{menu}<p><a href=\"/a\">A</a> again</p></body>");
        assert_eq!(
            html_to_text(&html, Some(&base)).1,
            "- [A](https://x.example/a)\n- [B](https://x.example/b)\n- [C](https://x.example/c)\nStory\n\
            [A](https://x.example/a) again"
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

    /// Real pages, trimmed (scripts, styles and unused attributes cut, the
    /// structure kept): what a reader came for must come through. Captured
    /// 2026-11-08.
    #[test]
    fn real_pages_keep_what_a_reader_came_for() {
        // A scores page: its header ticker and its match list, which reading
        // only a main element lost.
        let (_, scores) = html_to_text(include_str!("../tests/fixtures/pages/live_scores.html"), None);
        for fact in [
            "RDC vs ARA - ARA won",
            "INDU19 vs PAKU19 - PAKU19 won",
            "Royal Desert Champions vs Arabian Aces",
            "112-6 (10)",
        ] {
            assert!(scores.contains(fact), "the scores page lost {fact:?}");
        }
        // A docs page: its code, fenced and indented as written.
        let (_, docs) = html_to_text(include_str!("../tests/fixtures/pages/docs_macro.html"), None);
        assert!(
            docs.contains("```\nmacro_rules! select {\n    {\n        $(\n            biased;"),
            "the docs page's code lost its shape"
        );
        // An encyclopedia article: its prose.
        let (title, article) = html_to_text(include_str!("../tests/fixtures/pages/encyclopedia_article.html"), None);
        assert!(title.is_some_and(|t| t.contains("Ratatoskr")));
        assert!(article.contains("Ratatoskr runs up and down Yggdrasil"));
    }
}

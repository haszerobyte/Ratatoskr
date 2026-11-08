//! HTML to readable text for a model: the main content when the page marks
//! it (`<main>`, `<article>`), without scripts, styles or navigation chrome;
//! headings, list items and paragraphs on their own lines, table cells
//! separated by ` | `, and links as Markdown `[text](url)` with absolute URLs,
//! as Claude Code's own WebFetch gives its model: without them a page's
//! "Scorecard" or "Next page" could not be followed.

use scraper::{ElementRef, Html, Node, Selector};

const SKIP: &[&str] =
    &["script", "style", "noscript", "template", "svg", "nav", "footer", "header", "aside", "form", "iframe"];
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
    let root = ["main", "article", "body"]
        .iter()
        .filter_map(|tag| Selector::parse(tag).ok())
        .find_map(|s| document.select(&s).next())
        .unwrap_or_else(|| document.root_element());
    let mut out = String::new();
    walk(root, base, &mut out);
    // A heading that held only an image is a bare "#": nothing to read.
    let lines: Vec<String> =
        out.lines().map(squash).filter(|l| !l.is_empty() && !l.chars().all(|c| c == '#')).collect();
    (title, lines.join("\n"))
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

fn walk(element: ElementRef, base: Option<&url::Url>, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(e) => {
                let tag = e.name();
                if SKIP.contains(&tag) {
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
}

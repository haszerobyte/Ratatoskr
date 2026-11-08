//! Results as numbered plain text: what the MCP tools hand a model and the
//! command line prints. One block per result, its URL on its own line.

use crate::engine::Hit;
use crate::vertical::{BookResult, ImageResult, NewsResult, VideoResult};

fn numbered(blocks: Vec<Vec<String>>) -> String {
    blocks
        .into_iter()
        .enumerate()
        .map(|(i, lines)| {
            let lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
            let (first, rest) = lines.split_first().map(|(f, r)| (f.clone(), r.to_vec())).unwrap_or_default();
            std::iter::once(format!("{}. {first}", i + 1))
                .chain(rest.into_iter().map(|l| format!("   {l}")))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Joins the parts that are there with " · ".
fn line(parts: &[&str]) -> String {
    parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
}

pub fn hits(hits: &[Hit]) -> String {
    numbered(hits.iter().map(|h| vec![h.title.clone(), h.url.clone(), h.snippet.clone()]).collect())
}

pub fn news(news: &[NewsResult]) -> String {
    numbered(
        news.iter()
            .map(|n| vec![n.title.clone(), line(&[&n.source, &n.date]), n.url.clone(), n.body.clone()])
            .collect(),
    )
}

pub fn images(images: &[ImageResult]) -> String {
    numbered(
        images
            .iter()
            .map(|i| {
                let size = match (i.width, i.height) {
                    (Some(w), Some(h)) => format!("{w}×{h}"),
                    _ => String::new(),
                };
                vec![
                    if i.title.is_empty() { i.image.clone() } else { i.title.clone() },
                    format!("image: {}", i.image),
                    line(&[&size, &i.source]),
                    if i.url.is_empty() { String::new() } else { format!("on: {}", i.url) },
                ]
            })
            .collect(),
    )
}

pub fn videos(videos: &[VideoResult]) -> String {
    numbered(
        videos
            .iter()
            .map(|v| {
                let views = v.views.map(|n| format!("{n} views")).unwrap_or_default();
                let published = v.published.split('T').next().unwrap_or("");
                vec![
                    v.title.clone(),
                    line(&[&v.duration, &v.publisher, &v.uploader, published, &views]),
                    if v.url.is_empty() { v.embed_url.clone() } else { v.url.clone() },
                    v.description.clone(),
                ]
            })
            .collect(),
    )
}

pub fn books(books: &[BookResult]) -> String {
    numbered(
        books
            .iter()
            .map(|b| {
                let year = b.year.map(|y| y.to_string()).unwrap_or_default();
                vec![
                    b.title.clone(),
                    line(&[&b.authors.join(", "), &year, &b.publisher]),
                    b.url.clone(),
                    b.info.clone(),
                ]
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_numbered_blocks_without_empty_lines() {
        let text = news(&[NewsResult {
            title: "Pi price".into(),
            url: "https://e.example/a".into(),
            body: "".into(),
            date: "2026-11-07T23:00:00Z".into(),
            source: "Site".into(),
            image: "".into(),
        }]);
        assert_eq!(text, "1. Pi price\n   Site · 2026-11-07T23:00:00Z\n   https://e.example/a");
        let two = hits(&[
            Hit { title: "A".into(), url: "https://a".into(), snippet: "s".into() },
            Hit { title: "B".into(), url: "https://b".into(), snippet: "".into() },
        ]);
        assert_eq!(two, "1. A\n   https://a\n   s\n\n2. B\n   https://b");
        let book = books(&[BookResult {
            title: "T".into(),
            authors: vec!["X".into(), "Y".into()],
            year: Some(2018),
            url: "https://o".into(),
            ..Default::default()
        }]);
        assert_eq!(book, "1. T\n   X, Y · 2018\n   https://o");
    }
}

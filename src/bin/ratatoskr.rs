//! The command line:
//!
//! ```text
//! ratatoskr search|news|images|videos|books <query> [limit] [country] [options]
//! ratatoskr fetch <url>
//! ratatoskr mcp
//! ```
//!
//! Options: `--time day|week|month|year`, `--safesearch on|moderate|off`,
//! `--page N`, `--country XX`. `mcp` serves the tools to an MCP client over
//! stdio. Optional sources come from the environment (SEARCH_API_URL,
//! BRAVE_SEARCH_API_KEY, GOOGLE_CSE_KEY + GOOGLE_CSE_ID).

use std::process::ExitCode;

use ratatoskr::{Ratatoskr, SafeSearch, SearchRequest, Sources, TimeLimit, format};

const USAGE: &str = "usage: ratatoskr search|news|images|videos|books <query> [limit] [country] \
[--time day|week|month|year] [--safesearch on|moderate|off] [--page N] [--country XX]
       ratatoskr fetch <url>
       ratatoskr mcp";

/// A search's words and options from the arguments after the command.
fn request(args: &[String]) -> Result<SearchRequest, String> {
    let mut positional = Vec::new();
    let (mut time, mut safesearch, mut page, mut country) = (None, SafeSearch::default(), 1, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = |name: &str| rest.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--time" => time = Some(value("--time")?.parse::<TimeLimit>().map_err(|e| e.to_string())?),
            "--safesearch" => safesearch = value("--safesearch")?.parse::<SafeSearch>().map_err(|e| e.to_string())?,
            "--page" => page = value("--page")?.parse::<u32>().map_err(|_| "--page needs a number".to_string())?,
            "--country" => country = Some(value("--country")?),
            option if option.starts_with("--") => return Err(format!("unknown option {option}")),
            word => positional.push(word.to_string()),
        }
    }
    let query = positional.first().ok_or("a query is needed")?;
    let limit = positional.get(1).and_then(|n| n.parse().ok()).unwrap_or(5);
    let country = country.or_else(|| positional.get(2).cloned());
    Ok(SearchRequest::new(query, limit).country(country.as_deref()).time(time).safesearch(safesearch).page(page))
}

/// Print the formatted results, or the failure, and the matching exit code.
fn report<T>(found: Result<ratatoskr::Found<T>, ratatoskr::SearchFailure>, format: fn(&[T]) -> String) -> ExitCode {
    match found {
        Ok(found) => {
            println!(
                "sources: {} (cached: {})\n{}",
                found.sources.join(", "),
                found.from_cache,
                format(&found.results)
            );
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("{failure}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = Ratatoskr::standard(&Sources::from_env());
    let command = args.first().map(String::as_str);
    match command {
        Some("mcp") if args.len() == 1 => match ratatoskr::mcp::serve_stdio(std::sync::Arc::new(rt)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("ratatoskr mcp: {error}");
                ExitCode::FAILURE
            }
        },
        Some("fetch") if args.len() == 2 => match rt.fetch(&args[1]).await {
            Ok(page) => {
                println!(
                    "{} [{}{}]\n{}",
                    page.title.unwrap_or_default(),
                    page.content_type,
                    if page.truncated { ", truncated" } else { "" },
                    page.text
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Some(kind @ ("search" | "news" | "images" | "videos" | "books")) => {
            let request = match request(&args[1..]) {
                Ok(request) => request,
                Err(problem) => {
                    eprintln!("ratatoskr {kind}: {problem}\n{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match kind {
                "search" => match rt.search_with(&request).await {
                    Ok(outcome) => {
                        println!(
                            "engine: {} (cached: {})\n{}",
                            outcome.engine,
                            outcome.from_cache,
                            format::hits(&outcome.hits)
                        );
                        ExitCode::SUCCESS
                    }
                    Err(failure) => {
                        eprintln!("{failure}");
                        ExitCode::FAILURE
                    }
                },
                "news" => report(rt.news(&request).await, format::news),
                "images" => report(rt.images(&request).await, format::images),
                "videos" => report(rt.videos(&request).await, format::videos),
                _ => report(rt.books(&request).await, format::books),
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_become_a_request() {
        let args = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let req = request(&args("rust 3 IN --time week --page 2 --safesearch off")).unwrap();
        assert_eq!((req.query.as_str(), req.limit, req.country.as_deref()), ("rust", 3, Some("IN")));
        assert_eq!((req.time, req.page, req.safesearch), (Some(TimeLimit::Week), 2, SafeSearch::Off));
        assert_eq!(request(&args("rust --country de")).unwrap().country.as_deref(), Some("DE"));
        assert!(request(&args("rust --time fortnight")).unwrap_err().contains("day, week, month, year"));
        assert!(request(&args("--page")).is_err());
        assert!(request(&args("rust --bogus 1")).unwrap_err().contains("unknown option"));
    }
}

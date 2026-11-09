//! The command line:
//!
//! ```text
//! ratatoskr search <query> [limit] [country] [options]
//! ratatoskr fetch <url>
//! ratatoskr mcp
//! ratatoskr serve [--listen ADDRESS]
//! ```
//!
//! Options: `--type web|news|images|videos|books`, `--time day|week|month|year`,
//! `--safesearch on|moderate|off`, `--page N`, `--country XX`. `mcp` serves the tools to an MCP client over
//! stdio; `serve` serves them as a local HTTP API ([`ratatoskr::http`]) on
//! 127.0.0.1:8765, or on `--listen`, where any address but loopback needs
//! RATATOSKR_API_TOKEN. Optional sources come from the environment
//! (SEARCH_API_URL, BRAVE_SEARCH_API_KEY, GOOGLE_CSE_KEY + GOOGLE_CSE_ID).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use ratatoskr::{Ratatoskr, SafeSearch, SearchRequest, Sources, TimeLimit, format};

const USAGE: &str = "usage: ratatoskr search <query> [limit] [country] [--type web|news|images|videos|books] \
[--time day|week|month|year] [--safesearch on|moderate|off] [--page N] [--country XX]
       ratatoskr fetch <url>
       ratatoskr mcp
       ratatoskr serve [--listen ADDRESS]   (RATATOSKR_API_TOKEN required off loopback)";

/// Where `serve` listens unless told otherwise: this machine only.
const DEFAULT_LISTEN: &str = "127.0.0.1:8765";

/// `serve`'s address from its arguments, and the token it requires. An
/// address other than loopback without a token is refused.
fn listen(args: &[String], token: Option<String>) -> Result<(SocketAddr, Option<String>), String> {
    let address = match args {
        [] => DEFAULT_LISTEN,
        [flag, address] if flag == "--listen" => address.as_str(),
        _ => return Err("serve takes only --listen ADDRESS".into()),
    };
    let address: SocketAddr =
        address.parse().map_err(|_| format!("--listen needs an address like {DEFAULT_LISTEN}, not {address:?}"))?;
    let token = token.filter(|t| !t.trim().is_empty());
    if ratatoskr::http::needs_token(&address) && token.is_none() {
        return Err(format!(
            "listening on {address} lets other machines use this one's search: set RATATOSKR_API_TOKEN, \
             or listen on 127.0.0.1"
        ));
    }
    Ok((address, token))
}

/// What `search` can search.
const TYPES: [&str; 5] = ["web", "news", "images", "videos", "books"];

/// A search's type, words and options from the arguments after the command.
fn request(args: &[String]) -> Result<(String, SearchRequest), String> {
    let mut positional = Vec::new();
    let mut kind = "web".to_string();
    let (mut time, mut safesearch, mut page, mut country) = (None, SafeSearch::default(), 1, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = |name: &str| rest.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--time" => time = Some(value("--time")?.parse::<TimeLimit>().map_err(|e| e.to_string())?),
            "--safesearch" => safesearch = value("--safesearch")?.parse::<SafeSearch>().map_err(|e| e.to_string())?,
            "--page" => page = value("--page")?.parse::<u32>().map_err(|_| "--page needs a number".to_string())?,
            "--country" => country = Some(value("--country")?),
            "--type" => {
                kind = value("--type")?;
                if !TYPES.contains(&kind.as_str()) {
                    return Err(format!("--type must be one of {}, not {kind:?}", TYPES.join(", ")));
                }
            }
            option if option.starts_with("--") => return Err(format!("unknown option {option}")),
            word => positional.push(word.to_string()),
        }
    }
    let query = positional.first().ok_or("a query is needed")?;
    let limit = positional.get(1).and_then(|n| n.parse().ok()).unwrap_or(5);
    let country = country.or_else(|| positional.get(2).cloned());
    Ok((
        kind,
        SearchRequest::new(query, limit).country(country.as_deref()).time(time).safesearch(safesearch).page(page),
    ))
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
        Some("mcp") if args.len() == 1 => match ratatoskr::mcp::serve_stdio(Arc::new(rt)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("ratatoskr mcp: {error}");
                ExitCode::FAILURE
            }
        },
        Some("serve") => {
            let (address, token) = match listen(&args[1..], std::env::var("RATATOSKR_API_TOKEN").ok()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("ratatoskr serve: {error}");
                    return ExitCode::from(2);
                }
            };
            let listener = match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("ratatoskr serve: cannot listen on {address}: {error}");
                    return ExitCode::FAILURE;
                }
            };
            eprintln!(
                "ratatoskr serve: http://{address} (GET /health; POST /search, /fetch, /search/text){}",
                if token.is_some() { ", token required" } else { "" }
            );
            match ratatoskr::http::serve(Arc::new(rt), listener, token).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("ratatoskr serve: {error}");
                    ExitCode::FAILURE
                }
            }
        }
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
        Some("search") => {
            let (kind, request) = match request(&args[1..]) {
                Ok(parsed) => parsed,
                Err(problem) => {
                    eprintln!("ratatoskr search: {problem}\n{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match kind.as_str() {
                "web" => match rt.search_with(&request).await {
                    Ok(outcome) => {
                        println!("engine: {}\n{}", outcome.provenance(), format::hits(&outcome.hits));
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
        let (kind, req) = request(&args("rust 3 IN --time week --page 2 --safesearch off")).unwrap();
        assert_eq!(kind, "web");
        assert_eq!((req.query.as_str(), req.limit, req.country.as_deref()), ("rust", 3, Some("IN")));
        assert_eq!((req.time, req.page, req.safesearch), (Some(TimeLimit::Week), 2, SafeSearch::Off));
        assert_eq!(request(&args("rust --country de")).unwrap().1.country.as_deref(), Some("DE"));
        assert_eq!(request(&args("rust --type news")).unwrap().0, "news");
        assert!(request(&args("rust --type podcasts")).unwrap_err().contains("web, news, images, videos, books"));
        assert!(request(&args("rust --time fortnight")).unwrap_err().contains("day, week, month, year"));
        assert!(request(&args("--page")).is_err());
        assert!(request(&args("rust --bogus 1")).unwrap_err().contains("unknown option"));
    }

    #[test]
    fn serve_listens_on_loopback_unless_given_an_address_and_a_token() {
        let args = |line: &str| line.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(listen(&[], None).unwrap().0.to_string(), DEFAULT_LISTEN);
        assert!(listen(&args("--listen 0.0.0.0:9000"), None).unwrap_err().contains("RATATOSKR_API_TOKEN"));
        assert!(listen(&args("--listen 0.0.0.0:9000"), Some("  ".into())).is_err(), "a blank token is none");
        let (address, token) = listen(&args("--listen 0.0.0.0:9000"), Some("t".into())).unwrap();
        assert_eq!((address.port(), token.as_deref()), (9000, Some("t")));
        assert!(listen(&args("--listen nowhere"), None).is_err());
        assert!(listen(&args("--port 1"), None).is_err());
    }
}

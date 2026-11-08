//! ratatoskr search <query> [limit] [country] | ratatoskr fetch <url>
//! For trying the crate by hand; engines come from the environment
//! (DDGS_API_URL, BRAVE_SEARCH_API_KEY, GOOGLE_CSE_KEY + GOOGLE_CSE_ID).

use ratatoskr::{Ratatoskr, Sources};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = Ratatoskr::standard(&Sources::from_env());
    match args.first().map(String::as_str) {
        Some("search") if args.len() >= 2 => {
            let limit = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(5);
            match rt.search_in(&args[1], limit, args.get(3).map(String::as_str)).await {
                Ok(outcome) => {
                    println!("engine: {} (cached: {})", outcome.engine, outcome.from_cache);
                    for hit in outcome.hits {
                        println!("- {}\n  {}\n  {}", hit.title, hit.url, hit.snippet);
                    }
                    std::process::ExitCode::SUCCESS
                }
                Err(failure) => {
                    eprintln!("{failure}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        Some("fetch") if args.len() == 2 => match rt.fetch(&args[1]).await {
            Ok(page) => {
                println!("{} [{}{}]\n{}", page.title.unwrap_or_default(), page.content_type,
                         if page.truncated { ", truncated" } else { "" }, page.text);
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: ratatoskr search <query> [limit] [country] | ratatoskr fetch <url>");
            std::process::ExitCode::from(2)
        }
    }
}

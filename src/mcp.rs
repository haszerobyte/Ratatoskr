//! An MCP server (Model Context Protocol, https://modelcontextprotocol.io)
//! over stdio, offering two tools: `web_search` and `web_fetch`.
//!
//! Messages are JSON-RPC 2.0, one per line. Requests are answered concurrently
//! (a slow fetch does not hold up a search or a ping), and every reply goes out
//! through one writer, so lines never interleave. Nothing but protocol goes to
//! stdout; logs belong on stderr.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::Ratatoskr;

/// The newest protocol revision this server implements, and the ones before it
/// it also speaks (tools are the same in all three). A client asking for one
/// of them gets it; any other gets the newest, as the protocol's version
/// negotiation prescribes, and decides itself whether to go on.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

/// The most results one `web_search` call may ask for.
const MAX_RESULTS: u64 = 20;
const DEFAULT_RESULTS: u64 = 8;

/// Serve MCP on this process's stdin and stdout until stdin closes.
pub async fn serve_stdio(rt: Arc<Ratatoskr>) -> std::io::Result<()> {
    serve(rt, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Serve MCP over any line-based byte stream (tests use in-memory pipes).
pub async fn serve<R, W>(rt: Arc<Ratatoskr>, input: R, mut output: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (replies, mut outbox) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(reply) = outbox.recv().await {
            let mut line = reply.to_string();
            line.push('\n');
            output.write_all(line.as_bytes()).await?;
            output.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    });
    let mut lines = BufReader::new(input).lines();
    let mut running = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                let _ = replies.send(error_reply(Value::Null, -32700, &format!("parse error: {error}")));
                continue;
            }
        };
        let (rt, replies) = (rt.clone(), replies.clone());
        running.spawn(async move {
            if let Some(reply) = handle(&rt, message).await {
                let _ = replies.send(reply);
            }
        });
    }
    // Input closed: let the calls in flight finish and their replies go out.
    while running.join_next().await.is_some() {}
    drop(replies);
    writer.await.map_err(std::io::Error::other)?
}

/// The reply to one message, or none for a notification.
async fn handle(rt: &Ratatoskr, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    // A notification (no id) is never answered, whatever it is.
    let id = id?;
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => call_tool(rt, &params).await,
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error_reply(id, code, &message),
    })
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked.filter(|v| SUPPORTED_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "ratatoskr", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "web_search finds pages on the web; web_fetch reads one page as text. \
                         Search first, then fetch the most relevant results to read them.",
    })
}

/// The tools, with their JSON Schemas.
pub fn tools() -> Value {
    json!([
        {
            "name": "web_search",
            "title": "Web search",
            "description": "Search the web. Returns a numbered list of results, each with its title, URL and a snippet. \
                            Use web_fetch to read a result in full.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to search for."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS,
                              "description": format!("How many results (default {DEFAULT_RESULTS}).")},
                    "country": {"type": "string",
                                "description": "Localize results to a country: an ISO 3166-1 alpha-2 code such as US, IN or DE."},
                },
                "required": ["query"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": true},
        },
        {
            "name": "web_fetch",
            "title": "Fetch a web page",
            "description": "Read a web page (or PDF) as text: the main content, with headings, lists, tables and \
                            links as [text](url). Only public http(s) addresses can be fetched.",
            "inputSchema": {
                "type": "object",
                "properties": {"url": {"type": "string", "description": "The page's http(s) URL."}},
                "required": ["url"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": true},
        },
    ])
}

/// A `tools/call`: the tool's outcome as text content. A tool that ran and
/// failed (no results, a blocked URL) is a result with `isError`, which the
/// model reads; only a malformed call is a protocol error.
async fn call_tool(rt: &Ratatoskr, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let text_arg = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let outcome = match name {
        "web_search" => {
            let query = text_arg("query").ok_or((-32602, "web_search needs a query".to_string()))?;
            let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_RESULTS).clamp(1, MAX_RESULTS);
            match rt.search_in(query, limit as usize, text_arg("country")).await {
                Ok(found) if found.hits.is_empty() => Err(format!("No results for {query:?}.")),
                Ok(found) => Ok(found
                    .hits
                    .iter()
                    .enumerate()
                    .map(|(i, hit)| format!("{}. {}\n   {}\n   {}", i + 1, hit.title, hit.url, hit.snippet))
                    .collect::<Vec<_>>()
                    .join("\n\n")),
                Err(failure) => Err(failure.to_string()),
            }
        }
        "web_fetch" => {
            let url = text_arg("url").ok_or((-32602, "web_fetch needs a url".to_string()))?;
            match rt.fetch(url).await {
                Ok(page) => {
                    let title = page.title.map(|t| format!("# {t}\n")).unwrap_or_default();
                    let cut = if page.truncated { "\n\n[The page was cut at its size limit.]" } else { "" };
                    Ok(format!("{title}{}\n\n{}{cut}", page.url, page.text))
                }
                Err(error) => Err(format!("Could not fetch {url}: {error}")),
            }
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    Ok(match outcome {
        Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
        Err(text) => json!({"content": [{"type": "text", "text": text}], "isError": true}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;
    use crate::engine::{Engine, Hit, SearchFuture};

    struct Fixed;

    impl Engine for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn search<'a>(&'a self, query: &'a str, limit: usize) -> SearchFuture<'a> {
            let hits = (1..=limit)
                .map(|i| Hit {
                    title: format!("{query} {i}"),
                    url: format!("https://e.example/{i}"),
                    snippet: "s".into(),
                })
                .collect();
            Box::pin(async move { Ok(hits) })
        }
    }

    async fn exchange(messages: &[Value]) -> Vec<Value> {
        let rt = Arc::new(Ratatoskr::new(vec![Arc::new(Fixed)], Settings::default()));
        let input: String = messages.iter().map(|m| format!("{m}\n")).collect();
        let (client, server_out) = tokio::io::duplex(1 << 16);
        serve(rt, input.as_bytes(), server_out).await.unwrap();
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut BufReader::new(client), &mut out).await.unwrap();
        let mut replies: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        replies.sort_by_key(|r| r["id"].as_i64().unwrap_or(-1));
        replies
    }

    #[tokio::test]
    async fn a_client_initializes_lists_and_calls_the_tools() {
        let replies = exchange(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "web_search", "arguments": {"query": "rust", "limit": 2}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "web_fetch", "arguments": {"url": "http://127.0.0.1/"}}}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 6, "method": "nope"}),
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "web_search", "arguments": {}}}),
        ])
        .await;
        assert_eq!(replies.len(), 7, "the notification is not answered: {replies:?}");
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        let older = exchange(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
        ])
        .await;
        assert_eq!(older[0]["result"]["protocolVersion"], "2024-11-05", "an older revision it speaks is kept");
        let unknown = exchange(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}),
        ])
        .await;
        assert_eq!(unknown[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "ratatoskr");
        let names: Vec<&str> =
            replies[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["web_search", "web_fetch"]);
        let found = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(found, "1. rust 1\n   https://e.example/1\n   s\n\n2. rust 2\n   https://e.example/2\n   s");
        assert_eq!(replies[2]["result"]["isError"], false);
        // A local address is refused by the fetcher: a tool error the model reads.
        assert_eq!(replies[3]["result"]["isError"], true);
        assert!(replies[3]["result"]["content"][0]["text"].as_str().unwrap().contains("blocked"), "{}", replies[3]);
        assert_eq!(replies[4]["result"], json!({}));
        assert_eq!(replies[5]["error"]["code"], -32601);
        assert_eq!(replies[6]["error"]["code"], -32602, "a call without its query is malformed");
    }

    #[tokio::test]
    async fn garbage_is_a_parse_error_not_the_end() {
        let rt = Arc::new(Ratatoskr::new(vec![Arc::new(Fixed)], Settings::default()));
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
        let (client, server_out) = tokio::io::duplex(1 << 16);
        serve(rt, input.as_bytes(), server_out).await.unwrap();
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut BufReader::new(client), &mut out).await.unwrap();
        assert!(out.contains("-32700") && out.contains("\"id\":9"), "{out}");
    }
}

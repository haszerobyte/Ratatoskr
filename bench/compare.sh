#!/bin/bash
# Compare ratatoskr with ddgs (and the reference MCP fetch server) on this
# machine: one-shot command-line search and fetch (wall time, peak memory),
# MCP servers (startup, memory at idle and after calls, call time), and
# install size. Network-bound numbers vary from run to run and place to place;
# the runs are interleaved so both tools see the same conditions.
#
#   bench/compare.sh [path/to/ratatoskr]
#
# Needs: uv (for ddgs and mcp-server-fetch in a throwaway environment), python3.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
RATATOSKR="${1:-$here/../target/release/ratatoskr}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# Two environments: ddgs's MCP server needs the MCP SDK 1.x, mcp-server-fetch 2.x.
uv venv -q "$WORK/ddgs" && uv pip install -q --python "$WORK/ddgs/bin/python" "ddgs==9.14.4" "mcp<2"
uv venv -q "$WORK/fetch" && uv pip install -q --python "$WORK/fetch/bin/python" "mcp-server-fetch==2026.11.4"
DDGS="$WORK/ddgs/bin/ddgs"
FETCH_MCP="$WORK/fetch/bin/mcp-server-fetch"

QUERIES=("rust tokio select macro" "raspberry pi 5 price" "linux kernel 6.12 changes" "best pizza in naples" "python asyncio gather timeout")
URLS=("https://en.wikipedia.org/wiki/Raspberry_Pi" "https://docs.rs/tokio/latest/tokio/macro.select.html" "https://www.rust-lang.org/")

# Peak resident memory (MB) and wall seconds of one command.
measure() {
    local out; out="$WORK/time.$$"
    if [ "$(uname)" = Darwin ]; then
        /usr/bin/time -l "$@" >/dev/null 2>"$out" || true
        awk '/real/ {t=$1} /maximum resident set size/ {m=$1/1048576} END {printf "%.2f %.1f\n", t, m}' "$out"
    else
        /usr/bin/time -f "%e %M" "$@" >/dev/null 2>"$out" || true
        tail -1 "$out" | awk '{printf "%.2f %.1f\n", $1, $2/1024}'
    fi
}

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

run() { # label, then the command with QUERY or URL in it, then the list
    local label=$1 kind=$2; shift 2
    local times=() mems=()
    for item in "$@"; do
        if [ "$label" = ratatoskr ]; then
            if [ "$kind" = search ]; then r=$(measure "$RATATOSKR" search "$item" 5); else r=$(measure "$RATATOSKR" fetch "$item"); fi
        else
            if [ "$kind" = search ]; then r=$(measure "$DDGS" text -q "$item" -m 5); else r=$(measure "$DDGS" extract -u "$item"); fi
        fi
        times+=("${r% *}"); mems+=("${r#* }")
    done
    printf "%s %s time_median=%s mem_median=%s\n" "$label" "$kind" \
        "$(printf '%s\n' "${times[@]}" | median)" "$(printf '%s\n' "${mems[@]}" | median)"
}

echo "== one-shot command line (median over ${#QUERIES[@]} searches / ${#URLS[@]} fetches)"
for kind in search fetch; do
    if [ $kind = search ]; then items=("${QUERIES[@]}"); else items=("${URLS[@]}"); fi
    run ratatoskr $kind "${items[@]}"
    run ddgs $kind "${items[@]}"
done

echo "== MCP servers (5 calls each)"
probe="$here/mcp_probe.py"
echo "ratatoskr web_search $(python3 "$probe" web_search '{"query": "rust release {i}", "limit": 5}' 5 -- "$RATATOSKR" mcp)"
echo "ddgs search_text     $(python3 "$probe" search_text '{"query": "rust release {i}", "max_results": 5}' 5 -- "$DDGS" mcp)"
echo "ratatoskr web_fetch  $(python3 "$probe" web_fetch '{"url": "https://en.wikipedia.org/wiki/Rust_(programming_language)"}' 5 -- "$RATATOSKR" mcp)"
echo "mcp-server-fetch     $(python3 "$probe" fetch '{"url": "https://en.wikipedia.org/wiki/Rust_(programming_language)"}' 5 -- "$FETCH_MCP")"

echo "== install size"
echo "ratatoskr binary: $(du -k "$RATATOSKR" | awk '{printf "%.1f MB", $1/1024}')"
echo "ddgs environment (with its MCP server): $(du -sk "$WORK/ddgs" | awk '{printf "%.1f MB", $1/1024}')"
echo "mcp-server-fetch environment: $(du -sk "$WORK/fetch" | awk '{printf "%.1f MB", $1/1024}')"

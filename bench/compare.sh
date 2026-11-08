#!/bin/bash
# Measure ratatoskr on this machine, and compare its fetch with the reference
# MCP fetch server (mcp-server-fetch): one-shot command-line search and fetch
# (wall time, peak memory), the MCP servers (startup, memory at idle and after
# calls, call time), and install size. Network-bound numbers vary from run to
# run and place to place.
#
#   bench/compare.sh [path/to/ratatoskr]
#
# Needs: uv (for mcp-server-fetch in a throwaway environment), python3.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
RATATOSKR="${1:-$here/../target/release/ratatoskr}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
uv venv -q "$WORK/fetch" && uv pip install -q --python "$WORK/fetch/bin/python" "mcp-server-fetch==2026.11.4"
FETCH_MCP="$WORK/fetch/bin/mcp-server-fetch"

QUERIES=("rust tokio select macro" "raspberry pi 5 price" "linux kernel 6.12 changes" "best pizza in naples" "python asyncio gather timeout")
URLS=("https://en.wikipedia.org/wiki/Raspberry_Pi" "https://docs.rs/tokio/latest/tokio/macro.select.html" "https://www.rust-lang.org/")

# Peak resident memory (MB) and wall seconds of one command.
measure() {
    local out="$WORK/time.out"
    if [ "$(uname)" = Darwin ]; then
        /usr/bin/time -l "$@" >/dev/null 2>"$out" || true
        awk '/real/ {t=$1} /maximum resident set size/ {m=$1/1048576} END {printf "%.2f %.1f\n", t, m}' "$out"
    else
        /usr/bin/time -f "%e %M" "$@" >/dev/null 2>"$out" || true
        tail -1 "$out" | awk '{printf "%.2f %.1f\n", $1, $2/1024}'
    fi
}

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

oneshot() { # search|fetch, then the queries or URLs
    local kind=$1 times=() mems=() r; shift
    for item in "$@"; do
        if [ "$kind" = search ]; then r=$(measure "$RATATOSKR" search "$item" 5); else r=$(measure "$RATATOSKR" fetch "$item"); fi
        times+=("${r% *}"); mems+=("${r#* }")
    done
    printf "ratatoskr %s time_median=%s mem_median=%s\n" "$kind" \
        "$(printf '%s\n' "${times[@]}" | median)" "$(printf '%s\n' "${mems[@]}" | median)"
}

echo "== one-shot command line (median over ${#QUERIES[@]} searches / ${#URLS[@]} fetches)"
oneshot search "${QUERIES[@]}"
oneshot fetch "${URLS[@]}"

echo "== MCP servers (5 calls each)"
probe="$here/mcp_probe.py"
article='{"url": "https://en.wikipedia.org/wiki/Rust_(programming_language)"}'
echo "ratatoskr web_search $(python3 "$probe" web_search '{"query": "rust release {i}", "limit": 5}' 5 -- "$RATATOSKR" mcp)"
echo "ratatoskr web_fetch  $(python3 "$probe" web_fetch "$article" 5 -- "$RATATOSKR" mcp)"
echo "mcp-server-fetch     $(python3 "$probe" fetch "$article" 5 -- "$FETCH_MCP")"

echo "== install size"
echo "ratatoskr binary: $(du -k "$RATATOSKR" | awk '{printf "%.1f MB", $1/1024}')"
echo "mcp-server-fetch environment: $(du -sk "$WORK/fetch" | awk '{printf "%.1f MB", $1/1024}')"

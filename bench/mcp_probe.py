#!/usr/bin/env python3
"""Drive an MCP server over stdio and measure it.

    mcp_probe.py <tool-name> <arguments-json> <repeat> -- <server command...>

Prints one JSON line: seconds to the initialize reply, resident memory (MB,
the server's whole process tree) at idle and after the calls, and the calls'
median and max seconds. Standard library only.
"""
import json, subprocess, sys, time, statistics

def tree_rss_mb(pid):
    out = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss="], capture_output=True, text=True).stdout
    rows = [tuple(map(int, line.split())) for line in out.splitlines() if line.strip()]
    children, rss = {}, {}
    for p, pp, r in rows:
        children.setdefault(pp, []).append(p)
        rss[p] = r
    total, stack = 0, [pid]
    while stack:
        p = stack.pop()
        total += rss.get(p, 0)
        stack.extend(children.get(p, []))
    return round(total / 1024, 1)

def main():
    sep = sys.argv.index("--")
    tool, arguments, repeat = sys.argv[1], json.loads(sys.argv[2]), int(sys.argv[3])
    command = sys.argv[sep + 1:]
    started = time.monotonic()
    server = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    next_id = iter(range(1, 10_000))

    def request(method, params=None):
        rid = next(next_id)
        server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}) + "\n")
        server.stdin.flush()
        while True:
            line = server.stdout.readline()
            if not line:
                raise SystemExit(f"server closed: {command}")
            message = json.loads(line)
            if message.get("id") == rid:
                return message

    request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "mcp_probe", "version": "1"}})
    ready = time.monotonic() - started
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    server.stdin.flush()
    names = [t["name"] for t in request("tools/list")["result"]["tools"]]
    time.sleep(1.0)
    idle = tree_rss_mb(server.pid)
    times, errors = [], 0
    for i in range(repeat):
        args = {k: (v.replace("{i}", str(i)) if isinstance(v, str) else v) for k, v in arguments.items()}
        t = time.monotonic()
        reply = request("tools/call", {"name": tool, "arguments": args})
        times.append(time.monotonic() - t)
        errors += bool(reply.get("error") or reply.get("result", {}).get("isError"))
    busy = tree_rss_mb(server.pid)
    server.stdin.close()
    server.wait(timeout=10)
    print(json.dumps({"tools": names, "ready_s": round(ready, 2), "idle_mb": idle, "after_mb": busy,
                      "call_median_s": round(statistics.median(times), 2), "call_max_s": round(max(times), 2),
                      "errors": errors}))

main()

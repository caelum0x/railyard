#!/usr/bin/env python3
"""Minimal Replicate API helper using stdlib only. Reads token from .env.local; never prints it."""
import json, os, sys, time, urllib.request, urllib.error

ENV_PATH = "/Users/arhansubasi/products/videogen/apps/web/.env.local"

def load_token():
    tok = None
    with open(ENV_PATH) as f:
        for line in f:
            line = line.strip()
            if line.startswith("REPLICATE_API_TOKEN"):
                _, _, v = line.partition("=")
                tok = v.strip().strip('"').strip("'")
                break
    if not tok:
        raise SystemExit("token not found")
    return tok

TOKEN = load_token()

def req(method, url, body=None, extra_headers=None):
    data = None
    headers = {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json",
               "User-Agent": "railyard-lora/1.0 (python-urllib)"}
    if extra_headers:
        headers.update(extra_headers)
    if body is not None:
        data = json.dumps(body).encode()
    r = urllib.request.Request(url, data=data, method=method, headers=headers)
    for attempt in range(6):
        try:
            with urllib.request.urlopen(r) as resp:
                raw = resp.read().decode()
                return resp.status, json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            raw = e.read().decode()
            if e.code == 429:
                ra = e.headers.get("retry-after")
                wait = float(ra) if ra else (2 ** attempt) * 5
                sys.stderr.write(f"429 backoff {wait}s\n"); time.sleep(wait); continue
            return e.code, {"error": raw}
        except urllib.error.URLError as e:
            time.sleep(5); continue
    return 0, {"error": "max retries"}

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "account":
        s, j = req("GET", "https://api.replicate.com/v1/account")
        print(s, json.dumps(j))
    elif cmd == "get":
        s, j = req("GET", sys.argv[2])
        print(s, json.dumps(j)[:4000])

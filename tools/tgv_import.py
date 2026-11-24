#!/usr/bin/env python3
"""Bulk-import files into tgvault from remote URLs. Standard library only.

Usage:
    export TGV_BASE=http://127.0.0.1:8787 TGV_KEY=tgv_xxx_yyy
    python tgv_import.py BUCKET manifest.tsv [--workers 2] [--skip-existing]

manifest.tsv: one import per line, tab separated:
    <source url> <TAB> <object key> [<TAB> {"Header":"value"} as JSON]
Lines starting with # are ignored.
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

BASE = os.environ.get("TGV_BASE", "http://127.0.0.1:8787").rstrip("/")
KEY = os.environ.get("TGV_KEY", "")


class ApiError(Exception):
    def __init__(self, status, code, message, retry_after=None):
        super().__init__(f"{status} {code}: {message}")
        self.status, self.code, self.message, self.retry_after = status, code, message, retry_after


def call(method, path, body=None, timeout=120):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {KEY}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            err = json.loads(raw)["error"]
        except Exception:
            err = {"code": "http_error", "message": raw[:200].decode("utf-8", "replace")}
        ra = e.headers.get("Retry-After")
        raise ApiError(e.code, err.get("code"), err.get("message"), int(ra) if ra and ra.isdigit() else None)


def with_retry(fn, attempts=5):
    """Retry 429 (honouring Retry-After), 502 and network errors. Never retry 4xx validation errors."""
    delay = 2.0
    for i in range(attempts):
        try:
            return fn()
        except ApiError as e:
            if e.status not in (429, 502, 503) or i == attempts - 1:
                raise
            time.sleep(e.retry_after or delay)
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            if i == attempts - 1:
                raise
            time.sleep(delay)
        delay = min(delay * 2, 30)


def object_exists(bucket, key):
    try:
        _, page = call("GET", f"/api/v1/buckets/{bucket}/objects?prefix={urllib.parse.quote(key, safe='')}&limit=5")
        return any(o["key"] == key for o in page["objects"])
    except ApiError:
        return False


def import_one(bucket, url, key, headers=None, poll=2.0):
    """Start an import job and wait for it. Returns the stored object dict."""
    body = {"url": url, "key": key}
    if headers:
        body["headers"] = headers
    _, job = with_retry(lambda: call("POST", f"/api/v1/buckets/{bucket}/import", body))
    while True:
        _, job = with_retry(lambda: call("GET", f"/api/v1/imports/{job['id']}"))
        if job["state"] == "done":
            return job["object"]
        if job["state"] == "failed":
            raise RuntimeError(job["error"])
        time.sleep(poll)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bucket")
    ap.add_argument("manifest")
    ap.add_argument("--workers", type=int, default=2, help="the server runs at most 2 imports at once")
    ap.add_argument("--skip-existing", action="store_true")
    args = ap.parse_args()
    if not KEY:
        sys.exit("set TGV_KEY")

    jobs = []
    for line in open(args.manifest, encoding="utf-8"):
        line = line.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split("\t")
        jobs.append((parts[0].strip(), parts[1].strip(), json.loads(parts[2]) if len(parts) > 2 and parts[2].strip() else None))

    def work(job):
        url, key, headers = job
        t0 = time.time()
        try:
            if args.skip_existing and object_exists(args.bucket, key):
                return key, "skipped", "already stored"
            obj = import_one(args.bucket, url, key, headers)
            return key, "ok", f"{obj['size']} bytes in {time.time() - t0:.1f}s etag={obj['etag']}"
        except Exception as e:  # report and continue with the rest
            return key, "FAILED", str(e)

    failed = 0
    with ThreadPoolExecutor(max_workers=max(1, args.workers)) as pool:
        for key, status, info in pool.map(work, jobs):
            print(f"{status:8} {key}  {info}", flush=True)
            failed += status == "FAILED"
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

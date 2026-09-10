#!/usr/bin/env python3
"""End-to-end protocol test for a running BrewFS WebDAV gateway."""

import base64
import http.client
import os
import ssl
import sys
import time
from urllib.parse import quote, urlsplit

ENDPOINT = os.environ.get("GW_ENDPOINT", "http://127.0.0.1:19102").rstrip("/")
USER = os.environ.get("GW_USER", "testuser")
PASSWORD = os.environ.get("GW_PASSWORD", "testpass")
AUTH_REQUIRED = os.environ.get("GW_AUTH_REQUIRED", "1") != "0"
ATOMIC_PUT = os.environ.get("GW_ATOMIC_PUT", "1") != "0"
INSECURE_TLS = os.environ.get("GW_INSECURE_TLS", "0") == "1"

endpoint = urlsplit(ENDPOINT)
if endpoint.scheme not in {"http", "https"} or not endpoint.hostname:
    raise SystemExit(f"invalid GW_ENDPOINT: {ENDPOINT}")

passed = 0
failed = 0


def check(name, condition, detail=""):
    global passed, failed
    if condition:
        passed += 1
        print(f"  PASS {name}")
    else:
        failed += 1
        print(f"  FAIL {name} {detail}")


def connection():
    port = endpoint.port or (443 if endpoint.scheme == "https" else 80)
    if endpoint.scheme == "https":
        context = ssl._create_unverified_context() if INSECURE_TLS else None
        return http.client.HTTPSConnection(endpoint.hostname, port, timeout=30, context=context)
    return http.client.HTTPConnection(endpoint.hostname, port, timeout=30)


def request(method, path, body=None, headers=None, auth="correct", chunked=False):
    headers = dict(headers or {})
    if auth == "correct" and AUTH_REQUIRED:
        token = base64.b64encode(f"{USER}:{PASSWORD}".encode()).decode()
        headers["Authorization"] = f"Basic {token}"
    elif auth == "wrong":
        token = base64.b64encode(f"{USER}:wrong".encode()).decode()
        headers["Authorization"] = f"Basic {token}"

    encoded_path = quote(path, safe="/%")
    base_path = endpoint.path.rstrip("/")
    target = f"{base_path}{encoded_path}" or "/"
    payload = [body] if chunked and body is not None else body
    conn = connection()
    try:
        conn.request(method, target, body=payload, headers=headers, encode_chunked=chunked)
        response = conn.getresponse()
        data = response.read()
        return response.status, {key.lower(): value for key, value in response.getheaders()}, data
    finally:
        conn.close()


def destination(path):
    return f"{ENDPOINT}{quote(path, safe='/%')}"


def expect_status(name, response, expected):
    status, _, body = response
    allowed = {expected} if isinstance(expected, int) else set(expected)
    check(name, status in allowed, f"(got {status}, want {sorted(allowed)}, body={body[:200]!r})")
    return response


def wait_ready():
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            status, _, _ = request("OPTIONS", "/", auth="missing")
            if status in {200, 204, 401}:
                return
        except OSError:
            pass
        time.sleep(0.2)
    raise SystemExit(f"WebDAV gateway did not become ready at {ENDPOINT}")


ALLPROP = b'<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'
COLOR_PROP = b'''<?xml version="1.0"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:x="urn:brewfs:test">
  <D:set><D:prop><x:color>blue</x:color></D:prop></D:set>
</D:propertyupdate>'''
GET_COLOR = b'''<?xml version="1.0"?>
<D:propfind xmlns:D="DAV:" xmlns:x="urn:brewfs:test">
  <D:prop><x:color/></D:prop>
</D:propfind>'''
LOCK_INFO = b'''<?xml version="1.0"?>
<D:lockinfo xmlns:D="DAV:">
  <D:lockscope><D:exclusive/></D:lockscope>
  <D:locktype><D:write/></D:locktype>
  <D:owner><D:href>brewfs-e2e</D:href></D:owner>
</D:lockinfo>'''
XML_HEADERS = {"Content-Type": "application/xml; charset=utf-8"}

wait_ready()

print("== authentication and capabilities ==")
if AUTH_REQUIRED:
    status, headers, _ = request("OPTIONS", "/", auth="missing")
    check("missing credentials rejected", status == 401, str(status))
    check("basic challenge returned", headers.get("www-authenticate", "").startswith("Basic "), str(headers))
    expect_status("wrong credentials rejected", request("OPTIONS", "/", auth="wrong"), 401)
else:
    expect_status("anonymous access", request("OPTIONS", "/", auth="missing"), {200, 204})
status, headers, _ = request("OPTIONS", "/")
check("OPTIONS succeeds", status in {200, 204}, str(status))
check("DAV class 1 and 2 advertised", "1" in headers.get("dav", "") and "2" in headers.get("dav", ""), str(headers))
check("collection methods advertised", all(method in headers.get("allow", "") for method in ("PROPFIND", "LOCK")) and "PUT" not in headers.get("allow", ""), str(headers))

print("== root and namespace boundaries ==")
status, _, body = request("PROPFIND", "/", ALLPROP, {**XML_HEADERS, "Depth": "0"})
check("root PROPFIND", status == 207 and b"multistatus" in body.lower(), f"status={status} body={body[:200]!r}")
for method, path, request_body, request_headers in [
    ("GET", "/.brewfs.sys/webdav/tmp/intruder", None, None),
    ("PUT", "/.brewfs.sys/webdav/tmp/intruder", b"x", None),
    ("MKCOL", "/.brewfs.sys/intruder", None, None),
    ("PROPFIND", "/.brewfs.sys", ALLPROP, {**XML_HEADERS, "Depth": "1"}),
]:
    status, _, _ = request(method, path, request_body, request_headers)
    check(f"reserved namespace rejects {method}", status == 403, str(status))

print("== files, ranges, and partial updates ==")
expect_status("MKCOL docs", request("MKCOL", "/docs"), 201)
original = b"hello world"
expect_status("PUT creates file", request("PUT", "/docs/readme.txt", original), 201)
status, headers, _ = request("OPTIONS", "/docs/readme.txt")
check(
    "file write methods advertised",
    all(method in headers.get("allow", "") for method in ("PUT", "PATCH", "PROPFIND", "LOCK")),
    str(headers),
)
status, headers, body = request("GET", "/docs/readme.txt")
check("GET roundtrip", status == 200 and body == original, f"status={status} body={body!r}")
check("GET returns ETag", bool(headers.get("etag")), str(headers))
status, headers, body = request("HEAD", "/docs/readme.txt")
check("HEAD size", status == 200 and body == b"" and headers.get("content-length") == str(len(original)), str(headers))
status, headers, body = request("GET", "/docs/readme.txt", headers={"Range": "bytes=6-10"})
check("GET range", status == 206 and body == b"world", f"status={status} body={body!r}")
check("GET content-range", headers.get("content-range") == "bytes 6-10/11", str(headers))

patch_headers = {
    "Content-Type": "application/x-sabredav-partialupdate",
    "X-Update-Range": "bytes=6-11",
}
expect_status("PATCH range", request("PATCH", "/docs/readme.txt", b"brewfs", patch_headers), 204)
append_headers = {
    "Content-Type": "application/x-sabredav-partialupdate",
    "X-Update-Range": "append",
}
expect_status("PATCH append", request("PATCH", "/docs/readme.txt", b"!", append_headers), 204)
status, _, body = request("GET", "/docs/readme.txt")
check("partial updates persisted", status == 200 and body == b"hello brewfs!", repr(body))

unicode_body = "你好，BrewFS".encode()
expect_status("Unicode PUT", request("PUT", "/docs/中文.txt", unicode_body), 201)
status, _, body = request("GET", "/docs/中文.txt")
check("Unicode path roundtrip", status == 200 and body == unicode_body, f"status={status} body={body!r}")

print("== PROPFIND and dead properties ==")
status, _, body = request("PROPFIND", "/docs/", ALLPROP, {**XML_HEADERS, "Depth": "1"})
check("Depth 1 PROPFIND lists children", status == 207 and b"readme.txt" in body, f"status={status} body={body[:400]!r}")
status, _, body = request("PROPFIND", "/", ALLPROP, {**XML_HEADERS, "Depth": "infinity"})
check("Depth infinity PROPFIND", status == 207 and b"readme.txt" in body, f"status={status} body={body[:400]!r}")
check("system namespace hidden from root", b".brewfs.sys" not in body, body[:400])
expect_status("PROPPATCH set", request("PROPPATCH", "/docs/readme.txt", COLOR_PROP, XML_HEADERS), 207)
status, _, body = request("PROPFIND", "/docs/readme.txt", GET_COLOR, {**XML_HEADERS, "Depth": "0"})
check("dead property roundtrip", status == 207 and b"blue" in body and b"color" in body, f"status={status} body={body[:400]!r}")
expect_status("PUT replaces content", request("PUT", "/docs/readme.txt", b"replacement"), 204)
status, _, body = request("PROPFIND", "/docs/readme.txt", GET_COLOR, {**XML_HEADERS, "Depth": "0"})
check("PUT preserves dead property", status == 207 and b"blue" in body, f"status={status} body={body[:400]!r}")

print("== COPY and MOVE ==")
copy_headers = {"Destination": destination("/docs/copied.txt"), "Overwrite": "F"}
expect_status("COPY creates destination", request("COPY", "/docs/readme.txt", headers=copy_headers), 201)
status, _, body = request("GET", "/docs/copied.txt")
check("COPY preserves data", status == 200 and body == b"replacement", f"status={status} body={body!r}")
status, _, body = request("PROPFIND", "/docs/copied.txt", GET_COLOR, {**XML_HEADERS, "Depth": "0"})
check("COPY preserves dead property", status == 207 and b"blue" in body, f"status={status} body={body[:400]!r}")
expect_status("COPY Overwrite F", request("COPY", "/docs/readme.txt", headers=copy_headers), 412)
expect_status(
    "COPY Overwrite T",
    request("COPY", "/docs/readme.txt", headers={"Destination": destination("/docs/copied.txt"), "Overwrite": "T"}),
    204,
)

expect_status("PUT move source", request("PUT", "/docs/move-source.txt", b"moved"), 201)
expect_status("PUT move target", request("PUT", "/docs/move-target.txt", b"target"), 201)
move_no_overwrite = {"Destination": destination("/docs/move-target.txt"), "Overwrite": "F"}
expect_status("MOVE Overwrite F", request("MOVE", "/docs/move-source.txt", headers=move_no_overwrite), 412)
status, _, body = request("GET", "/docs/move-source.txt")
check("failed MOVE preserves source", status == 200 and body == b"moved", f"status={status} body={body!r}")
expect_status(
    "MOVE Overwrite T",
    request("MOVE", "/docs/move-source.txt", headers={"Destination": destination("/docs/move-target.txt"), "Overwrite": "T"}),
    204,
)
status, _, body = request("GET", "/docs/move-target.txt")
check("MOVE publishes source data", status == 200 and body == b"moved", f"status={status} body={body!r}")
expect_status("MOVE removes source", request("GET", "/docs/move-source.txt"), 404)

expect_status("MKCOL tree", request("MKCOL", "/tree"), 201)
expect_status("MKCOL tree child", request("MKCOL", "/tree/sub"), 201)
expect_status("PUT tree file", request("PUT", "/tree/sub/file.txt", b"tree"), 201)
expect_status(
    "recursive directory COPY",
    request(
        "COPY",
        "/tree/",
        headers={"Destination": destination("/tree-copy/"), "Depth": "infinity", "Overwrite": "F"},
    ),
    201,
)
status, _, body = request("GET", "/tree-copy/sub/file.txt")
check("recursive COPY data", status == 200 and body == b"tree", f"status={status} body={body!r}")

expect_status("MKCOL nonempty", request("MKCOL", "/nonempty"), 201)
expect_status("PUT nonempty child", request("PUT", "/nonempty/file.txt", b"x"), 201)
expect_status(
    "non-empty Depth 0 DELETE returns conflict",
    request("DELETE", "/nonempty/", headers={"Depth": "0"}),
    409,
)
expect_status("DELETE nonempty collection", request("DELETE", "/nonempty/"), 204)

print("== locking ==")
expect_status("PUT lock target", request("PUT", "/docs/locked.txt", b"before"), 201)
status, headers, _ = request(
    "LOCK",
    "/docs/locked.txt",
    LOCK_INFO,
    {**XML_HEADERS, "Depth": "0", "Timeout": "Second-3600"},
)
lock_token = headers.get("lock-token")
check("LOCK returns token", status == 200 and bool(lock_token), f"status={status} headers={headers}")
expect_status("locked PUT without token", request("PUT", "/docs/locked.txt", b"blocked"), 423)
if lock_token:
    expect_status(
        "locked PUT with token",
        request("PUT", "/docs/locked.txt", b"allowed", {"If": f"({lock_token})"}),
        204,
    )
    expect_status("UNLOCK", request("UNLOCK", "/docs/locked.txt", headers={"Lock-Token": lock_token}), 204)
status, _, body = request("GET", "/docs/locked.txt")
check("locked write persisted", status == 200 and body == b"allowed", f"status={status} body={body!r}")

status, headers, _ = request(
    "LOCK",
    "/docs/lock-null.txt",
    LOCK_INFO,
    {**XML_HEADERS, "Depth": "0", "Timeout": "Second-3600"},
)
null_lock_token = headers.get("lock-token")
check("LOCK creates missing resource", status == 201 and bool(null_lock_token), f"status={status} headers={headers}")
status, _, body = request("GET", "/docs/lock-null.txt")
check("lock-null resource is published", status == 200 and body == b"", f"status={status} body={body!r}")
if null_lock_token:
    expect_status(
        "UNLOCK lock-null resource",
        request("UNLOCK", "/docs/lock-null.txt", headers={"Lock-Token": null_lock_token}),
        204,
    )

print("== atomic body validation ==")
expect_status("PUT stable atomic target", request("PUT", "/docs/atomic.txt", b"stable"), 201)
short = request(
    "PUT",
    "/docs/atomic.txt",
    b"short",
    {"X-Expected-Entity-Length": "6"},
    chunked=True,
)
check("short declared body rejected", short[0] == 400, str(short[0]))
overlong = request(
    "PUT",
    "/docs/atomic.txt",
    b"too long",
    {"X-Expected-Entity-Length": "7"},
    chunked=True,
)
check("overlong declared body rejected", overlong[0] == 400, str(overlong[0]))
range_short = request(
    "PUT",
    "/docs/atomic.txt",
    b"short",
    {"Content-Range": "bytes 0-5/6"},
    chunked=True,
)
check("short Content-Range body rejected", range_short[0] == 400, str(range_short[0]))
range_overlong = request(
    "PUT",
    "/docs/atomic.txt",
    b"too long",
    {"Content-Range": "bytes 0-6/7"},
    chunked=True,
)
check("overlong Content-Range body rejected", range_overlong[0] == 400, str(range_overlong[0]))
status, _, body = request("GET", "/docs/atomic.txt")
if ATOMIC_PUT:
    check("failed atomic PUTs preserve target", status == 200 and body == b"stable", f"status={status} body={body!r}")
else:
    check("direct mode remains readable after failed PUT", status == 200, f"status={status} body={body!r}")

print("== recursive delete pagination ==")
expect_status("MKCOL wide", request("MKCOL", "/wide"), 201)
for index in range(260):
    status, _, _ = request("PUT", f"/wide/entry-{index:03d}.txt", b"x")
    if status != 201:
        check("create 260 delete fixtures", False, f"entry={index} status={status}")
        break
else:
    check("create 260 delete fixtures", True)
expect_status("DELETE >256-entry collection", request("DELETE", "/wide/"), {204, 207})
expect_status("deleted collection is gone", request("PROPFIND", "/wide/", ALLPROP, {**XML_HEADERS, "Depth": "0"}), 404)

print("== cleanup ==")
for path in ("/tree-copy/", "/tree/", "/docs/"):
    status, _, _ = request("DELETE", path)
    check(f"DELETE {path}", status in {204, 207}, str(status))
status, _, body = request("PROPFIND", "/", ALLPROP, {**XML_HEADERS, "Depth": "1"})
check("root remains available", status == 207 and b".brewfs.sys" not in body, f"status={status} body={body[:400]!r}")

print(f"\nRESULT: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)

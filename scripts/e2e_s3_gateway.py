#!/usr/bin/env python3
"""End-to-end smoke test for the BrewFS S3 gateway.

Covers: list_buckets, head_bucket, put/get/head/delete_object,
list_objects_v2 (prefix/delimiter/pagination), copy_object,
multipart upload, directory objects, and error codes.
"""
import io
import os
import sys

import boto3
from botocore.client import Config
from botocore.exceptions import ClientError

ENDPOINT = os.environ.get("GW_ENDPOINT", "http://127.0.0.1:19101")
AK, SK = "testkey", "testsecret"
BUCKET = "brewfs"

s3 = boto3.client(
    "s3",
    endpoint_url=ENDPOINT,
    aws_access_key_id=AK,
    aws_secret_access_key=SK,
    region_name="us-east-1",
    config=Config(s3={"addressing_style": "path"}, retries={"max_attempts": 1}),
)

passed = failed = 0


def check(name, cond, detail=""):
    global passed, failed
    if cond:
        passed += 1
        print(f"  PASS {name}")
    else:
        failed += 1
        print(f"  FAIL {name} {detail}")


def expect_error(name, code, fn):
    try:
        fn()
        check(name, False, "(no error raised)")
    except ClientError as e:
        got = e.response["Error"]["Code"]
        check(name, got == code, f"(got {got}, want {code})")


print("== bucket ops ==")
buckets = s3.list_buckets()["Buckets"]
check("list_buckets contains default bucket", any(b["Name"] == BUCKET for b in buckets), str(buckets))
s3.head_bucket(Bucket=BUCKET)
print("  PASS head_bucket")
expect_error("head_bucket wrong bucket -> 404/NoSuchBucket", "404", lambda: s3.head_bucket(Bucket="nope"))

print("== object basic ops ==")
body = b"hello brewfs s3 gateway\n" * 100
resp = s3.put_object(Bucket=BUCKET, Key="docs/readme.txt", Body=body, ContentType="text/plain")
etag = resp["ETag"].strip('"')
import hashlib
check("put_object etag = md5", etag == hashlib.md5(body).hexdigest(), etag)

got = s3.get_object(Bucket=BUCKET, Key="docs/readme.txt")
data = got["Body"].read()
check("get_object roundtrip", data == body)
check("get_object content-type", got.get("ContentType") == "text/plain", got.get("ContentType"))
check("get_object content-length", got.get("ContentLength") == len(body))

# ranged read
r = s3.get_object(Bucket=BUCKET, Key="docs/readme.txt", Range="bytes=6-10")
check("get_object range", r["Body"].read() == body[6:11], repr(r["Body"].read()))

h = s3.head_object(Bucket=BUCKET, Key="docs/readme.txt")
check("head_object size", h["ContentLength"] == len(body))
check("head_object metadata xattr path", "LastModified" in h)

expect_error("get_object missing key -> NoSuchKey", "NoSuchKey",
             lambda: s3.get_object(Bucket=BUCKET, Key="no/such/key"))

print("== list_objects_v2 ==")
for i in range(5):
    s3.put_object(Bucket=BUCKET, Key=f"logs/2026/day-{i}.log", Body=f"log line {i}".encode())
resp = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/")
keys = [o["Key"] for o in resp.get("Contents", [])]
check("list prefix", keys == [f"logs/2026/day-{i}.log" for i in range(5)], str(keys))
check("list etag present", all("ETag" in o for o in resp.get("Contents", [])))

resp = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/2026/", Delimiter="/")
check("list delimiter -> common prefix", resp.get("CommonPrefixes") is None, str(resp.get("CommonPrefixes")))
resp = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/", Delimiter="/")
cps = [p["Prefix"] for p in resp.get("CommonPrefixes", [])]
check("list delimiter -> common prefix", cps == ["logs/2026/"], str(cps))

# pagination
resp1 = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/", MaxKeys=2)
check("list truncated", resp1["IsTruncated"])
resp2 = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/", MaxKeys=10,
                           ContinuationToken=resp1["NextContinuationToken"])
all_keys = [o["Key"] for o in resp1.get("Contents", [])] + [o["Key"] for o in resp2.get("Contents", [])]
check("list pagination resumes", all_keys == [f"logs/2026/day-{i}.log" for i in range(5)], str(all_keys))

print("== copy_object ==")
s3.copy_object(Bucket=BUCKET, Key="docs/readme-copy.txt",
               CopySource={"Bucket": BUCKET, "Key": "docs/readme.txt"})
d = s3.get_object(Bucket=BUCKET, Key="docs/readme-copy.txt")["Body"].read()
check("copy_object data", d == body)

print("== multipart upload ==")
mp = s3.create_multipart_upload(Bucket=BUCKET, Key="big/blob.bin", ContentType="application/x-blob")
uid = mp["UploadId"]
part_size = 5 * 1024 * 1024
part1 = os.urandom(part_size)
part2 = os.urandom(1024 * 1024)
e1 = s3.upload_part(Bucket=BUCKET, Key="big/blob.bin", UploadId=uid, PartNumber=1, Body=part1)["ETag"]
e2 = s3.upload_part(Bucket=BUCKET, Key="big/blob.bin", UploadId=uid, PartNumber=2, Body=part2)["ETag"]
lp = s3.list_parts(Bucket=BUCKET, Key="big/blob.bin", UploadId=uid)
check("list_parts count", len(lp.get("Parts", [])) == 2, str(lp.get("Parts")))
resp = s3.complete_multipart_upload(
    Bucket=BUCKET, Key="big/blob.bin", UploadId=uid,
    MultipartUpload={"Parts": [
        {"ETag": e1, "PartNumber": 1},
        {"ETag": e2, "PartNumber": 2},
    ]})
etag = resp["ETag"].strip('"')
check("multipart etag has -2 suffix", etag.endswith("-2"), etag)
got = s3.get_object(Bucket=BUCKET, Key="big/blob.bin")
data = got["Body"].read()
check("multipart roundtrip", data == part1 + part2, f"len={len(data)}")
check("multipart content-type", got.get("ContentType") == "application/x-blob")

# abort flow
mp = s3.create_multipart_upload(Bucket=BUCKET, Key="big/aborted.bin")
uid2 = mp["UploadId"]
s3.upload_part(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2, PartNumber=1, Body=b"x" * 1024)
s3.abort_multipart_upload(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2)
expect_error("aborted upload -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.list_parts(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2))

print("== directory objects ==")
s3.put_object(Bucket=BUCKET, Key="empty-dir/", Body=b"")
r = s3.list_objects_v2(Bucket=BUCKET, Prefix="empty-dir")
check("dir object listed", any(o["Key"] == "empty-dir/" for o in r.get("Contents", [])), str(r.get("Contents")))
g = s3.get_object(Bucket=BUCKET, Key="empty-dir/")
check("dir object GET empty", g["Body"].read() == b"")
s3.delete_object(Bucket=BUCKET, Key="empty-dir/")

print("== delete ops ==")
s3.delete_object(Bucket=BUCKET, Key="docs/readme-copy.txt")
expect_error("deleted key gone", "NoSuchKey",
             lambda: s3.get_object(Bucket=BUCKET, Key="docs/readme-copy.txt"))
s3.delete_object(Bucket=BUCKET, Key="docs/readme.txt")
s3.delete_object(Bucket=BUCKET, Key="big/blob.bin")
for i in range(5):
    s3.delete_object(Bucket=BUCKET, Key=f"logs/2026/day-{i}.log")
r = s3.list_objects_v2(Bucket=BUCKET, Prefix="docs/")
check("empty prefixes pruned from listing", not r.get("Contents") and not r.get("CommonPrefixes"),
      str(r))

print("== delete_objects (batch) ==")
for i in range(3):
    s3.put_object(Bucket=BUCKET, Key=f"batch/f{i}.txt", Body=b"batch")
resp = s3.delete_objects(Bucket=BUCKET, Delete={
    "Objects": [{"Key": f"batch/f{i}.txt"} for i in range(3)]})
check("batch delete ok", len(resp.get("Deleted", [])) == 3 and not resp.get("Errors"),
      str(resp))

print(f"\nRESULT: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)

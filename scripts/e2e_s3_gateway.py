#!/usr/bin/env python3
"""End-to-end smoke test for the BrewFS S3 gateway.

Covers: list_buckets, head_bucket, put/get/head/delete_object,
list_objects_v2 (prefix/delimiter/pagination), copy_object,
multipart upload, directory objects, and error codes.
"""
import os
import sys
import threading
import time

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
check("head_bucket", True)
expect_error("head_bucket wrong bucket -> 404/NoSuchBucket", "404", lambda: s3.head_bucket(Bucket="nope"))
expect_error("get_bucket_location wrong bucket", "NoSuchBucket",
             lambda: s3.get_bucket_location(Bucket="nope"))

print("== object basic ops ==")
body = b"hello brewfs s3 gateway\n" * 100
resp = s3.put_object(
    Bucket=BUCKET,
    Key="docs/readme.txt",
    Body=body,
    ContentType="text/plain",
    Metadata={"source": "e2e"},
)
etag = resp["ETag"].strip('"')
import hashlib
check("put_object etag = md5", etag == hashlib.md5(body).hexdigest(), etag)

got = s3.get_object(Bucket=BUCKET, Key="docs/readme.txt")
data = got["Body"].read()
check("get_object roundtrip", data == body)
check("get_object content-type", got.get("ContentType") == "text/plain", got.get("ContentType"))
check("get_object user metadata", got.get("Metadata") == {"source": "e2e"}, str(got.get("Metadata")))
check("get_object content-length", got.get("ContentLength") == len(body))

# ranged read
r = s3.get_object(Bucket=BUCKET, Key="docs/readme.txt", Range="bytes=6-10")
ranged_body = r["Body"].read()
check("get_object range", ranged_body == body[6:11], repr(ranged_body))
check("get_object range status", r["ResponseMetadata"]["HTTPStatusCode"] == 206, str(r["ResponseMetadata"]))
check("get_object content-range", r.get("ContentRange") == f"bytes 6-10/{len(body)}", r.get("ContentRange"))

h = s3.head_object(Bucket=BUCKET, Key="docs/readme.txt")
check("head_object size", h["ContentLength"] == len(body))
check("head_object user metadata", h.get("Metadata") == {"source": "e2e"}, str(h.get("Metadata")))
check("head_object metadata xattr path", "LastModified" in h)

expect_error("get_object missing key -> NoSuchKey", "NoSuchKey",
             lambda: s3.get_object(Bucket=BUCKET, Key="no/such/key"))
s3.put_object(Bucket=BUCKET, Key="empty.bin", Body=b"")
expect_error("empty object range -> InvalidRange", "InvalidRange",
             lambda: s3.get_object(Bucket=BUCKET, Key="empty.bin", Range="bytes=-1"))

large_body = bytes(range(251)) * 100_000
large_etag = hashlib.md5(large_body).hexdigest()
replacement_body = b"replacement after streaming get"
s3.put_object(Bucket=BUCKET, Key="large/slow-read.bin", Body=large_body)
large_response = s3.get_object(Bucket=BUCKET, Key="large/slow-read.bin")
time.sleep(0.2)
overwrite_errors = []


def overwrite_large_object():
    try:
        s3.put_object(Bucket=BUCKET, Key="large/slow-read.bin", Body=replacement_body)
    except Exception as error:
        overwrite_errors.append(repr(error))


overwrite_thread = threading.Thread(target=overwrite_large_object, daemon=True)
overwrite_thread.start()
large_download = bytearray()
for chunk in large_response["Body"].iter_chunks(chunk_size=64 * 1024):
    large_download.extend(chunk)
    time.sleep(0.001)
overwrite_thread.join(timeout=30)
check("large slow get length", len(large_download) == len(large_body), str(len(large_download)))
check("large slow get md5", hashlib.md5(large_download).hexdigest() == large_etag)
current_large_body = None
if not overwrite_thread.is_alive() and not overwrite_errors:
    current_large_body = s3.get_object(Bucket=BUCKET, Key="large/slow-read.bin")["Body"].read()
check("concurrent overwrite publishes replacement",
      current_large_body == replacement_body,
      str(overwrite_errors or ["overwrite still running"]))

for operation, fn in [
    ("put", lambda: s3.put_object(Bucket=BUCKET, Key=".brewfs.sys/s3/tmp/intruder", Body=b"x")),
    ("get", lambda: s3.get_object(Bucket=BUCKET, Key=".brewfs.sys/s3/tmp/intruder")),
    ("delete", lambda: s3.delete_object(Bucket=BUCKET, Key=".brewfs.sys/s3/tmp/intruder")),
    ("list", lambda: s3.list_objects_v2(Bucket=BUCKET, Prefix=".brewfs.sys/")),
]:
    expect_error(f"reserved system path {operation}", "AccessDenied", fn)

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
check("list page uses full visible capacity",
      len(resp1.get("Contents", [])) == 2 and resp1.get("KeyCount") == 2,
      str(resp1))
resp2 = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/", MaxKeys=10,
                           ContinuationToken=resp1["NextContinuationToken"])
all_keys = [o["Key"] for o in resp1.get("Contents", [])] + [o["Key"] for o in resp2.get("Contents", [])]
check("list pagination resumes", all_keys == [f"logs/2026/day-{i}.log" for i in range(5)], str(all_keys))

deep_key = "/".join(["deep"] + [f"d{i}" for i in range(70)] + ["file.txt"])
s3.put_object(Bucket=BUCKET, Key=deep_key, Body=b"deep")
deep_keys = [o["Key"] for o in s3.list_objects_v2(Bucket=BUCKET, Prefix="deep/").get("Contents", [])]
check("list includes object deeper than 64 directories", deep_keys == [deep_key], str(deep_keys))

wide_keys = [f"wide/entry-{i:03d}.txt" for i in range(260)]
for wide_key in wide_keys:
    s3.put_object(Bucket=BUCKET, Key=wide_key, Body=b"wide")
listed_wide = [o["Key"] for o in s3.list_objects_v2(Bucket=BUCKET, Prefix="wide/").get("Contents", [])]
check("list reads directories beyond one readdir page", listed_wide == wide_keys, str(len(listed_wide)))
long_prefix_result = s3.list_objects_v2(Bucket=BUCKET, Prefix="x" * 1025)
check("list accepts prefix longer than object key limit",
      not long_prefix_result.get("Contents") and not long_prefix_result.get("CommonPrefixes"),
      str(long_prefix_result))
zero_page = s3.list_objects_v2(Bucket=BUCKET, Prefix="logs/", MaxKeys=0)
check("list zero max-keys is empty and not truncated",
      zero_page.get("KeyCount") == 0 and zero_page.get("IsTruncated") is False
      and not zero_page.get("NextContinuationToken"),
      str(zero_page))
expect_error("multiple trailing slashes are rejected", "InvalidArgument",
             lambda: s3.put_object(Bucket=BUCKET, Key="alias-dir//", Body=b""))

print("== copy_object ==")
s3.copy_object(Bucket=BUCKET, Key="docs/readme-copy.txt",
               CopySource={"Bucket": BUCKET, "Key": "docs/readme.txt"})
d = s3.get_object(Bucket=BUCKET, Key="docs/readme-copy.txt")["Body"].read()
check("copy_object data", d == body)
s3.copy_object(Bucket=BUCKET, Key="docs/readme-copy.txt",
               CopySource={"Bucket": BUCKET, "Key": "docs/readme-copy.txt"})
d = s3.get_object(Bucket=BUCKET, Key="docs/readme-copy.txt")["Body"].read()
check("copy_object self copy", d == body)
replace_copy = s3.copy_object(
    Bucket=BUCKET,
    Key="docs/readme-copy.txt",
    CopySource={"Bucket": BUCKET, "Key": "docs/readme-copy.txt"},
    MetadataDirective="REPLACE",
    Metadata={"copy-mode": "replace"},
)
check("copy metadata replace preserves body etag",
      replace_copy["CopyObjectResult"]["ETag"].strip('"') == hashlib.md5(body).hexdigest(),
      str(replace_copy))

print("== multipart upload ==")
expect_error("malformed unicode upload id -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.list_parts(Bucket=BUCKET, Key="big/blob.bin", UploadId="a😀"))
listed_uploads = []
for key in ["queued/a.bin", "queued/b.bin", "queued/c.bin"]:
    created = s3.create_multipart_upload(Bucket=BUCKET, Key=key)
    listed_uploads.append((key, created["UploadId"]))
upload_page1 = s3.list_multipart_uploads(Bucket=BUCKET, Prefix="queued/", MaxUploads=2)
check("list_multipart_uploads first page is truncated",
      [upload["Key"] for upload in upload_page1.get("Uploads", [])]
      == ["queued/a.bin", "queued/b.bin"]
      and upload_page1.get("IsTruncated") is True,
      str(upload_page1))
upload_page2 = s3.list_multipart_uploads(
    Bucket=BUCKET, Prefix="queued/", MaxUploads=2,
    KeyMarker=upload_page1["NextKeyMarker"],
    UploadIdMarker=upload_page1["NextUploadIdMarker"],
)
check("list_multipart_uploads pagination resumes",
      [upload["Key"] for upload in upload_page2.get("Uploads", [])] == ["queued/c.bin"]
      and upload_page2.get("IsTruncated") is False,
      str(upload_page2))
for key, upload_id in listed_uploads:
    s3.abort_multipart_upload(Bucket=BUCKET, Key=key, UploadId=upload_id)
s3.put_object(Bucket=BUCKET, Key="big/blob.bin", Body=b"old contents")
mp = s3.create_multipart_upload(Bucket=BUCKET, Key="big/blob.bin", ContentType="application/x-blob")
uid = mp["UploadId"]
expect_error("upload_part wrong key -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.upload_part(Bucket=BUCKET, Key="big/wrong.bin", UploadId=uid,
                                    PartNumber=1, Body=b"wrong"))
expect_error("upload_part wrong bucket -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.upload_part(Bucket="other", Key="big/blob.bin", UploadId=uid,
                                    PartNumber=1, Body=b"wrong"))
expect_error("list_parts wrong key -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.list_parts(Bucket=BUCKET, Key="big/wrong.bin", UploadId=uid))
expect_error("list_parts wrong bucket -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.list_parts(Bucket="other", Key="big/blob.bin", UploadId=uid))
expect_error("abort multipart wrong key -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.abort_multipart_upload(Bucket=BUCKET, Key="big/wrong.bin", UploadId=uid))
expect_error("abort multipart wrong bucket -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.abort_multipart_upload(Bucket="other", Key="big/blob.bin", UploadId=uid))
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
check("multipart overwrites existing object", data != b"old contents")
check("multipart content-type", got.get("ContentType") == "application/x-blob")

# S3 permits ascending, non-consecutive part numbers.
mp_sparse = s3.create_multipart_upload(Bucket=BUCKET, Key="big/sparse.bin")
uid_sparse = mp_sparse["UploadId"]
sparse_part2 = os.urandom(part_size)
sparse_part4 = os.urandom(1024 * 1024)
e_sparse2 = s3.upload_part(Bucket=BUCKET, Key="big/sparse.bin", UploadId=uid_sparse,
                            PartNumber=2, Body=sparse_part2)["ETag"]
e_sparse4 = s3.upload_part(Bucket=BUCKET, Key="big/sparse.bin", UploadId=uid_sparse,
                            PartNumber=4, Body=sparse_part4)["ETag"]
s3.complete_multipart_upload(
    Bucket=BUCKET, Key="big/sparse.bin", UploadId=uid_sparse,
    MultipartUpload={"Parts": [
        {"ETag": e_sparse2, "PartNumber": 2},
        {"ETag": e_sparse4, "PartNumber": 4},
    ]})
sparse_data = s3.get_object(Bucket=BUCKET, Key="big/sparse.bin")["Body"].read()
check("multipart non-consecutive parts", sparse_data == sparse_part2 + sparse_part4)

# abort flow
mp = s3.create_multipart_upload(Bucket=BUCKET, Key="big/aborted.bin")
uid2 = mp["UploadId"]
s3.upload_part(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2, PartNumber=1, Body=b"x" * 1024)
s3.abort_multipart_upload(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2)
expect_error("aborted upload -> NoSuchUpload", "NoSuchUpload",
             lambda: s3.list_parts(Bucket=BUCKET, Key="big/aborted.bin", UploadId=uid2))

many_parts = s3.create_multipart_upload(Bucket=BUCKET, Key="big/many-parts.bin")
many_parts_uid = many_parts["UploadId"]
for part_number in range(1, 261):
    s3.upload_part(Bucket=BUCKET, Key="big/many-parts.bin", UploadId=many_parts_uid,
                   PartNumber=part_number, Body=b"x")
many_parts_zero = s3.list_parts(Bucket=BUCKET, Key="big/many-parts.bin",
                                UploadId=many_parts_uid, MaxParts=0)
check("list_parts zero max-parts is empty and not truncated",
      not many_parts_zero.get("Parts") and many_parts_zero.get("IsTruncated") is False
      and not many_parts_zero.get("NextPartNumberMarker"),
      str(many_parts_zero))
many_parts_page1 = s3.list_parts(Bucket=BUCKET, Key="big/many-parts.bin",
                                 UploadId=many_parts_uid, MaxParts=100)
check("list_parts first page is truncated",
      len(many_parts_page1.get("Parts", [])) == 100
      and many_parts_page1.get("IsTruncated") is True
      and many_parts_page1.get("NextPartNumberMarker") == 100,
      str(many_parts_page1))
many_parts_page2 = s3.list_parts(
    Bucket=BUCKET, Key="big/many-parts.bin", UploadId=many_parts_uid, MaxParts=200,
    PartNumberMarker=many_parts_page1["NextPartNumberMarker"],
)
check("list_parts pagination resumes",
      [part["PartNumber"] for part in many_parts_page2.get("Parts", [])]
      == list(range(101, 261)) and many_parts_page2.get("IsTruncated") is False,
      str(many_parts_page2))
s3.abort_multipart_upload(Bucket=BUCKET, Key="big/many-parts.bin", UploadId=many_parts_uid)
expect_error("abort removes upload with more than one readdir page", "NoSuchUpload",
             lambda: s3.list_parts(Bucket=BUCKET, Key="big/many-parts.bin",
                                   UploadId=many_parts_uid))

s3.put_object(Bucket=BUCKET, Key="multipart-dir/", Body=b"")
blocked = s3.create_multipart_upload(Bucket=BUCKET, Key="multipart-dir")
blocked_uid = blocked["UploadId"]
blocked_etag = s3.upload_part(Bucket=BUCKET, Key="multipart-dir", UploadId=blocked_uid,
                              PartNumber=1, Body=b"blocked")["ETag"]
expect_error(
    "multipart target directory rejected", "InvalidArgument",
    lambda: s3.complete_multipart_upload(
        Bucket=BUCKET, Key="multipart-dir", UploadId=blocked_uid,
        MultipartUpload={"Parts": [{"ETag": blocked_etag, "PartNumber": 1}]},
    ),
)
s3.abort_multipart_upload(Bucket=BUCKET, Key="multipart-dir", UploadId=blocked_uid)
s3.delete_object(Bucket=BUCKET, Key="multipart-dir/")

print("== directory objects ==")
s3.put_object(Bucket=BUCKET, Key="slash-alias", Body=b"ordinary object")
expect_error("trailing slash PUT does not replace regular object", "InvalidArgument",
             lambda: s3.put_object(Bucket=BUCKET, Key="slash-alias/", Body=b""))
expect_error("trailing slash GET does not alias regular object", "NoSuchKey",
             lambda: s3.get_object(Bucket=BUCKET, Key="slash-alias/"))
s3.delete_object(Bucket=BUCKET, Key="slash-alias/")
check("trailing slash DELETE preserves regular object",
      s3.get_object(Bucket=BUCKET, Key="slash-alias")["Body"].read() == b"ordinary object")
expect_error("non-empty directory object is rejected", "InvalidArgument",
             lambda: s3.put_object(Bucket=BUCKET, Key="nonempty-dir/", Body=b"not empty"))
expect_error("non-empty copy to directory object is rejected", "InvalidArgument",
             lambda: s3.copy_object(Bucket=BUCKET, Key="copy-dir/",
                                    CopySource={"Bucket": BUCKET, "Key": "slash-alias"}))
s3.put_object(Bucket=BUCKET, Key="empty-source", Body=b"")
s3.copy_object(Bucket=BUCKET, Key="copied-dir/",
               CopySource={"Bucket": BUCKET, "Key": "empty-source"})
check("empty copy creates directory object",
      s3.get_object(Bucket=BUCKET, Key="copied-dir/")["Body"].read() == b"")
s3.delete_object(Bucket=BUCKET, Key="slash-alias")
s3.delete_object(Bucket=BUCKET, Key="empty-source")
s3.delete_object(Bucket=BUCKET, Key="copied-dir/")
s3.put_object(Bucket=BUCKET, Key="empty-dir/", Body=b"")
r = s3.list_objects_v2(Bucket=BUCKET, Prefix="empty-dir")
check("dir object listed", any(o["Key"] == "empty-dir/" for o in r.get("Contents", [])), str(r.get("Contents")))
r = s3.list_objects_v2(Bucket=BUCKET, Prefix="empty-dir/")
check("dir object listed with trailing-slash prefix",
      [o["Key"] for o in r.get("Contents", [])] == ["empty-dir/"], str(r))
r = s3.list_objects_v2(Bucket=BUCKET, Delimiter="/")
check("dir object rolls up under root delimiter",
      "empty-dir/" in [p["Prefix"] for p in r.get("CommonPrefixes", [])]
      and "empty-dir/" not in [o["Key"] for o in r.get("Contents", [])], str(r))
r = s3.list_objects_v2(Bucket=BUCKET, Prefix="empty-dir/", Delimiter="/")
check("dir object prefix delimiter listing",
      [o["Key"] for o in r.get("Contents", [])] == ["empty-dir/"], str(r))
h = s3.head_object(Bucket=BUCKET, Key="empty-dir/")
check("directory object HEAD size is zero", h["ContentLength"] == 0, str(h))
g = s3.get_object(Bucket=BUCKET, Key="empty-dir/")
check("dir object GET empty", g["Body"].read() == b"")
expect_error("dir object range -> InvalidRange", "InvalidRange",
             lambda: s3.get_object(Bucket=BUCKET, Key="empty-dir/", Range="bytes=0-0"))
s3.delete_object(Bucket=BUCKET, Key="empty-dir/")

print("== delete ops ==")
s3.delete_object(Bucket=BUCKET, Key="docs/readme-copy.txt")
expect_error("deleted key gone", "NoSuchKey",
             lambda: s3.get_object(Bucket=BUCKET, Key="docs/readme-copy.txt"))
s3.delete_object(Bucket=BUCKET, Key="docs/readme.txt")
s3.delete_object(Bucket=BUCKET, Key=deep_key)
for wide_key in wide_keys:
    s3.delete_object(Bucket=BUCKET, Key=wide_key)
s3.delete_object(Bucket=BUCKET, Key="empty.bin")
s3.delete_object(Bucket=BUCKET, Key="large/slow-read.bin")
s3.delete_object(Bucket=BUCKET, Key="big/blob.bin")
s3.delete_object(Bucket=BUCKET, Key="big/sparse.bin")
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

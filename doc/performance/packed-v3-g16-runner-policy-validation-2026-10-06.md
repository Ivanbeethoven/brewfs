# Packed v3 G16 runner policy validation — 2026-10-06

This checkpoint records a small G16 control-plane correction. The ECS runtime
role created by run_packed_campaign.py is limited to the owned campaign prefix
for object reads, creates, multipart aborts and part listing. It no longer has
DeleteObject: committed packed objects must be removed only by the separately
controlled cleanup path. The policy still has list access restricted to the
same exact prefix and bucket.

Evidence:

- tools/perf/test_aliyun_campaign.py: 3 Python unit tests passed.
- The policy test explicitly rejects DeleteBucket, wildcard actions and
  DeleteObject in the runtime role.
- bash -n docker/compose-xfstests/test_packed_fixture_targets.sh passed.
- Python bytecode compilation of the campaign control script and its tests
  passed.
- git diff --check passed.

This is only a credential-scope correction. It does not prove ECS/OSS/FUSE
execution, remote object-prefix cleanup, 005 cross-request pipeline behavior,
release build reproducibility, or the full G16/G17/S/X acceptance.

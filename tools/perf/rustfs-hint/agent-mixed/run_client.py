"""One frozen backend/round block of whole-file FUSE measurements on ACS."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import traceback
import boto3
from botocore.config import Config
import run_core as core

core.BASE = Path('/work')
core.BINARY = Path('/work/brewfs')


def cgroups():
    names = ['cpu.stat', 'cpu.max', 'memory.events', 'memory.current', 'memory.max', 'io.stat',
             'cpu/cpu.stat', 'cpu/cpu.cfs_quota_us', 'cpu/cpu.cfs_period_us', 'cpuacct/cpuacct.usage', 'cpuacct/cpuacct.stat',
             'memory/memory.failcnt', 'memory/memory.usage_in_bytes', 'memory/memory.limit_in_bytes']
    return {n: (Path('/sys/fs/cgroup') / n).read_text() for n in names if (Path('/sys/fs/cgroup') / n).is_file()}


class Experiment(core.Experiment):
    def __init__(self, job):
        self.job = job
        self.smoke = job['smoke']
        self.root = Path('/work/runs') / job['id']
        self.root.mkdir(parents=True, exist_ok=False)
        self.redis_id = None
        self.processes, self.mounts, self.rows = [], [], []
        auth = json.loads(Path('/work/private-auth.json').read_text())
        self.env = dict(os.environ, RUST_LOG='warn', AWS_ACCESS_KEY_ID=auth['access'],
                        AWS_SECRET_ACCESS_KEY=auth['secret'], AWS_REGION='us-east-1',
                        AWS_EC2_METADATA_DISABLED='true', BREWFS_FUSE_KEEP_CACHE='1',
                        BREWFS_FUSE_WRITEBACK='false', BREWFS_FUSE_DIRECT_IO='0')
        self.redis_host = job['redis_ip']
        self.endpoint = job['endpoint']
        self.s3 = boto3.client('s3', endpoint_url=self.endpoint, region_name='us-east-1',
                              aws_access_key_id=auth['access'], aws_secret_access_key=auth['secret'],
                              config=Config(signature_version='s3v4', connect_timeout=3, read_timeout=60,
                                            retries={'max_attempts': 0}, s3={'addressing_style': 'path'}))
        core.dump(self.root / 'resources-before.json', core.resources())

    def guard(self):
        assert shutil.disk_usage('/work').free > 10 * 1024**3
        assert subprocess.run(['pgrep', '-x', 'cargo|rustc|go'], stdout=subprocess.DEVNULL).returncode == 1

    def start(self):
        self.guard()
        assert hashlib.file_digest(core.BINARY.open('rb'), 'sha256').hexdigest() == self.job['brewfs_sha256']
        self.s3.list_buckets()
        assert subprocess.check_output(['redis-cli', '-h', self.redis_host, 'PING'], text=True).strip() == 'PONG'
        core.dump(self.root / 'protocol.json', self.job)
        core.dump(self.root / 'cgroups-before.json', cgroups())

    def reset_metadata(self):
        subprocess.run(['redis-cli', '-h', self.redis_host, 'FLUSHDB'], check=True, stdout=subprocess.DEVNULL, timeout=10)

    def mount(self, config, case, phase):
        config['meta']['redis']['url'] = 'redis://' + self.redis_host + ':6379/0'
        return super().mount(config, case, phase)

    def validate_objects(self, bucket, hint):
        objects = []
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            for obj in page.get('Contents', []):
                if not obj['Key'].startswith('chunks-v2/'):
                    continue
                head = self.s3.head_object(Bucket=bucket, Key=obj['Key'])
                assert head['ContentLength'] == obj['Size']
                header = head['ResponseMetadata']['HTTPHeaders'].get('x-rustfs-effective-ec-block-size')
                if self.job['arm']['binary'] == 'candidate':
                    expected = 4 * core.MIB if hint == 'auto' and obj['Size'] > core.MIB else core.MIB
                    assert header is not None and int(header) == expected
                else:
                    assert hint == 'off'
                    assert 'rustfs-ec-block-size-hint' not in head.get('Metadata', {})
                objects.append(dict(bucket=bucket, key=obj['Key'], bytes=obj['Size'],
                                    ec_bytes=int(header) if header is not None else None))
        return objects

    def execute(self):
        for number, spec in enumerate(self.job['cases']):
            before = cgroups()
            self.case(number, self.job['round'], spec['size'], spec['count'], spec['concurrency'],
                      self.job['arm']['hint'], spec['mixed'])
            row = self.rows[-1]
            row.update(arm=self.job['arm']['id'], job=self.job['id'])
            core.dump(self.root / f'case-{number:03d}/result.json', row)
            core.dump(self.root / f'case-{number:03d}/cgroups.json', dict(before=before, after=cgroups()))
            core.dump(self.root / 'results.json', self.rows)
        core.dump(self.root / 'objects.json', [o for row in self.rows for o in row['objects']])

    def close(self, success):
        super().close(success)
        core.dump(self.root / 'cgroups-after.json', cgroups())


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('job', type=Path)
    args = parser.parse_args()
    experiment = Experiment(json.loads(args.job.read_text()))
    success = False
    try:
        experiment.start()
        experiment.execute()
        success = True
    except BaseException:
        (experiment.root / 'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        experiment.close(success)

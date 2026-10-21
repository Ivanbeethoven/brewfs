"""Run the repository's unchanged compose workload inside the dedicated ACS Pod."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import traceback
import boto3
from botocore.config import Config

job=json.loads(Path(sys.argv[1]).read_text());root=Path('/work/runs')/job['id'];root.mkdir(parents=True,exist_ok=False)
def save(path,value):path.write_text(json.dumps(value,indent=2))
save(root/'protocol.json',job)
auth=json.loads(Path('/work/private-auth.json').read_text())
binary=Path(job['brewfs_binary']);assert hashlib.file_digest(binary.open('rb'),'sha256').hexdigest()==job['brewfs_sha256']
link=Path('/usr/local/bin/brewfs')
if link.is_symlink():
    assert os.readlink(link) in ['/work/brewfs','/work/brewfs-recovery']
    link.unlink()
assert not link.exists();link.symlink_to(binary)
bucket=job['id']
s3=boto3.client('s3',endpoint_url=job['endpoint'],region_name='us-east-1',aws_access_key_id=auth['access'],aws_secret_access_key=auth['secret'],
               config=Config(signature_version='s3v4',retries={'max_attempts':0},s3={'addressing_style':'path'}))
s3.create_bucket(Bucket=bucket)
subprocess.run(['redis-cli','-h',job['redis_ip'],'FLUSHDB'],check=True,stdout=subprocess.DEVNULL)
env=dict(os.environ,**job['client_environment'],AWS_ACCESS_KEY_ID=auth['access'],AWS_SECRET_ACCESS_KEY=auth['secret'],AWS_DEFAULT_REGION='us-east-1',AWS_REGION='us-east-1',AWS_EC2_METADATA_DISABLED='true',
         BREWFS_S3_BUCKET=bucket,BREWFS_S3_ENDPOINT=job['endpoint'],BREWFS_META_URL='redis://'+job['redis_ip']+':6379/0',
         BREWFS_CONFIG_PATH=str(root/'config.yaml'),BREWFS_MOUNT_POINT=str(root/'mount'),BREWFS_CACHE_ROOT=str(root/'cache'),BREWFS_LOG_FILE=str(root/'brewfs.log'),
         BREWFS_ARTIFACT_DIR=str(root/'artifacts'),BREWFS_ARTIFACT_ROOT=str(root/'artifacts'))
if job['upload_permits'] is None:env.pop('BREWFS_WRITEBACK_UPLOAD_CONCURRENCY',None)
else:env['BREWFS_WRITEBACK_UPLOAD_CONCURRENCY']=str(job['upload_permits'])
runner=Path('/work/perf-script-gate-canonical/docker/compose-xfstests/run_perf_in_container.sh')
assert hashlib.file_digest(runner.open('rb'),'sha256').hexdigest()==job['runner_sha256']
success=False;started=time.time()
try:
    with (root/'runner.log').open('wb') as log:
        code=subprocess.run(['bash',str(runner)],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,timeout=1200).returncode
    assert code==0, f'Canonical runner exited {code}'
    assert (root/'artifacts/perf.complete').exists()
    assert not os.path.ismount(root/'mount')
    live=[]
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():continue
        try:
            fields=(proc/'stat').read_text().split(') ',1)
            if fields[0].endswith('(brewfs') and fields[1].split()[0]!='Z':live.append(proc.name)
        except FileNotFoundError:
            continue
    assert not live, f'BrewFS processes still live: {live}'
    objects=[]
    for page in s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
        for obj in page.get('Contents',[]):
            if obj['Key'].startswith('chunks-v2/'):
                objects.append(dict(bucket=bucket,key=obj['Key'],bytes=obj['Size'],ec_bytes=1048576))
    save(root/'objects.json',objects)
    cache=root/'cache'
    assert cache.resolve()==root.resolve()/'cache' and not os.path.ismount(root/'mount')
    if cache.exists():shutil.rmtree(cache)
    save(root/'cache-cleanup.json',dict(unmounted=True,cache_removed=True,objects_recorded=len(objects)))
    success=True
except BaseException:
    (root/'failure.txt').write_text(traceback.format_exc());raise
finally:
    save(root/'closure.json',dict(success=success,started=started,ended=time.time(),remaining_mounts=[str(root/'mount')] if os.path.ismount(root/'mount') else [],
                                  runner_returncode=locals().get('code'),canonical_runner=True))

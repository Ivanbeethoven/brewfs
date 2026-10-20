"""Single controller for the fixed paired campaign. Timeouts never restart a job."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import traceback
from cloud import BASE, WORK, K, CLIENT, SERVER, execute, put, save


def launch(job, folder):
    path='/work/jobs/'+job['id']+'.json'
    for pod in [CLIENT,SERVER]:
        put(pod,path,json.dumps(job).encode())
    print(execute(SERVER,['python3','/work/server_control.py','start',path]).decode(),flush=True)
    ready='''import json,sys,time,boto3
from botocore.config import Config
a=json.load(open('/work/private-auth.json'))
s=boto3.client('s3',endpoint_url=sys.argv[1],region_name='us-east-1',aws_access_key_id=a['access'],aws_secret_access_key=a['secret'],config=Config(connect_timeout=2,read_timeout=3,retries={'max_attempts':0},s3={'addressing_style':'path'}))
for i in range(60):
 try:
  s.list_buckets();break
 except Exception:time.sleep(1)
else:raise RuntimeError('S3 not ready')
'''
    execute(CLIENT,['python3','-c',ready,job['endpoint']],timeout=300)
    start='''import json,subprocess,sys
from pathlib import Path
j=sys.argv[1];p=Path('/work/launch')/(j+'.log');p.parent.mkdir(exist_ok=True)
with p.open('xb') as f:
 c=subprocess.Popen(['python3','/work/run_agent_io.py','/work/jobs/'+j+'.json'],stdin=subprocess.DEVNULL,stdout=f,stderr=subprocess.STDOUT,start_new_session=True)
starttime=Path('/proc/'+str(c.pid)+'/stat').read_text().split(') ',1)[1].split()[19]
print(json.dumps(dict(pid=c.pid,starttime=starttime,job=j)))
'''
    handle=json.loads(execute(CLIENT,['python3','-c',start,job['id']]))
    save(folder/(job['id']+'-handle.json'),handle)
    return handle


def poll(job, handle):
    code='''import json,sys
from pathlib import Path
p=Path('/work/runs')/sys.argv[1]
s=Path('/proc/'+sys.argv[2]+'/stat')
fields=s.read_text().split(') ',1)[1].split() if s.exists() else []
alive=bool(fields and fields[0]!='Z' and fields[19]==sys.argv[3])
out=dict(alive=alive)
for name in ['closure','progress']:
 f=p/(name+'.json');out[name]=json.loads(f.read_text()) if f.exists() else None
print(json.dumps(out))
'''
    return json.loads(execute(CLIENT,['python3','-c',code,job['id'],str(handle['pid']),handle['starttime']]))


def follow(job,handle,folder):
    deadline=time.monotonic()+3600
    previous=None
    while time.monotonic()<deadline:
        status=poll(job,handle)
        save(folder/'progress.json',dict(job=job['id'],handle=handle,status=status,observed=time.time()))
        phase=(status.get('progress') or {}).get('phase')
        if phase!=previous:
            print(json.dumps(dict(job=job['id'],phase=phase,alive=status['alive'])),flush=True)
            previous=phase
        if not status['alive']:
            assert status['closure'] is not None, 'client ended without closure'
            return status['closure']
        time.sleep(10)
    raise TimeoutError('Observation timeout: keep and follow the original PID; never restart it')


def backup(job,folder):
    secret_values=list(json.loads((BASE/'.private-auth.json').read_text()).values())
    records=[]
    for pod,role in [(CLIENT,'client'),(SERVER,'server')]:
        dest=folder/'raw'/job['id']/role
        dest.mkdir(parents=True,exist_ok=False)
        with tempfile.TemporaryFile() as archive:
            subprocess.run(K+['exec',pod,'--','tar','--exclude=./mount','--exclude=./prefill-cache',
                             '--exclude=./measured-cache','--exclude=./verify-cache','-czf','-',
                             '-C','/work/runs/'+job['id'],'.'],stdout=archive,check=True,timeout=180)
            archive.seek(0)
            with tarfile.open(fileobj=archive) as t:
                t.extractall(dest,filter='data')
        for p in dest.rglob('*'):
            if p.is_file():
                raw=p.read_bytes()
                for secret in secret_values:
                    raw=raw.replace(secret.encode(),b'[REDACTED]')
                p.write_bytes(raw)
                records.append(dict(path=str(p.relative_to(folder)),bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest()))
    save(folder/(job['id']+'-export.json'),dict(files=records,completed=True))
    assert all(hashlib.sha256((folder/r['path']).read_bytes()).hexdigest()==r['sha256'] for r in records)


def reclaim(job):
    code='''import json,shutil,sys
from pathlib import Path
assert not Path('/work/active-server.json').exists()
r=json.loads((Path('/work/runs')/sys.argv[1]/'start.json').read_text())
assert r['job']['id']==sys.argv[1]
for i,v in enumerate(r['volumes']):
 p=Path(v['path']);assert p.resolve()==Path('/data'+str(i))/sys.argv[1]
 assert p.stat().st_dev==v['device'] and p.stat().st_ino==v['inode']
 shutil.rmtree(p)
print(json.dumps(dict(owned_volumes_removed=4)))
'''
    return json.loads(execute(SERVER,['python3','-c',code,job['id']],timeout=300))


def main(stage):
    plan_path=BASE/(stage+'-plan.json')
    plan=json.loads(plan_path.read_text())
    assert all(hashlib.sha256((WORK/n).read_bytes()).hexdigest()==h for n,h in plan['tools'].items())
    assert json.loads((BASE/(stage+'-staged-verification.json')).read_text())['passed']
    folder=BASE/stage;folder.mkdir(exist_ok=False)
    save(folder/'started.json',dict(stage=stage,plan_sha256=hashlib.sha256(plan_path.read_bytes()).hexdigest(),time=time.time()))
    completed=[]
    try:
        for job in plan['jobs']:
            handle=launch(job,folder)
            closure=follow(job,handle,folder)
            if not closure['success']:
                stop=json.loads(execute(SERVER,['python3','/work/server_control.py','stop']))
                backup(job,folder)
                save(folder/'closure.json',dict(success=False,failed_job=job['id'],client=closure,server_stop=stop,completed=completed))
                raise RuntimeError('Failed client; preserved original artifacts: '+job['id'])
            assert not closure['remaining_mounts'] and all(p['exit']==0 for p in closure['processes'])
            objects=execute(CLIENT,['cat','/work/runs/'+job['id']+'/objects.json'])
            put(SERVER,'/work/jobs/'+job['id']+'-objects.json',objects)
            layout=json.loads(execute(SERVER,['python3','/work/audit_policy_layout.py','/work/jobs/'+job['id']+'-objects.json'],timeout=900))
            stop=json.loads(execute(SERVER,['python3','/work/server_control.py','stop']))
            backup(job,folder)
            clean=reclaim(job)
            completed.append(dict(job=job['id'],layout=layout,server_stop=stop,cleanup=clean))
            save(folder/'completed.json',completed)
            print(json.dumps(dict(completed=len(completed),total=len(plan['jobs']),job=job['id'])),flush=True)
        save(folder/'closure.json',dict(success=True,completed=completed,time=time.time()))
    except BaseException:
        (folder/'interruption.txt').write_text(traceback.format_exc())
        raise


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('stage',choices=['smoke','formal']);main(p.parse_args().stage)

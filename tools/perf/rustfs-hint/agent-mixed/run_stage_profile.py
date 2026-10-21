"""Run one separate syscall diagnostic only after all formal jobs have closed."""
import hashlib
import json
import time
from cloud import BASE, WORK, CLIENT, SERVER, execute, put, save
from campaign import follow, backup, reclaim
from resume_formal import ensure_layout


def main():
    assert json.loads((BASE/'formal/closure.json').read_text())['success']
    assert json.loads((BASE/'independent-audit.json').read_text())['passed']
    folder=BASE/'stage-profile'
    folder.mkdir(exist_ok=False)
    install=execute(CLIENT,['bash','-ec',
        'command -v strace >/dev/null || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends strace'],timeout=180)
    (folder/'strace-install.log').write_bytes(install)
    # Probe ptrace and the process identity promised by -D before touching FUSE.
    probe='''import json,os,subprocess,time
from pathlib import Path
path=Path('/work/stage-profile-probe.txt')
p=subprocess.Popen(['strace','-D','-f','-c','-w','-e','trace=fsync','-o',str(path),'--','/bin/sleep','1'])
for i in range(50):
 try:
  exe=os.readlink('/proc/'+str(p.pid)+'/exe')
  if exe.endswith('/sleep'):break
 except FileNotFoundError:pass
 if p.poll() is not None:break
 time.sleep(.01)
assert exe.endswith('/sleep'), 'strace -D did not preserve tracee identity'
assert p.wait(timeout=10)==0
print(json.dumps(dict(passed=True,pid_identity_preserved=True)))
'''
    save(folder/'probe.json',json.loads(execute(CLIENT,['python3','-c',probe])))
    plan=json.loads((BASE/'formal-plan.json').read_text())
    job=next(j for j in plan['jobs'] if j['scene']=='edit' and j['round']==1 and j['arm']['id']=='dynamic-auto')
    job=dict(job,id='agent-diagnostic-stage-edit',diagnostic_only=True)
    source=(WORK/'profile_stage.py').read_bytes()
    put(CLIENT,'/work/profile_stage.py',source)
    for pod in [CLIENT,SERVER]:
        put(pod,'/work/jobs/'+job['id']+'.json',json.dumps(job).encode())
    save(folder/'plan.json',dict(job=job,runner_sha256=hashlib.sha256(source).hexdigest(),
         rationale='One preselected edit trace to locate write-path waits after untraced formal completion; excluded from performance acceptance.',started=time.time()))
    print(execute(SERVER,['python3','/work/server_control.py','start','/work/jobs/'+job['id']+'.json']).decode(),flush=True)
    launch='''import json,subprocess,sys
from pathlib import Path
j=sys.argv[1];p=Path('/work/launch')/(j+'.log')
with p.open('xb') as f:
 c=subprocess.Popen(['python3','/work/profile_stage.py','/work/jobs/'+j+'.json'],stdin=subprocess.DEVNULL,stdout=f,stderr=subprocess.STDOUT,start_new_session=True)
starttime=Path('/proc/'+str(c.pid)+'/stat').read_text().split(') ',1)[1].split()[19]
print(json.dumps(dict(pid=c.pid,starttime=starttime,job=j)))
'''
    handle=json.loads(execute(CLIENT,['python3','-c',launch,job['id']]))
    save(folder/'handle.json',handle)
    closure=follow(job,handle,folder)
    layout=None
    if closure['success']:
        objects=execute(CLIENT,['cat','/work/runs/'+job['id']+'/objects.json'])
        put(SERVER,'/work/jobs/'+job['id']+'-objects.json',objects)
        layout=ensure_layout(job,folder)
    stop=json.loads(execute(SERVER,['python3','/work/server_control.py','stop']))
    backup(job,folder)
    cleanup=reclaim(job) if closure['success'] else None
    save(folder/'closure.json',dict(success=closure['success'],client=closure,layout=layout,server_stop=stop,cleanup=cleanup))
    print(json.dumps(dict(diagnostic_complete=True,success=closure['success'],acceptance_sample=False)),flush=True)


if __name__=='__main__':
    main()

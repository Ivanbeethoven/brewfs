"""Reattach a completed client after a control disconnect; never repeat a measured job."""
import argparse
import hashlib
import json
import subprocess
import sys
import time
import traceback
from pathlib import Path
from cloud import BASE, WORK, SERVER, execute, put, save
from campaign import launch, poll, backup, reclaim


def follow_existing(job,handle,folder,recovery):
    previous=None
    consecutive_errors=0
    while True:
        try:
            status=poll(job,handle)
            consecutive_errors=0
        except (RuntimeError,subprocess.TimeoutExpired):
            # This retry is solely a read of the same PID and files. Mutations
            # below retain their original one-shot behavior on uncertain replies.
            with (recovery/'observation-errors.log').open('a') as log:
                log.write(str(time.time())+'\n'+traceback.format_exc()+'\n')
            consecutive_errors+=1
            if consecutive_errors>=10:
                raise
            time.sleep(10)
            continue
        save(folder/'progress.json',dict(job=job['id'],handle=handle,status=status,observed=time.time()))
        phase=(status.get('progress') or {}).get('phase')
        if phase!=previous:
            print(json.dumps(dict(job=job['id'],phase=phase,alive=status['alive'])),flush=True)
            previous=phase
        if not status['alive']:
            assert status['closure'] is not None, 'Original client exited without closure'
            return status['closure']
        time.sleep(10)


def ensure_layout(job,recovery):
    inspect='''import json,sys
from pathlib import Path
p=Path('/work/runs')/sys.argv[1];c=p/'layout-background-closure.json';h=p/'layout-background-handle.json'
handle=json.loads(h.read_text()) if h.exists() else None
s=Path('/proc')/str(handle['pid'])/'stat' if handle else None
a=s.read_text().split(') ',1)[1].split() if s and s.exists() else []
print(json.dumps(dict(closure=json.loads(c.read_text()) if c.exists() else None,handle=handle,alive=bool(a and a[0]!='Z' and a[19]==handle['starttime']))))
'''
    launch_code='''import subprocess,sys
from pathlib import Path
j=sys.argv[1]
with (Path('/work/runs')/j/'layout-background-launch.log').open('ab') as log:
 subprocess.Popen(['python3','/work/layout_background.py',j],stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
'''
    errors=0
    while True:
        try:
            status=json.loads(execute(SERVER,['python3','-c',inspect,job['id']]))
            save(recovery/'layout-progress.json',dict(job=job['id'],status=status,observed=time.time()))
            if status['closure'] is not None:
                assert status['closure']['success'], status['closure']
                return dict(status['closure']['receipt'],job=job['id'])
            if not status['alive']:
                assert status['handle'] is None, 'Layout worker exited without closure; inspect original PID/output'
                # The remote worker holds a per-job flock and checks its closed
                # receipt. Re-dispatch after a lost exec reply cannot repeat audit.
                execute(SERVER,['python3','-c',launch_code,job['id']])
            errors=0
        except (RuntimeError,subprocess.TimeoutExpired):
            with (recovery/'layout-observation-errors.log').open('a') as log:
                log.write(str(time.time())+'\n'+traceback.format_exc()+'\n')
            errors+=1
            if errors>=10:
                raise
        time.sleep(5)


def main(recovery_name):
    folder=BASE/'formal'
    assert recovery_name.startswith('formal-recovery') and Path(recovery_name).name == recovery_name
    recovery=BASE/recovery_name
    recovery.mkdir(exist_ok=False)
    plan=json.loads((BASE/'formal-plan.json').read_text())
    assert all(hashlib.sha256((WORK/n).read_bytes()).hexdigest()==h for n,h in plan['tools'].items())
    completed=json.loads((folder/'completed.json').read_text())
    first_index=len(completed)
    assert first_index<len(plan['jobs'])
    assert [c['job'] for c in completed]==[j['id'] for j in plan['jobs'][:first_index]]
    first=plan['jobs'][first_index]
    handle=json.loads((folder/(first['id']+'-handle.json')).read_text())
    status=poll(first,handle)
    assert not status['alive'] and status['closure']['success']
    active=json.loads(execute(SERVER,['cat','/work/active-server.json']))
    assert active['job']==first
    assert not (folder/(first['id']+'-export.json')).exists()
    assert not (folder/'raw'/first['id']).exists()
    execute(SERVER,['test','!','-e','/work/runs/'+first['id']+'/layouts.jsonl'])
    execute(SERVER,['test','!','-e','/work/runs/'+first['id']+'/layout-audit.json'])
    layout_source=(WORK/'layout_background.py').read_bytes()
    put(SERVER,'/work/layout_background.py',layout_source)
    save(recovery/'reattachment.json',dict(job=first['id'],original_handle=handle,original_status=status,
        server_pid=active['pid'],server_identity=active['identity'],started=time.time(),
        layout_supervisor_sha256=hashlib.sha256(layout_source).hexdigest(),
        reason='Controller exited on ACS API connect timeout. The original remote client completed successfully; no layout output exists. No measurement repeated or edited.'))
    try:
        for index,job in enumerate(plan['jobs'][first_index:],start=first_index):
            if index!=first_index:
                handle=launch(job,folder)
            save(recovery/'progress.json',dict(job=job['id'],phase='following',handle=handle,time=time.time()))
            closure=follow_existing(job,handle,folder,recovery)
            assert closure['success'] and not closure['remaining_mounts']
            assert all(p['exit']==0 for p in closure['processes'])
            save(recovery/'progress.json',dict(job=job['id'],phase='layout-and-export',time=time.time()))
            objects=execute('agent-client',['cat','/work/runs/'+job['id']+'/objects.json'])
            put(SERVER,'/work/jobs/'+job['id']+'-objects.json',objects)
            layout=ensure_layout(job,recovery)
            stop=json.loads(execute(SERVER,['python3','/work/server_control.py','stop']))
            backup(job,folder)
            clean=reclaim(job)
            completed.append(dict(job=job['id'],layout=layout,server_stop=stop,cleanup=clean))
            save(folder/'completed.json',completed)
            print(json.dumps(dict(completed=len(completed),total=len(plan['jobs']),job=job['id'])),flush=True)
        save(folder/'closure.json',dict(success=True,completed=completed,time=time.time(),
            observation_recovery=recovery_name+'/reattachment.json; all original interruptions preserved'))
        save(recovery/'closure.json',dict(success=True,time=time.time(),jobs_resumed=len(plan['jobs'])-first_index,measurements_repeated=0))
    except BaseException:
        (recovery/'interruption.txt').write_text(traceback.format_exc())
        raise
    print('Campaign recovered. Run the documented offline analysis/export steps.', flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--recovery-name',required=True)
    main(parser.parse_args().recovery_name)

"""Explicitly own and switch one benchmark backend in one ACS Pod."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import signal
import subprocess
import time

ROOT = Path('/work')
ACTIVE = ROOT / 'active-server.json'


def save(path, value):
    path.write_text(json.dumps(value, indent=2))


def cgroups():
    names = ['cpu.stat', 'cpu.max', 'memory.events', 'memory.current', 'memory.max', 'io.stat',
             'cpu/cpu.stat', 'cpuacct/cpuacct.usage', 'cpuacct/cpuacct.stat',
             'memory/memory.failcnt', 'memory/memory.usage_in_bytes']
    return {n: (Path('/sys/fs/cgroup') / n).read_text() for n in names if (Path('/sys/fs/cgroup') / n).is_file()}


def identity(pid):
    text = Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1].split()
    return dict(state=text[0], starttime=text[19])


def start(job):
    assert not ACTIVE.exists(), 'A backend already owns the Pod'
    assert len(set(Path('/data' + str(i)).stat().st_dev for i in range(4))) == 4
    assert job['id'].replace('-', '').isalnum()
    arm = job['arm']
    binary = ROOT / arm['binary']
    assert hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest() == job['backend_sha256']
    folder = ROOT / 'runs' / job['id']
    folder.mkdir(parents=True, exist_ok=False)
    volumes = [Path('/data' + str(i)) / job['id'] for i in range(4)]
    for volume in volumes:
        volume.mkdir(exist_ok=False)
    public = dict(arm['environment'])
    env = dict(os.environ, **public)
    if arm['binary'] == 'minio':
        env.update(MINIO_ROOT_USER=os.environ['RUSTFS_ACCESS_KEY'], MINIO_ROOT_PASSWORD=os.environ['RUSTFS_SECRET_KEY'])
    args = [str(binary), 'server', '--address', '0.0.0.0:9000', *map(str, volumes)]
    with (folder / 'server.log').open('wb') as log:
        proc = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True, cwd=ROOT)
    cpu_models=sorted(set(line.split(':',1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name')))
    record = dict(job=job, pid=proc.pid, identity=identity(proc.pid), command=args,
                  volumes=[dict(path=str(v), device=v.stat().st_dev, inode=v.stat().st_ino) for v in volumes],
                  kernel=os.uname().release, cpu_models=cpu_models, affinity=sorted(os.sched_getaffinity(0)),
                  clock_ticks=os.sysconf('SC_CLK_TCK'),
                  started=time.time(), cgroups_before=cgroups())
    save(ACTIVE, record)
    save(folder / 'start.json', record)
    actual = dict(item.split(b'=', 1) for item in Path(f'/proc/{proc.pid}/environ').read_bytes().split(b'\0') if b'=' in item)
    assert all(actual.get(k.encode()) == v.encode() for k, v in public.items()), 'Process flags differ'
    assert os.readlink(f'/proc/{proc.pid}/exe') == str(binary)
    save(folder / 'verified-public-environment.json', {k: actual[k.encode()].decode() for k in public})
    with (folder/'sampler.log').open('wb') as log:
        sampler=subprocess.Popen(['python3','/work/sample_resources.py',str(proc.pid),str(folder)],stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
    record['sampler']=dict(pid=sampler.pid,identity=identity(sampler.pid))
    save(ACTIVE,record)
    print(json.dumps(dict(started=True, job=job['id'], pid=proc.pid, public_environment=public)), flush=True)


def stop():
    record = json.loads(ACTIVE.read_text())
    pid = record['pid']
    current = identity(pid)
    assert current['starttime'] == record['identity']['starttime']
    assert os.readlink(f'/proc/{pid}/exe') == record['command'][0]
    before_stop = cgroups()
    folder=ROOT/'runs'/record['job']['id']
    (folder/'stop-sampler').touch()
    deadline=time.monotonic()+10
    while not (folder/'sampler-closed.json').exists() and time.monotonic()<deadline:
        time.sleep(.1)
    assert (folder/'sampler-closed.json').exists(),'Resource sampler did not close'
    process_before_stop=dict(stat=Path(f'/proc/{pid}/stat').read_text(),status=Path(f'/proc/{pid}/status').read_text(),
                             io=Path(f'/proc/{pid}/io').read_text())
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        try:
            current = identity(pid)
            assert current['starttime'] == record['identity']['starttime']
            if current['state'] == 'Z':
                break
        except FileNotFoundError:
            break
        time.sleep(.2)
    else:
        raise TimeoutError('Owned backend did not exit; no replacement started')
    save(ROOT / 'runs' / record['job']['id'] / 'stop.json',
         dict(stopped=True, job=record['job']['id'], ended=time.time(), cgroups_before_stop=before_stop,
              process_before_stop=process_before_stop,
              cgroups_after_stop=cgroups()))
    ACTIVE.unlink()
    print(json.dumps(dict(stopped=True, job=record['job']['id'])), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['start', 'stop'])
    parser.add_argument('job', type=Path, nargs='?')
    args = parser.parse_args()
    if args.action == 'start':
        start(json.loads(args.job.read_text()))
    else:
        stop()

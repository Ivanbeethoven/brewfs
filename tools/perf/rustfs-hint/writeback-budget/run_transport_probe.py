"""Run one fixed transport diagnostic using only the already-owned idle Pods."""
import hashlib
import json
import time
from cloud import BASE, WORK, SERVER, CLIENT, command, execute, put, save

assert json.loads((BASE/'body-stream-network-closure.json').read_text())['success']
assert not (BASE/'transport-probe-plan.json').exists()
execute(SERVER,['test','!','-f','/work/active-server.json'])
pods=json.loads(command(['get','pod',SERVER,CLIENT,'-o','json']))['items']
topology=json.loads((BASE/'topology.json').read_text())['resources']['items']
for pod in pods:
    assert any(pod['metadata']['uid']==old['metadata']['uid'] for old in topology)
host=next(pod['status']['podIP'] for pod in pods if pod['metadata']['name']==SERVER)
source=(WORK/'transport_probe.py').read_bytes()
save(BASE/'transport-probe-plan.json',dict(protocol='Single bounded pure TCP diagnostic, no product binaries or disk IO. Same existing Pods. Each case transfers2GiB with1/4flows, download then upload, full SHA256 on receivers, 8MiB progress samples. Fixed2second intervals. Payload generation and expected hash preparation outside timing. Hashing remains inside receive timing, CPU recorded. No throughput gate or candidate acceptance; retain all cases.',
    source_sha256=hashlib.sha256(source).hexdigest(),pod_uids=[p['metadata']['uid'] for p in pods]))
(BASE/'transport-probe-source.py').write_bytes(source)
for pod in [SERVER,CLIENT]:put(pod,'/work/transport_probe.py',source)
launcher='''import subprocess,json,sys
from pathlib import Path
with open('/work/transport-probe-server.log','xb') as f:
 p=subprocess.Popen(['python3','/work/transport_probe.py','server','--host',sys.argv[1],'--output','/work/transport-probe-server.json'],stdin=subprocess.DEVNULL,stdout=f,stderr=subprocess.STDOUT,start_new_session=True)
print(json.dumps(dict(pid=p.pid,starttime=Path('/proc/'+str(p.pid)+'/stat').read_text().split(') ',1)[1].split()[19])))
'''
identity=json.loads(execute(SERVER,['python3','-c',launcher,host]));save(BASE/'transport-probe-server-process.json',identity)
# Expected digests are prepared independently before each side connects.
time.sleep(3)
try:
    log=execute(CLIENT,['python3','/work/transport_probe.py','client','--host',host,'--output','/work/transport-probe-client.json'],timeout=300)
    (BASE/'transport-probe-client.log').write_bytes(log);print(log.decode(),flush=True)
finally:
    for pod,role in [(SERVER,'server'),(CLIENT,'client')]:
        for suffix in ['json']:
            data=execute(pod,['cat',f'/work/transport-probe-{role}.{suffix}'])
            (BASE/f'transport-probe-{role}.{suffix}').write_bytes(data)
    (BASE/'transport-probe-server.log').write_bytes(execute(SERVER,['cat','/work/transport-probe-server.log']))
server=json.loads((BASE/'transport-probe-server.json').read_text());client=json.loads((BASE/'transport-probe-client.json').read_text())
assert server['complete'] and client['complete']
assert len(server['rows'])==len(client['rows'])==4
assert all(all(f['full_sha256_verified'] for f in r['flows']) for side in [server,client] for r in side['rows'])
verify='''import sys,json
from pathlib import Path
p=Path('/proc/'+sys.argv[1]+'/stat')
if p.exists():
 s=p.read_text().split(') ',1)[1].split()
 assert s[19]!=sys.argv[2] or s[0]=='Z','probe still running'
print(json.dumps({'server_exited':True}))
'''
execute(SERVER,['python3','-c',verify,str(identity['pid']),identity['starttime']])
save(BASE/'transport-probe-closure.json',dict(success=True,bytes_transferred=4*2*1024**3,full_stream_sha256_checks=10,no_disks_created=True,server_exited=True))
print('Transport diagnostic complete: 8GiB verified, no retained payload or listener.',flush=True)

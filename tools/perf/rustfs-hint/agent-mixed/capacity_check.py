"""One bounded environment observation; not a product performance comparison."""
import hashlib
import json
import time
from cloud import BASE, WORK, CLIENT, SERVER, command, execute, put, save

assert not (BASE/'transport-probe-plan.json').exists()
execute(SERVER,['test','!','-f','/work/active-server.json'])
pods=json.loads(command(['get','pod',CLIENT,SERVER,'-o','json']))['items']
host=next(p['status']['podIP'] for p in pods if p['metadata']['name']==SERVER)
source=(WORK/'transport_probe.py').read_bytes()
save(BASE/'transport-probe-plan.json',dict(source_sha256=hashlib.sha256(source).hexdigest(),
    pod_uids=[p['metadata']['uid'] for p in pods],bytes=8*1024**3,
    note='Same full2GiB per direction/1-or4connections diagnostic as prior campaign. Hash inside receive timing, no product or disks, all tails retained. Not an isolated estimate of network link capacity if receiver CPU dominates.'))
start='''import json,subprocess,sys
from pathlib import Path
with open('/work/transport-probe-server.log','xb') as f:
 p=subprocess.Popen(['python3','/work/transport_probe.py','server','--host',sys.argv[1],'--output','/work/transport-probe-server.json'],stdin=subprocess.DEVNULL,stdout=f,stderr=subprocess.STDOUT,start_new_session=True)
print(json.dumps(dict(pid=p.pid,starttime=Path('/proc/'+str(p.pid)+'/stat').read_text().split(') ',1)[1].split()[19])))
'''
identity=json.loads(execute(SERVER,['python3','-c',start,host]))
save(BASE/'transport-probe-server-process.json',identity)
time.sleep(3)
log=execute(CLIENT,['python3','/work/transport_probe.py','client','--host',host,'--output','/work/transport-probe-client.json'],timeout=300)
(BASE/'transport-probe-client.log').write_bytes(log)
print(log.decode(),flush=True)
for pod,role in [(CLIENT,'client'),(SERVER,'server')]:
    raw=execute(pod,['cat',f'/work/transport-probe-{role}.json'])
    (BASE/f'transport-probe-{role}.json').write_bytes(raw)
    result=json.loads(raw)
    assert result['complete'] and len(result['rows'])==4
    assert all(f['full_sha256_verified'] for row in result['rows'] for f in row['flows'])
verify='''import json,sys
from pathlib import Path
p=Path('/proc/'+sys.argv[1]+'/stat')
if p.exists():
 f=p.read_text().split(') ',1)[1].split();assert f[19]!=sys.argv[2] or f[0]=='Z'
print(json.dumps(dict(server_exited=True)))
'''
execute(SERVER,['python3','-c',verify,str(identity['pid']),identity['starttime']])
save(BASE/'transport-probe-closure.json',dict(success=True,full_stream_hash_checks=10,total_bytes=8*1024**3,server_exited=True))

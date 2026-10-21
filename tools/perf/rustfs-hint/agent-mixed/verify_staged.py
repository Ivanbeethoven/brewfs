"""Read back the exact deployed tools, workload files and container identities."""
import argparse
import hashlib
import json
from cloud import BASE, WORK, CLIENT, SERVER, execute, save

p=argparse.ArgumentParser();p.add_argument('stage',choices=['smoke','formal']);stage=p.parse_args().stage
plan=json.loads((BASE/(stage+'-plan.json')).read_text())
snapshot=BASE/(stage+'-tools');snapshot.mkdir(exist_ok=False)
for name,digest in plan['tools'].items():
    raw=(WORK/name).read_bytes();assert hashlib.sha256(raw).hexdigest()==digest
    (snapshot/name).write_bytes(raw)
(snapshot/'verify_staged.py').write_bytes((WORK/'verify_staged.py').read_bytes())
checks=[]
for pod,names in [(CLIENT,['run_core.py','run_client.py','run_agent_io.py']),
                  (SERVER,['server_control.py','sample_resources.py','audit_policy_layout.py'])]:
    for name in names:
        actual=execute(pod,['sha256sum','/work/'+name]).decode().split()[0]
        assert actual==plan['tools'][name]
        checks.append(dict(pod=pod,file=name,sha256=actual))
for path,expected in {(j[k],j[h]) for j in plan['jobs'] for k,h in [('trace_path','trace_sha256'),('corpus_path','corpus_sha256')]}:
    actual=execute(CLIENT,['sha256sum',path]).decode().split()[0]
    assert actual==expected
    checks.append(dict(pod=CLIENT,file=path,sha256=actual))
execute(CLIENT,['python3','-m','py_compile','/work/run_agent_io.py','/work/run_client.py','/work/run_core.py'])
save(BASE/(stage+'-staged-verification.json'),dict(passed=True,checks=checks))
print(json.dumps(dict(passed=True,verified_files=len(checks))),flush=True)

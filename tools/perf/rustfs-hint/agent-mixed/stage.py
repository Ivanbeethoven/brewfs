"""Stage fixed binaries and trace files once Pods have initialized."""
from pathlib import Path
import hashlib
import json
import subprocess
import tarfile
from cloud import BASE,WORK,OUT,K,CLIENT,SERVER,LABEL,command,execute,put,save,load_binaries

manifest=load_binaries()
paths={name:Path(item['path']) for name,item in manifest.items()}
expected={name:item['sha256'] for name,item in manifest.items()}
protocol=json.loads((OUT/'experiment-plan.json').read_text())
assert expected['brewfs']==protocol['brewfs_sha256'] and expected['candidate']==protocol['rustfs_sha256']
assert all(hashlib.file_digest(p.open('rb'),'sha256').hexdigest()==expected[n] for n,p in paths.items())
names=['run_core.py','run_client.py','run_agent_io.py','server_control.py','sample_resources.py','audit_policy_layout.py','transport_probe.py']
for pod,binaries in [(CLIENT,['brewfs']),(SERVER,['candidate','compare_layout'])]:
    execute(pod,['test','-f','/work/ready'])
    archive=BASE/(pod+'-inputs.tar.gz')
    with tarfile.open(archive,'w:gz',compresslevel=1) as tar:
        for name in names:
            tar.add(WORK/name,arcname=name)
        for name in binaries:
            tar.add(paths[name],arcname=name)
        if pod==CLIENT:
            for name in ['corpus.json','smoke-corpus.json','smoke-trace.jsonl']:
                tar.add(BASE/name,arcname=name)
            tar.add(OUT/'traces',arcname='traces')
    with archive.open('rb') as source:
        subprocess.run(K+['exec','-i',pod,'--','tar','-xzf','-','-C','/work'],stdin=source,check=True,timeout=600)
    for name in binaries:
        assert execute(pod,['sha256sum','/work/'+name]).decode().split()[0]==expected[name]
put(CLIENT,'/work/private-auth.json',(BASE/'.private-auth.json').read_bytes())
execute(CLIENT,['chmod','600','/work/private-auth.json'])
resources=json.loads(command(['get','pods,pvc,svc','-l',LABEL,'-o','json']))
services={r['metadata']['name']:r['spec']['clusterIP'] for r in resources['items'] if r['kind']=='Service'}
volumes=[json.loads(command(['get','pv',r['spec']['volumeName'],'-o','json'])) for r in resources['items'] if r['kind']=='PersistentVolumeClaim']
assert len(volumes)==4
for pod in [r for r in resources['items'] if r['kind']=='Pod' and r['metadata']['name'] in [CLIENT,SERVER]]:
    client=pod['metadata']['name']==CLIENT
    assert pod['metadata']['labels']['alibabacloud.com/compute-class']==('general-purpose' if client else 'performance')
    requests=pod['spec']['containers'][0]['resources']['requests']
    assert requests['cpu']==('4' if client else '16') and requests['memory']==('8Gi' if client else '32Gi')
save(BASE/'topology.json',dict(resources=resources,services=services,volumes=volumes))
save(BASE/'binaries.json',dict(paths={n:str(p) for n,p in paths.items()},sha256=expected))
save(BASE/'execution-bindings.json',dict(
    protocol_sha256=hashlib.sha256((OUT/'experiment-plan.json').read_bytes()).hexdigest(),
    actual_topology_sha256=hashlib.sha256((BASE/'topology.json').read_bytes()).hexdigest(),
    actual=dict(client='general-purpose4CPU/8GiB',store='performance16CPU/32GiB',redis='general-purpose1CPU/2GiB'),
    reason='Dedicated FUSE-capable Pods; all arms use identical resources.',
    limits='Client nominal total ingress+egress1.5Gbit/s remains a potential limit; record complete tails/retransmissions and never attribute a resource difference to code.',
    payload='Version-specific pseudorandom first min(size,128KiB) bytes; shared pseudorandom remainder within each size. Compression disabled, no backend deduplication configured. All bytes written, no data generation inside timing.',
    telemetry_interval_seconds=.2,binaries=expected))
print(json.dumps(dict(staged=True,sha256=expected,disks=[v['spec']['csi']['volumeHandle'] for v in volumes])),flush=True)

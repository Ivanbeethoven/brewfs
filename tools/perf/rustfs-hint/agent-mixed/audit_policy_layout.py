"""Check every final data object on all four disks outside measurement."""
from pathlib import Path
import json
import subprocess
import sys

ROOT = Path('/work')
record = json.loads((ROOT / 'active-server.json').read_text())
job = record['job']
objects = json.loads(Path(sys.argv[1]).read_text())
folder = ROOT / 'runs' / job['id']
quantum = job['arm']['environment'].get('RUSTFS_EC_READ_QUANTUM_ENABLE') == 'true'
count = 0
entries=[]
for obj in objects:
    expected_b = job["auto_ec_bytes"] if obj["bytes"] > 1024**2 else 1024**2
    expected_q = str(1024**2) if quantum and expected_b == 4*1024**2 else None
    for disk, volume in enumerate(record['volumes']):
        path = Path(volume['path']) / obj['bucket'] / obj['key'] / 'xl.meta'
        assert path.resolve().is_relative_to(Path(volume['path']).resolve())
        entries.append((obj,disk,path,expected_b,expected_q))
with (folder / 'layouts.jsonl').open('w') as log:
    for offset in range(0,len(entries),128):
        batch=entries[offset:offset+128]
        lines = subprocess.check_output(['/work/compare_layout', *[str(x[2]) for x in batch]], text=True, timeout=60).splitlines()
        assert len(lines)==len(batch)
        for (obj,disk,path,expected_b,expected_q),line in zip(batch,lines):
            decoded=json.loads(line)
            assert decoded['path']==str(path)
            fields={k:decoded[k] for k in ['size','data','parity','block']}
            assert fields == dict(size=obj['bytes'], data=2, parity=2, block=expected_b), (obj, fields)
            meta=decoded['metadata']
            q = [meta.get(prefix+'ec-read-quantum') for prefix in ['x-rustfs-internal-', 'x-minio-internal-']]
            assert q == [expected_q, expected_q], (obj, q, expected_q)
            log.write(json.dumps(dict(object=obj, disk=disk, fields=fields, quantum=q, decoded=decoded))+'\n')
            count += 1
(folder / 'layout-audit.json').write_text(json.dumps(dict(passed=True, objects=len(objects),
    metadata_records=count, disks=4, data_shards=2, parity_shards=2, quantum=quantum), indent=2))
print(json.dumps(dict(passed=True, job=job['id'], objects=len(objects), metadata_records=count)), flush=True)

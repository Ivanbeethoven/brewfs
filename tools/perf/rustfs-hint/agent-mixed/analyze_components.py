"""Explain complete measured operation timings without changing acceptance gates."""
import json
import numpy as np
from cloud import BASE, save

assert json.loads((BASE/'formal/closure.json').read_text())['success']
jobs=json.loads((BASE/'formal-plan.json').read_text())['jobs']
out=[]
for job in jobs:
    p=BASE/'formal/raw'/job['id']/'client'
    r=json.loads((p/'results.json').read_text())[0]
    records=[x for w in json.loads((p/'operations.json').read_text())['workers'] for x in w]
    selected=[x for x in records if x['op']!='read']
    assert all(sum(x['write_components'].values())<=x['end_ns']-x['start_ns'] for x in selected)
    components={}
    for name in ['open_ns','write_ns','fsync_ns','close_ns','rename_ns']:
        values=np.array([x['write_components'][name]/1e6 for x in selected])
        components[name.removesuffix('_ns')]=dict(sum_ms=float(values.sum()),p50_ms=float(np.median(values)),p95_ms=float(np.quantile(values,.95)))
    delta=r['counters_delta']
    keys=['brewfs_fuse_read_ops_total','brewfs_fuse_read_bytes_total','brewfs_fuse_write_ops_total','brewfs_fuse_write_bytes_total',
          'brewfs_s3_get_ops_total','brewfs_s3_get_bytes_total','brewfs_s3_put_ops_total','brewfs_s3_put_bytes_total']
    out.append(dict(job=job['id'],scene=job['scene'],round=job['round'],arm=job['arm']['id'],
                    save_components=components,counters={k:delta.get(k) for k in keys},
                    read_logical_bytes=sum(x['bytes'] for x in records if x['op']=='read'),
                    write_logical_bytes=sum(x['bytes'] for x in selected),
                    hash_process_cpu_seconds=r['verification_cpu_seconds'],task_wall_seconds=r['active_seconds']))
save(BASE/'formal-components.json',dict(rows=out,
    interpretation='Components are application syscall spans. Concurrent sums overlap and do not partition wall time. fsync can include staging, metadata and queue waits; it is not pure disk time. FUSE counters include any filesystem prefetch, S3 counters describe the observed whole phase, not a causal proof.'))
print('Complete36-run component and request-count breakdown saved.',flush=True)

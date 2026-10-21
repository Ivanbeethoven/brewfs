"""Read process attribution with whole-file correctness and lifetime checks."""
import json
import argparse
import math
import statistics
from cloud import BASE, save

parser=argparse.ArgumentParser();parser.add_argument('--candidate',action='store_true');candidate=parser.parse_args().candidate
prefix='body-stream-first-process' if candidate else 'first-process'
closed=json.loads((BASE/(prefix+'-closure.json')).read_text());assert closed['success']
root=BASE/(prefix+'-round1')/'raw'/(prefix+'-round1-'+('stream' if candidate else 'base'))
closure=json.loads((root/'client/closure.json').read_text())
assert closure['success'] and not closure['remaining_mounts'] and all(p['exit']==0 for p in closure['processes'])
assert json.loads((root/'server/layout-audit.json').read_text())['passed']
assert json.loads((root/'server/stop.json').read_text())['stopped']
rows=json.loads((root/'client/results.json').read_text());files=[];hashes=0

def io(s):return {k:int(v) for k,v in (line.split(':',1) for line in s.splitlines())}

for row in rows:
    for phase in ['client_cold_read','warm_read']:
        result=row['phases'][phase]
        assert result['full_file_hash_checks']==len(result['per_file'])==row['count']
        hashes+=row['count']
        for item in result['per_file']:
            a=item['brewfs_before'];b=item['brewfs_after']
            assert (a['pid'],a['starttime'],a['clock_ticks'])==(b['pid'],b['starttime'],b['clock_ticks'])
            common=set(a['threads'])&set(b['threads']);new=set(b['threads'])-set(a['threads']);exited=set(a['threads'])-set(b['threads'])
            delta={k:sum(b['threads'][t][k]-a['threads'][t][k] for t in common) for k in ['runtime_ns','runqueue_ns','slices']}
            assert all(v>=0 for v in delta.values())
            counts=item['counters_delta']
            assert counts['brewfs_s3_get_ops_total']==(math.ceil(row['size']/4194304) if phase=='client_cold_read' else 0)
            ia,ib=io(a['io']),io(b['io'])
            files.append(dict(size=row['size'],phase=phase,index=item['index'],wall_ms=item['seconds']*1000,
                brewfs_user_ms=(b['user_ticks']-a['user_ticks'])*1000/a['clock_ticks'],
                brewfs_system_ms=(b['system_ticks']-a['system_ticks'])*1000/a['clock_ticks'],
                brewfs_runtime_ms=delta['runtime_ns']/1e6,brewfs_runqueue_ms=delta['runqueue_ns']/1e6,
                brewfs_minor_faults=b['minor_faults']-a['minor_faults'],brewfs_major_faults=b['major_faults']-a['major_faults'],
                common_threads=len(common),new_threads=len(new),exited_threads=len(exited),
                brewfs_read_bytes=ib['read_bytes']-ia['read_bytes'],brewfs_write_bytes=ib['write_bytes']-ia['write_bytes'],
                get_sum_ms=counts['brewfs_s3_get_lat_us_total']/1000,
                fuse_read_sum_ms=counts['brewfs_fuse_read_lat_us_total']/1000,
                reader_cpu_ms=(item['process_user_seconds']+item['process_system_seconds'])*1000))

summary=[]
for row in rows:
    for phase in ['client_cold_read','warm_read']:
        items=[f for f in files if f['size']==row['size'] and f['phase']==phase]
        keys=['wall_ms','get_sum_ms','brewfs_runtime_ms','brewfs_runqueue_ms','brewfs_user_ms','brewfs_system_ms','brewfs_minor_faults','brewfs_major_faults','brewfs_read_bytes','brewfs_write_bytes']
        summary.append(dict(size=row['size'],phase=phase,first={k:items[0][k] for k in keys},later_median={k:statistics.median(i[k] for i in items[1:]) for k in keys}))
assert hashes==128
save(BASE/(prefix+'-analysis.json'),dict(passed_data_audit=True,groups=len(rows),full_file_hash_checks=hashes,files=files,summary=summary,
    scope='Diagnostic only, 150ms between-file metric settling. BrewFS CPU/faults from process aggregate stat; runtime/runqueue from common threads only, with lifecycle counts retained. Process work can overlap service/wall time; these are not additive serial phases. Python reader CPU includes before-read /proc snapshot overhead.'))
for s in summary:
    if s['phase']=='client_cold_read':print(json.dumps(s),flush=True)

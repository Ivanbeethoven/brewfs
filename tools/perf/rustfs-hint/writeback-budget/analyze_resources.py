"""Summarize container cache and pressure; preserve sample scope and cost."""
import json
import statistics
import sys
from cloud import BASE,save

stage=sys.argv[1];closed=json.loads((BASE/stage/'closure.json').read_text());assert closed['success']
results=[]
def pairs(value):
    return {line.split()[0]:int(line.split()[1]) for line in value.splitlines() if len(line.split())==2 and line.split()[1].isdigit()}
for item in closed['completed']:
    folder=BASE/stage/'raw'/item['job']
    rows=json.loads((folder/'client/results.json').read_text())
    samples=[json.loads(l) for l in (folder/'server/resources.jsonl').read_text().splitlines()]
    assert (folder/'server/sampler-closed.json').exists() and len(samples)>2
    def values(s):
        v=s['values'];m=pairs(v.get('cgroup/memory.stat',v.get('cgroup/memory/memory.stat','')))
        return dict(memory_bytes=int(v.get('cgroup/memory.current',v.get('cgroup/memory/memory.usage_in_bytes','0'))),
                    file_bytes=m.get('file',m.get('total_cache',m.get('cache',0))),
                    anonymous_bytes=m.get('anon',m.get('total_rss',m.get('rss',0))),
                    active_file=m.get('active_file',m.get('total_active_file',0)),
                    inactive_file=m.get('inactive_file',m.get('total_inactive_file',0)),
                    pgmajfault=m.get('pgmajfault',m.get('total_pgmajfault',0)))
    phases=[]
    for row in rows:
        for name,phase in dict(write=row['write'],**row['phases']).items():
            start,end=phase['phase_start_utc_ns'],phase['phase_end_utc_ns']
            selected=[s for s in samples if start<=s['utc_ns']<=end]
            stats=[values(s) for s in selected]
            phases.append(dict(case=row['number'],kind=row['kind'],size=row['size'],concurrency=row['concurrency'],phase=name,
                samples=len(stats),utc_start=start,utc_end=end,
                peak={k:max(s[k] for s in stats) for k in stats[0]} if stats else {},
                first=stats[0] if stats else None,last=stats[-1] if stats else None))
    results.append(dict(job=item['job'],arm=rows[0]['arm'],round=rows[0]['round'],samples=len(samples),
        sample_seconds_median=statistics.median(s['sample_seconds'] for s in samples),
        sample_seconds_max=max(s['sample_seconds'] for s in samples),
        memory_start=values(samples[0]),memory_end=values(samples[-1]),
        pressure_scope=[n for n in samples[0]['values'] if 'pressure' in n],
        pressure_before={n:v for n,v in samples[0]['values'].items() if 'pressure' in n},
        pressure_after={n:v for n,v in samples[-1]['values'].items() if 'pressure' in n},phases=phases))
save(BASE/(stage+'-resources.json'),results)
for r in results:
    print(json.dumps({k:v for k,v in r.items() if k not in ['phases','pressure_before','pressure_after']}))

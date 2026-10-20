"""Compare complete paired blocks; never combine screen and formal rounds."""
import itertools
import json
import math
import statistics
import sys
import numpy as np
from cloud import BASE,save

stage=sys.argv[1]
closed=json.loads((BASE/stage/'closure.json').read_text());assert closed['success']
plan=json.loads((BASE/(stage+'-plan.json')).read_text())
rows=[]
for j in closed['completed']:rows.extend(json.loads((BASE/stage/'raw'/j['job']/'client/results.json').read_text()))
assert len(rows)==plan['groups']
groups={}
for row in rows:
    phases=['warm_read'] if row['kind']=='resident' else ['mixed'] if row['mixed'] else ['write','client_cold_read','warm_read']
    for phase in phases:
        value=row['write'] if phase=='write' else row['phases'][phase]
        groups.setdefault((row['kind'],row['size'],row['concurrency'],phase,row['arm']),[]).append((row['round'],value))

def paired(values):
    assert all(math.isfinite(v) and v>0 for v in values)
    logs=np.log(values);n=len(values)
    draws=[math.exp(float(np.mean(logs[list(i)]))) for i in itertools.product(range(n),repeat=n)]
    return float(np.exp(np.mean(logs))),np.percentile(draws,[2.5,97.5]).tolist()

summary=[]
for (kind,size,c,phase,arm),entries in sorted(groups.items()):
    entries.sort();assert [r for r,_ in entries]==list(range(1,plan['rounds']+1))
    vals=[v for _,v in entries]
    rates=[v['upload_mib_s'] if phase in ['write','mixed'] else v['active_mib_s'] for v in vals]
    summary.append(dict(kind=kind,size=size,concurrency=c,phase=phase,arm=arm,rates=rates,mib_s=statistics.median(rates),
        p95=[float(np.percentile(v['file_latencies_seconds'],95))*1000 for v in vals],
        strict=[v['total_mib_s'] for v in vals],active=[v['active_mib_s'] for v in vals],
        gets=[v['settled_counters_delta']['brewfs_s3_get_ops_total'] for v in vals],
        puts=[v['settled_counters_delta']['brewfs_s3_put_ops_total'] for v in vals],
        upload_seconds=[v.get('upload_seconds') for v in vals],active_seconds=[v['active_seconds'] for v in vals]))
comparisons=[]
for pair,a in [(pair,a) for pair in plan['pairs'] for a in summary if a['arm']==pair['baseline']]:
    b=next(b for b in summary if (b['kind'],b['size'],b['concurrency'],b['phase'],b['arm'])==(a['kind'],a['size'],a['concurrency'],a['phase'],pair['candidate']))
    ratio,ci=paired([y/x for x,y in zip(a['rates'],b['rates'])])
    p95,pci=paired([y/x for x,y in zip(a['p95'],b['p95'])])
    strict,sci=paired([y/x for x,y in zip(a['strict'],b['strict'])])
    comparisons.append(dict(pair=pair,kind=a['kind'],size=a['size'],concurrency=a['concurrency'],phase=a['phase'],
        before=a['mib_s'],after=b['mib_s'],ratio=ratio,ci=ci,before_p95=statistics.median(a['p95']),after_p95=statistics.median(b['p95']),p95_ratio=p95,p95_ci=pci,
        strict_ratio=strict,strict_ci=sci,passed=ci[0]>=.95 and pci[1]<=1.05,clear_gain=ratio>=1.05 and ci[0]>1,
        clear_regression=ci[1]<.95 or pci[0]>1.05,before_gets=a['gets'],after_gets=b['gets']))
save(BASE/(stage+'-analysis.json'),dict(stage=stage,groups=len(rows),summary=summary,comparisons=comparisons,
    full_file_hash_checks=sum(p.get('full_file_hash_checks',0) for r in rows for p in r['phases'].values())))
for c in comparisons:
    print(f'{c["pair"]["baseline"]}->{c["pair"]["candidate"]} {c["kind"]:8s} {c["phase"]:16s} {c["size"]:9d} c{c["concurrency"]}: ratio={c["ratio"]:.3f}; CI={c["ci"]}; p95={c["p95_ratio"]:.3f}; CI={c["p95_ci"]}; pass={c["passed"]}; regression={c["clear_regression"]}')

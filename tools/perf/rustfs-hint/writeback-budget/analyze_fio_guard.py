"""Use canonical artifact metrics and matched rounds, including close/drain costs."""
import csv
import argparse
import importlib.util
import itertools
import json
import math
import statistics
import sys
import numpy as np
from cloud import BASE,WORK,save

path=WORK.parents[1]/'compare_artifacts.py'
spec=importlib.util.spec_from_file_location('canonical_compare',path)
module=importlib.util.module_from_spec(spec);sys.modules[spec.name]=module;spec.loader.exec_module(module)
parser=argparse.ArgumentParser();parser.add_argument('--stage',default='tiny-fio-guard');stage=parser.parse_args().stage
plan=json.loads((BASE/(stage+'-plan.json')).read_text())
assert json.loads((BASE/stage/'closure.json').read_text())['success']
expected={tool+'-direct'+str(d) for tool in plan['tools'] for d in plan['direct']}
samples={};summaries=[]
for job in plan['jobs']:
    root=BASE/stage/'raw'/job['id'];artifact=root/'client/artifacts'
    assert json.loads((root/'client/closure.json').read_text())['success']
    assert json.loads((root/'server/layout-audit.json').read_text())['passed']
    assert json.loads((root/'server/stop.json').read_text())['stopped']
    with (artifact/'perf-summary.tsv').open() as f:rows=list(csv.DictReader(f,delimiter='\t'))
    assert {r['tool'] for r in rows}==expected and len(rows)==6 and all(r['status']=='pass' for r in rows)
    files=module.iter_fio_paths(artifact);assert {p.stem for p in files}==expected
    for p in files:
        data=json.loads(p.read_text());assert all(j['error']==0 for j in data['jobs'])
        assert int(module.first_options(data['jobs'])['direct'])==int(p.stem[-1])
    metrics=module.load_artifact(artifact)
    for item in expected:
        for field in ['post_write_drain_timeout','drain_pending_mib','drain_dirty_mib','drain_buffer_dirty_mib']:
            assert metrics[('drain',item,field)].value==0
        assert ('runtime',item,'write_effective_wall_bw_mib_s') in metrics
        assert ('runtime',item,'write_effective_active_plus_drain_bw_mib_s') in metrics
    for key,m in metrics.items():
        if m.kind in ['fio','runtime'] and (m.name.endswith('_bw_mib_s') or m.name.endswith('_p95_ms')):
            assert isinstance(m.value,(float,int)) and math.isfinite(m.value) and m.value>0
            samples.setdefault(key,{}).setdefault(job['arm']['id'],{})[job['round']]=m.value
    summaries.append(dict(job=job['id'],metrics=[dict(kind=m.kind,item=m.item,name=m.name,value=m.value,unit=m.unit) for m in metrics.values()]))
comparisons=[]
for (kind,item,name),arms in sorted(samples.items()):
    a,b=arms['base'],arms[plan.get('candidate','tiny')];assert set(a)==set(b)=={1,2}
    logs=[math.log(b[r]/a[r]) for r in [1,2]]
    ratio=math.exp(statistics.mean(logs));draws=[math.exp(statistics.mean(logs[i] for i in ix)) for ix in itertools.product(range(2),repeat=2)]
    ci=np.percentile(draws,[2.5,97.5]).tolist();latency=name.endswith('_p95_ms')
    comparisons.append(dict(kind=kind,item=item,metric=name,before=statistics.median(a.values()),after=statistics.median(b.values()),
        ratio=ratio,ci=ci,passed=ci[1]<=1.05 if latency else ci[0]>=.95,baseline_rounds=a,candidate_rounds=b))
assert {r['item'] for r in comparisons}==expected
result=dict(comparisons=comparisons,summary=summaries,passed=all(r['passed'] for r in comparisons),
            note='Two matched AB/BA guard rounds, 512 MiB per job,20 s,4 MiB requests. Canonical compose runner; all latency, active BW, wall BW and active+drain BW comparisons retained.')
save(BASE/(stage+'-analysis.json'),result)
print(json.dumps(dict(passed=result['passed'],comparisons=len(comparisons),unpassed=[r for r in comparisons if not r['passed']])),flush=True)

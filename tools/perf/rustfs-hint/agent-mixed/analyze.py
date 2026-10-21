"""Paired-round analysis of the complete, unchanged three-scene workload."""
from collections import Counter
import hashlib
import json
from pathlib import Path
import numpy as np
from cloud import BASE, OUT, save


def main():
    plan=json.loads((BASE/'formal-plan.json').read_text())
    protocol=json.loads((OUT/'experiment-plan.json').read_text())
    closed=json.loads((BASE/'formal/closure.json').read_text())
    assert closed['success'] and len(closed['completed'])==len(plan['jobs'])==36
    absolute=[]
    for job in plan['jobs']:
        folder=BASE/'formal/raw'/job['id']/'client'
        closure=json.loads((folder/'closure.json').read_text())
        assert closure['success'] and not closure['remaining_mounts']
        result=json.loads((folder/'results.json').read_text())[0]
        record=json.loads((folder/'operations.json').read_text())
        rows=[r for worker in record['workers'] for r in worker]
        assert result['operations']==len(rows)==8000 and result['correctness']=='pass'
        trace_path=OUT/next(j['trace'] for j in protocol['jobs'] if j['round']==job['round'] and j['scene']==job['scene'] and j['arm']==job['arm']['id'])
        raw=trace_path.read_bytes();assert hashlib.sha256(raw).hexdigest()==job['trace_sha256']
        trace=[json.loads(l) for l in raw.splitlines()]
        assert [(r['worker'],r['index'],r['op'],r['bytes']) for r in rows]==[(r['worker'],r['index'],r['op'],r['bytes']) for r in trace]
        assert all(result['start_ns']<=r['start_ns']<=r['end_ns']<=r['checked_ns']<=result['active_end_ns'] for r in rows)
        assert result['full_read_hash_checks']==sum(r['full_read_hash_passed'] for r in rows)==sum(r['op']=='read' for r in trace)
        assert result['remount_full_file_hash_checks']==3000+sum(r['op']=='create' for r in trace)
        assert result['logical_bytes']==sum(r['bytes'] for r in rows)
        assert abs(result['total_seconds']-(result['drained_end_ns']-result['start_ns'])/1e9)<1e-9
        assert all(not any(s['values'].values()) for s in result['drain_samples'][-3:])
        for kind in ['read','save']:
            selected=[r for r in rows if (r['op']=='read')==(kind=='read')]
            for size in [None]+[s['bytes'] for s in protocol['sizes']]:
                subset=[r for r in selected if size is None or r['bytes']==size]
                latency=np.array([(r['end_ns']-r['start_ns'])/1e6 for r in subset])
                absolute.append(dict(scene=job['scene'],round=job['round'],arm=job['arm']['id'],
                    metric=kind+'_p95_ms',file_bytes=size,value=float(np.quantile(latency,.95)),
                    p50_ms=float(np.quantile(latency,.5)),operations=len(subset),logical_bytes=sum(r['bytes'] for r in subset)))
        for metric,value in [('task_seconds',result['active_seconds']),('remote_complete_seconds',result['total_seconds']),
                             ('effective_mib_s',result['logical_bytes']/1048576/result['total_seconds'])]:
            absolute.append(dict(scene=job['scene'],round=job['round'],arm=job['arm']['id'],metric=metric,file_bytes=None,value=value))
    settings=protocol['statistics']
    rng=np.random.default_rng(settings['bootstrap_seed'])
    indices=rng.integers(0,6,size=(settings['bootstrap_samples'],6))
    comparisons=[]
    groups=sorted({(r['scene'],r['metric'],r['file_bytes'] or 0) for r in absolute})
    for scene,metric,size in groups:
        selected=[r for r in absolute if r['scene']==scene and r['metric']==metric and (r['file_bytes'] or 0)==size]
        values={arm:np.array([next(r['value'] for r in selected if r['arm']==arm and r['round']==round_number)
                             for round_number in range(1,7)]) for arm in ['no-dynamic-block','dynamic-auto']}
        ratios=values['dynamic-auto']/values['no-dynamic-block']
        assert np.all(ratios>0)
        log=np.log(ratios);gm=float(np.exp(np.mean(log)))
        low,high=np.quantile(np.exp(log[indices].mean(axis=1)),[.025,.975])
        higher=metric=='effective_mib_s'
        passed=low>=.95 if higher else high<=1.05
        # Task wall time is reported; the frozen acceptance uses drain-inclusive
        # throughput and read/save p95. Remote seconds is the reciprocal check.
        required=metric=='effective_mib_s' or metric.endswith('_p95_ms')
        comparisons.append(dict(scene=scene,metric=metric,file_bytes=size or None,
            baseline_values=values['no-dynamic-block'].tolist(),dynamic_values=values['dynamic-auto'].tolist(),
            baseline_geomean=float(np.exp(np.log(values['no-dynamic-block']).mean())),
            dynamic_geomean=float(np.exp(np.log(values['dynamic-auto']).mean())),paired_ratios=ratios.tolist(),
            ratio=gm,ci_low=float(low),ci_high=float(high),higher_is_better=higher,
            required=required,no_regression=bool(passed),clear_gain=bool(higher and gm>=1.05 and low>1)))
    required=[r for r in comparisons if r['required']]
    primary=next(r for r in comparisons if r['scene']=='edit' and r['metric']=='effective_mib_s')
    verdict=dict(all_required_no_regression=all(r['no_regression'] for r in required),
                 primary_edit_clear_gain=primary['clear_gain'],required_comparisons=len(required),
                 failed_required=[{k:r[k] for k in ['scene','metric','file_bytes','ratio','ci_low','ci_high']} for r in required if not r['no_regression']])
    verdict['primary_passed']=verdict['all_required_no_regression'] and verdict['primary_edit_clear_gain']
    save(BASE/'formal-analysis.json',dict(absolute=absolute,comparisons=comparisons,verdict=verdict,
        statistical_unit='paired round;6 independent paired rounds, not pooled file operations',
        intervals='per-comparison paired bootstrap percentile95%; no familywise confidence claim'))
    print(json.dumps(verdict),flush=True)


if __name__=='__main__':
    main()

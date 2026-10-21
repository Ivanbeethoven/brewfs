"""Paired old/new code regression statistics, preserving the complete whole-file scope."""
import itertools
import json
import math
import statistics

import numpy as np

from cloud import BASE, save


def paired(ratios):
    assert len(ratios) == 4 and all(math.isfinite(v) and v > 0 for v in ratios)
    logs = np.log(ratios)
    draws = [math.exp(float(np.mean(logs[list(indices)]))) for indices in itertools.product(range(4), repeat=4)]
    return float(np.exp(logs.mean())), np.percentile(draws, [2.5, 97.5]).tolist()


def main():
    plan = json.loads((BASE / 'guard-plan.json').read_text())
    closed = json.loads((BASE / 'guards/closure.json').read_text())
    assert closed['success'] and len(closed['completed']) == len(plan['jobs']) == 8
    samples, original_rows = {}, []
    for job in plan['jobs']:
        root = BASE / 'guards/raw' / job['id'] / 'client'
        closure = json.loads((root / 'closure.json').read_text())
        assert closure['success'] and not closure['remaining_mounts'] and all(p['exit'] == 0 for p in closure['processes'])
        rows = json.loads((root / 'results.json').read_text())
        assert len(rows) == len(job['cases']) == 25
        for row, spec in zip(rows, job['cases'], strict=True):
            assert all(row[key] == spec[key] for key in ['size','count','concurrency','mixed','kind'])
            assert row['arm'] == job['arm']['id'] and row['job'] == job['id']
            assert row['correctness'] == row['unmount'] == 'pass'
            phases = ['warm_read'] if row['kind'] == 'resident' else ['mixed'] if row['mixed'] else ['write','client_cold_read','warm_read']
            for phase in phases:
                result = row['write'] if phase == 'write' else row['phases'][phase]
                repeats = (4 if row['size'] <= 65536 else 16 if row['size'] <= 1048576 else 32) if row['kind'] == 'resident' else 1
                assert result['files'] == row['count'] * repeats == len(result['file_latencies_seconds'])
                assert result['logical_bytes'] == row['size'] * result['files']
                if phase not in ['write','mixed']:
                    assert result['full_file_hash_checks'] == result['files']
                else:
                    assert len(result['drain_samples']) >= 3
                    assert all(not any(s['values'].values()) for s in result['drain_samples'][-3:])
                if row['kind'] == 'resident':
                    assert result['settled_counters_delta']['brewfs_fuse_read_ops_total'] > 0
                    assert result['settled_counters_delta']['brewfs_s3_get_ops_total'] == 0
                key = (row['kind'], row['size'], row['concurrency'], phase, row['arm'])
                entries = samples.setdefault(key, {})
                assert job['round'] not in entries
                entries[job['round']] = dict(
                    throughput=result['upload_mib_s'] if phase in ['write','mixed'] else result['active_mib_s'],
                    p95_ms=float(np.percentile(result['file_latencies_seconds'],95))*1000,
                    strict_throughput=result['total_mib_s'])
            original_rows.append(row)
    summaries = []
    for key, entries in sorted(samples.items()):
        assert sorted(entries) == [1,2,3,4]
        summaries.append(dict(kind=key[0],size=key[1],concurrency=key[2],phase=key[3],arm=key[4],rounds=entries))
    comparisons = []
    for key in sorted(samples):
        kind, size, concurrency, phase, arm = key
        if arm != plan['reference']:
            continue
        a, b = samples[key], samples[(kind,size,concurrency,phase,plan['candidate'])]
        metrics = {}
        for field in ['throughput','p95_ms','strict_throughput']:
            before, after = [a[r][field] for r in range(1,5)], [b[r][field] for r in range(1,5)]
            ratio, ci = paired([y/x for x,y in zip(before,after)])
            metrics[field] = dict(before=statistics.median(before),after=statistics.median(after),
                baseline_values=before,candidate_values=after,ratio=ratio,ci=ci,
                passed=ci[1]<=1.05 if field=='p95_ms' else ci[0]>=.95)
        comparisons.append(dict(kind=kind,size=size,concurrency=concurrency,phase=phase,metrics=metrics,
            passed=all(m['passed'] for m in metrics.values())))
    assert len(comparisons) == 57 and len(original_rows) == 200
    save(BASE / 'guard-analysis.json', dict(passed=all(r['passed'] for r in comparisons),comparisons=comparisons,
        summaries=summaries,groups=len(original_rows),
        full_file_hash_checks=sum(p.get('full_file_hash_checks',0) for r in original_rows for p in r['phases'].values()),
        scope='Four paired rounds, complete original 17 normal + 8 resident file cases. 64MiB phases extended to 2GiB. Separate code-regression appendix, not the main hint ablation.'))
    print(json.dumps(dict(passed=all(r['passed'] for r in comparisons),comparisons=len(comparisons),
        failed=[r for r in comparisons if not r['passed']])),flush=True)


if __name__ == '__main__':
    main()

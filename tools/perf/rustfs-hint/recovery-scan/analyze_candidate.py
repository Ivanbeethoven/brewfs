"""Analyze all frozen paired rounds; keep runtime ablation and code regression separate."""
import hashlib
import json

import numpy as np

from cloud import BASE, OUT, save


def main():
    plan = json.loads((BASE / 'formal-plan.json').read_text())
    protocol = json.loads((OUT / 'experiment-plan.json').read_text())
    closed = json.loads((BASE / 'formal/closure.json').read_text())
    assert closed['success'] and len(closed['completed']) == len(plan['jobs']) == 54
    assert hashlib.sha256((OUT / 'experiment-plan.json').read_bytes()).hexdigest() == plan['protocol_sha256']
    absolute = []
    for job in plan['jobs']:
        folder = BASE / 'formal/raw' / job['id'] / 'client'
        closure = json.loads((folder / 'closure.json').read_text())
        assert closure['success'] and not closure['remaining_mounts']
        result = json.loads((folder / 'results.json').read_text())[0]
        rows = [r for worker in json.loads((folder / 'operations.json').read_text())['workers'] for r in worker]
        assert result['operations'] == len(rows) == 8000 and result['correctness'] == 'pass'
        source = OUT / next(j['trace'] for j in protocol['jobs'] if
                            (j['round'], j['scene']) == (job['round'], job['scene']))
        raw = source.read_bytes()
        assert hashlib.sha256(raw).hexdigest() == job['trace_sha256']
        trace = [json.loads(line) for line in raw.splitlines()]
        fields = ['worker', 'index', 'op', 'bytes']
        assert [[r[k] for k in fields] for r in rows] == [[r[k] for k in fields] for r in trace]
        assert all(result['start_ns'] <= r['start_ns'] <= r['end_ns'] <= r['checked_ns'] <= result['active_end_ns'] for r in rows)
        assert result['full_read_hash_checks'] == sum(r['full_read_hash_passed'] for r in rows) == sum(r['op'] == 'read' for r in trace)
        assert result['remount_full_file_hash_checks'] == 3000 + sum(r['op'] == 'create' for r in trace)
        assert result['logical_bytes'] == sum(r['bytes'] for r in rows)
        assert abs(result['total_seconds'] - (result['drained_end_ns'] - result['start_ns']) / 1e9) < 1e-9
        assert len(result['drain_samples']) >= 3 and all(not any(s['values'].values()) for s in result['drain_samples'][-3:])
        common = dict(scene=job['scene'], round=job['round'], arm=job['arm']['id'])
        for kind in ['read', 'save']:
            selected = [r for r in rows if (r['op'] == 'read') == (kind == 'read')]
            for size in [None] + [s['bytes'] for s in protocol['sizes']]:
                subset = [r for r in selected if size is None or r['bytes'] == size]
                latency = np.array([(r['end_ns'] - r['start_ns']) / 1e6 for r in subset])
                assert len(latency) and np.all(np.isfinite(latency)) and np.all(latency > 0)
                absolute.append(dict(common, metric=kind + '_p95_ms', file_bytes=size,
                    value=float(np.quantile(latency, .95)), p50_ms=float(np.quantile(latency, .5)),
                    operations=len(subset), logical_bytes=sum(r['bytes'] for r in subset)))
        for metric, value in [('task_seconds', result['active_seconds']),
                              ('remote_complete_seconds', result['total_seconds']),
                              ('effective_mib_s', result['logical_bytes'] / 1048576 / result['total_seconds'])]:
            absolute.append(dict(common, metric=metric, file_bytes=None, value=value))

    settings = protocol['statistics']
    assert settings == dict(unit='paired round', bootstrap_percentiles=[2.5, 97.5], bootstrap_samples=20000,
        bootstrap_seed=20261020, throughput_ratio_ci_lower=.95, p95_ratio_ci_upper=1.05,
        clear_gain_point_min=1.05, clear_gain_ci_lower_strict=1.0)
    indices = np.random.default_rng(settings['bootstrap_seed']).integers(0, 6, size=(settings['bootstrap_samples'], 6))
    groups = sorted({(r['scene'], r['metric'], r['file_bytes'] or 0) for r in absolute})
    assert len(groups) == 51
    comparisons, verdicts = [], {}
    for comparison in plan['comparisons']:
        measured = []
        for scene, metric, size in groups:
            selected = [r for r in absolute if (r['scene'], r['metric'], r['file_bytes'] or 0) == (scene, metric, size)]
            values = {}
            for role in ['reference', 'candidate']:
                arm = comparison[role]
                entries = [r for r in selected if r['arm'] == arm]
                assert sorted(r['round'] for r in entries) == list(range(1, 7))
                values[role] = np.array([next(r['value'] for r in entries if r['round'] == n) for n in range(1, 7)])
                assert np.all(np.isfinite(values[role])) and np.all(values[role] > 0)
            ratios = values['candidate'] / values['reference']
            log = np.log(ratios)
            ratio = float(np.exp(log.mean()))
            low, high = np.quantile(np.exp(log[indices].mean(axis=1)), [.025, .975])
            higher = metric == 'effective_mib_s'
            required = higher or metric.endswith('_p95_ms')
            measured.append(dict(comparison=comparison['id'], reference=comparison['reference'], candidate=comparison['candidate'],
                scene=scene, metric=metric, file_bytes=size or None,
                baseline_values=values['reference'].tolist(), dynamic_values=values['candidate'].tolist(),
                baseline_geomean=float(np.exp(np.log(values['reference']).mean())),
                dynamic_geomean=float(np.exp(np.log(values['candidate']).mean())),
                paired_ratios=ratios.tolist(), ratio=ratio, ci_low=float(low), ci_high=float(high),
                higher_is_better=higher, required=required,
                no_regression=bool(low >= .95 if higher else high <= 1.05),
                clear_gain=bool(higher and ratio >= 1.05 and low > 1)))
        required = [r for r in measured if r['required']]
        assert len(required) == 45
        verdicts[comparison['id']] = dict(required_comparisons=45,
            all_required_no_regression=all(r['no_regression'] for r in required),
            failed_required=[{k: r[k] for k in ['scene', 'metric', 'file_bytes', 'ratio', 'ci_low', 'ci_high']} for r in required if not r['no_regression']])
        comparisons.extend(measured)
    main_gain = next(r['clear_gain'] for r in comparisons if
        (r['comparison'], r['scene'], r['metric']) == ('dynamic-versus-off', 'edit', 'effective_mib_s'))
    repair_gain = any(r['clear_gain'] for r in comparisons if
        r['comparison'] == 'repair-versus-previous' and r['scene'] in ['edit', 'generate'] and r['metric'] == 'effective_mib_s')
    verdict = dict(comparisons=verdicts, main_edit_clear_gain=main_gain, repair_benefit=repair_gain,
        mixed_performance_passed=all(v['all_required_no_regression'] for v in verdicts.values()) and main_gain and repair_gain,
        product_accepted=False,
        remaining_acceptance='Independent raw/export audit, applicable whole-file and compose regression guards, and current CI gates')
    save(BASE / 'formal-analysis.json', dict(absolute=absolute, comparisons=comparisons, verdict=verdict,
        statistical_unit='Six paired rounds per comparison; never pool file operations as independent runs',
        intervals='Per-comparison paired bootstrap percentile 95%; no familywise confidence claim',
        limits='Runtime hint ablation; BrewFS data blocks stay 4 MiB. Physical EC layout audited independently.'))
    print(json.dumps(verdict), flush=True)


if __name__ == '__main__':
    main()

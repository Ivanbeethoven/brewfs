"""Inspect complete campaigns only, before deciding whether to integrate."""
import json
import sys
from cloud import BASE, save
from report import summarize, comparisons, label

stage=sys.argv[1]
assert stage in ['acceptance','hot_acceptance','policy_acceptance']
closed=json.loads((BASE/stage/'closure.json').read_text())
assert closed['success']
rows=[]
for job in closed['completed']:
    rows.extend(json.loads((BASE/stage/'raw'/job['job']/'client/results.json').read_text()))
resident=stage=='hot_acceptance'
assert len(rows)=={'acceptance':184,'hot_acceptance':64,'policy_acceptance':88}[stage]
summary=summarize(rows,resident)
pairs=[('before','after')] if resident else [('current','final-auto')] if stage=='policy_acceptance' else [('current','optimized'),('original','optimized'),('fixed-1m','optimized')]
contrasts=comparisons(summary,pairs)
save(BASE/(stage+'-descriptive.json'),dict(summary=summary,comparisons=contrasts))
for baseline,_ in pairs:
    for phase in sorted({x['phase'] for x in contrasts}):
        cells=[x for x in contrasts if (x['baseline'],x['phase'])==(baseline,phase)]
        print(json.dumps(dict(baseline=baseline,phase=phase,total=len(cells),
              passed=sum(x['passed'] for x in cells),clear_gains=sum(x['clear_gain'] for x in cells),
              clear_regressions=sum(x['clear_regression'] for x in cells),
              ratio_min=min(x['ratio'] for x in cells),ratio_max=max(x['ratio'] for x in cells))))
    for x in contrasts:
        if x['baseline']!=baseline:continue
        print(f'{baseline:9s} {x["phase"]:16s} {label(x["size"]):16s} c{x["concurrency"]} '
              f'{x["before"]:.1f}->{x["after"]:.1f} ratio={x["ratio"]:.3f} '
              f'CI={x["ci"][0]:.3f},{x["ci"][1]:.3f} '
              f'p95CI={x["p95_ci"][0]:.3f},{x["p95_ci"][1]:.3f} '
              f'strictCI={x["strict_ci"][0]:.3f},{x["strict_ci"][1]:.3f} pass={x["passed"]}')

"""Derive stage means from cumulative counters; never add enclosing timers."""
import json
import statistics
from cloud import BASE,save

rows=[]
for job in json.loads((BASE/'causal-plan.json').read_text())['causal']:
    for row in json.loads((BASE/'causal/raw'/job['id']/'client/results.json').read_text()):
        if row['kind']!='normal':continue
        for phase in ['write','client_cold_read']:
            v=row['write'] if phase=='write' else row['phases'][phase];c=v['settled_counters_delta']
            def avg(total,ops):return c.get(total,0)/c[ops]/1000 if c.get(ops) else None
            record=dict(arm=row['arm'],round=row['round'],size=row['size'],phase=phase,
                file_mean_ms=statistics.mean(v['file_latencies_seconds'])*1000,
                stage_mean_ms=avg('brewfs_writeback_stage_lat_us_total','brewfs_writeback_stage_ops_total'),
                enclosing_flush_mean_ms=avg('brewfs_writeback_flush_wait_us_total','brewfs_writeback_flush_wait_ops_total'),
                put_mean_ms=avg('brewfs_s3_put_lat_us_total','brewfs_s3_put_ops_total'),
                put_prepare_mean_ms=avg('brewfs_s3_put_prepare_lat_us_total','brewfs_s3_put_ops_total'),
                get_mean_ms=avg('brewfs_s3_get_lat_us_total','brewfs_s3_get_ops_total'),
                puts=c.get('brewfs_s3_put_ops_total'),gets=c.get('brewfs_s3_get_ops_total'),
                active_seconds=v['active_seconds'],upload_seconds=v.get('upload_seconds'),
                hard_backpressure_us=c.get('brewfs_writeback_backpressure_hard_wait_us',0))
            rows.append(record)
save(BASE/'causal-path-metrics.json',dict(rows=rows,scope='Means reconstructed from cumulative deltas. Stage includes persistence/seal/metadata and is enclosed by flush. S3 latency excludes upload permit waiting; sums across concurrent operations are not wall time.'))
for arm in ['new-three','size-default']:
    for size in [65536,262144,67108864]:
        rs=[r for r in rows if r['arm']==arm and r['size']==size and r['phase']=='write']
        print(json.dumps(dict(arm=arm,size=size,**{k:round(statistics.mean(r[k] for r in rs),3) for k in ['file_mean_ms','stage_mean_ms','enclosing_flush_mean_ms','put_mean_ms','put_prepare_mean_ms','active_seconds','upload_seconds','hard_backpressure_us']})))

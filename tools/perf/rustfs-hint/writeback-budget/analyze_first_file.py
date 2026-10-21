"""Attribute first-file costs without turning diagnostic pauses into benchmark wins."""
import hashlib
import json
import math
import statistics
from cloud import BASE,save

closure=json.loads((BASE/'first-file-closure.json').read_text());assert closure['success']
summary=[];files=[];hashes=0
for stage in closure['stages']:
    plan=json.loads((BASE/(stage+'-plan.json')).read_text());job=plan[stage][0];root=BASE/stage/'raw'/job['id']
    assert json.loads((root/'client/closure.json').read_text())['success']
    assert json.loads((root/'server/layout-audit.json').read_text())['passed']
    assert json.loads((root/'server/stop.json').read_text())['stopped']
    rows=json.loads((root/'client/results.json').read_text());assert len(rows)==3
    for row in rows:
        for phase in ['client_cold_read','warm_read']:
            result=row['phases'][phase];per=result['per_file'];assert len(per)==row['count']
            assert result['full_file_hash_checks']==row['count'];hashes+=row['count']
            normalized=[]
            for index,item in enumerate(per):
                assert item['index']==index
                d=item['counters_delta'];gets=d['brewfs_s3_get_ops_total']
                expected=math.ceil(row['size']/4194304) if phase=='client_cold_read' else 0
                assert gets==expected,(stage,row['size'],phase,index,gets,expected)
                entry=dict(stage=stage,size=row['size'],phase=phase,index=index,milliseconds=item['seconds']*1000,
                    gets=gets,get_latency_sum_ms=d['brewfs_s3_get_lat_us_total']/1000,
                    reader_cpu_ms=(item['process_user_seconds']+item['process_system_seconds'])*1000,
                    reader_minor_faults=item['minor_faults'],reader_major_faults=item['major_faults'])
                normalized.append(entry);files.append(entry)
            first=normalized[0];rest=normalized[1:]
            summary.append(dict(stage=stage,size=row['size'],phase=phase,files=len(per),
                first={k:first[k] for k in ['milliseconds','get_latency_sum_ms','reader_cpu_ms','reader_minor_faults']},
                later_median={k:statistics.median(r[k] for r in rest) for k in ['milliseconds','get_latency_sum_ms','reader_cpu_ms','reader_minor_faults']}))
assert hashes==256
save(BASE/'first-file-analysis.json',dict(passed_data_audit=True,groups=6,full_file_hash_checks=hashes,summary=summary,files=files,
    scope='Attribution only.150ms settling pauses and SHA256 outside each file interval may change connection idle state and background writeback. S3 latency is summed over requests that may overlap; do not subtract it from file wall time. Reader CPU is the Python caller, not all BrewFS/RustFS CPU. Both baseline jobs start after deleting prior owned, exported payloads.'))
for row in summary:
    if row['phase']=='client_cold_read':print(json.dumps(row),flush=True)

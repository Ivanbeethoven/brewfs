"""Read every block attribution sample and preserve the changed application shape."""
import json
import statistics
from cloud import BASE,save

closed=json.loads((BASE/'first-block-closure.json').read_text());assert closed['success']
root=BASE/'first-block-round1/raw/first-block-round1-base'
assert json.loads((root/'client/closure.json').read_text())['success']
assert json.loads((root/'server/layout-audit.json').read_text())['passed']
assert json.loads((root/'server/stop.json').read_text())['stopped']
row=json.loads((root/'client/results.json').read_text())[0];records=[]
for phase in ['client_cold_read','warm_read']:
    data=row['phases'][phase];assert data['full_file_hash_checks']==8
    for file in data['per_file']:
        assert file['counters_delta']['brewfs_s3_get_ops_total']==(16 if phase=='client_cold_read' else 0)
        assert len(file['blocks'])==16 and all(b['bytes']==4194304 and b['offset']==i*4194304 for i,b in enumerate(file['blocks']))
        records.append(dict(phase=phase,file=file['index'],milliseconds=file['seconds']*1000,
            get_latency_sum_ms=file['counters_delta']['brewfs_s3_get_lat_us_total']/1000,
            blocks_ms=[b['milliseconds'] for b in file['blocks']]))
save(BASE/'first-block-analysis.json',dict(passed_data_audit=True,full_file_hash_checks=16,records=records,
    scope='Diagnostic only: eight complete64MiB files, each read in sixteen whole4MiB blocks. No pause between blocks;150ms counter settling between files. Application request shape differs from acceptance.'))
for r in records:
    if r['phase']=='client_cold_read':print(json.dumps(r),flush=True)

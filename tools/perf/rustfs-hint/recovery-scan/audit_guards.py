"""Verify exported files, physical layouts, exact phases, mounts and FUSE cache paths."""
import hashlib
import json
import os
from pathlib import Path

from cloud import BASE, WORK, save


def read(path):
    return json.loads(path.read_text())


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream,'sha256').hexdigest()


def main():
    plan = read(BASE/'guard-plan.json')
    closure = read(BASE/'guards/closure.json')
    assert closure['success'] and len(closure['completed'])==len(plan['jobs'])==8
    assert sha(BASE/'guard-plan.json')==read(BASE/'guards/started.json')['plan_sha256']
    assert sha(BASE/'guard-scope.json')==plan['scope_sha256']
    for name,digest in plan['tools'].items():
        recorded = Path(os.environ.get('BENCH_RECORDED_TOOLS', str(WORK)))
        assert sha(recorded/name)==sha(BASE/'guard-tools'/name)==digest
    totals = dict(jobs=8,cases=0,layout_records=0,full_file_hash_checks=0,clean_mount_exits=0,files=0)
    for job in plan['jobs']:
        root = BASE/'guards/raw'/job['id']
        exported = read(BASE/'guards'/(job['id']+'-export.json'))
        assert exported['completed']
        paths = set()
        for item in exported['files']:
            path = BASE/'guards'/item['path']
            assert path.resolve().is_relative_to(root.resolve())
            assert path.stat().st_size==item['bytes'] and sha(path)==item['sha256']
            paths.add(path)
        assert paths=={p for p in root.rglob('*') if p.is_file()}
        totals['files']+=len(paths)
        client,server = root/'client',root/'server'
        assert read(client/'protocol.json')==job==read(server/'start.json')['job']
        assert read(server/'stop.json')['stopped'] and read(server/'layout-audit.json')['passed']
        final = read(client/'closure.json')
        assert final['success'] and not final['remaining_mounts'] and all(p['exit']==0 for p in final['processes'])
        totals['clean_mount_exits']+=len(final['processes'])
        rows = read(client/'results.json')
        assert len(rows)==len(job['cases'])==25
        for i,(row,spec) in enumerate(zip(rows,job['cases'],strict=True)):
            assert row==read(client/f'case-{i:03d}/result.json')
            assert all(row[k]==spec[k] for k in ['size','count','concurrency','mixed','kind'])
            assert len(row['expected_sha256'])==row['count'] and all(len(h)==64 for h in row['expected_sha256'])
            assert row['correctness']==row['unmount']=='pass'
            for phase,result in [('write',row['write']),*row['phases'].items()]:
                if phase in ['write','mixed']:
                    assert len(result['drain_samples'])>=3 and all(not any(s['values'].values()) for s in result['drain_samples'][-3:])
                else:
                    assert result['full_file_hash_checks']==result['files']
                assert result['logical_bytes']==row['size']*result['files']
                assert len(result['file_latencies_seconds'])==result['files']
                if row['kind']=='resident' and phase=='warm_read':
                    repetitions=4 if row['size']<=65536 else 16 if row['size']<=1048576 else 32
                    assert result['repetitions']==repetitions
                    assert len(result['repetition_results'])==repetitions
                    assert result['settled_counters_delta']['brewfs_fuse_read_ops_total']>0
                    assert result['settled_counters_delta']['brewfs_s3_get_ops_total']==0
            for observation in (client/f'case-{i:03d}').glob('*-public-env.json'):
                expected={'BREWFS_WRITEBACK_UPLOAD_CONCURRENCY':'3','BREWFS_FUSE_KEEP_CACHE':'0' if row['kind']=='resident' else '1',
                          'BREWFS_FUSE_WRITEBACK':'false','BREWFS_FUSE_DIRECT_IO':'0'}
                assert read(observation)['environment']==expected
            totals['cases']+=1
            totals['full_file_hash_checks']+=sum(p.get('full_file_hash_checks',0) for p in row['phases'].values())
        objects = read(client/'objects.json')
        by_key = {(o['bucket'],o['key']):o for o in objects}
        assert len(by_key)==len(objects)
        seen=set()
        with (server/'layouts.jsonl').open() as stream:
            for line in stream:
                layout=json.loads(line);obj=layout['object'];key=(obj['bucket'],obj['key'])
                assert by_key[key]==obj
                identity=(key,layout['disk']);assert identity not in seen and layout['disk'] in range(4)
                seen.add(identity)
                assert layout['fields']==dict(size=obj['bytes'],data=2,parity=2,block=1048576)
                assert layout['quantum']==[None,None]
                assert obj['stored_hint']==('1048576' if obj['bytes']>1048576 else None)
        assert len(seen)==4*len(objects)
        totals['layout_records']+=len(seen)
    assert totals['cases']==plan['groups']==200
    save(BASE/'guard-audit.json',dict(passed=True,totals=totals,scope='Full exports, exact cases and phase sizes, recorded hashes, resident FUSE path, strict drain, clean mount exits and all physical layouts.'))
    print(json.dumps(dict(passed=True,totals=totals)),flush=True)


if __name__ == '__main__':
    main()

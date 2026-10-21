"""Audit complete canonical-runner exports and every physical layout offline."""
import csv
import hashlib
import json
import os
from pathlib import Path

from cloud import BASE, WORK, save


def read(path):
    return json.loads(path.read_text())


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    plan = read(BASE / 'fio-guard-plan.json')
    folder = BASE / 'fio-guard'
    closure = read(folder / 'closure.json')
    assert closure['success']
    assert [c['job'] for c in closure['completed']] == [j['id'] for j in plan['jobs']]
    assert len(plan['jobs']) == 4
    assert sha(BASE / 'fio-guard-plan.json') == read(folder / 'started.json')['plan_sha256']
    assert sha(BASE / 'guard-scope.json') == plan['scope_sha256']
    for name, digest in plan['tool_sha256'].items():
        recorded = Path(os.environ.get('BENCH_RECORDED_TOOLS', str(WORK)))
        assert sha(recorded / name) == sha(BASE / 'fio-guard-tools' / name) == digest
    expected = {tool + '-direct' + str(d) for tool in plan['tools'] for d in plan['direct']}
    assert len(expected) == 6
    totals = dict(jobs=0, fio_scenes=0, exported_files=0, objects=0, physical_layout_records=0)
    for job in plan['jobs']:
        root = folder / 'raw' / job['id']
        exported = read(folder / (job['id'] + '-export.json'))
        assert exported['completed']
        paths = set()
        for item in exported['files']:
            path = folder / item['path']
            assert path.resolve().is_relative_to(root.resolve())
            assert path not in paths and path.stat().st_size == item['bytes']
            assert sha(path) == item['sha256']
            paths.add(path)
        assert paths == {p for p in root.rglob('*') if p.is_file()}
        client, server = root / 'client', root / 'server'
        assert read(client / 'protocol.json') == job == read(server / 'start.json')['job']
        assert read(server / 'verified-public-environment.json') == job['arm']['environment']
        assert job['arm']['hint'] == 'off' and job['upload_permits'] == 3
        assert job['runner_sha256'] == plan['canonical_source_hashes']['docker/compose-xfstests/run_perf_in_container.sh']
        result = read(client / 'closure.json')
        assert result['success'] and result['canonical_runner'] and result['runner_returncode'] == 0
        assert not result['remaining_mounts'] and read(server / 'stop.json')['stopped']
        assert read(client / 'cache-cleanup.json')['unmounted']
        assert read(client / 'cache-cleanup.json')['cache_removed']
        artifacts = client / 'artifacts'
        assert (artifacts / 'perf.complete').exists()
        with (artifacts / 'perf-summary.tsv').open() as stream:
            rows = list(csv.DictReader(stream, delimiter='\t'))
        assert len(rows) == 6 and {row['tool'] for row in rows} == expected
        assert all(row['status'] == 'pass' for row in rows)
        objects = read(client / 'objects.json')
        by_key = {(obj['bucket'], obj['key']): obj for obj in objects}
        assert objects and len(by_key) == len(objects)
        seen = set()
        with (server / 'layouts.jsonl').open() as stream:
            for line in stream:
                layout = json.loads(line)
                obj = layout['object']
                key = (obj['bucket'], obj['key'])
                assert by_key[key] == obj
                identity = (key, layout['disk'])
                assert identity not in seen and layout['disk'] in range(4)
                seen.add(identity)
                assert layout['fields'] == dict(size=obj['bytes'], data=2, parity=2, block=1048576)
                assert layout['quantum'] == [None, None]
        assert len(seen) == 4 * len(objects)
        receipt = read(server / 'layout-audit.json')
        assert receipt['passed'] and receipt['metadata_records'] == len(seen)
        completed = next(c for c in closure['completed'] if c['job'] == job['id'])
        assert completed['cleanup']['owned_volumes_removed'] == 4
        totals['jobs'] += 1
        totals['fio_scenes'] += len(rows)
        totals['exported_files'] += len(paths)
        totals['objects'] += len(objects)
        totals['physical_layout_records'] += len(seen)
    save(BASE / 'fio-guard-audit.json', dict(passed=True, totals=totals,
        scope='Frozen plan/tools, complete export hashes, canonical runner identity, all six scenes per job, clean unmount/cache removal, server stop, all four-disk layouts and owned payload cleanup. Performance thresholds remain in fio-guard-analysis.json.'))
    print(json.dumps(dict(passed=True, totals=totals)), flush=True)


if __name__ == '__main__':
    main()

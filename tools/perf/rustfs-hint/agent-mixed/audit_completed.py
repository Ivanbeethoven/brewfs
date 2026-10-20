"""Independent, offline checks of the complete frozen FUSE campaign."""
from collections import Counter
import argparse
import hashlib
import json
from pathlib import Path
import time

from cloud import BASE, OUT, WORK, save


def read(path):
    return json.loads(path.read_text())


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main(completed_only=False, recorded_tools=None):
    plan = read(BASE / 'formal-plan.json')
    if completed_only:
        closed = dict(completed=read(BASE / 'formal/completed.json'))
        completed_ids = [j['job'] for j in closed['completed']]
        jobs = plan['jobs'][:len(completed_ids)]
        assert completed_ids == [j['id'] for j in jobs]
    else:
        closed = read(BASE / 'formal/closure.json')
        assert closed['success'] and len(closed['completed']) == len(plan['jobs']) == 36
        jobs = plan['jobs']
    assert sha(BASE / 'formal-plan.json') == read(BASE / 'formal/started.json')['plan_sha256']
    assert sha(OUT / 'experiment-plan.json') == plan['protocol_sha256']
    assert sha(BASE / 'topology.json') == plan['topology_sha256']
    assert sha(BASE / 'execution-bindings.json') == plan['execution_bindings_sha256']
    for name, expected in plan['tools'].items():
        assert sha((recorded_tools or WORK) / name) == sha(BASE / 'formal-tools' / name) == expected, name
    assert read(BASE / 'formal-staged-verification.json')['passed']
    protocol = read(OUT / 'experiment-plan.json')
    corpus = read(BASE / 'corpus.json')
    assert len(corpus) == 3000
    totals = Counter()
    records = []
    configurations = {}
    object_histograms = []
    for job in jobs:
        root = BASE / 'formal/raw' / job['id']
        client, server = root / 'client', root / 'server'
        exported = read(BASE / 'formal' / (job['id'] + '-export.json'))
        assert exported['completed']
        paths = set()
        for entry in exported['files']:
            path = BASE / 'formal' / entry['path']
            assert path.resolve().is_relative_to(root.resolve())
            assert path.stat().st_size == entry['bytes'] and sha(path) == entry['sha256']
            paths.add(path)
        assert paths == {p for p in root.rglob('*') if p.is_file()}
        totals['exported_files'] += len(paths)
        assert read(client / 'protocol.json') == job == read(server / 'start.json')['job']
        assert read(server / 'verified-public-environment.json') == job['arm']['environment']
        assert read(server / 'stop.json')['stopped']
        closure = read(client / 'closure.json')
        assert closure['success'] and not closure['remaining_mounts']
        assert len(closure['processes']) == 3 and all(p['exit'] == 0 for p in closure['processes'])
        assert read(client / 'sampler-closure.json') == dict(errors=[], closed=True)
        assert (server / 'sampler-closed.json').is_file()
        totals['clean_mount_exits'] += 3
        observed = read(client / 'mount-observation.json')
        mount = observed['mount']['filesystems'][0]
        assert mount['fstype'].startswith('fuse') and mount['source'] == 'brewfs'
        assert observed['process_executable'] == '/work/brewfs'
        assert observed['public_environment'] == {
            'BREWFS_WRITEBACK_UPLOAD_CONCURRENCY': '3', 'BREWFS_FUSE_KEEP_CACHE': '1',
            'BREWFS_FUSE_WRITEBACK': 'false', 'BREWFS_FUSE_DIRECT_IO': '0'}
        config = read(client / 'measured.json')
        on = job['arm']['id'] == 'dynamic-auto'
        assert config['data']['s3'].pop('rustfs_ec_block_size_hint', None) == ('auto' if on else None)
        config.pop('mount_point')
        config['cache'].pop('cache_root')
        config['data']['s3'].pop('bucket')
        assert config == configurations.setdefault('common', config)
        assert config['layout'] == dict(block_size=4194304, chunk_size=67108864)
        assert config['cache']['writeback_persist_sync'] and config['cache']['writeback_require_stage_before_commit']
        source = OUT / next(j['trace'] for j in protocol['jobs'] if
                            (j['round'], j['scene'], j['arm']) == (job['round'], job['scene'], job['arm']['id']))
        assert sha(source) == job['trace_sha256']
        assert sha(BASE / 'corpus.json') == job['corpus_sha256']
        trace = [json.loads(line) for line in source.read_text().splitlines()]
        operations = read(client / 'operations.json')
        result = read(client / 'results.json')[0]
        assert operations['start_ns'] == result['start_ns']
        assert result['correctness'] == result['unmount'] == 'pass'
        assert result['start_ns'] < result['active_end_ns'] < result['drained_end_ns']
        state = {p: dict(bytes=s, content_version='initial:' + p) for p, s in corpus.items()}
        digests = read(client / 'content-digests.json')
        reads = creates = 0
        for row in trace:
            current = {k: row[k] for k in ['bytes', 'content_version']}
            assert len(digests[row['content_version']]) == 64
            if row['op'] == 'read':
                assert state[row['path']] == current
                reads += 1
            else:
                assert row['path'].startswith('worker-' + str(row['worker']) + '/')
                assert (row['path'] in state) == (row['op'] == 'replace')
                creates += row['op'] == 'create'
                state[row['path']] = current
        assert state == read(client / 'final-state.json')
        assert result['remount_full_file_hash_checks'] == len(state) == 3000 + creates
        assert result['full_read_hash_checks'] == reads
        flattened = []
        for worker, rows in enumerate(operations['workers']):
            assert len(rows) == 2000
            end = result['start_ns']
            for row in rows:
                assert row['worker'] == worker
                assert end <= row['start_ns'] <= row['end_ns'] <= row['checked_ns'] <= result['active_end_ns']
                end = row['checked_ns']
                assert row['full_read_hash_passed'] == (row['op'] == 'read')
                if row['op'] != 'read':
                    assert min(row['write_components'].values()) >= 0
                    assert sum(row['write_components'].values()) <= row['end_ns'] - row['start_ns']
            flattened.extend(rows)
        fields = ['worker', 'index', 'op', 'bytes']
        assert [[r[k] for k in fields] for r in flattened] == [[r[k] for k in fields] for r in trace]
        assert len(flattened) == result['operations'] == 8000
        assert sum(r['bytes'] for r in trace) == result['logical_bytes'] == int(3842.5 * 1048576)
        drain = result['drain_samples']
        assert len(drain) >= 3 and all(not any(r['values'].values()) for r in drain[-3:])
        assert all(drain[i+1]['time'] - drain[i]['time'] >= .019 for i in range(len(drain)-3, len(drain)-1))
        assert result['drain_seconds'] < 120
        for name, end in [('active_seconds', 'active_end_ns'), ('total_seconds', 'drained_end_ns')]:
            assert abs(result[name] - (result[end] - result['start_ns']) / 1e9) < 1e-9
        delta = result['counters_delta']
        assert delta['brewfs_fuse_write_bytes_total'] == sum(r['bytes'] for r in trace if r['op'] != 'read')
        assert delta['brewfs_writeback_stage_failures_total'] == delta['brewfs_writeback_commit_before_stage_ops_total'] == 0
        objects = read(client / 'objects.json')
        by_key = {(o['bucket'], o['key']): o for o in objects}
        assert len(by_key) == len(objects) == result['objects']
        histogram = Counter()
        seen = set()
        for line in (server / 'layouts.jsonl').open():
            layout = json.loads(line)
            obj = layout['object']
            key = (obj['bucket'], obj['key'])
            assert by_key[key] == obj
            identity = (key, layout['disk'])
            assert identity not in seen and layout['disk'] in range(4)
            seen.add(identity)
            expected = dict(size=obj['bytes'], data=2, parity=2, block=1048576)
            assert layout['fields'] == expected
            assert {k: layout['decoded'][k] for k in expected} == expected
            assert layout['quantum'] == [None, None]
            stored = obj['stored_hint']
            assert stored == ('1048576' if on and obj['bytes'] > 1048576 else None)
            meta = layout['decoded']['metadata']
            inline = meta.get('x-minio-internal-inline-data') == 'true'
            if layout['disk'] == 0:
                histogram[(obj['bytes'], stored, inline)] += 1
        assert len(seen) == len(objects) * 4
        receipt = read(server / 'layout-audit.json')
        assert receipt['passed'] and receipt['metadata_records'] == len(seen)
        completion = next(j for j in closed['completed'] if j['job'] == job['id'])
        assert completion['cleanup']['owned_volumes_removed'] == 4
        totals.update(operations=len(trace), immediate_full_read_hash_checks=reads,
                      remount_full_file_hash_checks=len(state), objects=len(objects), physical_layout_records=len(seen))
        records.append(dict(job=job['id'],passed=True,stage_filesystem=observed['stage_device']))
        object_histograms.append(dict(job=job['id'],sizes=[dict(bytes=k[0],hint=k[1],inline=k[2],count=v)
                                  for k,v in sorted(histogram.items())]))
    filename = 'completed-prefix-audit.json' if completed_only else 'independent-audit.json'
    save(BASE / filename, dict(passed=True, complete_campaign=not completed_only,
        jobs=records, totals=dict(totals),time=time.time(),
        scope='Frozen fingerprints, per-job exports, actual FUSE/configuration, all trace order/version/timing records, final namespaces, recorded read checks, strict drain, exits and every physical layout. No new performance measurement.'))
    if not completed_only:
        save(BASE / 'object-layout-distribution.json', object_histograms)
    print(json.dumps(dict(passed=True, complete_campaign=not completed_only, jobs=len(records), totals=dict(totals))))


if __name__ == '__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--completed-only',action='store_true')
    parser.add_argument('--recorded-tools', type=Path, help='Original archived tools when auditing a previous campaign')
    args = parser.parse_args()
    main(args.completed_only, args.recorded_tools)

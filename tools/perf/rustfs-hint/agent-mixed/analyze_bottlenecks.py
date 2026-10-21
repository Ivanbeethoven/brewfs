"""Describe the measured critical worker and counters; do not infer disk-only time."""
import json
import statistics
from collections import defaultdict
from cloud import BASE, save


def pairs(text):
    return {p[0]: int(p[1]) for line in text.splitlines() if len(p := line.split()) == 2 and p[1].isdigit()}


def snmp(text):
    lines = text.splitlines()
    result = {}
    for i in range(0, len(lines), 2):
        names, values = lines[i].split(), lines[i+1].split()
        assert names[0] == values[0]
        result.update({names[0] + k: int(v) for k,v in zip(names[1:], values[1:])})
    return result


def main():
    assert json.loads((BASE/'formal/closure.json').read_text())['success']
    jobs = json.loads((BASE/'formal-plan.json').read_text())['jobs']
    results = []
    for job in jobs:
        folder = BASE/'formal/raw'/job['id']/'client'
        r = json.loads((folder/'results.json').read_text())[0]
        workers = json.loads((folder/'operations.json').read_text())['workers']
        # The final worker determines the application finish; its own spans do
        # not overlap. Backend counters and sums across workers do overlap.
        last = max(workers, key=lambda w: w[-1]['checked_ns'])
        critical = defaultdict(float)
        for operation in last:
            if operation['op'] == 'read':
                critical['read_io_seconds'] += (operation['end_ns']-operation['start_ns'])/1e9
            else:
                for name, value in operation['write_components'].items():
                    critical['save_'+name.removesuffix('_ns')+'_seconds'] += value/1e9
                critical['save_wrapper_seconds'] += (operation['end_ns']-operation['start_ns']-sum(operation['write_components'].values()))/1e9
            critical['post_io_check_seconds'] += (operation['checked_ns']-operation['end_ns'])/1e9
        critical['other_worker_seconds'] = (last[-1]['checked_ns']-r['start_ns'])/1e9-sum(critical.values())
        critical['join_and_finish_seconds'] = (r['active_end_ns']-last[-1]['checked_ns'])/1e9
        critical['drain_seconds'] = r['drain_seconds']
        assert min(critical.values()) >= 0
        assert abs(sum(critical.values())-r['total_seconds']) < 1e-7
        d = r['counters_delta']
        names = ['brewfs_s3_get_ops_total','brewfs_s3_get_bytes_total','brewfs_s3_get_lat_us_total',
            'brewfs_s3_put_ops_total','brewfs_s3_put_bytes_total','brewfs_s3_put_lat_us_total',
            'brewfs_s3_put_prepare_lat_us_total','brewfs_s3_put_cache_lat_us_total',
            'brewfs_writeback_stage_ops_total','brewfs_writeback_stage_bytes_total','brewfs_writeback_stage_lat_us_total',
            'brewfs_writeback_flush_wait_us_total','brewfs_writeback_commit_wait_upload_us_total',
            'brewfs_writeback_backpressure_soft_sleep_ops','brewfs_writeback_backpressure_hard_wait_ops',
            'brewfs_writeback_upload_batch_single_block_ops_total','brewfs_writeback_upload_batch_multi_block_ops_total',
            'brewfs_writeback_flush_fragmentation_slices_total','brewfs_writeback_flush_fragmentation_bytes_total',
            'brewfs_cache_hits_total','brewfs_cache_requests_total',
            'brewfs_fuse_read_ops_total','brewfs_fuse_read_bytes_total','brewfs_fuse_write_ops_total','brewfs_fuse_write_bytes_total']
        counters = {k: d[k] for k in names}
        read_bytes = sum(o['bytes'] for w in workers for o in w if o['op'] == 'read')
        write_bytes = r['logical_bytes']-read_bytes
        all_samples = [json.loads(line) for line in (folder/'timeline.jsonl').read_text().splitlines()]
        samples = [s for s in all_samples if r['start_ns']/1e9 <= s['monotonic'] <= r['drained_end_ns']/1e9]
        first, final = samples[0], samples[-1]
        elapsed = final['monotonic']-first['monotonic']
        c0,c1 = first['cgroups'],final['cgroups']
        cpu0,cpu1 = pairs(c0['cpu/cpu.stat']),pairs(c1['cpu/cpu.stat'])
        n0,n1 = snmp(first['process_network']['/proc/net/snmp']),snmp(final['process_network']['/proc/net/snmp'])
        resources = dict(sampled_seconds=elapsed,task_seconds=r['total_seconds'],
            client_container_cpu_cores=(int(c1['cpuacct/cpuacct.usage'])-int(c0['cpuacct/cpuacct.usage']))/1e9/elapsed,
            client_throttle_seconds=(cpu1['throttled_time']-cpu0['throttled_time'])/1e9,
            client_memory_failures=int(c1['memory/memory.failcnt'])-int(c0['memory/memory.failcnt']),
            client_peak_memory_bytes=max(int(s['cgroups']['memory/memory.usage_in_bytes']) for s in samples),
            client_tcp_retrans_segments=n1['Tcp:RetransSegs']-n0['Tcp:RetransSegs'],
            client_tcp_out_segments=n1['Tcp:OutSegs']-n0['Tcp:OutSegs'],
            sampler_median_seconds=statistics.median(s['sampling_seconds'] for s in samples),
            sampler_max_seconds=max(s['sampling_seconds'] for s in samples))
        results.append(dict(job=job['id'],scene=job['scene'],round=job['round'],arm=job['arm']['id'],
            critical_worker=last[0]['worker'],critical_worker_seconds=dict(critical),counters=counters,
            logical_read_bytes=read_bytes,logical_write_bytes=write_bytes,resources=resources,
            fuse_average_read_request_bytes=d['brewfs_fuse_read_bytes_total']/d['brewfs_fuse_read_ops_total'],
            fuse_average_write_request_bytes=d['brewfs_fuse_write_bytes_total']/d['brewfs_fuse_write_ops_total'],
            get_bytes_per_logical_read_byte=d['brewfs_s3_get_bytes_total']/read_bytes,
            put_bytes_per_logical_write_byte=d['brewfs_s3_put_bytes_total']/write_bytes))
    save(BASE/'formal-bottlenecks.json',dict(rows=results,interpretation=[
        'The final worker spans partition the measured task wall time. They describe observed waits, not independent component speedups.',
        'Stage, flush, fsync and S3 durations overlap; never sum their cross-worker totals as a wall-time decomposition.',
        'Container CPU includes the replay/checker and BrewFS. Resource deltas cover the indicated sampled interval, not exact task boundaries.',
        'Only FUSE request averages are available; they are not a request-size histogram or proof of every request size.',
        'ACS client staging is ext4 emptyDir backed by /dev/vda5; its underlying physical medium is not established as local SSD.',
        'No measured case is removed based on resource counters, retransmissions or tails.']))
    print(json.dumps(dict(analyzed_jobs=len(results),output='formal-bottlenecks.json')))


if __name__ == '__main__':
    main()

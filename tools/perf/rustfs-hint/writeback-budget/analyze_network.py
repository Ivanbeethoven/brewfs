"""Audit complete diagnostics and report TCP counters without inferring causality."""
import hashlib
import json
from cloud import BASE, WORK, save

closed=json.loads((BASE/'body-stream-network-closure.json').read_text());assert closed['success']

def network(values):
    result={}
    for text in values.values():
        lines=text.splitlines()
        for index in range(0,len(lines),2):
            head=lines[index].split();data=lines[index+1].split()
            assert head[0]==data[0] and len(head)==len(data)
            result.update({head[0]+key:int(value) for key,value in zip(head[1:],data[1:])})
    return result

def cpustat(values):
    text=values.get('cpu/cpu.stat',values.get('cgroup/cpu/cpu.stat',''))
    return {line.split()[0]:int(line.split()[1]) for line in text.splitlines()}

selected=['Tcp:RetransSegs','TcpExt:TCPTimeouts','TcpExt:TCPFastRetrans','TcpExt:TCPLossProbes',
          'TcpExt:TCPFromZeroWindowAdv','TcpExt:TCPToZeroWindowAdv','TcpExt:TCPWantZeroWindowAdv',
          'TcpExt:TCPWinProbe','TcpExt:TCPRcvCollapsed','TcpExt:TCPBacklogDrop','TcpExt:TCPRcvQDrop',
          'TcpExt:TCPOFODrop','TcpExt:TCPMemoryPressures','TcpExt:TCPDeliveredCE','Ip:InDiscards','Ip:OutDiscards']
results=[];hashes=0
for stage in closed['stages']:
    root=BASE/stage/'raw'/stage
    closure=json.loads((root/'client/closure.json').read_text())
    assert closure['success'] and not closure['remaining_mounts'] and all(p['exit']==0 for p in closure['processes'])
    assert json.loads((root/'server/stop.json').read_text())['stopped']
    assert json.loads((root/'server/layout-audit.json').read_text())['passed']
    rows=json.loads((root/'client/results.json').read_text());assert len(rows)==3
    samples=[json.loads(line) for line in (root/'server/resources.jsonl').read_text().splitlines()]
    for row in rows:
        for phase in ['client_cold_read','warm_read']:
            data=row['phases'][phase];assert data['full_file_hash_checks']==row['count'];hashes+=row['count']
        data=row['phases']['client_cold_read'];counts=data['settled_counters_delta']
        assert counts['brewfs_s3_get_ops_total']==row['count']*((row['size']+4194303)//4194304)
        a=network(data['network_before']);b=network(data['network_after'])
        start=data['phase_start_utc_ns'];end=data['phase_end_utc_ns']
        before=max((s for s in samples if s['utc_ns']<=start),key=lambda s:s['utc_ns'])
        after=min((s for s in samples if s['utc_ns']>=end),key=lambda s:s['utc_ns'])
        sa=network({k:v for k,v in before['values'].items() if k.startswith('network_namespace/')})
        sb=network({k:v for k,v in after['values'].items() if k.startswith('network_namespace/')})
        ca=cpustat(data['diagnostic_cgroup_before']);cb=cpustat(data['diagnostic_cgroup_after'])
        sca=cpustat(before['values']);scb=cpustat(after['values'])
        result=dict(arm=row['arm'],size=row['size'],concurrency=row['concurrency'],rate=data['active_mib_s'],
            file_milliseconds=[v*1000 for v in data['file_latencies_seconds']],
            client_tcp_delta={k:b[k]-a[k] for k in selected if k in a and k in b},
            server_tcp_delta={k:sb[k]-sa[k] for k in selected if k in sa and k in sb},
            client_cpu_delta={k:cb[k]-ca[k] for k in ['nr_throttled','throttled_time']},
            server_cpu_delta={k:scb[k]-sca[k] for k in ['nr_throttled','throttled_time']},
            server_leading_margin_ms=(start-before['utc_ns'])/1e6,server_trailing_margin_ms=(after['utc_ns']-end)/1e6,
            gets=counts['brewfs_s3_get_ops_total'],get_mean_ms=counts['brewfs_s3_get_lat_us_total']/counts['brewfs_s3_get_ops_total']/1000)
        results.append(result)
    plan=json.loads((BASE/(stage+'-plan.json')).read_text());dest=BASE/'tool-versions'/('frozen-'+stage);dest.mkdir(exist_ok=False)
    for name,digest in plan['tools'].items():
        data=(WORK/name).read_bytes();assert hashlib.sha256(data).hexdigest()==digest;(dest/name).write_bytes(data)
assert hashes==1088
save(BASE/'body-stream-network-analysis.json',dict(passed_data_audit=True,full_file_hash_checks=hashes,rows=results,
    scope='One distinct diagnostic block per binary, not an acceptance comparison. Ordinary uninterrupted workload, hash retention matches screen. Client counters span phase including subsequent local hashing; server200ms samples bracket client UTC and may include boundary traffic/clock offset. Namespace counters cover all connections, not only the S3 connection. Zero CPU throttles does not exclude network shaping or remote resource constraints.'))
for r in results:print(json.dumps(r),flush=True)

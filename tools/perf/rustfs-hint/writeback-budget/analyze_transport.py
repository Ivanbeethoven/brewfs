"""Audit and explain the bounded transport diagnostic; no application acceptance."""
import hashlib
import json
import numpy as np
from cloud import BASE, save


def network(snapshot):
    out={}
    for name,text in snapshot.items():
        if not name.startswith('/proc/net/'):
            continue
        lines=text.splitlines()
        for i in range(0,len(lines),2):
            head,data=lines[i].split(),lines[i+1].split()
            assert head[0]==data[0] and len(head)==len(data)
            out.update({head[0]+k:int(v) for k,v in zip(head[1:],data[1:])})
    return out


assert json.loads((BASE/'transport-probe-closure.json').read_text())['success']
plan=json.loads((BASE/'transport-probe-plan.json').read_text())
assert hashlib.sha256((BASE/'transport-probe-source.py').read_bytes()).hexdigest()==plan['source_sha256']
sides={role:json.loads((BASE/f'transport-probe-{role}.json').read_text()) for role in ['client','server']}
rows=[]
for index,(direction,streams) in enumerate([('download',1),('download',4),('upload',1),('upload',4)]):
    recv_role='client' if direction=='download' else 'server'
    receiver=sides[recv_role]['rows'][index]
    assert (receiver['case'],receiver['direction'],receiver['streams'])==(index,direction,streams)
    assert receiver['bytes']==2*1024**3
    for role,data in sides.items():
        assert data['complete']
        r=data['rows'][index]
        assert (r['case'],r['direction'],r['streams'])==(index,direction,streams)
        assert sum(f['bytes'] for f in r['flows'])==2*1024**3
        assert all(f['full_sha256_verified'] for f in r['flows'])
    stop=max(f['seconds'] for f in receiver['flows'])
    edges=np.append(np.arange(0,int(stop)+1),stop)
    cumulative=sum(np.interp(edges,[0]+[p[0] for p in f['points']],
                             [0]+[p[1] for p in f['points']]) for f in receiver['flows'])
    assert cumulative[-1]==2*1024**3
    rates=(np.diff(cumulative)/np.diff(edges)/1048576).tolist()
    # A declared descriptive split, never remove early bytes from full-run rate.
    result=dict(direction=direction,streams=streams,whole_transfer_mib_s=receiver['mib_s'],
                whole_transfer_seconds=receiver['seconds'],interval_edges_seconds=edges.tolist(),
                one_second_mib_s=rates,
                after_two_seconds_mib_s=float((cumulative[-1]-cumulative[2])/(stop-2)/1048576),sides={})
    for role,data in sides.items():
        r=data['rows'][index];a=network(r['before']);b=network(r['after'])
        result['sides'][role]=dict(process_cpu_seconds=r['process_cpu_seconds'],
            retransmissions=b['Tcp:RetransSegs']-a['Tcp:RetransSegs'],
            timeouts=b['TcpExt:TCPTimeouts']-a['TcpExt:TCPTimeouts'])
    rows.append(result);print(json.dumps(result),flush=True)
save(BASE/'transport-probe-analysis.json',dict(passed_data_audit=True,rows=rows,
    full_stream_sha256_checks=10,bytes_transferred=8*1024**3,
    scope='Single diagnostic per direction/concurrency, no performance CI and no accepted application change. Full transfer includes every byte. One-second rates interpolate 8MiB receive progress; flow starts are independently timed with a common barrier. After-two-second rate is a descriptive view of the same full stream, not a filtered acceptance measurement. SHA256 runs inside receive timing; CPU recorded.'))
save(BASE/'acs-network-specification.json',dict(
    url='https://help.aliyun.com/zh/cs/user-guide/acs-pod-instance-overview',
    title='ACS Pod实例概述',checked_date='2026-10-20',page_update='2026-10-16 14:23:38',
    compute_class='general-purpose',bandwidth_definition='网络带宽（出+入）（Gbits/s）',
    client_vcpu=4,client_gbit_s=1.5,server_vcpu=8,server_gbit_s=2.5,
    table_rowspan_verified=True,
    note='Published specification, not a measured exact per-flow limiter. Short bursts may exceed nominal bandwidth.'))

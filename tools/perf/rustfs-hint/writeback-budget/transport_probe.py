"""Bounded in-memory TCP control, isolated from BrewFS, RustFS, Redis and disks."""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import argparse
import hashlib
import json
import socket
import threading
import time

CHUNK = hashlib.shake_256(b'brewfs-network-attribution-20261020').digest(262144)
TOTAL = 2 * 1024**3
CASES = [('download', 1), ('download', 4), ('upload', 1), ('upload', 4)]


def snapshot():
    out = {}
    for path in ['/proc/net/snmp', '/proc/net/netstat', '/sys/fs/cgroup/cpu/cpu.stat',
                 '/sys/fs/cgroup/cpuacct/cpuacct.usage']:
        out[path] = Path(path).read_text()
    return out


def line(conn):
    out = bytearray()
    while not out.endswith(b'\n'):
        v = conn.recv(1)
        if not v:
            raise EOFError('control connection closed')
        out.extend(v)
        assert len(out) < 100000
    return json.loads(out)


def send_json(conn, value):
    conn.sendall((json.dumps(value) + '\n').encode())


def flow(conn, sending, length, expected, barrier):
    conn.settimeout(60)
    buf = bytearray(len(CHUNK)); digest = hashlib.sha256(); points = []
    barrier.wait(); started = time.monotonic(); transferred = 0
    while transferred < length:
        if sending:
            conn.sendall(CHUNK); n = len(CHUNK)
        else:
            n = conn.recv_into(buf, min(len(buf), length - transferred))
            assert n > 0, 'incomplete TCP payload'
            digest.update(memoryview(buf)[:n])
        previous = transferred; transferred += n
        if transferred // (8*1024**2) != previous // (8*1024**2):
            points.append([time.monotonic() - started, transferred])
    finished = time.monotonic()
    if sending:
        acknowledgment = line(conn)
        assert acknowledgment == dict(bytes=length, sha256=expected)
    else:
        assert digest.hexdigest() == expected
        send_json(conn, dict(bytes=length, sha256=digest.hexdigest()))
    conn.close()
    return dict(sending=sending, bytes=transferred, seconds=finished-started,
                points=points, full_sha256_verified=True)


def main():
    parser=argparse.ArgumentParser();parser.add_argument('role',choices=['server','client'])
    parser.add_argument('--host',required=True);parser.add_argument('--output',required=True)
    args=parser.parse_args();output=Path(args.output);assert not output.exists()
    expected={}
    for streams in [1,4]:
        h=hashlib.sha256()
        for _ in range(TOTAL//streams//len(CHUNK)):h.update(CHUNK)
        expected[streams]=h.hexdigest()
    if args.role=='server':
        listener=socket.socket();listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        listener.bind((args.host,9000));listener.listen(8);listener.settimeout(120)
    results=[]
    try:
        for index,(direction,streams) in enumerate(CASES):
            # New case has a fixed quiet interval; it is not a perf warmup removal.
            if args.role=='client':time.sleep(2)
            sockets=[]
            header=dict(case=index,direction=direction,streams=streams,bytes=TOTAL//streams)
            for _ in range(streams):
                if args.role=='server':
                    conn,_=listener.accept();conn.settimeout(60);assert line(conn)==header
                else:
                    conn=socket.create_connection((args.host,9000),timeout=60);send_json(conn,header)
                sockets.append(conn)
            if args.role=='server':
                for conn in sockets:send_json(conn,dict(ready=True))
            else:
                for conn in sockets:assert line(conn)==dict(ready=True)
            barrier=threading.Barrier(streams+1)
            sending=(args.role=='server')==(direction=='download')
            before=snapshot();cpu_before=time.process_time();start=time.monotonic()
            with ThreadPoolExecutor(max_workers=streams) as pool:
                futures=[pool.submit(flow,c,sending,TOTAL//streams,expected[streams],barrier) for c in sockets]
                barrier.wait();rows=[f.result() for f in futures]
            elapsed=time.monotonic()-start
            result=dict(case=index,direction=direction,streams=streams,bytes=TOTAL,
                seconds=elapsed,mib_s=TOTAL/1024**2/elapsed,process_cpu_seconds=time.process_time()-cpu_before,
                before=before,after=snapshot(),flows=rows,role=args.role)
            results.append(result);output.write_text(json.dumps(dict(complete=False,rows=results)))
            print(json.dumps({k:result[k] for k in ['case','direction','streams','seconds','mib_s']}),flush=True)
    finally:
        if args.role=='server':listener.close()
    output.write_text(json.dumps(dict(complete=True,rows=results)))


if __name__=='__main__':main()

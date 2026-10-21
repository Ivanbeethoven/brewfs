"""Whole-file FUSE screening: release BrewFS / Redis / fixed RustFS binary.

No production source changes. Each invocation owns a fresh directory and Redis
container. A failed invocation is retained and is never resumed or spliced.
"""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from datetime import datetime, timezone
import argparse
import hashlib
import json
import os
import random
import secrets
import shutil
import socket
import subprocess
import threading
import time
import traceback

import boto3
from botocore.config import Config

BASE = Path(os.environ.get('BENCH_LOCAL_ROOT', '/work'))
BINARY = Path(os.environ.get('BREWFS_BINARY', str(BASE / 'brewfs')))
RUSTFS = Path(os.environ.get('RUSTFS_BINARY', str(BASE / 'candidate')))
MIB = 1024 * 1024
REQUEST_BYTES = None
SIZES = [(4096,512),(65536,256),(262144,128),(MIB,64),(4*MIB,32),(16*MIB,8),(64*MIB,4)]
DEBT = ['brewfs_writeback_dirty_bytes','brewfs_writeback_live_dirty_bytes',
        'brewfs_writeback_recent_pending_upload_bytes','brewfs_writeback_remote_upload_inflight_bytes']


def dump(path, obj):
    path.write_text(json.dumps(obj, indent=2, ensure_ascii=False))


def resources():
    return dict(wall_time=time.time(),load=os.getloadavg(),
                meminfo=Path('/proc/meminfo').read_text(),
                vmstat=Path('/proc/vmstat').read_text(),stat=Path('/proc/stat').read_text(),
                diskstats=Path('/proc/diskstats').read_text(),
                processes=subprocess.check_output(['ps','-eo','pid,comm,pcpu,rss'],text=True))


def metrics(mount):
    raw = (mount / '.stats').read_text()
    parsed = {}
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) == 2 and not fields[0].startswith('#'):
            try:
                parsed[fields[0]] = float(fields[1])
            except ValueError:
                pass
    return raw, parsed


def drain(mount):
    deadline, stable = time.monotonic()+120, 0
    samples = []
    while time.monotonic() < deadline:
        raw, data = metrics(mount)
        assert all(k in data for k in DEBT), 'missing drain counter'
        sample = dict(time=time.monotonic(), values={k:data[k] for k in DEBT})
        samples.append(sample)
        stable = stable+1 if not any(sample['values'].values()) else 0
        if stable >= 3:
            return raw, data, samples
        time.sleep(.02)
    raise TimeoutError('upload drain did not reach stable zero')


def payloads(size, count, salt):
    # Preparation and all correctness hashing are outside measured intervals.
    return [hashlib.shake_256(f'brewfs-local-{size}-{i}-{salt}'.encode()).digest(size) for i in range(count)]


def io_phase(mount, size, count, concurrency, phase, expected, salt='initial'):
    data = payloads(size, count, salt) if phase in ['write','mixed'] else None
    hashes = [hashlib.sha256(x).hexdigest() for x in data] if data else expected
    barrier = threading.Barrier(concurrency+1)
    def one(index):
        filename = mount / f'file-{index:04d}'
        writing = phase == 'write' or (phase == 'mixed' and index >= count//2)
        start = time.perf_counter_ns()
        content = None
        if writing:
            with filename.open('wb', buffering=0) as stream:
                view = memoryview(data[index])
                while view:
                    n = stream.write(view[:REQUEST_BYTES])
                    assert n > 0
                    view = view[n:]
                os.fsync(stream.fileno())
        else:
            with filename.open('rb', buffering=0) as stream:
                if REQUEST_BYTES is None:
                    content = stream.read()
                else:
                    pieces = []
                    while piece := stream.read(REQUEST_BYTES):
                        pieces.append(piece)
                    content = b''.join(pieces)
        elapsed = (time.perf_counter_ns()-start)/1e9
        return index, elapsed, content, writing
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        ready = [pool.submit(barrier.wait) for _ in range(concurrency)]
        barrier.wait()
        for item in ready:
            item.result()
        before_raw, before = metrics(mount)
        start = time.monotonic()
        order = ([index for pair in zip(range(count//2),range(count//2,count)) for index in pair]
                 if phase == 'mixed' else range(count))
        results = list(pool.map(one, order))
        active_end = time.monotonic()
        if phase in ['write','mixed']:
            after_raw, after, samples = drain(mount)
            total_end = time.monotonic()
        else:
            total_end = active_end
            after_raw, after = metrics(mount)
            samples = []
    verified = 0
    for index, _, content, writing in results:
        if not writing:
            assert len(content) == size
            assert hashlib.sha256(content).hexdigest() == expected[index], (phase,index,'hash')
            verified += 1
    if phase == 'mixed':
        hashes[:count//2] = expected[:count//2]
    row = dict(phase=phase, file_bytes=size, files=count, concurrency=concurrency,
               logical_bytes=size*count, active_seconds=active_end-start,
               total_seconds=total_end-start, drain_seconds=total_end-active_end,
               active_mib_s=size*count/MIB/(active_end-start),
               total_mib_s=size*count/MIB/(total_end-start),
               file_latencies_seconds=[x[1] for x in results], full_file_hash_checks=verified,
               counters_before=before, counters_after=after,
               counters_delta={k:after[k]-before.get(k,0) for k in after}, drain_samples=samples)
    return row, hashes, before_raw, after_raw


class Experiment:
    def __init__(self, smoke):
        self.smoke = smoke
        identity = ('smoke-' if smoke else 'screen-') + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
        self.root = BASE / identity
        self.root.mkdir(exist_ok=False)
        dump(self.root/'resources-before.json',resources())
        self.redis_name = 'brewfs-local-hint-' + identity.lower()
        self.redis_id = None
        self.processes, self.mounts, self.rows = [], [], []
        self.env = dict(os.environ, RUST_LOG='warn', AWS_ACCESS_KEY_ID=secrets.token_hex(12),
                        AWS_SECRET_ACCESS_KEY=secrets.token_hex(24), AWS_REGION='us-east-1',
                        AWS_EC2_METADATA_DISABLED='true', BREWFS_FUSE_KEEP_CACHE='1',
                        BREWFS_FUSE_WRITEBACK='false', BREWFS_FUSE_DIRECT_IO='0')
        self.endpoint = 'http://127.0.0.1:19090'
        self.s3 = boto3.client('s3', endpoint_url=self.endpoint,
                              aws_access_key_id=self.env['AWS_ACCESS_KEY_ID'],
                              aws_secret_access_key=self.env['AWS_SECRET_ACCESS_KEY'], region_name='us-east-1',
                              config=Config(signature_version='s3v4', connect_timeout=3, read_timeout=60,
                                            retries={'max_attempts':0}, s3={'addressing_style':'path'}))

    def spawn(self, args, name, env):
        with (self.root / (name+'.log')).open('wb') as log:
            proc = subprocess.Popen(args, env=env, cwd=BASE, stdout=log, stderr=subprocess.STDOUT)
        self.processes.append(proc)
        return proc

    def wait(self, check, proc, timeout=60):
        deadline = time.monotonic()+timeout
        while time.monotonic() < deadline:
            assert proc.poll() is None, f'service {proc.pid} exited {proc.returncode}'
            try:
                if check():
                    return
            except Exception:
                pass
            time.sleep(.1)
        raise TimeoutError('service startup')

    def start(self):
        build = json.loads((BASE / 'build.json').read_text())
        assert build['exit_code'] == 0
        assert hashlib.file_digest(BINARY.open('rb'),'sha256').hexdigest() == build['binary_sha256']
        assert hashlib.file_digest(RUSTFS.open('rb'),'sha256').hexdigest() == 'dc5e70da04d904f5ba3ec114776eb680551a2554107b36e5521393d80a9ffc9b'
        self.guard()
        for port in [19090,16379]:
            with socket.socket() as sock:
                sock.bind(('127.0.0.1',port))
        self.redis_id = subprocess.check_output(['docker','run','-d','--name',self.redis_name,
                         '-p','127.0.0.1:16379:6379','redis:7.2-alpine',
                         'redis-server','--save','','--appendonly','no'], text=True).strip()
        volumes = [self.root / f'volume{i}' for i in range(4)]
        for path in volumes:
            path.mkdir()
        env = dict(self.env, RUSTFS_ACCESS_KEY=self.env['AWS_ACCESS_KEY_ID'],
                   RUSTFS_SECRET_KEY=self.env['AWS_SECRET_ACCESS_KEY'], RUSTFS_CONSOLE_ENABLE='false',
                   RUSTFS_OBS_LOG_DIRECTORY='', RUSTFS_UNSAFE_BYPASS_DISK_CHECK='true',
                   RUSTFS_EC_BLOCK_SIZE_HINT_ENABLE='true', RUSTFS_EC_BLOCK_SIZE_HINT_FLEET_CONFIRMED='true',
                   RUSTFS_EC_READ_QUANTUM_ENABLE='false', RUSTFS_EC_READ_QUANTUM_FLEET_CONFIRMED='false')
        server = self.spawn([str(RUSTFS),'server','--address','127.0.0.1:19090',*map(str,volumes)], 'rustfs', env)
        self.wait(lambda:self.s3.list_buckets() is not None, server)
        self.server = server
        protocol = dict(brewfs_build=build, rustfs_commit='cc4926fac1107aee76540f28bf22dbd47fcbca24',
                        rustfs_sha256='dc5e70da04d904f5ba3ec114776eb680551a2554107b36e5521393d80a9ffc9b',
                        sizes_and_counts=SIZES, rounds=4, concurrency=[1,4], hint_modes=['off','auto'],
                        application='whole-file open/write/fsync/close; whole-file open/read/close',
                        write_accounting='includes 3 consecutive zero debt samples at 20ms intervals',
                        cache='fresh client remount for first read; server and device caches not dropped; repeat is warm kernel/client',
                        acceptance='local descriptive screen; not formal no-regression or cloud acceptance',
                        timings='monotonic; SHA256 and HEAD layout checks outside timing',
                        smoke=self.smoke, root=str(self.root), source_sha256=hashlib.file_digest(open(__file__,'rb'),'sha256').hexdigest(),
                        platform=subprocess.check_output(['uname','-a'],text=True).strip(),
                        cpu=subprocess.check_output(['lscpu'],text=True), memory=Path('/proc/meminfo').read_text(),
                        docker_image=subprocess.check_output(['docker','image','inspect','redis:7.2-alpine','--format','{{.Id}}'],text=True).strip())
        dump(self.root/'protocol.json',protocol)
        shutil.copy2(__file__,self.root/'run_local.py')
        print(json.dumps(dict(event='started',path=str(self.root))),flush=True)

    def guard(self):
        assert subprocess.run(['pgrep','-x','cargo|rustc'],stdout=subprocess.DEVNULL).returncode == 1, 'competing compilation'
        assert shutil.disk_usage(BASE).free > 40*1024**3, 'low WSL space'

    def mount(self, config, case, phase):
        config_path = case / (phase+'.json')
        dump(config_path,config)
        mount = Path(config['mount_point'])
        mount.mkdir(exist_ok=True)
        proc = self.spawn([str(BINARY),'mount','--config',str(config_path)],case.name+'-'+phase,self.env)
        self.mounts.append((mount,proc))
        self.wait(lambda:os.path.ismount(mount),proc)
        # Fresh Redis volumes initialize the root inode as root-owned (0755).
        # Set only this disposable volume's root owner before any timed I/O.
        if mount.stat().st_uid != os.getuid():
            subprocess.run(['sudo','-n','chown',f'{os.getuid()}:{os.getgid()}',str(mount)],
                           check=True,timeout=10)
        return proc

    def reset_metadata(self):
        subprocess.run(['docker','exec',self.redis_id,'redis-cli','FLUSHDB'],
                       check=True,stdout=subprocess.DEVNULL,timeout=10)

    def unmount(self,mount,proc):
        subprocess.run(['fusermount3','-u',str(mount)],check=True,timeout=30)
        assert proc.wait(timeout=60) == 0
        self.mounts.remove((mount,proc))

    def case(self,number,round_id,size,count,concurrency,hint,mixed=False):
        self.guard()
        case = self.root / f'case-{number:03d}'
        case.mkdir()
        started=time.time()
        dump(case/'resources-before.json',resources())
        bucket = f'local-{self.root.name.lower()}-{number:03d}'
        self.s3.create_bucket(Bucket=bucket)
        self.reset_metadata()
        mount = case/'mount'
        config = dict(mount_point=str(mount), fuse=dict(workers=4,max_background=64),
                      data=dict(backend='s3',s3=dict(bucket=bucket,endpoint=self.endpoint,region='us-east-1',force_path_style=True)),
                      meta=dict(backend='redis',redis=dict(url='redis://127.0.0.1:16379/0')),
                      layout=dict(block_size=4*MIB,chunk_size=64*MIB),
                      cache=dict(cache_root=str(case/'write-cache'),compression='none',
                                 writeback_mode='commit_before_upload',writeback_persist_sync=True,
                                 writeback_require_stage_before_commit=True,read_memory_bytes=64*MIB,
                                 read_ssd_bytes=0,write_memory_bytes=64*MIB,write_ssd_bytes=1024*MIB,
                                 memory_budget_bytes=512*MIB,dirty_slice_max_age_ms=2000,
                                 upload_concurrency=8,prefetch_enabled=True,range_background_prefetch=True,
                                 populate_write_cache_after_upload=True,persist_write_cache_after_upload=False))
        if hint == 'auto':
            config['data']['s3']['rustfs_ec_block_size_hint'] = 'auto'
        proc = self.mount(config,case,'write-mount')
        write, hashes, before, after = io_phase(mount,size,count,concurrency,'write',None)
        (case/'write-before.stats').write_text(before)
        (case/'write-after.stats').write_text(after)
        dump(case/'write.json',write)
        self.unmount(mount,proc)
        config['cache']['cache_root'] = str(case/'read-cache')
        proc = self.mount(config,case,'read-mount')
        phases = {}
        if mixed:
            result, hashes, before, after = io_phase(mount,size,count,4,'mixed',hashes,'overwrite')
            phases['mixed'] = result
            (case/'mixed-before.stats').write_text(before)
            (case/'mixed-after.stats').write_text(after)
            self.unmount(mount,proc)
            config['cache']['cache_root'] = str(case/'verify-cache')
            proc = self.mount(config,case,'verify-mount')
            verify,_,_,_ = io_phase(mount,size,count,4,'verify',hashes)
            phases['verification'] = verify
        else:
            for phase in ['client_cold_read','warm_read']:
                result,_,before,after = io_phase(mount,size,count,concurrency,phase,hashes)
                phases[phase] = result
                (case/(phase+'-before.stats')).write_text(before)
                (case/(phase+'-after.stats')).write_text(after)
        self.unmount(mount,proc)
        objects = self.validate_objects(bucket, hint)
        assert objects
        row = dict(number=number,round=round_id,size=size,count=count,concurrency=concurrency,hint=hint,
                   mixed=mixed,started=started,ended=time.time(),write=write,phases=phases,objects=objects,
                   expected_sha256=hashes,correctness='pass',unmount='pass')
        dump(case/'result.json',row)
        dump(case/'resources-after.json',resources())
        self.rows.append(row)
        dump(self.root/'results.json',self.rows)
        # These are solely this case's cache directories, already unmounted.
        for name in ['write-cache','read-cache','verify-cache']:
            path=case/name
            assert path.resolve().is_relative_to(self.root.resolve())
            if path.exists():
                shutil.rmtree(path)
        print(json.dumps(dict(event='case_complete',number=number,round=round_id,size=size,
                              concurrency=concurrency,hint=hint,mixed=mixed,objects=len(objects))),flush=True)

    def validate_objects(self, bucket, hint):
        objects = []
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            for obj in page.get('Contents',[]):
                if not obj['Key'].startswith('chunks-v2/'):
                    continue
                head = self.s3.head_object(Bucket=bucket,Key=obj['Key'])
                actual = int(head['ResponseMetadata']['HTTPHeaders']['x-rustfs-effective-ec-block-size'])
                expected = 4*MIB if hint == 'auto' and obj['Size'] > MIB else MIB
                assert actual == expected, (obj['Key'],obj['Size'],actual,expected)
                objects.append(dict(key=obj['Key'],bytes=obj['Size'],ec_bytes=actual))
        return objects

    def execute(self):
        number=0
        if self.smoke:
            for size in [4096,MIB,4*MIB]:
                for hint in ['off','auto']:
                    self.case(number,0,size,4,2,hint)
                    number+=1
            self.case(number,0,4*MIB,8,4,'auto',True)
            return
        for round_id in range(1,5):
            pairs=[(size,count,concurrency) for size,count in SIZES for concurrency in [1,4]]
            random.Random(8719+round_id).shuffle(pairs)
            for size,count,concurrency in pairs:
                pair_index=SIZES.index((size,count))*2+(concurrency==4)
                modes=['off','auto'] if (round_id+pair_index)%2 else ['auto','off']
                for hint in modes:
                    self.case(number,round_id,size,count,concurrency,hint)
                    number+=1
            for hint in (['off','auto'] if round_id%2 else ['auto','off']):
                self.case(number,round_id,4*MIB,64,4,hint,True)
                number+=1

    def close(self,success):
        cleanups=[]
        for mount,proc in list(self.mounts):
            try:
                self.unmount(mount,proc)
            except Exception as error:
                cleanups.append(dict(mount=str(mount),error=str(error)))
        for proc in reversed(self.processes):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    cleanups.append(dict(pid=proc.pid,error='service did not stop'))
        if self.redis_id:
            result=subprocess.run(['docker','rm','-f',self.redis_id],capture_output=True,text=True,timeout=30)
            cleanups.append(dict(redis_removed=result.returncode==0))
        for log in self.root.glob('*.log'):
            content=log.read_bytes()
            for key in ['AWS_ACCESS_KEY_ID','AWS_SECRET_ACCESS_KEY']:
                content=content.replace(self.env[key].encode(),b'[REDACTED]')
            log.write_bytes(content)
        dump(self.root/'closure.json',dict(success=success,completed_cases=len(self.rows),
             processes=[dict(pid=p.pid,exit=p.poll()) for p in self.processes],
             remaining_mounts=[str(p) for p,_ in self.mounts],cleanup=cleanups,
             retained_volumes=True,ended=time.time()))
        dump(self.root/'resources-after.json',resources())


if __name__ == '__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--smoke',action='store_true')
    args=parser.parse_args()
    experiment=Experiment(args.smoke)
    success=False
    try:
        experiment.start()
        experiment.execute()
        success=True
    except BaseException:
        (experiment.root/'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        experiment.close(success)

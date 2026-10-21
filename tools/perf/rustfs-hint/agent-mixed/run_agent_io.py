"""Replay a frozen mixed workload through a real BrewFS mount on ACS."""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path, PurePosixPath
import gc
import hashlib
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import traceback
import run_client as base

core = base.core
REQUEST = 131072
TAILS = {}


def digest_payload(version, size):
    # Version-specific first request, incompressible shared tail. This keeps
    # prepared inputs bounded without adding a tiny write or generation in timing.
    first=hashlib.shake_256(('brewfs-agent-mixed:' + version).encode()).digest(min(size,REQUEST))
    return (first,TAILS[size]) if size>REQUEST else (first,)


def payload_hash(parts):
    h=hashlib.sha256()
    for part in parts:
        h.update(part)
    return h.hexdigest()


def write_file(path, payload, temporary=None):
    started=time.monotonic_ns()
    with (temporary or path).open('xb' if temporary else 'wb', buffering=0) as stream:
        opened=time.monotonic_ns()
        for part in payload:
            view = memoryview(part)
            while view:
                n = stream.write(view[:REQUEST])
                assert n > 0
                view = view[n:]
        written=time.monotonic_ns()
        os.fsync(stream.fileno())
        synced=time.monotonic_ns()
    closed=time.monotonic_ns()
    if temporary:
        os.replace(temporary, path)
    renamed=time.monotonic_ns()
    return dict(open_ns=opened-started,write_ns=written-opened,fsync_ns=synced-written,
                close_ns=closed-synced,rename_ns=renamed-closed)


def read_file(path, size, buffer):
    view = memoryview(buffer)[:size]
    with path.open('rb', buffering=0) as stream:
        offset = 0
        while offset < size:
            n = stream.readinto(view[offset:min(offset+REQUEST,size)])
            assert n > 0, 'short file: ' + str(path)
            offset += n
        assert stream.read(1) == b'', 'excess file: ' + str(path)
    return view


def filesystem(path):
    return json.loads(subprocess.check_output(['findmnt','-J','-T',str(path),'-o','TARGET,FSTYPE,SOURCE,OPTIONS'],text=True))


class Experiment(base.Experiment):
    def __init__(self, job):
        core.BINARY = Path(job['brewfs_binary'])
        super().__init__(job)
        self.env.update(BREWFS_WRITEBACK_UPLOAD_CONCURRENCY='3')
        self.stop_sample = threading.Event()
        self.sampler = None
        self.sample_errors = []

    def progress(self, phase, **extra):
        core.dump(self.root/'progress.json',dict(phase=phase,time=time.time(),**extra))
        print(json.dumps(dict(job=self.job['id'],phase=phase,**extra)),flush=True)

    def config(self, mount, bucket):
        config=dict(mount_point=str(mount),fuse=dict(workers=4,max_background=64),
            data=dict(backend='s3',s3=dict(bucket=bucket,endpoint=self.endpoint,region='us-east-1',force_path_style=True)),
            meta=dict(backend='redis',redis=dict(url='redis://'+self.redis_host+':6379/0')),
            layout=dict(block_size=4194304,chunk_size=67108864),
            cache=dict(cache_root=str(self.root/'prefill-cache'),compression='none',
                writeback_mode='commit_before_upload',writeback_persist_sync=True,
                writeback_require_stage_before_commit=True,read_memory_bytes=67108864,
                read_ssd_bytes=0,write_memory_bytes=67108864,write_ssd_bytes=1073741824,
                memory_budget_bytes=536870912,dirty_slice_max_age_ms=2000,upload_concurrency=8,
                prefetch_enabled=True,range_background_prefetch=True,
                populate_write_cache_after_upload=True,persist_write_cache_after_upload=False))
        if self.job['arm']['hint']=='auto':
            config['data']['s3']['rustfs_ec_block_size_hint']='auto'
        return config

    def sample(self, mount, proc):
        paths=[Path('/proc/net/snmp'),Path('/proc/net/netstat')]
        paths += [Path(f'/proc/{proc.pid}')/n for n in ['stat','io','status']]
        with (self.root/'timeline.jsonl').open('x') as output:
            while not self.stop_sample.is_set():
                started=time.monotonic()
                try:
                    _,counters=core.metrics(mount)
                    row=dict(monotonic=started,utc_ns=time.time_ns(),counters=counters,
                             process_network={str(p):p.read_text() for p in paths},cgroups=base.cgroups())
                    row['sampling_seconds']=time.monotonic()-started
                    output.write(json.dumps(row)+'\n');output.flush()
                except BaseException:
                    self.sample_errors.append(traceback.format_exc())
                    return
                self.stop_sample.wait(max(0,.2-(time.monotonic()-started)))

    def execute(self):
        raw=Path(self.job['trace_path']).read_bytes()
        assert hashlib.sha256(raw).hexdigest()==self.job['trace_sha256']
        rows=[json.loads(line) for line in raw.splitlines()]
        corpus_raw=Path(self.job['corpus_path']).read_bytes()
        assert hashlib.sha256(corpus_raw).hexdigest()==self.job['corpus_sha256']
        initial=json.loads(corpus_raw)
        for size in set(initial.values()) | {r['bytes'] for r in rows}:
            if size>REQUEST:
                TAILS[size]=hashlib.shake_256(f'brewfs-agent-tail:{size}'.encode()).digest(size-REQUEST)
        states={p:dict(bytes=s,content_version='initial:'+p) for p,s in initial.items()}
        for row in rows:
            p=PurePosixPath(row['path'])
            assert not p.is_absolute() and '..' not in p.parts
            assert p.parts[0] in ['shared',f'worker-{row["worker"]}']
            if row['op']=='read':
                assert states[row['path']]=={k:row[k] for k in ['bytes','content_version']}
            else:
                assert p.parts[0]==f'worker-{row["worker"]}'
                assert (row['path'] in states)==(row['op']=='replace')
                states[row['path']]={k:row[k] for k in ['bytes','content_version']}
        expected_final=states
        bucket='agent-'+self.job['id']
        self.s3.create_bucket(Bucket=bucket)
        self.reset_metadata()
        mount=self.root/'mount'
        config=self.config(mount,bucket)
        self.progress('prefill',files=len(initial))
        proc=self.mount(config,self.root,'prefill')
        assert filesystem(mount)['filesystems'][0]['fstype'].startswith('fuse')
        parents={str(PurePosixPath(p).parent) for p in [*initial,*expected_final]}
        for p in sorted(parents):
            (mount/p).mkdir(parents=True,exist_ok=True)
        hashes={}
        def prefill(item):
            path,size=item;version='initial:'+path
            value=digest_payload(version,size)
            digest=payload_hash(value)
            write_file(mount/path,value)
            return version,digest
        with ThreadPoolExecutor(max_workers=4) as pool:
            hashes.update(pool.map(prefill,initial.items()))
        raw_stats,_,samples=core.drain(mount)
        (self.root/'prefill-drained.stats').write_text(raw_stats)
        core.dump(self.root/'prefill-drain.json',samples)
        self.unmount(mount,proc)

        self.progress('prepare-content',operations=len(rows))
        payloads={}
        for row in rows:
            if row['op']!='read':
                version=row['content_version']
                value=digest_payload(version,row['bytes'])
                hashes[version]=payload_hash(value)
                payloads[version]=value
        core.dump(self.root/'content-digests.json',hashes)
        core.dump(self.root/'payload-preparation.json',dict(
            first_request_bytes=REQUEST,first_request_varies_by_content_version=True,
            tails_shared_within_file_size=True,compression='none',generation_inside_timing=False,
            unique_prepared_payload_bytes=sum(len(p[0]) for p in payloads.values())+sum(map(len,TAILS.values())),
            note='No payload deduplication is configured in either backend; all bytes still traverse FUSE writes.'))
        config['cache']['cache_root']=str(self.root/'measured-cache')
        proc=self.mount(config,self.root,'measured')
        public=['BREWFS_WRITEBACK_UPLOAD_CONCURRENCY','BREWFS_FUSE_KEEP_CACHE','BREWFS_FUSE_WRITEBACK','BREWFS_FUSE_DIRECT_IO']
        env=dict(p.split(b'=',1) for p in Path(f'/proc/{proc.pid}/environ').read_bytes().split(b'\0') if b'=' in p)
        assert all(env[k.encode()].decode()==self.env[k] for k in public)
        core.dump(self.root/'mount-observation.json',dict(mount=filesystem(mount),stage_device=filesystem(self.root),
            public_environment={k:env[k.encode()].decode() for k in public},pid=proc.pid,
            process_executable=os.readlink(f'/proc/{proc.pid}/exe'),
            mountinfo=[l for l in Path('/proc/self/mountinfo').read_text().splitlines() if str(mount) in l]))
        partitions=[[r for r in rows if r['worker']==w] for w in range(4)]
        result_rows=[[] for _ in range(4)]
        barrier=threading.Barrier(5)
        abort=threading.Event()
        buffers=[bytearray(max(r['bytes'] for r in part)) for part in partitions]
        # Complete path/payload preparation before the common timed release.
        operations=[[(row,mount/row['path'],(mount/row['path']).with_name(f'.agent-tmp-w{w}-op{row["index"]}')) for row in part]
                    for w,part in enumerate(partitions)]
        gc.collect()
        def worker(w):
            barrier.wait()
            try:
                for row,path,temporary in operations[w]:
                    if abort.is_set():
                        break
                    begin=time.monotonic_ns()
                    if row['op']=='read':
                        content=read_file(path,row['bytes'],buffers[w])
                        components=None
                    else:
                        components=write_file(path,payloads[row['content_version']],temporary)
                    end=time.monotonic_ns()
                    cpu_before=time.thread_time_ns()
                    if row['op']=='read':
                        assert hashlib.sha256(content).hexdigest()==hashes[row['content_version']], ('content mismatch',row)
                    checked=time.monotonic_ns()
                    result_rows[w].append(dict(worker=w,index=row['index'],op=row['op'],bytes=row['bytes'],
                        start_ns=begin,end_ns=end,checked_ns=checked,verification_cpu_ns=time.thread_time_ns()-cpu_before,
                        write_components=components,full_read_hash_passed=row['op']=='read'))
            except BaseException:
                abort.set()
                raise
        before_raw,before=core.metrics(mount)
        (self.root/'before.stats').write_text(before_raw)
        (self.root/'redis-before.txt').write_text(subprocess.check_output(['redis-cli','-h',self.redis_host,'INFO','commandstats'],text=True))
        self.progress('measuring',operations=len(rows))
        self.sampler=threading.Thread(target=self.sample,args=(mount,proc),daemon=True)
        self.sampler.start()
        started=None
        try:
            with ThreadPoolExecutor(max_workers=4) as pool:
                futures=[pool.submit(worker,w) for w in range(4)]
                utc_ns=time.time_ns();started=time.monotonic_ns()
                barrier.wait()
                for future in futures:
                    future.result()
                active_end=time.monotonic_ns()
            after_raw,after,samples=core.drain(mount)
            drained_end=time.monotonic_ns()
        finally:
            self.stop_sample.set()
            self.sampler.join(timeout=10)
            assert not self.sampler.is_alive(), 'sampler did not close'
            core.dump(self.root/'operations.json',dict(start_ns=started,workers=result_rows))
            core.dump(self.root/'sampler-closure.json',dict(errors=self.sample_errors,closed=True))
        assert not self.sample_errors
        assert sum(map(len,result_rows))==len(rows)
        (self.root/'after.stats').write_text(after_raw)
        (self.root/'redis-after.txt').write_text(subprocess.check_output(['redis-cli','-h',self.redis_host,'INFO','commandstats'],text=True))
        result=dict(job=self.job['id'],arm=self.job['arm']['id'],round=self.job['round'],scene=self.job['scene'],
            operations=len(rows),logical_bytes=sum(r['bytes'] for r in rows),start_ns=started,start_utc_ns=utc_ns,
            active_end_ns=active_end,drained_end_ns=drained_end,active_seconds=(active_end-started)/1e9,
            total_seconds=(drained_end-started)/1e9,drain_seconds=(drained_end-active_end)/1e9,
            verification_cpu_seconds=sum(r['verification_cpu_ns'] for part in result_rows for r in part)/1e9,
            full_read_hash_checks=sum(r['op']=='read' for r in rows),counters_before=before,counters_after=after,
            counters_delta={k:v-before.get(k,0) for k,v in after.items()},drain_samples=samples)
        core.dump(self.root/'timing-result.json',result)
        payloads.clear();gc.collect()
        self.unmount(mount,proc)
        self.progress('verify-after-remount',files=len(expected_final))
        config['cache']['cache_root']=str(self.root/'verify-cache')
        proc=self.mount(config,self.root,'verify')
        verify_buffers=threading.local()
        def verify(item):
            path,state=item
            if not hasattr(verify_buffers,'value'):
                verify_buffers.value=bytearray(max(s['bytes'] for s in expected_final.values()))
            content=read_file(mount/path,state['bytes'],verify_buffers.value)
            assert hashlib.sha256(content).hexdigest()==hashes[state['content_version']],('remount mismatch',path)
        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(verify,expected_final.items()))
        observed={str(p.relative_to(mount)) for p in mount.rglob('*') if p.is_file() and p.name!='.stats'}
        assert observed==set(expected_final),('final namespace differs',list(observed-set(expected_final))[:10])
        self.unmount(mount,proc)
        self.progress('verify-layout')
        entries=[]
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            entries.extend(o for o in page.get('Contents',[]) if o['Key'].startswith('chunks-v2/'))
        def inspect(obj):
            h=self.s3.head_object(Bucket=bucket,Key=obj['Key'])
            assert h['ContentLength']==obj['Size']
            actual=h['ResponseMetadata']['HTTPHeaders'].get('x-rustfs-effective-ec-block-size')
            if actual is not None:
                assert int(actual)==1048576
            if self.job['arm']['hint']=='off':
                assert 'rustfs-ec-block-size-hint' not in h.get('Metadata',{})
            return dict(bucket=bucket,key=obj['Key'],bytes=obj['Size'],ec_bytes=int(actual) if actual else None,
                        stored_hint=h.get('Metadata',{}).get('rustfs-ec-block-size-hint'))
        with ThreadPoolExecutor(max_workers=8) as pool:
            objects=list(pool.map(inspect,entries))
        assert objects
        core.dump(self.root/'objects.json',objects)
        core.dump(self.root/'final-state.json',expected_final)
        result.update(correctness='pass',remount_full_file_hash_checks=len(expected_final),objects=len(objects),unmount='pass')
        self.rows.append(result)
        core.dump(self.root/'results.json',self.rows)
        for name in ['prefill-cache','measured-cache','verify-cache']:
            p=self.root/name
            assert p.resolve().is_relative_to(self.root.resolve())
            if p.exists():
                shutil.rmtree(p)
        self.progress('complete',files=len(expected_final))

    def close(self,success):
        self.stop_sample.set()
        if self.sampler:
            self.sampler.join(timeout=10)
        super().close(success)


if __name__=='__main__':
    experiment=Experiment(json.loads(Path(sys.argv[1]).read_text()))
    success=False
    try:
        experiment.start();experiment.execute();success=True
    except BaseException:
        (experiment.root/'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        experiment.close(success)

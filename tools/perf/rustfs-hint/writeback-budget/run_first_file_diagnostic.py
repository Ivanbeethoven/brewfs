"""Per-file attribution only: pauses settle counters and are not acceptance timing."""
from pathlib import Path
import hashlib
import json
import os
import resource
import sys
import time
import traceback
import run_client as base

job=json.loads(Path(sys.argv[1]).read_text());base.core.BINARY=Path(job['brewfs_binary'])
original_phase=base.core.io_phase
mount_processes={}


def process_snapshot(mount):
    if not job.get('record_brewfs_process'):
        return {}
    pid=mount_processes[str(mount)];root=Path('/proc')/str(pid)
    fields=(root/'stat').read_text().split(') ',1)[1].split()
    # /proc/PID/stat aggregates process CPU/faults; schedstat is per-thread.
    threads={}
    for task in (root/'task').iterdir():
        try:
            values=list(map(int,(task/'schedstat').read_text().split()))
            threads[task.name]=dict(runtime_ns=values[0],runqueue_ns=values[1],slices=values[2])
        except FileNotFoundError:
            pass
    return dict(pid=pid,starttime=fields[19],clock_ticks=os.sysconf('SC_CLK_TCK'),
        user_ticks=int(fields[11]),system_ticks=int(fields[12]),minor_faults=int(fields[7]),major_faults=int(fields[9]),
        io=(root/'io').read_text(),status=(root/'status').read_text(),threads=threads)

def diagnostic_phase(mount,size,count,concurrency,phase,expected,salt='initial'):
    if phase not in ['client_cold_read','warm_read']:
        return original_phase(mount,size,count,concurrency,phase,expected,salt)
    assert concurrency==1
    time.sleep(.15);before_raw,before=base.core.metrics(mount)
    start_utc=time.time_ns();start=time.monotonic();per_file=[]
    for index in range(count):
        _,a=base.core.metrics(mount);usage_a=resource.getrusage(resource.RUSAGE_SELF)
        process_before=process_snapshot(mount)
        pieces=[];blocks=[];file_start=time.time_ns();t=time.perf_counter_ns()
        with (mount/f'file-{index:04d}').open('rb',buffering=0) as stream:
            if job.get('record_block_timings'):
                for offset in range(0,size,4194304):
                    bt=time.perf_counter_ns();piece=stream.read(min(4194304,size-offset))
                    blocks.append(dict(offset=offset,bytes=len(piece),milliseconds=(time.perf_counter_ns()-bt)/1e6))
                    pieces.append(piece)
            else:content=stream.read()
        elapsed=(time.perf_counter_ns()-t)/1e9;usage_b=resource.getrusage(resource.RUSAGE_SELF)
        process_after=process_snapshot(mount)
        if job.get('record_block_timings'):
            content=b''.join(pieces);del pieces,piece
        assert len(content)==size and hashlib.sha256(content).hexdigest()==expected[index]
        del content
        time.sleep(.15);after_raw,b=base.core.metrics(mount)
        delta={k:v-a.get(k,0) for k,v in b.items() if k.endswith('_total')}
        per_file.append(dict(index=index,utc_start_ns=file_start,seconds=elapsed,counters_delta=delta,blocks=blocks,
            brewfs_before=process_before,brewfs_after=process_after,
            process_user_seconds=usage_b.ru_utime-usage_a.ru_utime,process_system_seconds=usage_b.ru_stime-usage_a.ru_stime,
            minor_faults=usage_b.ru_minflt-usage_a.ru_minflt,major_faults=usage_b.ru_majflt-usage_a.ru_majflt,
            voluntary_switches=usage_b.ru_nvcsw-usage_a.ru_nvcsw,involuntary_switches=usage_b.ru_nivcsw-usage_a.ru_nivcsw))
    wall=time.monotonic()-start;active=sum(r['seconds'] for r in per_file)
    result=dict(phase=phase,file_bytes=size,files=count,concurrency=1,phase_start_utc_ns=start_utc,
        phase_end_utc_ns=start_utc+int(wall*1e9),phase_start_monotonic=start,logical_bytes=size*count,
        active_seconds=active,total_seconds=wall,active_mib_s=size*count/base.core.MIB/active,
        total_mib_s=size*count/base.core.MIB/wall,drain_seconds=0,drain_samples=[],
        file_latencies_seconds=[r['seconds'] for r in per_file],full_file_hash_checks=count,
        counters_before=before,counters_after=b,counters_delta={k:v-before.get(k,0) for k,v in b.items()},
        per_file=per_file,diagnostic_only=True,settling_pause_seconds=.15,
        timing_scope='Each file open/read/close timed; hashing, allocation release, stats reads and settling pauses excluded. Not an acceptance throughput measurement.')
    return result,expected,before_raw,after_raw

base.core.io_phase=diagnostic_phase

class Diagnostic(base.Experiment):
    def __init__(self,j):
        super().__init__(j);self.env['BREWFS_WRITEBACK_UPLOAD_CONCURRENCY']='3'
    def mount(self,config,case,phase):
        proc=super().mount(config,case,phase)
        mount_processes[str(config['mount_point'])]=proc.pid
        return proc
    def validate_objects(self,bucket,hint):
        objects=[]
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            for obj in page.get('Contents',[]):
                if not obj['Key'].startswith('chunks-v2/'):continue
                head=self.s3.head_object(Bucket=bucket,Key=obj['Key'])
                actual=int(head['ResponseMetadata']['HTTPHeaders']['x-rustfs-effective-ec-block-size']);assert actual==1048576
                objects.append(dict(bucket=bucket,key=obj['Key'],bytes=obj['Size'],ec_bytes=actual))
        return objects
    def execute(self):
        for i,spec in enumerate(self.job['cases']):
            before=base.cgroups();self.case(i,self.job['round'],spec['size'],spec['count'],1,'auto',False)
            self.rows[-1].update(arm=self.job['arm']['id'],job=self.job['id'],kind='diagnostic',upload_permits=3)
            base.core.dump(self.root/f'case-{i:03d}/result.json',self.rows[-1])
            base.core.dump(self.root/f'case-{i:03d}/cgroups.json',dict(before=before,after=base.cgroups()))
            base.core.dump(self.root/'results.json',self.rows)
        base.core.dump(self.root/'objects.json',[o for r in self.rows for o in r['objects']])

experiment=Diagnostic(job);success=False
try:experiment.start();experiment.execute();success=True
except BaseException:
    (experiment.root/'failure.txt').write_text(traceback.format_exc());raise
finally:experiment.close(success)

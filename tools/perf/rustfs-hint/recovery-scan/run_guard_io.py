"""Whole-file old/new auto regressions; upload scheduling stays fixed at three."""
from pathlib import Path
import json
import sys
import time
import traceback
import run_client as base

job=json.loads(Path(sys.argv[1]).read_text())
base.core.BINARY=Path(job['brewfs_binary'])
original_phase=base.core.io_phase
current_kind='normal'

def measured_phase(*args,**kwargs):
    phase=args[4]; size=args[1]
    repeats=(4 if size<=65536 else 16 if size<=1048576 else 32) if current_kind=='resident' and phase=='warm_read' else 1
    phases=[original_phase(*args,**kwargs) for _ in range(repeats)]
    result,hashes,before,after=phases[0]
    if repeats>1:
        # Keep every individual timed repetition as well as its aggregate.
        result=dict(result,repetition_results=[p[0] for p in phases])
        for key in ['logical_bytes','files','active_seconds','total_seconds','full_file_hash_checks']:
            result[key]=sum(p[0][key] for p in phases)
        result['file_latencies_seconds']=[v for p in phases for v in p[0]['file_latencies_seconds']]
        result['active_mib_s']=result['logical_bytes']/base.core.MIB/result['active_seconds']
        result['total_mib_s']=result['active_mib_s']
        result['phase_end_utc_ns']=phases[-1][0]['phase_end_utc_ns']
    result['repetitions']=repeats
    samples=result['drain_samples']
    if samples:
        last_busy=max((i for i,s in enumerate(samples) if any(s['values'][k] for k in base.core.DEBT[1:])),default=-1)
        complete=samples[last_busy+3]['time']
        result['upload_seconds']=result['total_seconds']-(samples[-1]['time']-complete)
        result['upload_mib_s']=result['logical_bytes']/base.core.MIB/result['upload_seconds']
    time.sleep(.15)
    after,settled=base.core.metrics(args[0])
    result['settled_counters_delta']={k:v-result['counters_before'].get(k,0) for k,v in settled.items()}
    if current_kind=='resident' and phase=='warm_read':
        assert result['settled_counters_delta']['brewfs_fuse_read_ops_total']>0
        assert result['settled_counters_delta']['brewfs_s3_get_ops_total']==0
    return result,hashes,before,after

base.core.io_phase=measured_phase

class Experiment(base.Experiment):
    def __init__(self,job):
        super().__init__(job)
        if job['upload_permits'] is None:
            self.env.pop('BREWFS_WRITEBACK_UPLOAD_CONCURRENCY',None)
        else:
            self.env['BREWFS_WRITEBACK_UPLOAD_CONCURRENCY']=str(job['upload_permits'])

    def mount(self,config,case,phase):
        proc=super().mount(config,case,phase)
        fields=['BREWFS_WRITEBACK_UPLOAD_CONCURRENCY','BREWFS_FUSE_KEEP_CACHE','BREWFS_FUSE_WRITEBACK','BREWFS_FUSE_DIRECT_IO']
        actual=dict(v.split(b'=',1) for v in Path(f'/proc/{proc.pid}/environ').read_bytes().split(b'\0') if b'=' in v)
        public={k:actual[k.encode()].decode() if k.encode() in actual else None for k in fields}
        assert all(public[k]==self.env.get(k) for k in fields)
        base.core.dump(case/(phase+'-public-env.json'),dict(pid=proc.pid,environment=public))
        return proc

    def validate_objects(self,bucket,hint):
        assert hint=='auto'
        objects=[]
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            for obj in page.get('Contents',[]):
                if not obj['Key'].startswith('chunks-v2/'):continue
                head=self.s3.head_object(Bucket=bucket,Key=obj['Key'])
                assert head['ContentLength']==obj['Size']
                actual=int(head['ResponseMetadata']['HTTPHeaders']['x-rustfs-effective-ec-block-size'])
                assert actual==1048576
                stored=head.get('Metadata',{}).get('rustfs-ec-block-size-hint')
                assert stored==('1048576' if obj['Size']>1048576 else None)
                objects.append(dict(bucket=bucket,key=obj['Key'],bytes=obj['Size'],ec_bytes=actual,stored_hint=stored))
        return objects

    def execute(self):
        global current_kind
        for number,spec in enumerate(self.job['cases']):
            current_kind=spec.get('kind','normal')
            self.env['BREWFS_FUSE_KEEP_CACHE']='0' if current_kind=='resident' else '1'
            before=base.cgroups()
            self.case(number,self.job['round'],spec['size'],spec['count'],spec['concurrency'],'auto',spec['mixed'])
            self.rows[-1].update(arm=self.job['arm']['id'],job=self.job['id'],kind=current_kind,upload_permits=self.job['upload_permits'])
            base.core.dump(self.root/f'case-{number:03d}/result.json',self.rows[-1])
            base.core.dump(self.root/f'case-{number:03d}/cgroups.json',dict(before=before,after=base.cgroups()))
            base.core.dump(self.root/'results.json',self.rows)
        base.core.dump(self.root/'objects.json',[o for r in self.rows for o in r['objects']])

experiment=Experiment(job)
success=False
try:
    experiment.start();experiment.execute();success=True
except BaseException:
    (experiment.root/'failure.txt').write_text(traceback.format_exc())
    raise
finally:
    experiment.close(success)

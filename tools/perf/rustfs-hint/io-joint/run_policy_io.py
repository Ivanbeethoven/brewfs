"""Same whole-file timing, with the declared auto EC size checked per object."""
from pathlib import Path
import json
import sys
import time
import traceback
import run_client as base

job=json.loads(Path(sys.argv[1]).read_text())
base.core.BINARY=Path(job['brewfs_binary'])
original_phase=base.core.io_phase


def measured_phase(*args,**kwargs):
    result,hashes,before,after=original_phase(*args,**kwargs)
    samples=result['drain_samples']
    if samples:
        last_busy=max((i for i,s in enumerate(samples) if any(s['values'][k] for k in base.core.DEBT[1:])),default=-1)
        complete=samples[last_busy+3]['time']
        result['upload_seconds']=result['total_seconds']-(samples[-1]['time']-complete)
        result['upload_mib_s']=result['logical_bytes']/base.core.MIB/result['upload_seconds']
    time.sleep(.15)
    _,settled=base.core.metrics(args[0])
    result['settled_counters_delta']={k:v-result['counters_before'].get(k,0) for k,v in settled.items()}
    return result,hashes,before,after


base.core.io_phase=measured_phase


class Experiment(base.Experiment):
    def validate_objects(self,bucket,hint):
        assert hint=='auto'
        objects=[]
        for page in self.s3.get_paginator('list_objects_v2').paginate(Bucket=bucket):
            for obj in page.get('Contents',[]):
                if not obj['Key'].startswith('chunks-v2/'):continue
                head=self.s3.head_object(Bucket=bucket,Key=obj['Key'])
                assert head['ContentLength']==obj['Size']
                actual=int(head['ResponseMetadata']['HTTPHeaders']['x-rustfs-effective-ec-block-size'])
                expected=self.job['auto_ec_bytes'] if obj['Size']>base.core.MIB else base.core.MIB
                assert actual==expected,(obj['Size'],actual,expected)
                objects.append(dict(bucket=bucket,key=obj['Key'],bytes=obj['Size'],ec_bytes=actual))
        return objects


experiment=Experiment(job)
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

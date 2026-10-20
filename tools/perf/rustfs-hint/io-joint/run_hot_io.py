"""Resident-cache measurements: repeated full-file reads, no kernel data reuse."""
from pathlib import Path
import json
import sys
import time
import traceback
import run_client as base

job = json.loads(Path(sys.argv[1]).read_text())
base.core.BINARY = Path(job['brewfs_binary'])
original_phase = base.core.io_phase


def measured_phase(*args, **kwargs):
    phase = args[4] if len(args) > 4 else kwargs['phase']
    size = args[1]
    repeats = (4 if size <= 65536 else 16 if size <= 1024**2 else 32) if phase == 'warm_read' else 1
    phases = [original_phase(*args, **kwargs) for _ in range(repeats)]
    result, hashes, before, _ = phases[0]
    if repeats > 1:
        for key in ['logical_bytes','files','active_seconds','total_seconds','full_file_hash_checks']:
            result[key] = sum(p[0][key] for p in phases)
        result['file_latencies_seconds'] = [v for p in phases for v in p[0]['file_latencies_seconds']]
        result['active_mib_s'] = result['logical_bytes']/base.core.MIB/result['active_seconds']
        result['total_mib_s'] = result['active_mib_s']
    result['repetitions'] = repeats
    samples = result['drain_samples']
    if samples:
        last_busy = max((i for i,s in enumerate(samples) if any(s['values'][k] for k in base.core.DEBT[1:])),default=-1)
        complete = samples[last_busy+3]['time']
        result['upload_seconds'] = result['total_seconds']-(samples[-1]['time']-complete)
        result['upload_mib_s'] = result['logical_bytes']/base.core.MIB/result['upload_seconds']
    time.sleep(.15)
    after, settled = base.core.metrics(args[0])
    result['settled_counters_delta'] = {k:v-result['counters_before'].get(k,0) for k,v in settled.items()}
    if phase == 'warm_read':
        result['counters_after'] = settled
        result['counters_delta'] = result['settled_counters_delta']
        assert result['counters_delta']['brewfs_fuse_read_ops_total'] > 0
        assert result['counters_delta']['brewfs_s3_get_ops_total'] == 0
    return result, hashes, before, after


base.core.io_phase = measured_phase


class Experiment(base.Experiment):
    def __init__(self, job):
        super().__init__(job)
        self.env['BREWFS_FUSE_KEEP_CACHE'] = '0'


experiment = Experiment(job)
success = False
try:
    experiment.start()
    experiment.execute()
    success = True
except BaseException:
    (experiment.root/'failure.txt').write_text(traceback.format_exc())
    raise
finally:
    experiment.close(success)

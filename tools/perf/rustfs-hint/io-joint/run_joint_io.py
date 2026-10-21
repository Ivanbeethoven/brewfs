"""Whole-file measurements for the write and cache collaboration study."""
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
    result, hashes, before, after = original_phase(*args, **kwargs)
    samples = result['drain_samples']
    if samples:
        last_busy = max((i for i, s in enumerate(samples) if any(s['values'][k] for k in base.core.DEBT[1:])), default=-1)
        complete = samples[last_busy + 3]['time']
        result['upload_seconds'] = result['total_seconds'] - (samples[-1]['time'] - complete)
        result['upload_mib_s'] = result['logical_bytes'] / base.core.MIB / result['upload_seconds']
    time.sleep(.15)
    _, settled = base.core.metrics(args[0])
    result['settled_counters_delta'] = {k:v-result['counters_before'].get(k,0) for k,v in settled.items()}
    return result, hashes, before, after


base.core.io_phase = measured_phase


class Experiment(base.Experiment):
    def __init__(self, job):
        super().__init__(job)
        self.env['BREWFS_FUSE_KEEP_CACHE'] = '0' if job.get('resident_cache') else '1'

    def mount(self, config, case, phase):
        if self.job['arm']['hint'] not in ['auto', 'off']:
            config['data']['s3']['rustfs_ec_block_size_hint'] = int(self.job['arm']['hint'])
        return super().mount(config, case, phase)


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

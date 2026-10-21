"""Observe resident BrewFS cache reads with kernel data cache invalidated on open."""
from pathlib import Path
import json
import os
import sys
import time
import traceback
import run_client as base

job = json.loads(Path(sys.argv[1]).read_text())
base.core.BINARY = Path('/work/brewfs-joint')


class Probe(base.Experiment):
    def __init__(self, job):
        super().__init__(job)
        self.env['BREWFS_FUSE_KEEP_CACHE'] = '0'
        self.env['BREWFS_VFS_TIMING'] = '1'

    def mount(self, config, case, phase):
        proc = super().mount(config, case, phase)
        mount = Path(config['mount_point'])
        device = mount.stat().st_dev
        bdi = Path('/sys/class/bdi') / f'{os.major(device)}:{os.minor(device)}'
        names = ['read_ahead_kb', 'min_ratio', 'max_ratio']
        base.core.dump(case / (phase + '-kernel.json'), dict(
            mountinfo=[line for line in Path('/proc/self/mountinfo').read_text().splitlines() if str(mount) in line],
            bdi={n:(bdi/n).read_text().strip() for n in names if (bdi/n).exists()}))
        return proc

    def execute(self):
        for number, spec in enumerate(self.job['cases']):
            self.case(number, 0, spec['size'], spec['count'], spec['concurrency'], 'auto')
            base.core.dump(self.root / 'results.json', self.rows)
        base.core.dump(self.root / 'objects.json', [o for r in self.rows for o in r['objects']])


experiment = Probe(job)
success = False
try:
    experiment.start()
    experiment.execute()
    success = True
except BaseException:
    (experiment.root / 'failure.txt').write_text(traceback.format_exc())
    raise
finally:
    experiment.close(success)

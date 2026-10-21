"""Untimed request-shape diagnosis; operation logging is never used for acceptance."""
from pathlib import Path
import json
import sys
import traceback
import run_client as base

job = json.loads(Path(sys.argv[1]).read_text())
base.core.BINARY = Path('/work/brewfs-joint')


class Probe(base.Experiment):
    def __init__(self, job):
        super().__init__(job)
        self.env.update(BREWFS_FUSE_OP_LOG='1',
                        RUST_LOG='warn,asyncfuse::raw::logfs=debug,asyncfuse::raw::session=debug')

    def execute(self):
        for number, spec in enumerate(self.job['cases']):
            base.core.REQUEST_BYTES = spec['request_bytes']
            self.case(number, 0, spec['size'], spec['count'], spec['concurrency'], 'auto')
            self.rows[-1].update(application_request_bytes=spec['request_bytes'])
            base.core.dump(self.root / f'case-{number:03d}/result.json', self.rows[-1])
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

"""Trace only opened paths, leaving the unchanged replay and correctness checks intact."""
import json
from pathlib import Path
import sys
import time
import traceback
from run_agent_io import Experiment, core


class PathExperiment(Experiment):
    def spawn(self,args,name,env):
        if name.endswith('-measured'):
            args=['strace','-D','-f','-qq','-ttt','-T','-yy','-e','trace=openat',
                  '-o',str(self.root/'open-paths.log'),'--',*args]
        return super().spawn(args,name,env)


if __name__=='__main__':
    experiment=PathExperiment(json.loads(Path(sys.argv[1]).read_text()))
    success=False
    try:
        experiment.start()
        experiment.execute()
        # The real measured process has exited; the detached tracer can finish its log.
        time.sleep(.5)
        assert (experiment.root/'open-paths.log').stat().st_size>0
        core.dump(experiment.root/'profile-scope.json',dict(diagnostic_only=True,acceptance_sample=False,
            instrumented='openat of measured BrewFS and its threads only; prefill and final verification are untraced',
            interpretation='Paths/counts diagnose file-call amplification. Traced durations are not benchmark results.'))
        success=True
    except BaseException:
        (experiment.root/'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        experiment.close(success)

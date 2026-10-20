"""One diagnostic replay: aggregate file syscalls for the measured FUSE process."""
import json
import subprocess
import sys
import time
import traceback
from pathlib import Path
from run_agent_io import Experiment, core


class ProfiledExperiment(Experiment):
    def spawn(self, args, name, env):
        if name.endswith('-measured'):
            # -D keeps the real BrewFS process as the child seen by the runner;
            # environment, executable, mount and normal teardown checks remain.
            args = ['strace','-D','-f','-c','-w','-e',
                'trace=openat,close,read,write,pread64,pwrite64,readv,writev,fsync,fdatasync,lseek,rename,renameat,renameat2,unlink,unlinkat,fallocate',
                '-o',str(self.root/'stage-syscalls.txt'),'--',*args]
        return super().spawn(args,name,env)


if __name__ == '__main__':
    experiment=ProfiledExperiment(json.loads(Path(sys.argv[1]).read_text()))
    success=False
    try:
        for attempt in range(60):
            try:
                experiment.s3.list_buckets()
                break
            except Exception:
                time.sleep(1)
        else:
            raise RuntimeError('Diagnostic backend did not become ready')
        experiment.start()
        experiment.execute()
        # The detached tracer closes after the tracee exits on unmount.
        for attempt in range(50):
            p=experiment.root/'stage-syscalls.txt'
            if p.exists() and 'total' in p.read_text():
                break
            time.sleep(.1)
        else:
            raise RuntimeError('Diagnostic syscall summary did not close')
        core.dump(experiment.root/'profile-scope.json',dict(
            diagnostic_only=True,acceptance_sample=False,
            instrumented='measured BrewFS mount process and its threads; prefill and remount verification are untraced',
            interpretation='strace perturbs scheduling and elapsed times. Use syscall counts and relative wait locations for diagnosis only; retain the untraced formal campaign as the performance reference.',
            version=subprocess.check_output(['strace','--version'],text=True)))
        success=True
    except BaseException:
        (experiment.root/'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        experiment.close(success)

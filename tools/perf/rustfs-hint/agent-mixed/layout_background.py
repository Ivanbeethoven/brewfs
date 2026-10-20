"""Run the unchanged post-measurement layout audit once, independently of exec."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import traceback


def save(path,value):
    temporary=path.with_suffix(path.suffix+'.tmp')
    temporary.write_text(json.dumps(value,indent=2)+'\n')
    temporary.replace(path)


def main(job):
    assert job.replace('-','').isalnum()
    root=Path('/work/runs')/job
    assert root.is_dir()
    with (root/'layout-background.lock').open('a') as lock:
        try:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:
            return
        closure=root/'layout-background-closure.json'
        if closure.exists():
            return
        handle=dict(pid=os.getpid(),starttime=Path('/proc/self/stat').read_text().split(') ',1)[1].split()[19],started=time.time())
        save(root/'layout-background-handle.json',handle)
        try:
            receipt=root/'layout-audit.json'
            if not receipt.exists():
                assert not (root/'layouts.jsonl').exists(), 'Unclosed previous audit output; inspect it before proceeding'
                active=json.loads(Path('/work/active-server.json').read_text())
                assert active['job']['id']==job
                with (root/'layout-background-audit.log').open('xb') as log:
                    result=subprocess.run(['python3','/work/audit_policy_layout.py','/work/jobs/'+job+'-objects.json'],stdout=log,stderr=subprocess.STDOUT)
                assert result.returncode==0, 'Layout audit failed; original log retained'
            result=json.loads(receipt.read_text())
            assert result['passed']
            save(closure,dict(success=True,receipt=result,ended=time.time()))
        except BaseException:
            save(closure,dict(success=False,error=traceback.format_exc(),ended=time.time()))
            raise


if __name__=='__main__':
    main(sys.argv[1])

"""Task-scoped CLI transport, with no credential content in logs."""
from pathlib import Path
import json
import os
import subprocess
import tempfile

WORK = Path(__file__).resolve().parent
BASE = Path(os.environ.get('BENCH_BASE', str(WORK / 'runs'))).resolve()
OUT = Path(os.environ.get('BENCH_OUT', str(WORK / 'results'))).resolve()
NAMESPACE = os.environ.get('BENCH_NAMESPACE', 'brewfs-writeback-budget')
K = [os.environ.get('KUBECTL', 'kubectl'), '--kubeconfig', os.environ.get('BENCH_KUBECONFIG', ''), '-n', NAMESPACE]
LABEL = 'app.kubernetes.io/part-of=' + NAMESPACE
SERVER = 'budget-store'
CLIENT = 'budget-client'


def command(args, data=None, timeout=90):
    if not os.environ.get('BENCH_KUBECONFIG'):
        raise ValueError('Set BENCH_KUBECONFIG to the dedicated benchmark cluster config')
    # WSL-to-Windows subprocess pipes can stall on larger kubectl output.
    # File handles also close stdin explicitly, without changing remote work.
    with tempfile.TemporaryFile() as source, tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as error:
        if data is not None:
            source.write(data)
        source.seek(0)
        result = subprocess.run(K + args, stdin=source if data is not None else subprocess.DEVNULL,
                                stdout=output, stderr=error, timeout=timeout)
        output.seek(0)
        error.seek(0)
        if result.returncode:
            raise RuntimeError('kubectl failed: ' + error.read().decode(errors='replace')[-2000:])
        return output.read()


def execute(pod, args, data=None, timeout=90):
    return command(['exec'] + (['-i'] if data is not None else []) + [pod, '--'] + args, data, timeout)


def put(pod, path, data):
    code = 'import sys; from pathlib import Path; p=Path(sys.argv[1]); p.parent.mkdir(parents=True,exist_ok=True); p.write_bytes(sys.stdin.buffer.read())'
    execute(pod, ['python3', '-c', code, path], data)


def save(path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')

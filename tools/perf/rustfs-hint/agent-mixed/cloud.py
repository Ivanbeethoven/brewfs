"""Dedicated ACS campaign transport; credentials are never printed."""
from pathlib import Path
import json
import os
import subprocess
import tempfile

WORK = Path(__file__).resolve().parent
BASE = Path(os.environ.get('BENCH_BASE', str(WORK / 'runs'))).resolve()
OUT = Path(os.environ.get('BENCH_OUT', str(WORK / 'results'))).resolve()
NAMESPACE = os.environ.get('BENCH_NAMESPACE', 'brewfs-agent-mixed')
K = [os.environ.get('KUBECTL', 'kubectl'), '--kubeconfig', os.environ.get('BENCH_KUBECONFIG', ''), '-n', NAMESPACE]
LABEL = 'app.kubernetes.io/part-of=' + NAMESPACE
CLIENT, SERVER, REDIS = 'agent-client', 'agent-store', 'agent-redis'


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def api(*args):
    return json.loads(subprocess.check_output([os.environ.get('ALIYUN', 'aliyun'), *args], timeout=60))


def command(args, data=None, timeout=90):
    if not os.environ.get('BENCH_KUBECONFIG'):
        raise ValueError('Set BENCH_KUBECONFIG to the dedicated benchmark cluster config')
    with tempfile.TemporaryFile() as source, tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        if data is not None:
            source.write(data)
        source.seek(0)
        result = subprocess.run(K+args, stdin=source if data is not None else subprocess.DEVNULL,
                                stdout=out, stderr=err, timeout=timeout)
        out.seek(0); err.seek(0)
        if result.returncode:
            raise RuntimeError('kubectl failed: ' + err.read().decode(errors='replace')[-2000:])
        return out.read()


def execute(pod, args, data=None, timeout=90):
    return command(['exec'] + (['-i'] if data is not None else []) + [pod, '--'] + args, data, timeout)


def put(pod, path, data):
    execute(pod, ['python3', '-c',
                 'import sys;from pathlib import Path;p=Path(sys.argv[1]);p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(sys.stdin.buffer.read())',
                 path], data)


def load_binaries():
    """Local binary paths and immutable build identities, provided by the operator."""
    manifest = Path(os.environ['BENCH_BINARIES'])
    data = json.loads(manifest.read_text())
    assert set(data) == {'brewfs', 'candidate', 'compare_layout'}
    for name, item in data.items():
        assert len(item['sha256']) == 64 and all(c in '0123456789abcdef' for c in item['sha256'])
        path = Path(item['path']).expanduser()
        item['path'] = str((manifest.parent / path).resolve() if not path.is_absolute() else path)
    return data

"""Bind the frozen user-visible workload to this one actual ACS topology."""
import argparse
import hashlib
import json
from cloud import BASE, WORK, OUT, CLIENT, SERVER, REDIS, save


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main(stage):
    output=BASE/(stage+'-plan.json')
    assert not output.exists()
    protocol=json.loads((OUT/'experiment-plan.json').read_text())
    topology=json.loads((BASE/'topology.json').read_text())
    if stage=='formal':
        assert json.loads((BASE/'smoke/closure.json').read_text())['success']
        assert json.loads((BASE/'transport-probe-closure.json').read_text())['success']
    environment=dict(RUST_LOG='warn',RUSTFS_CONSOLE_ENABLE='false',RUSTFS_OBS_LOG_DIRECTORY='',
        RUSTFS_STORAGE_CLASS_STANDARD='EC:2',RUSTFS_GET_LOCKSTEP_DATA_SHARDS_ONLY_ENABLE='true',
        RUSTFS_EC_READ_QUANTUM_ENABLE='false',RUSTFS_EC_READ_QUANTUM_FLEET_CONFIRMED='false',
        RUSTFS_OBJECT_MMAP_READ_ENABLE='true',RUSTFS_OBJECT_FILE_CACHE_RECLAIM_READ_ENABLE='true',
        RUSTFS_OBJECT_FILE_CACHE_RECLAIM_WRITE_ENABLE='false',RUSTFS_OBJECT_FILE_CACHE_RECLAIM_THRESHOLD='4194304')
    skeletons=protocol['jobs'] if stage=='formal' else [dict(run=i+1,round=0,scene='smoke',arm=a,
         trace='smoke-trace.jsonl',trace_sha256=sha(BASE/'smoke-trace.jsonl')) for i,a in enumerate(['no-dynamic-block','dynamic-auto'])]
    jobs=[]
    corpus='corpus.json' if stage=='formal' else 'smoke-corpus.json'
    for item in skeletons:
        arm=item['arm'];on=arm=='dynamic-auto'
        public=dict(environment,RUSTFS_EC_BLOCK_SIZE_HINT_ENABLE=str(on).lower(),RUSTFS_EC_BLOCK_SIZE_HINT_FLEET_CONFIRMED=str(on).lower())
        jobs.append(dict(id=f'agent-{stage}-{item["run"]:02d}-{arm}',round=item['round'],scene=item['scene'],
            smoke=stage=='smoke',arm=dict(id=arm,binary='candidate',hint='auto' if on else 'off',environment=public),
            brewfs_binary='/work/brewfs',brewfs_sha256=protocol['brewfs_sha256'],backend_sha256=protocol['rustfs_sha256'],
            trace_path='/work/'+item['trace'],trace_sha256=item['trace_sha256'],corpus_path='/work/'+corpus,
            corpus_sha256=sha(BASE/corpus),endpoint='http://'+topology['services'][SERVER]+':9000',
            redis_ip=topology['services'][REDIS],auto_ec_bytes=1048576,network_diagnostic=True))
    names=['run_agent_io.py','run_core.py','run_client.py','server_control.py','sample_resources.py',
           'audit_policy_layout.py','campaign.py','cloud.py','prepare_jobs.py','analyze.py','verify_staged.py']
    save(output,dict(stage=stage,jobs=jobs,protocol_sha256=sha(OUT/'experiment-plan.json'),
        topology_sha256=sha(BASE/'topology.json'),execution_bindings_sha256=sha(BASE/'execution-bindings.json'),
        tools={n:sha(WORK/n) for n in names},
        rationale='Existing-code runtime ablation; paired inputs fixed before measurement; no new product candidate.'))
    print(json.dumps(dict(stage=stage,jobs=len(jobs),sha256=sha(output))),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('stage',choices=['smoke','formal']);main(p.parse_args().stage)

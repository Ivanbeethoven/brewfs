"""Prepare tools and smoke inputs locally, without executing the workload."""
import hashlib
import json
import shutil
from cloud import BASE, WORK, OUT, save
from freeze_workloads import SIZES, initial_corpus


def main():
    save(BASE/'corpus.json',initial_corpus())
    corpus={}
    rows=[]
    for worker in range(4):
        for size,_ in SIZES:
            shared=f'shared/size-{size}/file-0000.bin'
            private=f'worker-{worker}/size-{size}/file-0000.bin'
            created=f'worker-{worker}/generated/size-{size}.bin'
            corpus[shared]=corpus[private]=size
            replacement=f'smoke:replace:{worker}:{size}'
            creation=f'smoke:create:{worker}:{size}'
            for op,path,version in [('read',private,'initial:'+private),('read',shared,'initial:'+shared),
                                    ('replace',private,replacement),('read',private,replacement),
                                    ('create',created,creation),('read',created,creation)]:
                rows.append(dict(worker=worker,index=sum(r['worker']==worker for r in rows),op=op,
                                 bytes=size,path=path,content_version=version,locality='smoke'))
    save(BASE/'smoke-corpus.json',corpus)
    (BASE/'smoke-trace.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
    print(json.dumps(dict(smoke_operations=len(rows),smoke_initial_files=len(corpus),
                         formal_corpus_files=len(initial_corpus()))),flush=True)


if __name__=='__main__':
    main()

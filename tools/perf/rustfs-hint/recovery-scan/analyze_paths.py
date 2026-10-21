"""Summarize the one openat-path diagnostic without treating its time as performance."""
from collections import Counter, defaultdict
import ast
import hashlib
import json
from pathlib import PurePosixPath
import re
from cloud import BASE, OUT, save


def main():
    folder=BASE/'path-diagnostic'
    assert json.loads((folder/'closure.json').read_text())['success']
    root=folder/'raw/recovery-path-diagnostic-original/client'
    source=root/'open-paths.log'
    counts=Counter();paths=Counter();kinds=defaultdict(set);total=0;invocations=0
    for line in source.read_text().splitlines():
        invocations += ' openat(' in line
        # Count the invocation line once, including unfinished calls. Resumed lines
        # have no new openat( invocation and must not be counted a second time.
        match=re.search(r'openat\(([^,]+), "([^"\\]*(?:\\.[^"\\]*)*)", (.*)',line)
        if not match:
            continue
        directory,encoded,flags=match.groups()
        # strace uses C/octal escapes for some directory names, not JSON escapes.
        path=ast.literal_eval('"'+encoded+'"')
        if not path.startswith('/') and '<' in directory:
            path=str(PurePosixPath(directory.split('<',1)[1].rsplit('>',1)[0])/path)
        readonly='O_RDONLY' in flags
        if '/dirty/' in path and path.endswith('.meta') and readonly:
            category='dirty_metadata_read'
        elif '/dirty/' in path and path.endswith('.meta.tmp'):
            category='dirty_metadata_write'
        elif '/dirty/' in path and path.endswith(('.slice','.sealed')):
            category='dirty_slice_data'
        elif '/dirty' in path and 'O_DIRECTORY' in flags:
            category='dirty_directory_scan'
        elif 'O_DIRECTORY' in flags:
            category='other_directory'
        else:
            category='other_file'
        total+=1;counts[category]+=1;paths[path]+=1;kinds[category].add(path)
    assert total == invocations, (total, invocations)
    result=json.loads((root/'results.json').read_text())[0]
    stage=result['counters_delta']['brewfs_writeback_stage_ops_total']
    record=dict(diagnostic_only=True,acceptance_sample=False,job='recovery-path-diagnostic-original',
        total_openat_calls=total,categories={k:dict(calls=v,distinct_paths=len(kinds[k]),fraction=v/total) for k,v in counts.items()},
        stage_operations=stage,readonly_metadata_opens_per_stage=counts['dirty_metadata_read']/stage,
        most_reopened_paths=[dict(path=p,calls=n) for p,n in paths.most_common(20)],
        trace_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
        scope='Measured BrewFS process and its threads only, unchanged release binary, original edit/auto round1 trace.',
        interpretation='Counts identify on-disk metadata amplification. Code attribution uses read_meta callers; no wall-time speedup is inferred from strace. A new untraced paired candidate measurement is still required.')
    save(BASE/'path-analysis.json',record);save(OUT/'path-analysis.json',record)
    print(json.dumps({k:v for k,v in record.items() if k not in ['most_reopened_paths','interpretation']}),flush=True)


if __name__=='__main__':
    main()

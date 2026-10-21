"""Export raw measurements and rejected preflights; verify the complete archive."""
import hashlib
import json
import shutil
import tarfile
from cloud import BASE, OUT, WORK, save


def main():
    assert json.loads((BASE/'formal/closure.json').read_text())['success']
    assert json.loads((BASE/'independent-audit.json').read_text())['passed']
    assert not (OUT/'export.json').exists(), 'An export already exists; inspect it instead of replacing it'
    secrets = list(json.loads((BASE/'.private-auth.json').read_text()).values())
    files = []
    def copy(source, relative):
        data = source.read_bytes()
        for secret in secrets:
            assert secret.encode() not in data, 'Private authentication in an export source; do not publish'
        target = OUT/relative
        target.parent.mkdir(parents=True,exist_ok=True)
        target.write_bytes(data)
        files.append(dict(path=str(relative),bytes=len(data),sha256=hashlib.sha256(data).hexdigest()))
    suffixes = {'.json','.jsonl','.log','.stats','.txt','.md','.patch','.env','.yaml','.py','.sh'}
    for source in sorted(BASE.rglob('*')):
        relative=source.relative_to(BASE)
        complete_raw=relative.parts[0] in {'formal','smoke','stage-profile'} and 'raw' in relative.parts
        if source.is_file() and (complete_raw or (not source.name.startswith('.') and source.suffix in suffixes)):
            copy(source, 'records'/source.relative_to(BASE))
    for source in sorted(WORK.glob('*.py')):
        copy(source, 'tools'/source.relative_to(WORK))
    for name in ['formal-analysis.json','formal-components.json','formal-bottlenecks.json',
                 'independent-audit.json','object-layout-distribution.json']:
        shutil.copyfile(BASE/name, OUT/name)
    archive = OUT/'raw-records.tar.gz'
    with tarfile.open(archive,'w:gz',compresslevel=3) as tar:
        tar.add(OUT/'records',arcname='records')
        tar.add(OUT/'tools',arcname='tools')
        tar.add(OUT/'traces',arcname='traces')
    for source in (OUT/'traces').glob('*'):
        files.append(dict(path=str(source.relative_to(OUT)),bytes=source.stat().st_size,
                          sha256=hashlib.sha256(source.read_bytes()).hexdigest()))
    expected={e['path']:e for e in files}
    seen=set()
    with tarfile.open(archive,'r|gz') as tar:
        for member in tar:
            if member.isfile():
                assert member.name not in seen
                entry=expected[member.name]
                assert member.size==entry['bytes']
                assert hashlib.file_digest(tar.extractfile(member),'sha256').hexdigest()==entry['sha256']
                seen.add(member.name)
    assert seen==set(expected)
    with archive.open('rb') as stream:
        digest=hashlib.file_digest(stream,'sha256').hexdigest()
    save(OUT/'export.json',dict(passed=True,files=files,archive_sha256=digest,
        passed_refers_to='Export integrity only; performance verdict remains in formal-analysis.json',
        excluded='Private authentication, kubeconfig, compiled binaries and upload tarballs. Binary paths and SHA256 are in binaries.json.'))
    print(json.dumps(dict(exported=True,files=len(files),archive_bytes=archive.stat().st_size,sha256=digest)))


if __name__=='__main__':
    main()

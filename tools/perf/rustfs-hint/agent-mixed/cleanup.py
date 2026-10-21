"""Remove only recorded benchmark resources after a hash-verified export."""
import argparse
import hashlib
import json

from cloud import BASE, OUT, NAMESPACE, SERVER, command, execute, save


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    identities = json.loads((BASE / 'deployment-identities.json').read_text())
    if not args.execute:
        print(json.dumps([dict(kind=r['kind'], name=r['metadata']['name'], uid=r['metadata']['uid']) for r in identities], indent=2))
        return
    assert json.loads((BASE / 'formal/closure.json').read_text())['success']
    receipt = json.loads((OUT / 'export.json').read_text())
    with (OUT / 'raw-records.tar.gz').open('rb') as stream:
        assert receipt['passed'] and hashlib.file_digest(stream, 'sha256').hexdigest() == receipt['archive_sha256']
    execute(SERVER, ['test', '!', '-f', '/work/active-server.json'])
    # Read back all identities before deleting any object.
    for record in identities:
        current = json.loads(command(['get', record['kind'], record['metadata']['name'], '-o', 'json']))
        assert current['metadata']['uid'] == record['metadata']['uid']
    for kind in ['Pod', 'Service', 'PersistentVolumeClaim', 'Secret', 'StorageClass', 'Namespace']:
        for record in identities:
            if record['kind'] == kind:
                command(['delete', kind, record['metadata']['name'], '--wait=true', '--timeout=60s'], timeout=80)
    save(OUT / 'workloads-removed.json', dict(passed=True, namespace=NAMESPACE,
         remaining='Check CSI/provider disk release. Cluster and shared network were not deleted.'))
    print('Recorded namespace resources removed. Verify all four CSI disks are released in ACS.')


if __name__ == '__main__':
    main()

"""Render or deploy the dedicated ACS benchmark namespace into an existing cluster."""
import argparse
import json
import os
import re
import secrets

from cloud import BASE, WORK, NAMESPACE, LABEL, command, save


def resources(zone, registry):
    assert re.fullmatch(r'[a-z0-9][a-z0-9-]{0,42}', NAMESPACE)
    assert NAMESPACE.startswith('brewfs-')
    text = (WORK / 'acs-resources.json').read_text()
    for key, value in {'NAMESPACE': NAMESPACE, 'ZONE': zone, 'REGISTRY': registry}.items():
        # Substitute only these placeholders; shell expansion in Pod setup is literal.
        text = text.replace('${' + key + '}', json.dumps(value)[1:-1])
    return json.loads(text)


def identity(resource):
    # Ownership records are public export inputs, so never persist Secret.data.
    return dict(kind=resource['kind'], metadata={key: resource['metadata'][key] for key in ['name', 'uid']})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--zone', required=True)
    parser.add_argument('--registry', default='docker.io')
    parser.add_argument('--execute', action='store_true', help='Create billable Pods and four 40 GiB disks')
    args = parser.parse_args()
    plan = resources(args.zone, args.registry)
    if not args.execute:
        print(json.dumps(plan, indent=2))
        return
    BASE.mkdir(parents=True, exist_ok=True)
    assert not (BASE / 'resources-plan.json').exists(), 'Inspect the existing deployment instead of applying twice'
    # Require an unused namespace; its Redis database is reset between jobs.
    existing = json.loads(command(['get', 'namespace', NAMESPACE, '--ignore-not-found', '-o', 'json']) or b'{}')
    assert not existing, 'Use a new dedicated namespace'
    namespace = dict(apiVersion='v1', kind='Namespace', metadata=dict(name=NAMESPACE, labels={'app.kubernetes.io/part-of': NAMESPACE}))
    command(['create', '-f', '-'], json.dumps(namespace).encode())
    auth = dict(access=secrets.token_hex(12), secret=secrets.token_hex(24))
    with os.fdopen(os.open(BASE / '.private-auth.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
        json.dump(auth, stream)
    secret = dict(apiVersion='v1', kind='Secret', metadata=dict(name='agent-auth', namespace=NAMESPACE, labels=namespace['metadata']['labels']), type='Opaque', stringData=auth)
    command(['create', '-f', '-'], json.dumps(secret).encode())
    save(BASE / 'resources-plan.json', plan)
    print(command(['create', '-f', '-'], json.dumps(plan).encode()).decode())
    # Save immutable identities so cleanup cannot delete replacements with reused names.
    observed = json.loads(command(['get', 'pods,pvc,svc,secret', '-l', LABEL, '-o', 'json']))
    classes = json.loads(command(['get', 'storageclass', NAMESPACE + '-essd', '-o', 'json']))
    namespace = json.loads(command(['get', 'namespace', NAMESPACE, '-o', 'json']))
    save(BASE / 'deployment-identities.json', [identity(r) for r in observed['items'] + [classes, namespace]])


if __name__ == '__main__':
    main()

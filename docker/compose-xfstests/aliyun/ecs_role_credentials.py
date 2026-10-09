#!/usr/bin/env python3
"""AWS credential_process adapter for Aliyun ECS RAM role (no static credentials)."""
import argparse
import json
import os
import sys
import urllib.request


def credentials(role, opener=None):
    if not role or not all(c.isalnum() or c in '-_' for c in role):
        raise ValueError('invalid instance role name')
    opener = opener or urllib.request.build_opener(urllib.request.ProxyHandler({}))
    token_request = urllib.request.Request('http://100.100.100.200/latest/api/token', method='PUT', headers={'X-aliyun-ecs-metadata-token-ttl-seconds':'900'})
    with opener.open(token_request, timeout=5) as response:
        token=response.read(4096).decode().strip()
    if not token:
        raise ValueError('empty IMDSv2 token')
    request=urllib.request.Request('http://100.100.100.200/latest/meta-data/ram/security-credentials/'+role, headers={'X-aliyun-ecs-metadata-token':token})
    with opener.open(request, timeout=5) as response:
        value=json.loads(response.read(16384))
    if value.get('Code')!='Success' or not all(value.get(k) for k in ('AccessKeyId','AccessKeySecret','SecurityToken','Expiration')):
        raise ValueError('instance-role credentials unavailable')
    return {'Version':1,'AccessKeyId':value['AccessKeyId'],'SecretAccessKey':value['AccessKeySecret'],'SessionToken':value['SecurityToken'],'Expiration':value['Expiration']}


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--role',default=os.getenv('BREWFS_TEST_RAM_ROLE'))
    args=parser.parse_args()
    try:
        value=credentials(args.role)
    except Exception:
        print('instance-role credential process failed',file=sys.stderr)
        return 1
    # This output is consumed only by the SDK pipe; never tee/log/capture it as
    # an artifact or call this program in a diagnostics-printing subprocess.
    print(json.dumps(value,separators=(',',':')))
    return 0


if __name__=='__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""Bounded Aliyun campaign control plane. Never copies long-lived credentials."""
import argparse
import datetime
import json
import pathlib
import re
import subprocess
import time
import uuid


class ApiError(RuntimeError):
    pass


class Aliyun:
    def __init__(self, executable):self.executable=str(executable)
    def call(self, service, operation, **params):
        args=[self.executable,service,operation]
        for name,value in params.items():
            if value is not None:args.extend(['--'+name,json.dumps(value,separators=(',',':')) if isinstance(value,(dict,list)) else str(value)])
        process=subprocess.run(args,capture_output=True,timeout=120)
        try:value=json.loads(process.stdout.decode('utf-8-sig'))
        except ValueError:
            text=(process.stdout+process.stderr).decode(errors='replace')
            match=re.search(r'(?im)^\s*(?:ERROR:)?\s*(?:ErrorCode|Code)\s*[:=]\s*([A-Za-z0-9_.-]+)',text)
            raise ApiError(f'{service}.{operation}: {match.group(1) if match else "unclassified API failure"}') from None
        if process.returncode or 'Code' in value and 'Message' in value:
            code=str(value.get('Code','failed'))
            raise ApiError(f'{service}.{operation}: {code if re.fullmatch(r"[A-Za-z0-9_.-]+",code) else "failed"}')
        return value
    def oss(self, *arguments):
        process=subprocess.run([self.executable,'oss',*map(str,arguments)],capture_output=True,timeout=180)
        if process.returncode:raise ApiError('OSS operation failed; output withheld')
        return process.stdout.decode('utf-8-sig',errors='replace')


def role_policy(bucket,prefix):
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,62}',bucket) or not prefix.startswith('brewfs-campaign-') or not re.fullmatch(r'[A-Za-z0-9_-]+',prefix):
        raise ValueError('test prefix/bucket failed ownership validation')
    resource=f'acs:oss:*:*:{bucket}'
    return {'Version':'1','Statement':[
        {'Effect':'Allow','Action':['oss:GetObject','oss:PutObject','oss:AbortMultipartUpload','oss:ListParts'],'Resource':[resource+'/'+prefix+'/*']},
        {'Effect':'Allow','Action':['oss:ListObjects','oss:ListMultipartUploads'],'Resource':[resource],'Condition':{'StringLike':{'oss:Prefix':[prefix+'/',prefix+'/*']}}},
        {'Effect':'Allow','Action':['oss:GetBucketInfo'],'Resource':[resource]},
    ]}


def role_trust():
    return {'Version':'1','Statement':[{'Effect':'Allow','Action':'sts:AssumeRole','Principal':{'Service':['ecs.aliyuncs.com']}}]}


class Campaign:
    def __init__(self, api, artifact, bucket, region='cn-hangzhou'):
        self.api=api;self.artifact=pathlib.Path(artifact);self.bucket=bucket;self.region=region
        self.artifact.mkdir(parents=True,exist_ok=True)
        self.run_id=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-'+uuid.uuid4().hex[:8]
        self.prefix='brewfs-campaign-'+self.run_id
        self.role='brewfs-campaign-'+uuid.uuid4().hex[:20]
        self.policy=self.role+'-oss'
        self.state={'run_id':self.run_id,'prefix':self.prefix,'bucket':bucket,'region':region,'role':self.role,'policy':self.policy,'role_created':False,'policy_created':False,'policy_attached':False,'instance_id':None,'instance_creation_started':False,'cleanup':{}}
        self.save()
    def save(self):
        path=self.artifact/'resource-journal.json';tmp=path.with_suffix('.tmp');tmp.write_text(json.dumps(self.state,sort_keys=True,indent=2)+'\n');tmp.replace(path)
    def create_identity(self):
        # Persist intended unique names before each API. An ambiguous network
        # outcome is reconciled by exact owned names, never by account-wide lists.
        self.state['role_creation_started']=True;self.save()
        self.api.call('ram','CreateRole',RoleName=self.role,AssumeRolePolicyDocument=role_trust(),Description='Temporary isolated BrewFS validation role',MaxSessionDuration=3600)
        self.state['role_created']=True;self.save()
        self.state['policy_creation_started']=True;self.save()
        self.api.call('ram','CreatePolicy',PolicyName=self.policy,PolicyDocument=role_policy(self.bucket,self.prefix),Description='Temporary BrewFS run-prefix-only OSS policy')
        self.state['policy_created']=True;self.save()
        self.api.call('ram','AttachPolicyToRole',PolicyType='Custom',PolicyName=self.policy,RoleName=self.role)
        self.state['policy_attached']=True;self.save()
    def cleanup_identity(self):
        failures=[]
        if self.state.get('policy_creation_started'):
            try:self.api.call('ram','DetachPolicyFromRole',PolicyType='Custom',PolicyName=self.policy,RoleName=self.role)
            except ApiError as error:
                if not any(x in str(error) for x in ['EntityNotExist','NoSuchEntity']):failures.append(str(error))
            try:self.api.call('ram','DeletePolicy',PolicyName=self.policy)
            except ApiError as error:
                if not any(x in str(error) for x in ['EntityNotExist','NoSuchEntity']):failures.append(str(error))
        if self.state.get('role_creation_started'):
            try:self.api.call('ram','DeleteRole',RoleName=self.role)
            except ApiError as error:
                if not any(x in str(error) for x in ['EntityNotExist','NoSuchEntity']):failures.append(str(error))
        for operation,parameters in [('GetRole',dict(RoleName=self.role)),('GetPolicy',dict(PolicyType='Custom',PolicyName=self.policy))]:
            try:
                self.api.call('ram',operation,**parameters)
                failures.append(operation+': resource still exists')
            except ApiError as error:
                if not any(x in str(error) for x in ['EntityNotExist','NoSuchEntity']):failures.append(str(error))
        self.state['cleanup']['identity_failures']=failures;self.save()
        if failures:raise ApiError('temporary identity cleanup incomplete; see resource journal')


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--aliyun',type=pathlib.Path,required=True)
    parser.add_argument('--bucket',default='brewfs-tikv-100k-20260928')
    parser.add_argument('--artifact',type=pathlib.Path,required=True)
    parser.add_argument('--action',choices=['dry-run','identity-check'],default='dry-run')
    args=parser.parse_args()
    campaign=Campaign(Aliyun(args.aliyun),args.artifact,args.bucket)
    if args.action=='dry-run':
        role_policy(args.bucket,campaign.prefix)
        print(json.dumps(dict(action='dry-run',max_files=100000,max_instance_hours=6,instances=1,credential_transport='IMDSv2 credential_process',long_lived_credential_upload=False)))
        return 0
    try:
        campaign.create_identity()
        print('temporary prefix-scoped RAM identity creation and attachment succeeded')
    finally:
        campaign.cleanup_identity()
    print('temporary RAM identity removed; no ECS instance created')
    return 0


if __name__=='__main__':
    try:raise SystemExit(main())
    except ApiError as error:
        print(str(error))
        raise SystemExit(1)

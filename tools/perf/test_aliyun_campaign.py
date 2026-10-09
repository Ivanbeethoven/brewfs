import importlib.util
import pathlib
import tempfile
import unittest

PATH=pathlib.Path(__file__).resolve().parents[2]/'docker/compose-xfstests/aliyun/run_packed_campaign.py'
SPEC=importlib.util.spec_from_file_location('campaign',PATH)
MODULE=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class FakeApi:
    def __init__(self):self.calls=[]
    def call(self,service,operation,**params):
        self.calls.append((service,operation,params))
        if operation in ['GetRole','GetPolicy']:raise MODULE.ApiError('EntityNotExist')
        return {}


class CampaignTests(unittest.TestCase):
    def test_policy_never_authorizes_other_prefix_or_bucket_deletion(self):
        policy=MODULE.role_policy('brewfs-test','brewfs-campaign-unique')
        actions=[a for statement in policy['Statement'] for a in statement['Action']]
        self.assertNotIn('oss:*',actions)
        self.assertNotIn('oss:DeleteBucket',actions)
        self.assertNotIn("oss:DeleteObject",actions, "runtime ECS credentials must not delete committed objects")
        self.assertEqual(policy['Statement'][0]['Resource'],['acs:oss:*:*:brewfs-test/brewfs-campaign-unique/*'])
        with self.assertRaises(ValueError):MODULE.role_policy('brewfs-test','user-owned')
    def test_owned_identity_has_durable_journal_and_exact_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            api=FakeApi();campaign=MODULE.Campaign(api,directory,'brewfs-test')
            campaign.create_identity();campaign.cleanup_identity()
            self.assertEqual([c[1] for c in api.calls],['CreateRole','CreatePolicy','AttachPolicyToRole','DetachPolicyFromRole','DeletePolicy','DeleteRole','GetRole','GetPolicy'])
            self.assertTrue((pathlib.Path(directory)/'resource-journal.json').exists())
            self.assertTrue(all(c[2].get('RoleName',campaign.role)==campaign.role for c in api.calls))
    def test_dry_run_construction_creates_no_cloud_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            api=FakeApi();MODULE.Campaign(api,directory,'brewfs-test');self.assertEqual(api.calls,[])


if __name__=='__main__':unittest.main()

import importlib.util
import json
import pathlib
import unittest

PATH=pathlib.Path(__file__).resolve().parents[2]/'docker/compose-xfstests/aliyun/ecs_role_credentials.py'
SPEC=importlib.util.spec_from_file_location('ecs_credentials',PATH)
MODULE=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Response:
    def __init__(self,value):self.value=value
    def __enter__(self):return self
    def __exit__(self,*args):pass
    def read(self,*args):return self.value


class RoleTests(unittest.TestCase):
    def test_imdsv2_credential_process_contract(self):
        class Opener:
            calls=[]
            def open(self,request,timeout):
                self.calls.append(request)
                if request.get_method()=='PUT':return Response(b'synthetic-imds-token')
                return Response(json.dumps(dict(Code='Success',AccessKeyId='synthetic-id',AccessKeySecret='synthetic-secret',SecurityToken='synthetic-sts',Expiration='2030-01-01T00:00:00Z')).encode())
        opener=Opener();value=MODULE.credentials('brewfs-test-role',opener)
        self.assertEqual(value['Version'],1)
        self.assertEqual(value['SessionToken'],'synthetic-sts')
        self.assertEqual(len(opener.calls),2)
        self.assertTrue(any(k.lower()=='x-aliyun-ecs-metadata-token' for k in opener.calls[1].headers))

    def test_rejects_path_escape_and_failed_role_response(self):
        with self.assertRaises(ValueError):MODULE.credentials('../another-role')
        class Opener:
            def open(self,request,timeout):return Response(b'token' if request.get_method()=='PUT' else b'{"Code":"Failed"}')
        with self.assertRaises(ValueError):MODULE.credentials('valid-role',Opener())


if __name__=='__main__':unittest.main()

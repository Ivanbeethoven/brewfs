"""Offline checks: frozen traces, ownership, and portable ACS configuration."""
import hashlib
import json
from pathlib import Path
import unittest

from freeze_workloads import SCENES, SIZES, initial_corpus, trace, encoded, audit
from deploy import identity, resources


class WorkloadTests(unittest.TestCase):
    def test_ownership_receipt_does_not_export_secret_data(self):
        secret = dict(kind='Secret', metadata=dict(name='agent-auth', uid='unique-id', annotations={'credential': 'private'}), data={'secret': 'base64-private'})
        self.assertEqual(identity(secret), dict(kind='Secret', metadata=dict(name='agent-auth', uid='unique-id')))

    def test_all_frozen_traces_match_measured_inputs(self):
        expected = json.loads(Path(__file__).with_name('reference-traces.json').read_text())
        seen = {}
        for round_number in range(1, 7):
            for scene_index, (scene, _, reads) in enumerate(SCENES):
                rows = trace(round_number, scene_index, reads)
                summary = audit(rows, reads)
                self.assertEqual(summary['operations'], 8000)
                self.assertEqual(summary['logical_bytes'], 3842.5 * 1048576)
                seen[f'traces/round-{round_number}-{scene}.jsonl'] = hashlib.sha256(b''.join(encoded(r) for r in rows)).hexdigest()
        self.assertEqual(seen, expected)
        self.assertEqual(len(initial_corpus()), 3000)

    def test_wrong_read_version_is_rejected(self):
        rows = trace(1, 0, 90)
        next(r for r in rows if r['op'] == 'read')['content_version'] = 'wrong-content'
        with self.assertRaises(AssertionError):
            audit(rows, 90)

    def test_cross_worker_write_is_rejected(self):
        rows = trace(1, 0, 90)
        row = next(r for r in rows if r['worker'] == 0 and r['op'] == 'replace')
        row['path'] = row['path'].replace('worker-0/', 'worker-1/')
        with self.assertRaises(AssertionError):
            audit(rows, 90)

    def test_manifest_has_equal_fixed_resources_and_no_credentials(self):
        plan = resources('example-zone', 'example.invalid')
        pods = {r['metadata']['name']: r for r in plan['items'] if r['kind'] == 'Pod'}
        client = pods['agent-client']['spec']['containers'][0]
        self.assertEqual(client['resources']['limits'], dict(cpu='4', memory='8Gi'))
        self.assertIn('SYS_ADMIN', client['securityContext']['capabilities']['add'])
        self.assertEqual(pods['agent-store']['spec']['nodeSelector']['topology.kubernetes.io/zone'], 'example-zone')
        disks = [r for r in plan['items'] if r['kind'] == 'PersistentVolumeClaim']
        self.assertEqual(len(disks), 4)
        self.assertTrue(all(r['spec']['resources']['requests']['storage'] == '40Gi' for r in disks))
        self.assertNotIn('${', json.dumps(plan))
        self.assertTrue(all('value' not in e for e in pods['agent-store']['spec']['containers'][0]['env']))


if __name__ == '__main__':
    unittest.main()

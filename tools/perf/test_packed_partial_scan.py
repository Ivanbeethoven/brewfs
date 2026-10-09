import pathlib
import tempfile
import unittest
from unittest.mock import patch
try:
    from .smallfiles_scan import FileSpec, expected_pattern, expected_payload_chunk
    from .packed_partial_scan import requests, scan_one
except ImportError:  # direct execution from tools/perf
    from smallfiles_scan import FileSpec, expected_pattern, expected_payload_chunk
    from packed_partial_scan import requests, scan_one


class PartialScanTests(unittest.TestCase):
    def test_partial_requests_verify_random_boundary_and_eof_without_full_reads(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'file'
            size = 512 * 1024
            path.write_bytes(expected_payload_chunk(expected_pattern(7), 0, size))
            spec = FileSpec(path, 7, size)
            with patch('os.pread', wraps=__import__('os').pread) as observed:
                reads, received, _, latency = scan_one(spec, [4096,65536,204800], 20261003)
            self.assertEqual(reads, len(latency))
            self.assertGreater(received, 0)
            self.assertTrue(all(call.args[1] <= 204800 for call in observed.call_args_list))
            offsets = [offset for offset, _ in requests(spec,[4096],20261003)]
            self.assertIn(size, offsets)
            self.assertIn(256*1024-2048, offsets)

    def test_corruption_and_short_read_fail_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            path=pathlib.Path(directory)/'file'
            path.write_bytes(b'bad-data')
            with self.assertRaises(ValueError):scan_one(FileSpec(path,1,8),[4],0)
            with self.assertRaises(ValueError):scan_one(FileSpec(path,1,10),[4],0)

    def test_trace_is_deterministic(self):
        spec=FileSpec(pathlib.Path('unused'),23,1048576)
        self.assertEqual(list(requests(spec,[4096],9)),list(requests(spec,[4096],9)))


if __name__ == '__main__':
    unittest.main()

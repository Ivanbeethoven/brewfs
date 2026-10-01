#!/usr/bin/env python3

import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import smallfiles_scan


class SmallfilesScanTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.levels = 1
        self.fanout = 2
        self.files_per_leaf = 3
        self.minimum = 1024
        self.maximum = 2048
        for leaf in range(self.fanout):
            directory = self.root / f"d{leaf:03d}"
            directory.mkdir()
            for file_index in range(self.files_per_leaf):
                file_number = leaf * self.files_per_leaf + file_index
                size = smallfiles_scan.expected_size(
                    file_number, self.minimum, self.maximum
                )
                pattern = smallfiles_scan.expected_pattern(file_number)
                payload = (pattern * ((size + len(pattern) - 1) // len(pattern)))[:size]
                (directory / f"f{file_index:05d}").write_bytes(payload)

    def tearDown(self):
        self.temporary.cleanup()

    def run_scanner(
        self, mode: str, extra_args: list[str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        scanner = pathlib.Path(__file__).with_name("smallfiles_scan.py")
        command = [
            sys.executable,
            str(scanner),
            "--root",
            str(self.root),
            "--label",
            "test",
            "--mode",
            mode,
            "--expected-files",
            str(self.fanout * self.files_per_leaf),
            "--min-size",
            str(self.minimum),
            "--max-size",
            str(self.maximum),
            "--dir-levels",
            str(self.levels),
            "--dirs-per-level",
            str(self.fanout),
            "--files-per-leaf",
            str(self.files_per_leaf),
            "--workers",
            "2",
            "--chunk-bytes",
            "257",
        ]
        if extra_args:
            command.extend(extra_args)
        return subprocess.run(
            command,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_tree_stat_and_full_modes(self):
        for mode in ("tree", "stat", "full"):
            with self.subTest(mode=mode):
                result = self.run_scanner(mode)
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                self.assertIn(f"mode={mode}", result.stdout)
                self.assertIn("files=6", result.stdout)
                self.assertIn("errors=0", result.stdout)
        full = self.run_scanner("full")
        self.assertIn("payload_bytes=", full.stdout)
        self.assertIn("latency_p95_ms=", full.stdout)

    def test_shuffle_epochs_are_deterministic_and_bounded(self):
        result = self.run_scanner(
            "full",
            [
                "--order",
                "shuffle",
                "--shuffle-seed",
                "17",
                "--epochs",
                "2",
                "--batch-size",
                "2",
                "--max-inflight-batches",
                "1",
            ],
        )

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        summaries = [
            line for line in result.stdout.splitlines() if line.startswith("smallfiles_scan_summary")
        ]
        self.assertEqual(len(summaries), 2)
        self.assertIn("order=shuffle", summaries[0])
        self.assertIn("epoch=1", summaries[0])
        self.assertIn("epoch=2", summaries[1])
        self.assertIn("checksum=21", summaries[0])
        self.assertIn("checksum=21", summaries[1])

    def test_full_mode_rejects_payload_corruption(self):
        path = self.root / "d001" / "f00002"
        payload = bytearray(path.read_bytes())
        payload[len(payload) // 2] ^= 0xFF
        path.write_bytes(payload)

        result = self.run_scanner("full")

        self.assertEqual(result.returncode, 1)
        self.assertIn("content mismatch", result.stdout)
        self.assertIn("errors=1", result.stdout)

    def test_stat_mode_rejects_wrong_size(self):
        path = self.root / "d000" / "f00000"
        path.write_bytes(path.read_bytes()[:-1])

        result = self.run_scanner("stat")

        self.assertEqual(result.returncode, 1)
        self.assertIn("size=", result.stdout)
        self.assertIn("errors=1", result.stdout)


if __name__ == "__main__":
    unittest.main()

import json
import pathlib
import tempfile
import unittest

from packed_run_manifest import ArtifactError, finalize_manifest, init_manifest


class PackedRunManifestTests(unittest.TestCase):
    def test_old_encoding_is_rejected_before_creating_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "rejected"
            with self.assertRaises(ArtifactError):
                init_manifest(
                    artifact,
                    run_id="rejected",
                    wire_version=4,
                    controls={"mode": "stat", "scanner_seed": 20261001},
                    fixture_prefix="local-validation",
                )
            self.assertFalse(artifact.exists())

    def test_success_manifest_requires_seed_trace_and_measurement_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory)
            init_manifest(
                artifact,
                run_id="20261006T120000Z-123",
                wire_version=5,
                controls={
                    "files": 100,
                    "file_bytes": 102400,
                    "metadata_bytes": 8388608,
                    "workers": 16,
                    "epochs": 2,
                    "mode": "stat",
                    "scanner_seed": 20261001,
                },
                fixture_prefix="local-validation",
            )
            (artifact / "manifest-key.txt").write_text("local-validation/manifest\n")
            (artifact / "binary-sha256.txt").write_text("a" * 64 + "  brewfs\n")
            (artifact / "source-sha256.json").write_text(json.dumps({"src/lib.rs": "b" * 64}))
            (artifact / "profile.env").write_text("mode=stat\n")
            (artifact / "cache-proof.env").write_text("page_cache=dropped\n")
            (artifact / "summary.json").write_text(
                json.dumps(
                    {
                        "epochs": [
                            {
                                "mode": "stat",
                                "order": "shuffle",
                                "shuffle_seed": 20261001,
                                "files": 100,
                                "expected_files": 100,
                                "errors": 0,
                                "trace_sha256": "c" * 64,
                            }
                        ]
                    }
                )
            )
            (artifact / "timing.json").write_text(
                json.dumps(
                    {
                        "mount_seconds": 1.0,
                        "active_seconds": 2.0,
                        "drain_seconds": 0.1,
                        "total_seconds": 3.1,
                    }
                )
            )
            finalize_manifest(artifact, status=0)
            manifest = json.loads((artifact / "run-manifest.json").read_text())
            self.assertEqual(manifest["schema"], "packed-v3-run-manifest-v1")
            self.assertEqual(manifest["packed_version"], "v3")
            self.assertEqual(manifest["status"], "passed")
            self.assertEqual(manifest["controls"]["scanner_seed"], 20261001)
            self.assertEqual(manifest["measurement"]["request_trace_sha256"], "c" * 64)
            self.assertEqual(manifest["measurement"]["phases"], ["mount", "active", "drain", "total"])

    def test_layout_controls_are_validated_when_present(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory)
            init_manifest(
                artifact,
                run_id="layout-controls",
                wire_version=5,
                controls={"mode": "stat", "scanner_seed": 1, "frame_policy": "static-1mib", "inline_data": "off", "metadata_codec": "raw", "data_codec": "zstd", "access_profile": "mixed"},
                fixture_prefix="local-validation",
            )
            manifest = json.loads((artifact / "run-manifest.json").read_text())
            self.assertEqual(manifest["controls"]["frame_policy"], "static-1mib")
            with self.assertRaises(ArtifactError):
                init_manifest(
                    pathlib.Path(directory) / "bad",
                    run_id="layout-controls-bad",
                    wire_version=5,
                    controls={"mode": "stat", "scanner_seed": 1, "metadata_codec": "bogus"},
                    fixture_prefix="local-validation",
                )

    def test_failed_run_records_status_without_claiming_measurement(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory)
            init_manifest(
                artifact,
                run_id="20261006T120000Z-125",
                wire_version=5,
                controls={"mode": "stat", "scanner_seed": 20261001},
                fixture_prefix="local-validation",
            )
            finalize_manifest(artifact, status=124)
            manifest = json.loads((artifact / "run-manifest.json").read_text())
            self.assertEqual(manifest["status"], "failed")
            self.assertEqual(manifest["exit_status"], 124)
            self.assertNotIn("measurement", manifest)

    def test_success_manifest_fails_closed_when_seed_or_timing_is_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory)
            init_manifest(
                artifact,
                run_id="20261006T120000Z-124",
                wire_version=5,
                controls={"mode": "stat", "scanner_seed": 20261001},
                fixture_prefix="local-validation",
            )
            (artifact / "manifest-key.txt").write_text("local-validation/manifest\n")
            (artifact / "summary.json").write_text(
                json.dumps({"files": 100, "expected_files": 100, "errors": 0})
            )
            with self.assertRaises(ArtifactError):
                finalize_manifest(artifact, status=0)


if __name__ == "__main__":
    unittest.main()

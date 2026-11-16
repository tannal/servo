# Copyright 2026 The Servo Project Developers. See the COPYRIGHT
# file at the top-level directory of this distribution.
#
# Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
# http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
# <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
# option. This file may not be copied, modified, or distributed
# except according to those terms.

import hashlib
import io
import json
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from servo.fuzzing import obtain_actions_bundle, obtain_runner, replay


class ReplayTests(unittest.TestCase):
    def test_full_output_selected_instead_of_panic_subset(self) -> None:
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as zipped:
            zipped.writestr("generated/freedom/one.html", "first")
            zipped.writestr("generated/freedom/two.html", "second")
            zipped.writestr("generated/freedom/image.png", b"resource")
            zipped.writestr("report.html", "report")
            zipped.writestr("crashes/one.html", "duplicate")
        payload = archive.getvalue()
        full = {"id": 42, "name": "fuzzing-artifacts", "expired": False, "workflow_run": {"id": 7}}
        panic = dict(full, id=43, name="new-panic-corpus")
        with (
            tempfile.TemporaryDirectory() as directory,
            patch(
                "servo.fuzzing.subprocess.check_output",
                side_effect=[json.dumps([{"artifacts": [panic, full]}]), json.dumps(full)],
            ),
            patch("servo.fuzzing.subprocess.run") as download,
        ):
            download.side_effect = lambda *args, **kwargs: kwargs["stdout"].write(payload)
            bundle = obtain_actions_bundle("https://github.com/a/b/actions/runs/7", Path(directory), {})
            with tarfile.open(bundle) as tar:
                self.assertEqual(
                    tar.getnames(),
                    ["generated/freedom/one.html", "generated/freedom/two.html", "generated/freedom/image.png"],
                )
            self.assertEqual(download.call_args.args[0][-1], "repos/a/b/actions/artifacts/42/zip")

    def test_run_with_only_panic_subset_requires_explicit_selection(self) -> None:
        with (
            tempfile.TemporaryDirectory() as directory,
            patch(
                "servo.fuzzing.subprocess.check_output",
                return_value=json.dumps([{"artifacts": [{"id": 43, "name": "new-panic-corpus", "expired": False}]}]),
            ),
            patch("servo.fuzzing.subprocess.run") as download,
        ):
            with self.assertRaisesRegex(ValueError, "explicit artifact URL"):
                obtain_actions_bundle("https://github.com/a/b/actions/runs/7", Path(directory), {})
            download.assert_not_called()

    def test_actions_artifact_download_and_run_selection(self) -> None:
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as zipped:
            zipped.writestr("nested/fuzz-bundle-latest.tar.gz", b"corpus bytes")
        payload = archive.getvalue()
        metadata = {
            "id": 42,
            "name": "fuzz-bundle-latest",
            "expired": False,
            "workflow_run": {"id": 7},
            "digest": "sha256:" + hashlib.sha256(payload).hexdigest(),
        }
        for suffix in ("", "/artifacts/42"):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as directory:
                responses = [json.dumps(metadata)]
                if not suffix:
                    responses.insert(0, json.dumps([{"artifacts": [metadata]}]))
                with (
                    patch("servo.fuzzing.subprocess.check_output", side_effect=responses),
                    patch("servo.fuzzing.subprocess.run") as download,
                ):
                    download.side_effect = lambda *args, **kwargs: kwargs["stdout"].write(payload)
                    bundle = obtain_actions_bundle(
                        "https://github.com/tannal/servo-fuzzing/actions/runs/7" + suffix,
                        Path(directory),
                        {},
                    )
                    self.assertEqual(bundle.read_bytes(), b"corpus bytes")
                    self.assertEqual(
                        download.call_args.args[0][-1], "repos/tannal/servo-fuzzing/actions/artifacts/42/zip"
                    )

    def test_actions_wrong_run_is_rejected(self) -> None:
        with (
            tempfile.TemporaryDirectory() as directory,
            patch(
                "servo.fuzzing.subprocess.check_output",
                return_value=json.dumps({"expired": False, "workflow_run": {"id": 8}}),
            ),
            patch("servo.fuzzing.subprocess.run") as download,
        ):
            with self.assertRaises(ValueError):
                obtain_actions_bundle("https://github.com/a/b/actions/runs/7/artifacts/42", Path(directory), {})
            download.assert_not_called()

    def test_managed_checkout_is_sparse_pinned_and_reused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            (source / "servo_fuzzing").mkdir(parents=True)
            (source / "servo_fuzzing" / "runner.py").touch()
            (source / "generators").mkdir()
            (source / "generators" / "unused.py").touch()
            subprocess.run(["git", "init", "--quiet", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(source),
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.org",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "Fixture",
                ],
                check=True,
            )
            revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            with (
                patch("servo.fuzzing.RUNNER_REPOSITORY", str(source)),
                patch("servo.fuzzing.RUNNER_REVISION", revision),
            ):
                checkout = obtain_runner(root / "cache")
                self.assertTrue((checkout / "servo_fuzzing" / "runner.py").is_file())
                self.assertFalse((checkout / "generators").exists())
                with patch("servo.fuzzing.subprocess.run", side_effect=AssertionError("Cache must be reused")):
                    self.assertEqual(obtain_runner(root / "cache"), checkout)

    def test_bundle_forwarding_and_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = Path(directory)
            (runner / "servo_fuzzing").mkdir()
            (runner / "servo_fuzzing" / "runner.py").touch()
            for bundle in ("../fuzzing/target/bundle.tar.gz", "https://example.org/bundle.tar.gz", None):
                with self.subTest(bundle=bundle), patch("servo.fuzzing.subprocess.call", return_value=7) as call:
                    result = replay(
                        "target/debug/servoshell",
                        directory,
                        None,
                        None,
                        "target/fuzzing",
                        None,
                        4,
                        {},
                        bundle=bundle,
                        force_download=True,
                    )
                    self.assertEqual(result, 7)
                    args = call.call_args.args[0]
                    self.assertEqual(call.call_args.kwargs["cwd"], runner)
                    self.assertNotIn("--testcases", args)
                    self.assertNotIn("--signatures", args)
                    self.assertNotIn("--timeout", args)
                    self.assertIn("--force-download", args)
                    self.assertNotIn("--generate", args)
                    if bundle is None:
                        self.assertNotIn("--bundle", args)
                    else:
                        expected = bundle if bundle.startswith("https://") else str(Path(bundle).resolve())
                        self.assertEqual(args[args.index("--bundle") + 1], expected)

    def test_explicit_paths_are_resolved_before_changing_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = Path(directory)
            (runner / "servo_fuzzing").mkdir()
            (runner / "servo_fuzzing" / "runner.py").touch()
            with patch("servo.fuzzing.subprocess.call", return_value=0) as call:
                self.assertEqual(replay("servo", directory, "cases", "signatures.json", "output", 5.0, None, {}), 0)
                args = call.call_args.args[0]
                for option, value in (("--testcases", "cases"), ("--signatures", "signatures.json")):
                    self.assertEqual(args[args.index(option) + 1], str(Path(value).resolve()))


if __name__ == "__main__":
    unittest.main()

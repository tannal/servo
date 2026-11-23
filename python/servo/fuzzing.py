# Copyright 2026 The Servo Project Developers. See the COPYRIGHT
# file at the top-level directory of this distribution.
#
# Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
# http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
# <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
# option. This file may not be copied, modified, or distributed
# except according to those terms.

"""Launch the separately maintained replay runner; policy belongs to that runner."""

import hashlib
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

RUNNER_REPOSITORY = "https://github.com/tannal/servo-fuzzing.git"
RUNNER_REVISION = "89f8686aeb6aa4188026c367166de432daf92ad2"


def obtain_actions_bundle(url: str, output: Path, env: dict[str, str]) -> Path:
    """Resolve a run or artifact page and download its bundled corpus via gh."""
    match = re.fullmatch(r"https://github\.com/([\w.-]+)/([\w.-]+)/actions/runs/(\d+)(?:/artifacts/(\d+))?/?", url)
    if not match:
        raise ValueError("Expected a GitHub Actions run or artifact URL")
    owner, repo, run_id, artifact_id = match.groups()
    base = f"repos/{owner}/{repo}/actions"
    if artifact_id is None:
        pages = json.loads(
            subprocess.check_output(
                ["gh", "api", f"{base}/runs/{run_id}/artifacts", "--paginate", "--slurp"], env=env, text=True
            )
        )
        available = [a for page in pages for a in page["artifacts"] if not a["expired"]]
        candidates = []
        for name in ("fuzz-bundle-latest", "fuzzing-artifacts"):
            candidates = [a for a in available if a["name"] == name]
            if candidates:
                break
        if len(candidates) != 1:
            names = ", ".join(a["name"] for a in available) or "none"
            raise ValueError(
                f"Cannot select a unique full corpus artifact; available artifacts: {names}. "
                "Use an explicit artifact URL to select one."
            )
        artifact_id = str(candidates[0]["id"])
        print(f"Selected {candidates[0]['name']} from run {run_id}")
    metadata = json.loads(subprocess.check_output(["gh", "api", f"{base}/artifacts/{artifact_id}"], env=env, text=True))
    if metadata["expired"] or str(metadata["workflow_run"]["id"]) != run_id:
        raise ValueError("Artifact is expired or does not belong to the requested run")
    destination = output / "inputs" / owner / repo / run_id / artifact_id
    destination.mkdir(parents=True, exist_ok=True)
    print(f"Downloading Actions artifact {owner}/{repo} run {run_id}, artifact {artifact_id}")
    with tempfile.TemporaryDirectory(dir=destination) as directory:
        archive = Path(directory) / "artifact.zip"
        with archive.open("wb") as stream:
            subprocess.run(["gh", "api", f"{base}/artifacts/{artifact_id}/zip"], env=env, stdout=stream, check=True)
        digest = metadata.get("digest")
        if digest:
            checksum = hashlib.sha256()
            with archive.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    checksum.update(chunk)
            actual = "sha256:" + checksum.hexdigest()
            if actual != digest:
                raise ValueError("Downloaded artifact digest does not match GitHub metadata")
        with zipfile.ZipFile(archive) as zipped:
            bundles = [
                entry
                for entry in zipped.infolist()
                if not entry.is_dir() and entry.filename.endswith((".tar.gz", ".tgz"))
            ]
            temporary = Path(directory) / "bundle.tar.gz"
            if len(bundles) == 1:
                # Copy just the bundle; never extract ZIP-provided filesystem paths.
                with zipped.open(bundles[0]) as source, temporary.open("wb") as target:
                    shutil.copyfileobj(source, target)
            elif bundles:
                raise ValueError("Actions artifact contains multiple corpus archives")
            else:
                # Older panic-corpus artifacts contain files directly. Convert
                # them to the runner's bundle format without extracting paths.
                files = [entry for entry in zipped.infolist() if not entry.is_dir()]
                if metadata.get("name") == "fuzzing-artifacts":
                    # The full output also includes reports and crash copies.
                    # Replay generated inputs once, preserving their resources.
                    files = [entry for entry in files if PurePosixPath(entry.filename).parts[0] == "generated"]
                if not any(entry.filename.lower().endswith((".html", ".htm")) for entry in files):
                    raise ValueError("Actions artifact contains neither a corpus archive nor HTML testcases")
                with tarfile.open(temporary, "w:gz") as tar:
                    for entry in files:
                        name = PurePosixPath(entry.filename)
                        if name.is_absolute() or ".." in name.parts or "\\" in entry.filename:
                            raise ValueError("Unsafe path in Actions artifact")
                        info = tarfile.TarInfo(str(name))
                        info.size = entry.file_size
                        with zipped.open(entry) as source:
                            tar.addfile(info, source)
        bundle = destination / "bundle.tar.gz"
        temporary.replace(bundle)
    return bundle


def obtain_runner(cache: Path) -> Path:
    """Cache an immutable sparse checkout of the replay code and its data."""
    destination = cache / RUNNER_REVISION
    if destination.is_dir():
        return destination
    cache.mkdir(parents=True, exist_ok=True)
    print(f"Fetching fuzzing replay runner {RUNNER_REVISION}")
    with tempfile.TemporaryDirectory(prefix="fetch-", dir=cache) as directory:
        checkout = Path(directory) / "runner"
        subprocess.run(["git", "init", "--quiet", str(checkout)], check=True)
        for args in (
            ["remote", "add", "origin", RUNNER_REPOSITORY],
            ["config", "remote.origin.promisor", "true"],
            ["config", "remote.origin.partialclonefilter", "blob:none"],
            ["fetch", "--quiet", "--depth=1", "--filter=blob:none", "origin", RUNNER_REVISION],
            ["sparse-checkout", "set", "servo_fuzzing", "signatures"],
            ["checkout", "--quiet", "--detach", RUNNER_REVISION],
        ):
            subprocess.run(["git", "-C", str(checkout), *args], check=True)
        if not (checkout / "servo_fuzzing" / "runner.py").is_file():
            raise OSError("Pinned fuzzing revision does not contain the replay runner")
        # Publish only a complete checkout. Another invocation may have won.
        try:
            checkout.rename(destination)
        except OSError:
            if not destination.is_dir():
                raise
    return destination


def replay(
    binary: str,
    runner: str | None,
    testcases: str | None,
    signatures: str | None,
    output: str,
    timeout: float | None,
    jobs: int | None,
    env: dict[str, str],
    bundle: str | None = None,
    force_download: bool = False,
    timeouts: str | None = None,
) -> int:
    try:
        runner_path = (
            Path(runner).resolve()
            if runner
            else obtain_runner(Path(__file__).resolve().parents[2] / "target" / "fuzzing-runner")
        )
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"Error obtaining fuzzing runner: {error}")
        return 2
    if not (runner_path / "servo_fuzzing" / "runner.py").is_file():
        print("Error: --runner must name a checkout containing servo_fuzzing/runner.py")
        return 2

    # Resolve local paths before switching cwd so callers can use paths relative
    # to Servo. URLs and symbolic release specs must reach the runner unchanged.
    args = [
        sys.executable,
        "-m",
        "servo_fuzzing.runner",
        "--binary",
        str(Path(binary).resolve()),
        "--output",
        str(Path(output).resolve()),
    ]
    for option, value in (("--testcases", testcases), ("--signatures", signatures)):
        if value is not None:
            args.extend([option, str(Path(value).resolve())])
    if bundle is not None:
        if re.match(r"https://github\.com/[^/]+/[^/]+/actions/runs/", bundle):
            try:
                bundle = str(obtain_actions_bundle(bundle, Path(output).resolve(), env))
            except (OSError, ValueError, KeyError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
                print(f"Error obtaining Actions bundle: {error}")
                return 2
        is_local_path = Path(bundle).exists() or "/" in bundle or "\\" in bundle or bundle.endswith((".tar.gz", ".tgz"))
        source = (
            bundle if bundle.startswith(("https://", "http://")) or not is_local_path else str(Path(bundle).resolve())
        )
        args.extend(["--bundle", source])
    for option, value in (("--timeout", timeout), ("--timeouts", timeouts), ("--jobs", jobs)):
        if value is not None:
            args.extend([option, str(value)])
    if force_download:
        args.append("--force-download")
    try:
        return subprocess.call(args, cwd=runner_path, env=env)
    except OSError as error:
        print(f"Error launching fuzzing replay: {error}")
        return 2

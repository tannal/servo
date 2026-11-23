# Fuzzing replay

`./mach test-fuzzing` selects the Servo binary and automatically obtains a pinned
`servo-fuzzing` checkout. Bundle download, caching, extraction, testcase
selection, signatures, reporting, and exit status belong to the external runner.
The adapter does not install generators or inspect the runner's report files.

```sh
./mach test-fuzzing --dev
```

The first run needs Git and network access. The runner is cached under
`target/fuzzing-runner/<revision>` and reused without fetching on subsequent
runs. The sparse checkout includes `servo_fuzzing` and `signatures`. It does not
check out generator repositories or install generator, reducer, or GitHub-triage
dependencies. Replay uses the existing Mach Python interpreter and the runner's
standard-library-only entry point.

`RUNNER_REPOSITORY` and `RUNNER_REVISION` in `python/servo/fuzzing.py` identify
the dependency. Update the revision explicitly after publishing and testing a
runner change. The current pin is published commit
`89f8686aeb6aa4188026c367166de432daf92ad2`; uncommitted neighboring-checkout
changes are not included. Use `--runner ../fuzzing` to test those changes locally.

Replay a versioned release bundle while skipping manifest-tagged timeouts:

```sh
./mach test-fuzzing --dev --bundle bundle-2026-11-23 --timeouts exclude --jobs 4
```

Replay a local bundle:

```sh
./mach test-fuzzing --dev \
  --runner ../fuzzing \
  --bundle ../fuzzing/target/fuzz-bundle-latest.tar.gz \
  --signatures ../fuzzing/signatures/rust-panic.json \
  --jobs 4
```

Replay a specific GitHub Actions artifact with automatic download:

```sh
./mach test-fuzzing --bundle https://github.com/tannal/servo-fuzzing/actions/runs/42979207681/artifacts/13837370791
```

You can also pass the run URL alone; the adapter selects its single unexpired
`fuzz-bundle-latest` artifact, or `fuzzing-artifacts` when no full bundle is
available. For `fuzzing-artifacts`, only the `generated/` inputs and their resources
are repackaged for replay; reports and crash copies are excluded. A run URL never
silently selects the smaller `new-panic-corpus`; pass its explicit artifact URL
to replay only new panic cases. This requires GitHub CLI (`gh`) authentication
(`gh auth login`, or a token supplied through `GH_TOKEN`). The adapter verifies
the artifact belongs to the run, checks its digest when provided by GitHub,
and unwraps the artifact ZIP before passing its corpus archive to the runner.
Downloads are stored below `<output>/inputs/` using repository, run, and artifact
IDs. A download failure stops replay rather than falling back to latest.

To let the runner select its default bundle and signatures:

```sh
./mach test-fuzzing --dev --runner ../fuzzing --jobs 4
```

Run fuzzing replay on CI via try:

```sh
./mach try fuzzing
```

Use `--testcases PATH` for a local HTML file or directory, `--bundle SPEC` for a
local archive, HTTP(S) URL, or versioned release spec (such as `bundle-YYYY-MM-DD`
or `latest`), `--timeouts include|exclude` to control manifest-tagged timeout
testcases, and `--force-download` to request a cache refresh. Local paths are
relative to the invocation directory; HTTP(S) URLs and symbolic release specs are
forwarded unchanged. `--timeout`, `--jobs`, and `--signatures` override runner
defaults when supplied. Artifacts default to Servo's `target/fuzzing`; change this
with `--output`.

An explicit runner checkout executes Python code and must be trusted. Its own policies
control fallback behavior, known crashes, empty suites, and timeouts. For CI,
pin the runner revision and a versioned corpus (for example
`--bundle bundle-2026-11-23 --timeouts exclude`) rather than relying on a moving
latest bundle or an implicit fallback. Use the Servo build artifact for the
commit under test. Discovery and corpus publication stay in the fuzzing project.

Run adapter tests:

```sh
PYTHONPATH=python python3 -m unittest servo.test_fuzzing
```

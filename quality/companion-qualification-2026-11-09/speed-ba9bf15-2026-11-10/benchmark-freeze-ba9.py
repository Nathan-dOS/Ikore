#!/usr/bin/env python3
"""Freeze/verify the approved 608cfd3 benchmark inputs using Python stdlib only.

Run ONLY after the final Stage 7 gate and the separate standalone composition
build have completed, with no Cargo, compiler, test or benchmark still running.
This utility runs read-only Git commands. It NEVER executes Cargo, rustc, a
benchmark/test executable, or an application executable.

Prerequisites reviewed by the operator:
  * Stage 7 output.txt/timings.json/result.json belong to 608cfd3 and passed.
  * The completed example build used precisely:
      cargo build --locked -p hvc-test-support --example composition_benchmark --example pulsation_ui_fixture
    This builds both examples with the same package/profile/feature graph; no
    redundant rebuild is required. Preserve its successful dev-profile log.
    The integration-test binary
    tests/composition_benchmark.rs and test-profile example builds are different
    artifacts. A supplied build log is evidence to review, not an automatic
    cryptographic proof that a particular executable was produced by it.
  * The core is extracted from the final gate log, never guessed from a name.
    It must also be uniquely newest by mtime, matching the first inventory
    candidate in gate-speed.ps1. This utility does NOT run --list and cannot
    assert probe availability. Confirm the speed gate selected the same core
    using its inventory log and report.baselineArtifacts/candidateArtifacts.
  * Keep HEAD, all input files, OS/fonts/display/driver and process environment
    stable through the single official preflight and the unchanged speed gate.

Usage (paths may be absolute or relative to HARD-VC):
  python target/companion-final/benchmark-freeze-ba9.py capture \
    --composition-build-log target/<completed-standalone-build.log> \
    --output target/companion-final/<new-run>/freeze-before.json
  python target/companion-final/benchmark-freeze-ba9.py verify \
    --snapshot target/companion-final/<new-run>/freeze-before.json \
    --output target/companion-final/<new-run>/freeze-after.json

Output and snapshot paths must be under this repository's target directory.
Existing output files are never overwritten. Current candidate fingerprints
are separate from historical baseline fingerprint fields; the historical
original paths may already have been overwritten by the candidate build.
"""

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys


EXPECTED_HEAD = "608cfd3262e8c7c634f5d02cbb5d9979932c87aa"
EXPECTED_BRANCH = "perf/companion-paragraph-layout"
BASELINE_HEAD = "3a2c973a5bbe58e3d6e28cc88739c5f86e1ae9de"
SOURCE_COUNT = 966
FIXTURE_COUNT = 20
SOURCE_SCOPES = [
    ".cargo", "Cargo.lock", "Cargo.toml", "crates", "rust-toolchain.toml",
    "scripts", "third_party",
]
FIXTURE_REL = "crates/hvc-test-support/tests/fixtures/composition"
SCRIPT_PATHS = [
    "scripts/benchmark-ui-latency.ps1", "scripts/gate-speed.ps1",
    "scripts/lib/speed-metrics.ps1",
]
FP_FIELDS = [
    "rustc", "features", "declared_features", "target", "profile", "path",
    "rustflags", "config", "compile_kind",
]
REFERENCE_FILES = {
    "sourceManifest": (
        "quality/companion-qualification-2026-11-09/source-ba9bf15.json",
        "22e887485785f3b6e66f13fe4b6846d7f8f1871407c7a25bf05c1f0e62810b1f",
    ),
    "baselineRecreated": (
        "quality/companion-qualification-2026-11-09/baseline-recreated-after-wip.json",
        "cdde4fd5eeed9be047783c70c21c79cc7e754dc9c44b69d9e76aa40636d193ac",
    ),
    "baselineFingerprints": (
        "quality/companion-qualification-2026-11-08/baseline-build-fingerprints.json",
        "31b2557e556ba7ac5cde1adb848a46959c773785a4e2fb410cbbbe603ddaab7c",
    ),
}
BASELINE_HASHES = {
    "core": "efd5638ef0881df0f72ef6c18b0a8d374511f893fafece5163b02bcdf0c59e3f",
    "composition": "cd1b60805ac03b49eaef66bc8ef2ba4a4599af4e9bb940680f848c47c057dcfb",
    "ui": "024e10d90ee80e8f364b3b43751d1cc11a3f15194aa84db2d954fe549509938d",
}
GIT_REDIRECT_NAMES = {
    "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE",
    "GIT_CONFIG", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS", "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM", "GIT_EXEC_PATH",
}
ROOT = Path(os.path.abspath(__file__)).parents[2]


class FreezeError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise FreezeError(message)


def utc_now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def absolute_path(value, root=ROOT):
    path = Path(value)
    return Path(os.path.abspath(path if path.is_absolute() else root / path))


def inside(path, scope):
    try:
        path.relative_to(scope)
        return True
    except ValueError:
        return False


def reject_links(path, scope, allow_missing=False):
    require(inside(path, scope), "Path escapes the permitted scope: " + str(path))
    current = scope
    for part in (None, *path.relative_to(scope).parts):
        if part is not None:
            current = current / part
        try:
            info = current.lstat()
        except FileNotFoundError:
            if allow_missing:
                continue
            raise FreezeError("Required path is missing: " + str(current))
        reparse = getattr(info, "st_file_attributes", 0) & 0x400
        require(not stat.S_ISLNK(info.st_mode) and not reparse,
                "Link/reparse point is not allowed: " + str(current))


def relative_path(value):
    require(isinstance(value, str) and "\\" not in value and ":" not in value,
            "Invalid manifest path")
    path = PurePosixPath(value)
    require(not path.is_absolute() and path.parts and ".." not in path.parts,
            "Manifest path escapes its repository")
    return Path(*path.parts)


def file_record(path, scope):
    path = absolute_path(path)
    reject_links(path, scope)
    before = path.stat()
    require(stat.S_ISREG(before.st_mode), "Not a regular file: " + str(path))
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(4 * 1024 * 1024), b""):
            digest.update(chunk)
    after = path.stat()
    require((before.st_size, before.st_mtime_ns, before.st_ino) ==
            (after.st_size, after.st_mtime_ns, after.st_ino),
            "File changed while being hashed: " + str(path))
    return {"path": str(path), "bytes": after.st_size,
            "sha256": digest.hexdigest(), "modifiedNs": after.st_mtime_ns}


def text_file(path, scope):
    reject_links(path, scope)
    data = path.read_bytes()
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        return data.decode("utf-16")
    return data.decode("utf-8-sig")


def json_file(path, scope):
    return json.loads(text_file(path, scope))


def git_environment():
    present = sorted(name for name in os.environ if (
        name.upper() in GIT_REDIRECT_NAMES or
        name.upper().startswith(("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"))
    ))
    require(not present, "Git redirection environment names are set: " + ", ".join(present))
    return {"checkedNames": sorted(GIT_REDIRECT_NAMES),
            "checkedPrefixes": ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"],
            "presentNames": []}


def git(root, *arguments):
    # Suppress optional index refresh writes; never launch a pager or prompt.
    environment = dict(os.environ, GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    command = ["git", "--no-optional-locks", "-c", "core.fsmonitor=false",
               "-c", "core.untrackedCache=false", "-C", str(root), *arguments]
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=environment, timeout=60, check=False,
                            creationflags=(0x08000000 if os.name == "nt" else 0))
    require(result.returncode == 0,
            "Read-only git command failed: " + " ".join(arguments[:3]))
    return result.stdout.decode("utf-8", errors="strict")


def git_identity(root, expected, branch):
    head = git(root, "rev-parse", "HEAD").strip()
    actual_branch = git(root, "rev-parse", "--abbrev-ref", "HEAD").strip()
    require(head == expected and actual_branch == branch,
            "Unexpected HEAD/branch at " + str(root))
    diff = git(root, "diff", "--no-ext-diff", "--no-textconv", expected,
               "--", *SOURCE_SCOPES)
    status = git(root, "status", "--porcelain=v1", "--untracked-files=all",
                 "--", *SOURCE_SCOPES)
    require(not diff and not status, "Source-scoped Git diff/status is not empty: " + str(root))
    return {"root": str(root), "head": head, "branch": actual_branch,
            "sourceDiffEmpty": True, "sourceStatusEmpty": True,
            "sourceScopes": SOURCE_SCOPES}


def source_files(root, manifest):
    require(manifest.get("sourceCommit") == EXPECTED_HEAD and
            manifest.get("sourceDiffEmpty") is True, "Wrong source manifest identity")
    expected = manifest.get("files", [])
    require(len(expected) == SOURCE_COUNT, "Source manifest must contain exactly 966 files")
    indexed = {entry["path"]: entry for entry in expected}
    require(len(indexed) == SOURCE_COUNT, "Duplicate source manifest paths")
    tree = {}
    for item in git(root, "ls-tree", "-r", "-z", EXPECTED_HEAD, "--", *SOURCE_SCOPES).split("\0"):
        if item:
            header, name = item.split("\t", 1)
            _mode, kind, blob = header.split(" ")
            require(kind == "blob", "Unexpected non-blob source tree member: " + name)
            tree[name] = blob
    require(set(tree) == set(indexed), "Source manifest/tree path inventory differs")
    records = []
    for name, expected_file in sorted(indexed.items()):
        require(tree[name] == expected_file["gitBlob"], "Source Git blob differs: " + name)
        record = file_record(root / relative_path(name), root)
        require(record["sha256"] == expected_file["sha256"].lower() and
                record["bytes"] == expected_file["bytes"], "Raw source bytes differ: " + name)
        record["path"] = name
        record["gitBlob"] = tree[name]
        records.append(record)
    return records


def fixture_files(root, expected):
    directory = root / FIXTURE_REL
    reject_links(directory, root)
    require(directory.is_dir(), "Fixture path is not a directory")
    expected_by_name = {}
    for entry in expected:
        relative = relative_path(entry["path"])
        require(relative.parent.as_posix() == FIXTURE_REL,
                "Unexpected approved fixture directory")
        require(relative.suffix == ".json", "Approved fixture is not a .json file")
        expected_by_name[relative.name] = entry
    require(len(expected) == FIXTURE_COUNT and len(expected_by_name) == FIXTURE_COUNT,
            "Exactly 20 unique approved JSON fixtures are required")
    actual = list(directory.iterdir())  # Includes hidden/untracked/ignored entries.
    require({path.name for path in actual} == set(expected_by_name) and
            len(actual) == FIXTURE_COUNT,
            "Fixture directory has missing/extra entries: " + str(directory))
    records = []
    for path in sorted(actual, key=lambda item: item.name):
        record = file_record(path, root)
        approved = expected_by_name[path.name]
        require(record["sha256"] == approved["sha256"].lower() and
                record["bytes"] == approved["bytes"],
                "Fixture bytes differ from 3a2c973 proof: " + path.name)
        record["path"] = approved["path"]
        records.append(record)
    return records


def core_inventory(root):
    directory = root / "target/debug/deps"
    reject_links(directory, root)
    records = []
    for path in directory.glob("hvc_core-*.exe"):
        reject_links(path, root)
        info = path.stat()
        require(stat.S_ISREG(info.st_mode), "Core candidate is not a regular file")
        records.append({"name": path.name, "bytes": info.st_size,
                        "modifiedNs": info.st_mtime_ns})
    records.sort(key=lambda entry: (-entry["modifiedNs"], entry["name"]))
    require(records, "No core library test executables exist")
    return records


def gate_evidence(root, directory):
    require(inside(directory, root), "Gate record directory must be inside HARD-VC")
    result_path = directory / "result.json"
    result = json_file(result_path, root)
    require(result.get("sourceCommit") == EXPECTED_HEAD and
            result.get("gatePassed") is True and result.get("checks") == 9 and
            result.get("sourceFilesVerified") == SOURCE_COUNT and
            result.get("sourceDiffEmpty") is True and result.get("sourceDrift") == [] and
            result.get("totals", {}).get("failed") == 0,
            "Final Stage 7 receipt is incomplete or failed")
    expected_artifacts = {entry["path"]: entry for entry in result.get("artifacts", [])}
    require(set(expected_artifacts) == {"output.txt", "timings.json"},
            "Stage 7 receipt has unexpected evidence artifacts")
    records = {"result": file_record(result_path, root)}
    for name, approved in expected_artifacts.items():
        record = file_record(directory / name, root)
        require(record["sha256"] == approved["sha256"].lower() and
                record["bytes"] == approved["bytes"], "Stage 7 archived evidence changed: " + name)
        records[name] = record
    timing = json_file(directory / "timings.json", root)
    steps = timing.get("steps", [])
    require(timing.get("passed") is True and timing.get("finishedAt") and
            len(steps) == 9 and all(step.get("passed") is True and
                                 step.get("exitCode") == 0 for step in steps),
            "Stage 7 timings do not describe nine completed passing checks")
    log = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text_file(directory / "output.txt", root))
    matches = re.findall(
        r"Running\s+unittests\s+src[\\/]lib\.rs\s*\(\s*"
        r"target[\\/]debug[\\/]deps[\\/](hvc_core-[0-9a-f]+\.exe)\s*\)", log)
    require(len(matches) == 1, "Final gate log must identify exactly one executed core library binary")
    return {"files": records, "loggedCoreName": matches[0],
            "checks": result["checks"], "testTotals": result["totals"],
            "testTargets": result.get("testTargets"), "passed": True}


def comparable_fingerprint(document):
    require(all(name in document for name in FP_FIELDS), "Fingerprint fields are missing")
    return {name: document[name] for name in FP_FIELDS}


def candidate_fingerprint(root, name, baseline, core_name):
    expected_fields = comparable_fingerprint(baseline["fingerprintFields"])
    if name == "core":
        suffix = core_name.removeprefix("hvc_core-").removesuffix(".exe")
        paths = [root / ("target/debug/.fingerprint/hvc-core-" + suffix) / "test-lib-hvc_core.json"]
    else:
        pattern = ("target/debug/.fingerprint/hvc-test-support-*/example-composition_benchmark.json"
                   if name == "composition" else
                   "target/release/.fingerprint/hvc-app-*/bin-hard-vibe-code.json")
        paths = sorted(root.glob(pattern))
    matches = []
    for path in paths:
        document = json_file(path, root)
        if comparable_fingerprint(document) == expected_fields:
            matches.append((path, document))
    require(len(matches) == 1,
            "Expected exactly one candidate fingerprint matching baseline compiler/profile/flags for " + name)
    path, document = matches[0]
    return {"file": file_record(path, root), "fields": document,
            "comparedFields": FP_FIELDS, "baselineRelevantFieldsMatch": True}


def collect_identity(composition_log, gate_directory):
    root = ROOT
    reject_links(root, root)
    environment = git_environment()  # Before any Git command; values are never retained.
    references = {}
    documents = {}
    for name, (relative, expected_sha) in REFERENCE_FILES.items():
        path = root / relative
        record = file_record(path, root)
        require(record["sha256"] == expected_sha, "Pinned reference document changed: " + relative)
        references[name] = record
        documents[name] = json_file(path, root)
    recreated = documents["baselineRecreated"]
    historical = documents["baselineFingerprints"]
    baseline_root = absolute_path(root.parent / ".worktrees/companion-speed-baseline")
    require(absolute_path(recreated["baselineRoot"]) == baseline_root and
            recreated.get("baselineSourceCommit") == BASELINE_HEAD,
            "Wrong recreated baseline root/source")
    candidate_git = git_identity(root, EXPECTED_HEAD, EXPECTED_BRANCH)
    baseline_git = git_identity(baseline_root, BASELINE_HEAD, "HEAD")
    sources = source_files(root, documents["sourceManifest"])
    gate = gate_evidence(root, gate_directory)
    logged_core = gate["loggedCoreName"]
    candidate_inventory = core_inventory(root)
    require(candidate_inventory[0]["name"] == logged_core and
            (len(candidate_inventory) == 1 or candidate_inventory[0]["modifiedNs"] >
             candidate_inventory[1]["modifiedNs"]),
            "Logged core is not uniquely newest; review probe/mtime selection without guessing")
    baseline_inventory = core_inventory(baseline_root)
    require([entry["name"] for entry in baseline_inventory] == ["hvc_core-b3ff09f63410ebf4.exe"],
            "Detached baseline core inventory is ambiguous")
    candidate_paths = {
        "core": root / "target/debug/deps" / logged_core,
        "composition": root / "target/debug/examples/composition_benchmark.exe",
        "ui": root / "target/release/hard-vibe-code.exe",
    }
    baseline_artifacts = {}
    approved_baseline = {entry["name"]: entry for entry in recreated["artifacts"]}
    require(set(approved_baseline) == set(BASELINE_HASHES), "Wrong baseline artifact inventory")
    candidate_artifacts = {}
    for name, expected_sha in BASELINE_HASHES.items():
        approved = approved_baseline[name]
        require(approved["sha256"].lower() == expected_sha, "Wrong approved baseline executable SHA")
        path = absolute_path(approved["path"])
        record = file_record(path, baseline_root)
        require(record["sha256"] == expected_sha and record["bytes"] == approved["bytes"],
                "Copied baseline executable changed: " + name)
        baseline_artifacts[name] = record
        candidate_artifacts[name] = file_record(candidate_paths[name], root)
    fixtures = {
        "compiledBaselineManifestDirectory": recreated["compiledCompositionManifestDirectory"],
        "note": "Copied baseline composition still reads the original HARD-VC fixture directory.",
        "candidate": fixture_files(root, recreated["runtimeFixtures"]),
        "detachedBaseline": fixture_files(baseline_root, recreated["runtimeFixtures"]),
    }
    historical_fingerprints = {}
    current_fingerprints = {}
    for name in BASELINE_HASHES:
        reference = historical["artifacts"][name]
        require(reference["approvedSha256"].lower() == BASELINE_HASHES[name] and
                reference["verifiedSha256"].lower() == BASELINE_HASHES[name],
                "Historical fingerprint record refers to another baseline executable")
        historical_fingerprints[name] = {
            "originalPathHistoricalOnly": reference["fingerprintPath"],
            "recordedSha256": reference["fingerprintSha256"],
            "recordedFields": reference["fingerprintFields"],
            "provenanceQualification": reference["provenanceQualification"],
            "note": "Reference from the pinned historical JSON; mutable original path is NOT read for baseline identity.",
        }
        current_fingerprints[name] = candidate_fingerprint(root, name, reference, logged_core)
    require(inside(composition_log, root), "Composition build log must be inside HARD-VC")
    build_record = file_record(composition_log, root)
    build_text = text_file(composition_log, root)
    require(re.search(r"Finished\s+`dev`\s+profile", build_text) is not None and
            re.search(r"(?m)^\s*error(?:\[|:)", build_text) is None,
            "Standalone composition log has no successful dev-profile completion, or contains an error")
    current_rustc_path = root / "target/.rustc_info.json"
    current_rustc = json_file(current_rustc_path, root)
    compiler_outputs = sorted(set(output["stdout"].strip() for output in
                                  current_rustc.get("outputs", {}).values()
                                  if output.get("success") is True and
                                  output.get("stdout", "").startswith("rustc ")))
    expected_compiler = historical["toolchain"]["cachedCompilerOutput"].strip()
    require(compiler_outputs == [expected_compiler], "Current cached compiler identity differs from baseline")
    by_source_path = {entry["path"]: entry for entry in sources}
    scripts = {name: by_source_path[name] for name in SCRIPT_PATHS}
    # Recheck HEAD/status at the end so hashing cannot conceal a source transition.
    require(git_identity(root, EXPECTED_HEAD, EXPECTED_BRANCH) == candidate_git and
            git_identity(baseline_root, BASELINE_HEAD, "HEAD") == baseline_git,
            "Repository identity changed during capture")
    return {
        "configuration": {"candidateRoot": str(root), "baselineRoot": str(baseline_root),
                          "compositionBuildLog": str(composition_log),
                          "gateRecordDirectory": str(gate_directory)},
        "gitEnvironment": environment, "references": references,
        "utility": file_record(absolute_path(__file__), root),
        "candidate": {"git": candidate_git, "sourceFiles": sources,
                      "artifacts": candidate_artifacts, "coreInventory": candidate_inventory,
                      "fingerprintsCurrent": current_fingerprints},
        "baseline": {"git": baseline_git, "artifacts": baseline_artifacts,
                     "coreInventory": baseline_inventory,
                     "fingerprintsHistoricalReferenceOnly": historical_fingerprints},
        "runtimeFixtures": fixtures, "scripts": scripts, "finalStage7": gate,
        "compositionBuild": {
            "log": build_record, "successfulDevFinishObserved": True,
            "requiredReviewedCommand": "cargo build --locked -p hvc-test-support --example composition_benchmark --example pulsation_ui_fixture",
            "limitation": "The operator must review the command/log and artifact association; no Cargo or executable was invoked by this utility.",
        },
        "rustcInfo": {
            "current": file_record(current_rustc_path, root),
            "currentCachedCompilerOutput": compiler_outputs[0],
            "baselineCacheHistoricalOnly": {
                "path": historical["toolchain"]["rustcCachePath"],
                "recordedSha256": historical["toolchain"]["rustcCacheSha256"],
                "cachedCompilerOutput": historical["toolchain"]["cachedCompilerOutput"],
            },
            "note": "Cache/fingerprint evidence only; rustc was not executed. Runtime composition metadata invokes only git using process cwd.",
        },
        "limitations": [
            "No executable probe inventory or benchmark was run; compare the speed gate's actual selected artifact paths/SHA with this snapshot.",
            "Raw bytes and mtimes are checked; OS DLLs, fonts, display state, GPU drivers, ambient load and power policy are not measured or certified.",
            "A snapshot comparison detects changes between observations, not arbitrary transient changes restored in between.",
        ],
    }


def differences(expected, actual, path="identity"):
    if type(expected) is not type(actual):
        return [path + ": type changed"]
    if isinstance(expected, dict):
        result = []
        for key in sorted(set(expected) | set(actual)):
            child = path + "." + key
            if key not in expected or key not in actual:
                result.append(child + ": key added/removed")
            else:
                result.extend(differences(expected[key], actual[key], child))
        return result
    if isinstance(expected, list):
        if len(expected) != len(actual):
            return [path + ": length changed"]
        return [change for index, (left, right) in enumerate(zip(expected, actual))
                for change in differences(left, right, path + "[" + str(index) + "]")]
    return [] if expected == actual else [path + ": value changed"]


def target_path(value):
    path = absolute_path(value)
    target = ROOT / "target"
    require(inside(path, target) and path != target,
            "Snapshot/output must be a file inside HARD-VC/target")
    reject_links(path, target, allow_missing=True)
    return path


def write_json(path, document):
    reject_links(path, ROOT / "target", allow_missing=True)
    require(not path.exists(), "Refusing to overwrite existing output: " + str(path))
    path.parent.mkdir(parents=True, exist_ok=True)
    reject_links(path.parent, ROOT / "target")
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(document, stream, ensure_ascii=False, indent=2, sort_keys=True)
        stream.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="mode", required=True)
    capture = commands.add_parser("capture", help="Capture a reviewed final candidate before measurement")
    capture.add_argument("--output", required=True)
    capture.add_argument("--composition-build-log", required=True)
    capture.add_argument("--gate-record-directory", default=
                         "quality/companion-qualification-2026-11-09/stage7-ba9bf15")
    verify = commands.add_parser("verify", help="Compare current inputs against a captured snapshot")
    verify.add_argument("--snapshot", required=True)
    verify.add_argument("--output", required=True)
    args = parser.parse_args()
    output = target_path(args.output)
    require(not output.exists(), "Refusing to overwrite existing output: " + str(output))
    if args.mode == "capture":
        identity = collect_identity(absolute_path(args.composition_build_log),
                                    absolute_path(args.gate_record_directory))
        document = {"schemaVersion": 1, "kind": "benchmark-freeze-ba9bf15",
                    "capturedAtUtc": utc_now(), "identity": identity}
        write_json(output, document)
        print(json.dumps({"passed": True, "operation": "capture", "output": str(output),
                          "sourceFiles": SOURCE_COUNT, "fixtures": FIXTURE_COUNT,
                          "executables": 6, "measuredExecutablesRun": 0}))
        return 0
    snapshot = target_path(args.snapshot)
    original = json_file(snapshot, ROOT / "target")
    require(original.get("schemaVersion") == 1 and
            original.get("kind") == "benchmark-freeze-ba9bf15", "Wrong snapshot schema/kind")
    configuration = original["identity"]["configuration"]
    require(configuration["candidateRoot"] == str(ROOT), "Snapshot belongs to another candidate root")
    report = {"schemaVersion": 1, "kind": "benchmark-freeze-verification-ba9bf15",
              "verifiedAtUtc": utc_now(), "snapshot": file_record(snapshot, ROOT / "target"),
              "passed": False}
    try:
        current = collect_identity(absolute_path(configuration["compositionBuildLog"]),
                                   absolute_path(configuration["gateRecordDirectory"]))
        changes = differences(original["identity"], current)
        report.update(passed=not changes, differences=changes, currentIdentity=current)
    except (FreezeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        report["error"] = str(error)
    write_json(output, report)
    print(json.dumps({"passed": report["passed"], "operation": "verify", "output": str(output),
                      "differences": report.get("differences", []),
                      "error": report.get("error"), "measuredExecutablesRun": 0}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (FreezeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(json.dumps({"passed": False, "error": str(error),
                          "measuredExecutablesRun": 0}), file=sys.stderr)
        sys.exit(1)

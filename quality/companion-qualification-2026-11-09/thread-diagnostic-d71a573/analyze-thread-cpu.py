from pathlib import Path
from collections import defaultdict
import csv
import hashlib
import json
import math
import statistics

OUT = Path(__file__).resolve().parent
ROOT = OUT.parents[2]
t = json.loads((OUT / "thread-cpu.json").read_text(encoding="utf-8-sig"))
r = json.loads((OUT / "diagnostic-latency.json").read_text(encoding="utf-8-sig"))
points = t["points"]
info = {item["index"]: item for item in t["threads"]}
rows = defaultdict(dict)
for row in t["rows"]:
    rows[row["point"]][row["threadIndex"]] = row

def total(row):
    return row["user100ns"] + row["kernel100ns"]

def percentile(values, p):
    values = sorted(values)
    return values[max(0, math.ceil(len(values) * p / 100) - 1)] if values else None

ui_ids = {point["uiThreadId"] for point in points if point["uiThreadId"]}
groups = defaultdict(lambda: {"wallMs": 0.0, "processCpuMs": 0.0, "capturedThreadCpuMs": 0.0, "uiCpuMs": 0.0,
                              "uiUserMs": 0.0, "uiKernelMs": 0.0, "nonUiUserMs": 0.0, "nonUiKernelMs": 0.0,
                              "newThreadInitialCpuMs": 0.0, "intervals": 0})
thread_deltas = defaultdict(lambda: defaultdict(float))
intervals = []
for previous, point in zip(points, points[1:]):
    phase = point["phase"] if previous["phase"] == point["phase"] else "visibility-transition"
    delta = {"point": point["index"], "startMs": previous["elapsedMs"], "endMs": point["elapsedMs"],
             "phase": phase, "wallMs": point["elapsedMs"] - previous["elapsedMs"],
             "processCpuMs": ((point["processUser100ns"] + point["processKernel100ns"]) - (previous["processUser100ns"] + previous["processKernel100ns"])) / 10000,
             "capturedThreadCpuMs": 0.0, "uiCpuMs": 0.0, "newThreadInitialCpuMs": 0.0,
             "uiUserMs": 0.0, "uiKernelMs": 0.0, "nonUiUserMs": 0.0, "nonUiKernelMs": 0.0}
    for index, row in rows[point["index"]].items():
        before = rows[previous["index"]].get(index)
        # New lifetime identities accrued this CPU after creation. Keep their
        # first counters separate, because enumeration cannot time their birth exactly.
        value = (total(row) - (total(before) if before else 0)) / 10000
        if before is None:
            delta["newThreadInitialCpuMs"] += value
        delta["capturedThreadCpuMs"] += value
        thread_deltas[index][phase] += value
        user_ms = (row["user100ns"] - (before["user100ns"] if before else 0)) / 10000
        kernel_ms = (row["kernel100ns"] - (before["kernel100ns"] if before else 0)) / 10000
        if info[index]["threadId"] in ui_ids:
            delta["uiCpuMs"] += value
            delta["uiUserMs"] += user_ms
            delta["uiKernelMs"] += kernel_ms
        else:
            delta["nonUiUserMs"] += user_ms
            delta["nonUiKernelMs"] += kernel_ms
    delta["nonUiCapturedCpuMs"] = delta["capturedThreadCpuMs"] - delta["uiCpuMs"]
    delta["uiPercentOneCore"] = delta["uiCpuMs"] / delta["wallMs"] * 100
    for key in ("wallMs", "processCpuMs", "capturedThreadCpuMs", "uiCpuMs", "uiUserMs", "uiKernelMs", "nonUiUserMs", "nonUiKernelMs", "newThreadInitialCpuMs"):
        groups[phase][key] += delta[key]
    groups[phase]["intervals"] += 1
    intervals.append(delta)

for group in groups.values():
    group["nonUiCapturedCpuMs"] = group["capturedThreadCpuMs"] - group["uiCpuMs"]
    group["uiPercentOneCore"] = group["uiCpuMs"] / group["wallMs"] * 100
    group["processEquivalentCores"] = group["processCpuMs"] / group["wallMs"]

ranking = []
for index, values in thread_deltas.items():
    identity = info[index]
    ranking.append({"threadIndex": index, "threadId": identity["threadId"],
                    "description": identity["description"], "descriptionStatus": identity["descriptionStatus"],
                    "uiThread": identity["threadId"] in ui_ids,
                    "preWindowCpuMs": values["pre-window"], "visibleCpuMs": values["window-visible"],
                    "transitionCpuMs": values["visibility-transition"], "totalCapturedDeltaMs": sum(values.values())})
ranking.sort(key=lambda item: item["visibleCpuMs"], reverse=True)
visible_origin = min(point["elapsedMs"] for point in points if point["windowVisible"])
visible_bins = defaultdict(lambda: {"wallMs": 0.0, "uiCpuMs": 0.0, "nonUiCapturedCpuMs": 0.0, "processCpuMs": 0.0})
for interval in intervals:
    if interval["phase"] != "window-visible":
        continue
    # Assign whole intervals by end time; boundaries are approximate, not frame boundaries.
    bucket = int((interval["endMs"] - visible_origin) // 200)
    for key in ("wallMs", "uiCpuMs", "nonUiCapturedCpuMs", "processCpuMs"):
        visible_bins[bucket][key] += interval[key]
runtime_visible_cpu = sum(item["visibleCpuMs"] for item in ranking if item["description"] == "hvc-runtime")

manifest = json.loads((ROOT / "quality/companion-qualification-2026-11-09/source-d71a573.json").read_text(encoding="utf-8-sig"))
mismatches = [item["path"] for item in manifest["files"] if hashlib.sha256((ROOT / item["path"]).read_bytes()).hexdigest() != item["sha256"]]
original_hash = hashlib.sha256((ROOT / "scripts/benchmark-ui-latency.ps1").read_bytes()).hexdigest()
derivation = json.loads((OUT / "derivation.json").read_text())
before = json.loads((OUT / "before-run-integrity.json").read_text())
binary_hash = hashlib.sha256((ROOT / "target/release/hard-vibe-code.exe").read_bytes()).hexdigest()
source_integrity = {"checkedFiles": len(manifest["files"]), "allMatch": not mismatches, "mismatches": mismatches,
                    "officialScriptSha256": original_hash, "officialScriptUnchanged": original_hash == derivation["originalSha256"],
                    "binarySha256": binary_hash, "binaryUnchanged": binary_hash == before["binarySha256"],
                    "isolatedRuntimeEmpty": not any((OUT / "isolated-runtime").iterdir())}

summary = {"diagnosticOnly": True, "doesNotReplacePreflightFail": True, "oneApplicationExecution": True,
           "sourceCommit": derivation["sourceCommit"], "processId": t["processId"], "uiThreadIds": sorted(ui_ids),
           "latency": {key: r.get(key) for key in ("scenario", "fixtureFrames", "updatedConversations", "fixtureReady", "viewportWidth", "viewportHeight", "samples", "warmupSamples", "p50Ms", "p95Ms", "p99Ms", "passed", "displayRefreshRatesHz")},
           "sampling": {"points": len(points), "rows": len(t["rows"]), "threads": len(t["threads"]), "firstVisibleMs": t["firstVisibleMs"],
                        "elapsedMs": t["elapsedMs"], "queryErrors": t["queryErrors"], "droppedThreads": t["droppedThreads"], "saturated": t["saturated"],
                        "intervalMsP50": percentile([item["wallMs"] for item in intervals], 50), "intervalMsP95": percentile([item["wallMs"] for item in intervals], 95),
                        "collectionMsTotal": sum(point["collectionMs"] for point in points), "collectionMsP50": percentile([point["collectionMs"] for point in points], 50),
                        "collectionMsP95": percentile([point["collectionMs"] for point in points], 95), "collectionMsMax": max(point["collectionMs"] for point in points)},
           "phaseCpu": dict(groups), "threadsByVisibleCpu": ranking,
           "visibleApproximate200msBins": {str(index): value for index, value in visible_bins.items()},
           "hvcRuntimeVisibleCpuMs": runtime_visible_cpu,
           "integrity": source_integrity,
           "limits": ["External sampler perturbs scheduling; not a gate run.", "CPU counters are coarse Windows accounting, not wall-clock stack samples.",
                      "Sampled visibility excludes transition intervals; no exact measured-frame timeline is available.",
                      "First counters of newly observed threads are included separately; process totals can differ due sampling time and short-lived threads.",
                      "Only launched process threads are measured; child CLI process CPU and system-wide contention are not observed.",
                      "Thread identities/descriptions cannot prove work categories or causality for individual latency spikes."]}
(OUT / "analysis-summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8", newline="\n")
(OUT / "after-run-integrity.json").write_text(json.dumps(source_integrity, indent=2), encoding="utf-8", newline="\n")
with (OUT / "cpu-intervals.csv").open("w", encoding="utf-8", newline="") as stream:
    writer = csv.DictWriter(stream, fieldnames=list(intervals[0]))
    writer.writeheader()
    writer.writerows(intervals)
print(json.dumps({key: summary[key] for key in ("processId", "uiThreadIds", "latency", "sampling", "phaseCpu", "integrity")}, indent=2))
print("Top visible CPU threads:")
print(json.dumps(ranking[:8], indent=2))
print("Approximate visible-window bins:")
print(json.dumps(summary["visibleApproximate200msBins"], indent=2))

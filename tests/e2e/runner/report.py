"""Run artifacts: environment record, per-test report.md, manifest.json, summary.md."""

import json
import os
import platform
import shutil
import subprocess
from pathlib import Path

RUNNER_VERSION = "1.0"


def _git(args, cwd):
    try:
        r = subprocess.run(["git"] + args, cwd=cwd, capture_output=True, timeout=10)
        return r.stdout.decode().strip()
    except Exception:
        return ""


def build_environment(root, bin_dir):
    env = {
        "os": platform.system(),
        "kernel": platform.release(),
        "gpu": "unknown",
        "mesa": None,
        "xvfb_version": None,
        "screen_size": [1280, 720],
        "output_scale": 1.0,
        "stack": "x11",
        "veyra_config_sha256": None,
    }
    try:
        r = subprocess.run(["glxinfo", "-B"], env={**os.environ, "DISPLAY": ":99"},
                           capture_output=True, timeout=15)
        for line in r.stdout.decode().splitlines():
            if "OpenGL renderer string" in line:
                env["gpu"] = line.split(":", 1)[1].strip()
            if "OpenGL version" in line and "ES" not in line:
                env["mesa"] = line.split(":", 1)[1].strip()
    except Exception:
        pass
    try:
        r = subprocess.run(["Xvfb", "-version"], capture_output=True, timeout=10)
        first = (r.stderr.decode() or r.stdout.decode()).strip().splitlines()
        if first:
            env["xvfb_version"] = first[0][:120]
    except Exception:
        pass
    try:
        r = subprocess.run(["rustc", "--version"], capture_output=True, timeout=10)
        env["rustc"] = r.stdout.decode().strip()
    except Exception:
        env["rustc"] = "unknown"
    env["veyra_git_sha"] = _git(["rev-parse", "HEAD"], root) or "unknown"
    env["veyra_dirty"] = bool(_git(["status", "--porcelain"], root))
    env["binary_path"] = str(bin_dir)
    return env


def render_report(run, result):
    lines = []
    lines.append("## %s — %s" % (result["id"], result["title"]))
    lines.append("")
    lines.append("Status: **%s**  Duration: %.1fs  Stack: %s"
                 % (result["status"], result["duration_ms"] / 1000.0,
                    result["stack"]))
    if result.get("failure_class"):
        lines.append("Failure class: **%s**" % result["failure_class"])
    if result.get("block_reason"):
        lines.append("Block reason: %s" % result["block_reason"])
    lines.append("")

    lines.append("### Deterministic checks")
    lines.append("")
    lines.append("| Check | Result | Detail |")
    lines.append("|---|---|---|")
    for c in result["deterministic"]:
        lines.append("| %s | %s | %s |" % (c["name"], c["status"], c["detail"]))
    if not result["deterministic"]:
        lines.append("| (none) | | |")
    lines.append("")

    if result["screenshots"]:
        lines.append("### Screenshots")
        lines.append("")
        for s in result["screenshots"]:
            meta = {}
            try:
                with open(run.test_dir / s["file"] + ".json") as f:
                    meta = json.load(f)
            except Exception:
                pass
            extra = ""
            if meta.get("changed_pct_vs_previous") is not None:
                extra = " — changed %.3f%% vs previous" % meta["changed_pct_vs_previous"]
            lines.append("- `%s`%s" % (s["file"], extra))
        lines.append("")

    if result["vlm_results"]:
        lines.append("### VLM")
        lines.append("")
        for v in result["vlm_results"]:
            lines.append("- `%s` [%s] — %s (conf %.2f) — response: %s"
                         % (v["checkpoint"], v["template"], v["verdict"],
                            v["confidence"], v["response_ref"] or "n/a"))
        if result["visual"]["anomalies"]:
            lines.append("")
            lines.append("Anomalies:")
            for a in result["visual"]["anomalies"]:
                lines.append("- %s (conf %.2f): %s"
                             % (a["class"], a["confidence"], a["detail"]))
        lines.append("")

    if result["compositor_errors"]:
        lines.append("### Compositor errors")
        lines.append("")
        for e in result["compositor_errors"][:10]:
            lines.append("- `%s`" % e)
        lines.append("")

    if result["notes"]:
        lines.append("### Notes")
        lines.append("")
        for n in result["notes"]:
            lines.append("- %s" % n)
        lines.append("")

    lines.append("### Final verdict")
    lines.append("")
    verdict = result["status"]
    if verdict == "FAIL" and result.get("failure_class"):
        verdict = "FAIL (%s)" % result["failure_class"]
    if verdict == "BLOCKED" and result.get("block_reason"):
        verdict = "BLOCKED — %s" % result["block_reason"]
    lines.append("**%s**" % verdict)
    lines.append("")
    return "\n".join(lines)


def build_manifest(run_dir, scenario_files, tags, results, env, vlm_info, seed=None):
    counts = {"total": len(results), "pass": 0, "fail": 0, "blocked": 0,
              "skipped": 0, "not_applicable": 0, "crashes": 0,
              "vlm_anomalies": 0}
    failures = []
    tests = []
    for r in results:
        if r["status"] == "FAIL":
            if r.get("failure_class") == "CRASH":
                counts["crashes"] += 1
            failures.append({
                "id": r["id"],
                "failure_class": r.get("failure_class") or "UNKNOWN",
                "summary": (r.get("block_reason")
                            or "; ".join(c["name"] for c in r["deterministic"]
                                         if c["status"] == "FAIL"))[:200],
            })
        counts["vlm_anomalies"] += len(r.get("visual", {}).get("anomalies", []))
        tests.append({
            "id": r["id"],
            "status": r["status"],
            "failure_class": r.get("failure_class"),
            "duration_ms": r["duration_ms"],
            "result_ref": "tests/%s/result.json" % r["id"],
        })
    counts["pass"] = sum(1 for r in results if r["status"] == "PASS")
    counts["fail"] = sum(1 for r in results if r["status"] == "FAIL")
    counts["blocked"] = sum(1 for r in results if r["status"] == "BLOCKED")
    counts["skipped"] = sum(1 for r in results if r["status"] == "SKIPPED")
    counts["not_applicable"] = sum(1 for r in results
                                   if r["status"] == "NOT_APPLICABLE")

    comparison = _compare_with_previous(run_dir, failures)

    manifest = {
        "run_id": Path(run_dir).name,
        "started_at": results[0]["started_at"] if results else "",
        "finished_at": results[-1]["finished_at"] if results else "",
        "runner_version": RUNNER_VERSION,
        "veyra": {
            "git_sha": env["veyra_git_sha"],
            "dirty": env["veyra_dirty"],
            "binary_path": env["binary_path"],
            "rustc": env.get("rustc", "unknown"),
        },
        "environment": env,
        "vlm": vlm_info,
        "suite": {
            "scenario_files": scenario_files,
            "tags": tags,
            "seed": seed,
        },
        "counts": counts,
        "failures": failures,
        "tests": tests,
    }
    if comparison:
        manifest["comparison_with_previous"] = comparison
    return manifest


def _compare_with_previous(run_dir, failures):
    runs = sorted(Path(run_dir).parent.glob("20*"))
    prev = None
    for r in reversed(runs):
        if r.resolve() != Path(run_dir).resolve() and (r / "manifest.json").exists():
            prev = r
            break
    if prev is None:
        return None
    try:
        with open(prev / "manifest.json") as f:
            prev_manifest = json.load(f)
    except Exception:
        return None
    prev_fail_ids = {f["id"] for f in prev_manifest.get("failures", [])}
    cur_fail_ids = {f["id"] for f in failures}
    prev_all = {t["id"]: t["status"] for t in prev_manifest.get("tests", [])}
    cur_all = {}
    return {
        "previous_run_id": prev.name,
        "new_failures": sorted(cur_fail_ids - prev_fail_ids),
        "recurring_failures": sorted(cur_fail_ids & prev_fail_ids),
        "fixed": sorted(i for i in prev_fail_ids
                        if prev_all.get(i) == "FAIL" and i not in cur_fail_ids),
    }


def render_summary(manifest, run_dir):
    c = manifest["counts"]
    env = manifest["environment"]
    vlm = manifest["vlm"]
    lines = []
    lines.append("# E2E RUN SUMMARY — %s" % manifest["run_id"])
    lines.append("")
    lines.append("- veyra: `%s`%s" % (env["veyra_git_sha"][:12],
                                      " (dirty tree)" if env["veyra_dirty"] else ""))
    lines.append("- GPU: %s" % env.get("gpu", "unknown"))
    lines.append("- VLM: %s (model %s, %s)"
                 % ("enabled" if vlm.get("enabled") else "disabled",
                    vlm.get("model", "n/a"), vlm.get("base_url_host", "n/a")))
    lines.append("")
    lines.append("| Status | Count |")
    lines.append("|---|---|")
    for key, label in (("total", "Total"), ("pass", "PASS"), ("fail", "FAIL"),
                       ("blocked", "BLOCKED"), ("skipped", "SKIPPED"),
                       ("not_applicable", "NOT_APPLICABLE")):
        lines.append("| %s | %d |" % (label, c[key]))
    lines.append("")
    lines.append("Crashes: %d" % c.get("crashes", 0))
    lines.append("")
    lines.append("VLM anomalies recorded: %d" % c.get("vlm_anomalies", 0))
    lines.append("")

    if manifest.get("failures"):
        lines.append("## Failures")
        lines.append("")
        for f in manifest["failures"]:
            lines.append("- **%s** [%s] — %s"
                         % (f["id"], f["failure_class"], f["summary"]))
        lines.append("")

    comp = manifest.get("comparison_with_previous")
    if comp:
        lines.append("## Comparison with %s" % comp["previous_run_id"])
        lines.append("")
        lines.append("- New failures: %s"
                     % (", ".join(comp["new_failures"]) or "none"))
        lines.append("- Recurring failures: %s"
                     % (", ".join(comp["recurring_failures"]) or "none"))
        lines.append("- Fixed since previous run: %s"
                     % (", ".join(comp["fixed"]) or "none"))
        lines.append("")

    not_executable = [t for t in manifest["tests"]
                      if t["status"] in ("SKIPPED", "BLOCKED")]
    if not_executable:
        lines.append("## Not executable in this environment")
        lines.append("")
        for t in not_executable:
            reason = ""
            try:
                with open(Path(run_dir) / "tests" / t["id"] / "result.json") as f:
                    r = json.load(f)
                reason = r.get("block_reason") or "; ".join(
                    n["detail"] for n in r["deterministic"]) or \
                    "; ".join(r.get("notes", []))
            except Exception:
                pass
            lines.append("- %s [%s] — %s" % (t["id"], t["status"], reason[:200]))
        lines.append("")
    return "\n".join(lines)

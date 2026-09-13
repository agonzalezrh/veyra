#!/usr/bin/env python3
"""Veyra E2E validation runner — implements tests/e2e/README.md.

Commands:
  validate [--all | SCENARIO...]     schema-validate scenario YAML
  run [--tags daily|nightly|soak] [--only ID,...] [--list] [--dry-run]
      [--keep-all-shots] [--bin DIR] [--no-vlm] [--seed N]
  report RUN_DIR                     regenerate manifest/summary from results
"""

import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import yaml

import report as report_mod
import steps as steps_mod
import vlm as vlm_mod
from steps import ScenarioRun

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))
SCENARIO_DIR = os.path.join(ROOT, "tests", "e2e", "scenarios")
RUNS_DIR = os.path.join(ROOT, "tests", "e2e", "runs")
SCHEMA_PATH = os.path.join(ROOT, "tests", "e2e", "spec", "scenario.schema.json")


def load_scenarios(paths):
    scenarios = []
    for p in paths:
        with open(p) as f:
            sc = yaml.safe_load(f)
        sc["_path"] = p
        scenarios.append(sc)
    return scenarios


def all_scenario_files():
    return sorted(glob.glob(os.path.join(SCENARIO_DIR, "*.yaml")))


def validate_files(paths):
    try:
        import jsonschema
    except ImportError:
        print("jsonschema not installed; falling back to YAML-parse-only check")
        jsonschema = None
    with open(SCHEMA_PATH) as f:
        schema = json.load(f)
    failed = 0
    for p in paths:
        try:
            with open(p) as f:
                sc = yaml.safe_load(f)
            if jsonschema:
                jsonschema.validate(sc, schema)
            print("OK   %s (%s)" % (sc.get("id", "?"), os.path.basename(p)))
        except Exception as e:
            failed += 1
            print("BAD  %s: %s" % (p, str(e)[:300]))
    return failed


def preflight(bin_dir, run_dir):
    missing = []
    for b in ("veyra", "client-kit"):
        if not os.access(os.path.join(bin_dir, b), os.X_OK):
            missing.append(b)
    if missing:
        print("[e2e] missing binaries: %s" % ", ".join(missing))
        print("       run: cargo build && cargo build -p client-kit")
        print("       (or point --bin / VEYRA_HARNESS_BIN at your build dir)")
        return False
    stale = []
    veyra_bin = os.path.join(bin_dir, "veyra")
    kit_bin = os.path.join(bin_dir, "client-kit")
    for src_dir, bin_path in ((os.path.join(ROOT, "src"), veyra_bin),
                              (os.path.join(ROOT, "tests", "harness", "src"), kit_bin)):
        newer = subprocess.run(
            ["find", src_dir, "-name", "*.rs", "-newer", bin_path],
            capture_output=True).stdout.decode().strip()
        if newer:
            stale.append(os.path.basename(bin_path))
    if stale:
        print("[e2e] stale binaries (%s); building..." % ", ".join(stale))
        log = open(os.path.join(run_dir, "build.log"), "wb")
        r1 = subprocess.run(["cargo", "build"], cwd=ROOT, stdout=log,
                            stderr=subprocess.STDOUT)
        r2 = subprocess.run(["cargo", "build", "-p", "client-kit"], cwd=ROOT,
                            stdout=log, stderr=subprocess.STDOUT)
        log.close()
        if r1.returncode != 0 or r2.returncode != 0:
            print("[e2e] cargo build FAILED — see tests/e2e/runs/%s/build.log"
                  % os.path.basename(run_dir))
            return False
    return True


def select_scenarios(files, args):
    scenarios = load_scenarios(files)
    if args.only:
        wanted = {x.strip() for x in args.only.split(",")}
        scenarios = [s for s in scenarios if s["id"] in wanted]
    if args.tags:
        tagset = set(args.tags)
        scenarios = [s for s in scenarios
                     if s.get("priority") in tagset
                     or tagset & set(s.get("tags", []))]
    return scenarios


def run_suite(args):
    if not os.path.exists(RUNS_DIR):
        os.makedirs(RUNS_DIR)
    run_id = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%SZ")
    run_dir = os.path.join(RUNS_DIR, run_id)
    os.makedirs(os.path.join(run_dir, "logs"), exist_ok=True)
    os.makedirs(os.path.join(run_dir, "vlm", "raw"), exist_ok=True)

    bin_dir = args.bin or os.environ.get("VEYRA_HARNESS_BIN") \
        or os.path.join(ROOT, "target", "debug")

    files = all_scenario_files()
    scenarios = select_scenarios(files, args)
    if args.list:
        for s in scenarios:
            print("%s [%s] %s" % (s["id"], s.get("priority"), s["title"]))
        return 0
    if args.dry_run:
        failed = validate_files([s["_path"] for s in scenarios])
        return 1 if failed else 0
    if not scenarios:
        print("[e2e] no scenarios selected")
        return 0
    if not preflight(bin_dir, run_dir):
        return 2

    vlm_client = None
    if not args.no_vlm:
        vlm_client = vlm_mod.Vlm()
        if not vlm_client.available:
            print("[e2e] VLM: no API key — required visual checks will BLOCK")
    vlm_info = {
        "enabled": vlm_client is not None and vlm_client.available,
        "base_url_host": vlm_client.url.split("//")[-1].split("/")[0] if vlm_client else "n/a",
        "model": vlm_client.model if vlm_client else "n/a",
        "timeout_s": vlm_client.timeout_s if vlm_client else 0,
        "max_tokens": vlm_client.max_tokens if vlm_client else 0,
    }

    print("[e2e] run %s — %d scenario(s), bin=%s, vlm=%s"
          % (run_id, len(scenarios), bin_dir, vlm_info["enabled"]))
    results = []
    for sc in scenarios:
        print("[e2e] %s — %s" % (sc["id"], sc["title"]))
        run = ScenarioRun(sc, run_dir, bin_dir, vlm_client,
                          keep_all_shots=args.keep_all_shots)
        result = run.execute()
        results.append(result)
        marker = result["status"]
        extra = ""
        if result.get("failure_class"):
            extra = " [%s]" % result["failure_class"]
        if result.get("block_reason"):
            extra = " (%s)" % result["block_reason"][:80]
        print("[e2e] %s —> %s%s (%.1fs)"
              % (sc["id"], marker, extra, result["duration_ms"] / 1000.0))

    env = report_mod.build_environment(ROOT, bin_dir)
    with open(os.path.join(run_dir, "environment.json"), "w") as f:
        json.dump(env, f, indent=2)
    scenario_files = [os.path.relpath(s["_path"], ROOT) for s in scenarios]
    manifest = report_mod.build_manifest(run_dir, scenario_files,
                                         sorted(args.tags), results, env,
                                         vlm_info, seed=args.seed)
    with open(os.path.join(run_dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    with open(os.path.join(run_dir, "summary.md"), "w") as f:
        f.write(report_mod.render_summary(manifest, run_dir))
    last_log = run_dir + "/tests/" + results[-1]["id"] + "/compositor.log"
    if os.path.exists(last_log):
        shutil.copy(last_log, os.path.join(run_dir, "logs", "compositor.log"))
    latest = os.path.join(RUNS_DIR, "latest")
    if os.path.islink(latest) or os.path.exists(latest):
        try:
            os.remove(latest)
        except OSError:
            pass
    try:
        os.symlink(run_id, latest)
    except OSError:
        pass

    c = manifest["counts"]
    print("[e2e] done: %d total — %d PASS, %d FAIL, %d BLOCKED, %d SKIPPED"
          % (c["total"], c["pass"], c["fail"], c["blocked"], c["skipped"]))
    print("[e2e] artifacts: %s" % run_dir)
    if c["fail"] or c["blocked"]:
        return 1
    return 0


def report_cmd(run_dir):
    results = []
    tests_dir = os.path.join(run_dir, "tests")
    for res in sorted(glob.glob(os.path.join(tests_dir, "*", "result.json"))):
        with open(res) as f:
            results.append(json.load(f))
    env_path = os.path.join(run_dir, "environment.json")
    env = json.load(open(env_path)) if os.path.exists(env_path) \
        else report_mod.build_environment(ROOT, os.path.join(ROOT, "target", "debug"))
    old_manifest = os.path.join(run_dir, "manifest.json")
    tags = []
    seed = None
    scenario_files = []
    if os.path.exists(old_manifest):
        m = json.load(open(old_manifest))
        tags = m["suite"].get("tags", [])
        seed = m["suite"].get("seed")
        scenario_files = m["suite"].get("scenario_files", [])
    vlm_info = {"enabled": False, "model": "n/a", "base_url_host": "n/a",
                "timeout_s": 0, "max_tokens": 0}
    manifest = report_mod.build_manifest(run_dir, scenario_files, tags,
                                         results, env, vlm_info, seed)
    with open(os.path.join(run_dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    with open(os.path.join(run_dir, "summary.md"), "w") as f:
        f.write(report_mod.render_summary(manifest, run_dir))
    print("[e2e] regenerated manifest + summary in %s" % run_dir)
    return 0


def main():
    ap = argparse.ArgumentParser(description="Veyra E2E validation runner")
    sub = ap.add_subparsers(dest="cmd", required=True)

    ap_val = sub.add_parser("validate")
    ap_val.add_argument("scenarios", nargs="*")
    ap_val.add_argument("--all", action="store_true")

    ap_run = sub.add_parser("run")
    ap_run.add_argument("--tags", nargs="*", default=[])
    ap_run.add_argument("--only", type=str, default="")
    ap_run.add_argument("--list", action="store_true")
    ap_run.add_argument("--dry-run", action="store_true")
    ap_run.add_argument("--keep-all-shots", action="store_true", default=True)
    ap_run.add_argument("--bin", type=str, default="")
    ap_run.add_argument("--no-vlm", action="store_true")
    ap_run.add_argument("--seed", type=int, default=None)

    ap_rep = sub.add_parser("report")
    ap_rep.add_argument("run_dir")

    args = ap.parse_args()
    if args.cmd == "validate":
        if args.all or not args.scenarios:
            files = all_scenario_files()
        else:
            files = args.scenarios
        sys.exit(validate_files(files))
    elif args.cmd == "run":
        sys.exit(run_suite(args))
    elif args.cmd == "report":
        sys.exit(report_cmd(args.run_dir))


if __name__ == "__main__":
    main()

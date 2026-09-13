"""Scenario execution: step handlers, assertions, checkpoints, result composition."""

import json
import os
import re
import shutil
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

import shots
import vlm as vlm_mod

EXPR_RE = re.compile(r"\{\{(.*?)\}\}")
FULL_EXPR_RE = re.compile(r"^\{\{(.*?)\}\}$", re.DOTALL)

FAILURE_KEYWORDS = [
    ("panicked", "CRASH"),
    ("surface mapped", "WINDOW_LIFECYCLE"),
    ("surface destroyed", "WINDOW_LIFECYCLE"),
    ("minimize", "WINDOW_LIFECYCLE"),
    ("maximize", "WINDOW_LIFECYCLE"),
    ("fullscreen", "WINDOW_LIFECYCLE"),
    ("close sent", "WINDOW_LIFECYCLE"),
    ("resize", "WINDOW_LIFECYCLE"),
    ("focus", "FOCUS"),
    ("workspace", "WORKSPACE"),
    ("camera", "CAMERA"),
    ("taskbar", "SHELL"),
    ("menu", "SHELL"),
    ("xwayland", "XWAYLAND"),
    ("xterm", "XWAYLAND"),
    ("dnd", "DND"),
    ("clip", "CLIPBOARD"),
    ("stability", "TIMING"),
    ("wait_log", "TIMING"),
    ("capture", "RENDERING"),
]

ANOMALY_CLASS_MAP = {
    "DETACHED_DECORATION": "RENDERING",
    "MISSING_SURFACE": "RENDERING",
    "BLACK_SURFACE": "RENDERING",
    "WHITE_SURFACE": "RENDERING",
    "WRONG_POPUP_POSITION": "GEOMETRY",
    "WRONG_TASKBAR_STATE": "SHELL",
    "WRONG_FOCUS": "FOCUS",
    "WRONG_SPATIAL_POSE": "CAMERA",
    "TEXT_CORRUPTION": "RENDERING",
}

CLIENT_JSON_BUILTINS = {
    "any": any, "all": all, "sum": sum, "len": len, "min": min, "max": max,
    "abs": abs, "enumerate": enumerate, "float": float, "int": int, "str": str,
}

EXPR_BUILTINS = {"int": int, "float": float, "min": min, "max": max,
                 "abs": abs, "round": round}


class CrashAbort(Exception):
    pass


class TimeoutAbort(Exception):
    pass


def now_iso():
    return datetime.now(timezone.utc).isoformat()


def sha256(path):
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


class ScenarioRun:
    def __init__(self, sc, run_dir, bin_dir, vlm, keep_all_shots=True):
        self.sc = sc
        self.id = sc["id"]
        self.run_dir = Path(run_dir)
        self.test_dir = self.run_dir / "tests" / self.id
        self.shots_dir = self.test_dir / "shots"
        self.clients_dir = self.test_dir / "clients"
        self.tmp_dir = self.test_dir / "tmp"
        self.raw_vlm_dir = self.run_dir / "vlm" / "raw" / self.id
        self.bin_dir = bin_dir
        self.vlm = vlm
        self.keep_all_shots = keep_all_shots
        self.checks = []
        self.vlm_results = []
        self.screenshots = []
        self.client_logs = []
        self.client_procs = []
        self.notes = []
        self.anchors = {}
        self.deferred = []
        self.vars = {}
        self.seq = 0
        self.spatial_toggles = 0
        self.ws_shifts = 0
        self.crash = None
        self.timeout = None
        self.vlm_required_blocked = None
        self.started_at = None
        self.started_mono = None
        self.stack = None
        self._prev_shot = None
        self._anomalies = []
        self.visual_failed = False
        self.visual_class = None

    # ---------- infrastructure ----------

    def check_preconditions(self):
        req = self.sc.get("requires", {})
        missing = []
        for b in req.get("binaries", []):
            if not os.access(os.path.join(self.bin_dir, b), os.X_OK):
                missing.append("binary:%s" % b)
        for t in req.get("tools", []):
            if shutil.which(t) is None:
                missing.append("tool:%s" % t)
        for a in req.get("apps", []):
            if shutil.which(a) is None:
                missing.append("app:%s" % a)
        for e in req.get("env", []):
            if not os.environ.get(e):
                missing.append("env:%s" % e)
        return missing

    def seed_vars(self):
        w, h = self.stack.win_w, self.stack.win_h
        cx, cy = w // 2, h // 2
        self.vars.update({
            "WIN_W": w, "WIN_H": h,
            "CX": cx, "CY": cy,
            "XL": cx - 320, "XR": cx + 320,
            "YT": cy - 255, "YB": cy + 255,
            "TB_TOP": h - 36, "TB_H": 36,
            "WID0": self.stack.wid0,
            "VEYRA_SOCKET": self.stack.socket,
            "VEYRA_RUNTIME": self.stack.workdir,
        })

    # ---------- expression evaluation ----------

    def resolve(self, value):
        if isinstance(value, list):
            return [self.resolve(v) for v in value]
        if not isinstance(value, str) or "{{" not in value:
            return value
        full = FULL_EXPR_RE.match(value.strip())
        if full:
            return self.eval_expr(full.group(1))
        return EXPR_RE.sub(lambda m: str(self.eval_expr(m.group(1))), value)

    def eval_expr(self, expr):
        ns = dict(self.vars)
        ns["win_cx"] = self._win_cx
        ns["win_cy"] = self._win_cy
        ns["mapped_count"] = self._mapped_count
        ns["xdisplay"] = self.stack.xdisplay
        return eval(expr, {"__builtins__": EXPR_BUILTINS}, ns)

    def _win_center(self, token):
        c = self.stack.win_center(token)
        if c is None:
            raise ValueError("no 'surface mapped' log line matching %r" % token)
        return c

    def _win_cx(self, token):
        return self._win_center(token)[0]

    def _win_cy(self, token):
        return self._win_center(token)[1]

    def _mapped_count(self, token):
        return sum(1 for l in self.stack.lines()
                   if "surface mapped" in l and token in l)

    # ---------- recording ----------

    def check(self, name, ok, detail=""):
        self.checks.append({
            "name": name,
            "status": "PASS" if ok else "FAIL",
            "detail": str(detail)[:400],
        })
        if not ok:
            print("    FAIL: %s (%s)" % (name, str(detail)[:160]))
        return ok

    def note(self, text):
        self.notes.append(str(text)[:400])

    # ---------- xdotool ----------

    def xdo(self, *args, timeout=15):
        env = dict(os.environ)
        env["DISPLAY"] = ":99"
        r = subprocess.run(["xdotool"] + list(args), env=env,
                           capture_output=True, timeout=timeout)
        if r.returncode != 0:
            raise RuntimeError("xdotool %s failed: %s"
                               % (" ".join(args), r.stderr.decode()[:120]))
        return r.stdout.decode(errors="replace").strip()

    def client_env(self):
        env = dict(os.environ)
        env["XDG_RUNTIME_DIR"] = self.stack.workdir
        env["WAYLAND_DISPLAY"] = self.stack.socket
        return env

    # ---------- step dispatch ----------

    def run_steps(self):
        handlers = {
            "launch": self.step_launch,
            "exec": self.step_exec,
            "kill": self.step_kill,
            "quiesce": self.step_quiesce,
            "click": self.step_click,
            "drag": self.step_drag,
            "key": self.step_key,
            "chord": self.step_chord,
            "type": self.step_type,
            "scroll": self.step_scroll,
            "focus_x11": self.step_focus_x11,
            "release_mods": self.step_release_mods,
            "wait_log": self.step_wait_log,
            "wait_stable": self.step_wait_stable,
            "capture": self.step_capture,
            "assert": self.step_assert,
            "set_context": self.step_set_context,
            "sleep_ms": self.step_sleep,
        }
        deadline = time.time() + self.sc["timeout_s"]
        for step in self.sc["steps"]:
            active = time.time() - self.started_mono - getattr(self, "paused", 0.0)
            if active > self.sc["timeout_s"]:
                raise TimeoutAbort("scenario timeout (%ds active)"
                                   % self.sc["timeout_s"])
            key = next(iter(step))
            handler = handlers.get(key)
            if handler is None:
                self.check("step:%s" % key, False, "unknown step type")
                continue
            try:
                handler(step[key])
            except (CrashAbort, TimeoutAbort):
                raise
            except Exception as e:
                self.check("step:%s" % key, False, "%s: %s" % (type(e).__name__, e))

    # ---------- client/process steps ----------

    def step_launch(self, spec):
        kit = spec["kit"]
        args = [str(self.resolve(a)) for a in spec.get("args", [])]
        cmd = [os.path.join(self.bin_dir, "client-kit"), kit] + args
        app_id = spec.get("app_id")
        if app_id:
            cmd += ["--app-id", app_id]
        out = self.clients_dir / spec.get(
            "json", "%s-%d.json" % (kit, len(self.client_logs)))
        name = "launch %s%s" % (kit, " (%s)" % app_id if app_id else "")
        proc = subprocess.Popen(cmd, stdout=open(out, "wb"),
                                stderr=open(str(out) + ".err", "wb"),
                                env=self.client_env())
        self.client_procs.append(proc)
        self.client_logs.append(str(out.relative_to(self.test_dir)))
        time.sleep(0.5)
        rc = proc.poll()
        if rc is not None and rc != 0:
            self.check(name, False, "client exited immediately rc=%s" % rc)
        else:
            self.check(name, True, " ".join(args))

    def step_exec(self, spec):
        cmd = [str(self.resolve(c)) for c in spec["cmd"]]
        env = dict(os.environ)
        disp = spec.get("display", "none")
        if disp == "xvfb":
            env["DISPLAY"] = ":99"
        elif disp == "xwayland":
            env["DISPLAY"] = ":%d" % self.stack.xdisplay()
        if spec.get("wayland"):
            env["XDG_RUNTIME_DIR"] = self.stack.workdir
            env["WAYLAND_DISPLAY"] = self.stack.socket
        for k, v in (spec.get("env") or {}).items():
            env[k] = str(self.resolve(v))
        out = self.clients_dir / ("exec-%d.log" % len(self.client_procs))
        name = "exec %s" % cmd[0]
        proc = subprocess.Popen(cmd, stdout=open(out, "wb"),
                                stderr=subprocess.STDOUT, env=env)
        self.client_procs.append(proc)
        self.client_logs.append(str(out.relative_to(self.test_dir)))
        wl = spec.get("wait_log")
        if wl:
            self.step_wait_log(wl)
        else:
            time.sleep(0.5)
        rc = proc.poll()
        if rc is not None:
            self.check(name, False, "exited rc=%s" % rc)
        else:
            self.check(name, True, "pid=%d" % proc.pid)

    def step_kill(self, spec):
        sig = {"TERM": "-TERM", "KILL": "-KILL"}[spec.get("signal", "KILL")]
        r = subprocess.run(["pkill", sig, "-f", spec["match"]], capture_output=True)
        self.check("kill %s" % spec["match"], r.returncode == 0,
                   "pkill rc=%d" % r.returncode)

    def step_quiesce(self, _spec):
        subprocess.run(["pkill", "-f",
                        os.path.join(self.bin_dir, "client-kit")],
                       capture_output=True)
        time.sleep(0.4)
        self.check("quiesce", True, "client-kit processes killed")

    # ---------- input steps ----------

    def step_click(self, spec):
        x = int(self.resolve(spec["x"]))
        y = int(self.resolve(spec["y"]))
        button = spec.get("button", 1)
        name = "click @%d,%d btn=%d" % (x, y, button)
        try:
            self.xdo("mousemove", str(x), str(y))
            time.sleep(0.1)
            if spec.get("double"):
                self.xdo("click", "--repeat", "2", "--delay", "60", str(button))
            else:
                self.xdo("click", str(button))
            self.check(name, True)
        except Exception as e:
            self.check(name, False, e)

    def step_drag(self, spec):
        try:
            fx = int(self.resolve(spec["from"][0]))
            fy = int(self.resolve(spec["from"][1]))
            tx = int(self.resolve(spec["to"][0]))
            ty = int(self.resolve(spec["to"][1]))
        except Exception as e:
            self.check("drag", False, "resolve: %s" % e)
            return
        button = str(spec.get("button", 1))
        steps = spec.get("steps", 6)
        delay = spec.get("step_delay_ms", 150) / 1000.0
        name = "drag (%d,%d)->(%d,%d) btn=%s" % (fx, fy, tx, ty, button)
        try:
            self.xdo("mousemove", str(fx), str(fy))
            time.sleep(0.15)
            self.xdo("mousedown", button)
            for i in range(1, steps + 1):
                xi = fx + (tx - fx) * i // steps
                yi = fy + (ty - fy) * i // steps
                self.xdo("mousemove", str(xi), str(yi))
                time.sleep(delay)
            self.xdo("mouseup", button)
            self.check(name, True)
        except Exception as e:
            self.check(name, False, e)

    def step_key(self, spec):
        key = str(self.resolve(spec["key"]))
        try:
            self.xdo("keydown", key)
            time.sleep(0.08)
            self.xdo("keyup", key)
            self.check("key %s" % key, True)
        except Exception as e:
            self.check("key %s" % key, False, e)
        if key == "F5":
            self.spatial_toggles += 1

    def step_chord(self, spec):
        mods = spec.get("mods", [])
        key = str(self.resolve(spec["key"]))
        hold = spec.get("hold_ms", 150) / 1000.0
        name = "chord %s+%s" % ("+".join(mods), key)
        try:
            for m in mods:
                self.xdo("keydown", m)
            time.sleep(hold)
            self.xdo("key", key)
            time.sleep(hold)
            for m in reversed(mods):
                self.xdo("keyup", m)
            self.check(name, True)
        except Exception as e:
            self.check(name, False, e)
        if key == "Tab" and mods == ["ctrl"]:
            self.ws_shifts += 1
        elif key == "Tab" and mods == ["ctrl", "shift"]:
            self.ws_shifts -= 1
        elif (key == "F5" and not mods) or (key == "Tab" and mods == ["super"]):
            self.spatial_toggles += 1

    def step_type(self, spec):
        text = str(self.resolve(spec["text"]))
        try:
            if spec.get("clear_mods", True):
                self.xdo("keyup", "super", "alt", "ctrl", "shift")
            self.xdo("type", "--delay", "40", text)
            self.check("type %r" % text, True)
        except Exception as e:
            self.check("type %r" % text, False, e)

    def step_scroll(self, spec):
        x = int(self.resolve(spec["x"]))
        y = int(self.resolve(spec["y"]))
        direction = spec["direction"]
        clicks = spec.get("clicks", 3)
        button = {"up": "4", "down": "5", "left": "6", "right": "7"}[direction]
        name = "scroll %s x%d @%d,%d" % (direction, clicks, x, y)
        try:
            self.xdo("mousemove", str(x), str(y))
            time.sleep(0.1)
            for _ in range(clicks):
                self.xdo("click", button)
                time.sleep(0.1)
            self.check(name, True)
        except Exception as e:
            self.check(name, False, e)

    def step_focus_x11(self, _spec):
        try:
            if not self.stack.wid0:
                raise RuntimeError("no veyra X window found")
            self.xdo("windowfocus", self.stack.wid0)
            self.check("focus_x11", True, "wid=%s" % self.stack.wid0)
        except Exception as e:
            self.check("focus_x11", False, e)

    def step_release_mods(self, _spec):
        try:
            self.xdo("keyup", "super", "alt", "ctrl", "shift")
            self.check("release_mods", True)
        except Exception as e:
            self.check("release_mods", False, e)

    # ---------- observation steps ----------

    def _match_lines(self, pattern, app_id=None, after=None):
        start = 0
        if after:
            if after not in self.anchors:
                return None, "unknown anchor %r" % after
            start = self.anchors[after]
        out = []
        for i, line in enumerate(self.stack.lines(), 1):
            if i <= start:
                continue
            if pattern in line and (not app_id or "app_id=%s" % app_id in line
                                    or app_id in line):
                out.append((i, line))
        return out, None

    def step_wait_log(self, spec):
        pattern = spec["pattern"]
        app_id = spec.get("app_id")
        after = spec.get("after")
        within_s = spec.get("within_s", 10)
        anchor = spec.get("anchor")
        name = "wait_log: %s%s" % (pattern, " [%s]" % app_id if app_id else "")
        deadline = time.time() + within_s
        while time.time() < deadline:
            matches, err = self._match_lines(pattern, app_id, after)
            if err:
                self.check(name, False, err)
                return
            if matches:
                if anchor:
                    self.anchors[anchor] = matches[-1][0]
                self.check(name, True, "line %d" % matches[-1][0])
                return
            time.sleep(0.5)
        self.check(name, False, "timeout after %ds" % within_s)

    def step_wait_stable(self, spec):
        _, attempts, achieved, pct = shots.wait_stable(
            self.tmp_dir, spec.get("max_changed_pct", 0.3),
            spec.get("sample_interval_ms", 150), spec.get("timeout_s", 6))
        self.check("wait_stable", achieved,
                   "attempts=%d changed=%.3f%%" % (attempts, pct))

    # ---------- capture ----------

    def step_capture(self, spec):
        name = spec["name"]
        category = spec["category"]
        self.seq += 1
        seq = self.seq
        final = self.shots_dir / ("%03d-%s.png" % (seq, name))
        stab = spec.get("stability")
        attempts = 1
        achieved = None
        maxpct = None
        if not shots.capture(self.tmp_dir / "cap.png"):
            self.check("capture:%s" % name, False, "import failed")
            return
        if stab:
            src, attempts, achieved, pct = shots.wait_stable(
                self.tmp_dir, stab.get("max_changed_pct", 0.3),
                stab.get("sample_interval_ms", 150), stab.get("timeout_s", 6))
            maxpct = stab.get("max_changed_pct", 0.3)
            shutil.copy(src, final)
            self.check("stability:%s" % name, achieved,
                       "attempts=%d changed=%.3f%% max=%.2f%%"
                       % (attempts, pct, maxpct))
        else:
            shutil.copy(self.tmp_dir / "cap.png", final)
        changed_prev = None
        if self._prev_shot is not None:
            changed_prev = shots.changed_pct(self._prev_shot, final)
        self.screenshots.append({
            "file": str(final.relative_to(self.test_dir)),
            "meta_ref": str(final) + ".json",
        })
        meta = {
            "test_id": self.id,
            "checkpoint": name,
            "sequence": seq,
            "category": category,
            "timestamp": now_iso(),
            "display_size": [self.stack.win_w, self.stack.win_h],
            "wayland_display": self.stack.socket,
            "x11_display": ":99",
            "sha256": sha256(final),
            "changed_pct_vs_previous": changed_prev,
            "stability": None if stab is None else {
                "achieved": bool(achieved),
                "attempts": attempts,
                "max_changed_pct": maxpct,
            },
            "expected_spatial_mode": self._expected_spatial(),
            "expected_workspace": max(0, self.ws_shifts),
            "focused_window": None,
        }
        with open(str(final) + ".json", "w") as f:
            json.dump(meta, f, indent=2)
        self.check("capture:%s" % name, True, final.name)

        for chk in spec.get("image_checks", []):
            self._image_check(name, final, chk)

        vspec = spec.get("vlm")
        if vspec:
            self._vlm_checkpoint(seq, name, final, vspec)

        self._prev_shot = final
        self._invariants(name)

    def _image_check(self, name, final, chk):
        if isinstance(chk, str):
            if chk == "not_black":
                mean, _ = shots.gray_stats(final)
                self.check("img:%s:not_black" % name, mean >= 0.02, "mean=%.4f" % mean)
            elif chk == "not_white":
                mean, _ = shots.gray_stats(final)
                self.check("img:%s:not_white" % name, mean <= 0.98, "mean=%.4f" % mean)
            elif chk == "size_matches_display":
                got = shots.image_size(final)
                want = (self.stack.win_w, self.stack.win_h)
                self.check("img:%s:size_matches_display" % name, got == want,
                           "got=%s want=%s" % (got, want))
            else:
                self.check("img:%s:%s" % (name, chk), False, "unknown image check")
            return
        if "region_not_uniform" in chk:
            region = self._region(chk["region_not_uniform"])
            if region is None:
                self.check("img:%s:region_not_uniform" % name, False, "bad region")
                return
            _, stddev = shots.gray_stats(final, region)
            self.check("img:%s:region_not_uniform" % name, stddev >= 0.01,
                       "region=%s stddev=%.4f" % (region, stddev))
        elif "region_min_changed_pct" in chk:
            spec = chk["region_min_changed_pct"]
            region = self._region(spec["region"])
            if self._prev_shot is None:
                self.check("img:%s:region_min_changed_pct" % name, False,
                           "no previous capture")
                return
            pct = shots.changed_pct(self._prev_shot, final, region=region)
            self.check("img:%s:region_min_changed_pct" % name, pct >= spec["pct"],
                       "region=%s changed=%.3f%% want>=%.2f%%"
                       % (region, pct, spec["pct"]))

    def _region(self, region):
        if isinstance(region, list):
            return tuple(region)
        w, h = self.stack.win_w, self.stack.win_h
        if region == "taskbar":
            return (0, h - 36, w, 36)
        if region == "fullscreen_area":
            return (0, 0, w, h - 36)
        if region == "center":
            return (w // 2 - 200, h // 2 - 150, 400, 300)
        return None

    def _vlm_checkpoint(self, seq, name, final, vspec):
        template = vspec["template"]
        required = vspec.get("required", False)
        min_conf = vspec.get("min_confidence", 0.7)
        images = [final]
        if template == "compare":
            ref = vspec.get("compare_with")
            ref_path = self._shot_by_name(ref)
            if ref_path is None:
                self.check("vlm:%s" % name, False, "compare_with %r not found" % ref)
                return
            images = [ref_path, final]
        expected = vspec.get("expected")
        t0 = time.time()
        res = vlm_mod.inspect(
            self.vlm, images, template, str(self.raw_vlm_dir),
            "%03d-%s" % (seq, name),
            expected=expected, ignore=(expected or {}).get("ignore"),
            question=vspec.get("question"), text=vspec.get("text"),
            min_conf=min_conf, required=required)
        self.paused = getattr(self, "paused", 0.0) + (time.time() - t0)
        self.vlm_results.append({
            "checkpoint": "%03d-%s" % (seq, name),
            "template": template,
            "verdict": res.verdict,
            "confidence": res.confidence,
            "response_ref": os.path.relpath(res.response_ref, self.run_dir)
            if res.response_ref else "",
        })
        if res.error:
            if required:
                if self.vlm_required_blocked is None:
                    self.vlm_required_blocked = ("VLM unavailable at checkpoint %s: %s"
                                                 % (name, res.error))
                self.check("vlm:%s" % name, False, "INFRA %s" % res.error)
            else:
                self.note("vlm skipped at %s: %s" % (name, res.error))
            return
        anom_txt = "; ".join("%s(%s,%.2f)" % (a["class"], a["detail"][:40], a["confidence"])
                             for a in res.anomalies) or "none"
        self.check("vlm:%s" % name, res.status == "PASS",
                   "%s conf=%.2f anomalies=%s" % (res.verdict, res.confidence, anom_txt))
        for a in res.anomalies:
            self._anomalies.append(a)
        for n in res.notes:
            self.note("vlm %s: %s" % (name, n))
        if res.status == "FAIL":
            self.visual_failed = True
            if self.visual_class is None and res.anomalies:
                self.visual_class = res.anomalies[0]["class"]

    def _invariants(self, checkpoint):
        if self.stack.veyra_proc.poll() is not None:
            self.check("invariant:process_alive", False,
                       "veyra exited at %s" % checkpoint)
            raise CrashAbort("veyra process exited at checkpoint %s" % checkpoint)
        if not self.stack.socket_path():
            self.check("invariant:socket_alive", False,
                       "socket vanished at %s" % checkpoint)
            raise CrashAbort("wayland socket vanished at checkpoint %s" % checkpoint)
        for line in self.stack.lines():
            if "panicked" in line:
                self.check("invariant:no_panic", False, line[:200])
                raise CrashAbort("compositor panic at checkpoint %s" % checkpoint)

    # ---------- assert steps ----------

    def step_assert(self, spec):
        kind = next(iter(spec))
        body = spec[kind]
        if kind == "log_contains":
            pattern = body["pattern"]
            after = body.get("after")
            within_s = body.get("within_s", 10)
            name = "log_contains: %s" % pattern
            deadline = time.time() + within_s
            while time.time() < deadline:
                matches, err = self._match_lines(pattern, None, after)
                if err:
                    self.check(name, False, err)
                    return
                if matches:
                    self.check(name, True, "line %d" % matches[-1][0])
                    return
                time.sleep(0.5)
            self.check(name, False, "not found within %ds" % within_s)
        elif kind == "log_not_contains":
            since = body.get("since")
            self.deferred.append((body["pattern"],
                                  self.anchors.get(since, 0) if since else 0,
                                  since))
        elif kind == "client_json":
            path = self.clients_dir / body["file"]
            name = "client_json: %s" % body["file"]
            try:
                events = []
                with open(path) as f:
                    for line in f:
                        line = line.strip()
                        if line:
                            try:
                                events.append(json.loads(line))
                            except json.JSONDecodeError:
                                pass
                ok = bool(eval(body["expr"],
                               {"__builtins__": CLIENT_JSON_BUILTINS},
                               {"events": events}))
                self.check(name, ok, body["expr"][:160])
            except FileNotFoundError:
                self.check(name, False, "client log missing: %s" % path)
            except Exception as e:
                self.check(name, False, "expr error: %s" % e)
        elif kind == "process_alive":
            ok = self.stack.veyra_proc.poll() is None
            self.check("process_alive: veyra", ok)
        elif kind == "wayland_socket_alive":
            self.check("wayland_socket_alive", bool(self.stack.socket_path()))
        elif kind == "x11_window_visible":
            try:
                wids = self.xdo("search", "--onlyvisible", "--name", ".")
                self.check("x11_window_visible", bool(wids), wids[:80])
            except Exception as e:
                self.check("x11_window_visible", False, e)

    def run_deferred(self):
        lines = self.stack.lines()
        for pattern, since_line, since_name in self.deferred:
            scope = "since %s" % since_name if since_name else "whole log"
            ok = not any(pattern in l for l in lines[since_line:])
            self.check("log_not_contains: %s (%s)" % (pattern, scope), ok)

    def step_set_context(self, spec):
        try:
            value = self.resolve(spec["value"])
            self.vars[spec["name"]] = value
            self.check("set_context %s" % spec["name"], True, str(value))
        except Exception as e:
            self.check("set_context %s" % spec["name"], False, e)

    def step_sleep(self, spec):
        time.sleep(spec["value"] / 1000.0)

    # ---------- result composition ----------

    def _expected_spatial(self):
        base = "--normal" in self.sc.get("veyra", {}).get("args", [])
        return bool(base) ^ bool(self.spatial_toggles % 2)

    def _shot_by_name(self, name):
        for s in self.screenshots:
            if s["file"].endswith("-%s.png" % name):
                return self.test_dir / s["file"]
        return None

    def _classify_machine(self):
        for c in self.checks:
            if c["status"] != "FAIL":
                continue
            text = (c["name"] + " " + c["detail"]).lower()
            if "panicked" in text or "invariant" in text:
                return "CRASH"
            for kw, cls in FAILURE_KEYWORDS:
                if kw in text:
                    return cls
            if c["name"].startswith("client_json"):
                expr = c["detail"].lower()
                for kw, cls in (("dnd", "DND"), ("clip", "CLIPBOARD"),
                                ("kb_", "INPUT"), ("ptr_", "INPUT"),
                                ("key", "INPUT"), ("button", "INPUT"),
                                ("config", "WAYLAND_PROTOCOL"),
                                ("commit", "WAYLAND_PROTOCOL"),
                                ("frame", "WAYLAND_PROTOCOL")):
                    if kw in expr:
                        return cls
        return "UNKNOWN"

    def _classify_visual(self):
        for a in self._anomalies:
            if a["class"] in ANOMALY_CLASS_MAP:
                return ANOMALY_CLASS_MAP[a["class"]]
        return "VISUAL"

    def _compositor_errors(self, cap=50):
        out = []
        for line in self.stack.lines():
            if " ERROR " in line or "panicked" in line:
                out.append(line.strip()[:300])
                if len(out) >= cap:
                    break
        return out

    def compose_result(self):
        machine_fail = any(
            c["status"] == "FAIL"
            and not (c["name"].startswith("vlm:") and c["detail"].startswith("INFRA"))
            for c in self.checks)
        status = "PASS"
        failure_class = None
        block_reason = None
        if self.crash:
            status, failure_class = "FAIL", "CRASH"
        elif self.timeout:
            status, failure_class = "FAIL", "TIMING"
        elif machine_fail:
            status = "FAIL"
            failure_class = self._classify_machine()
        elif self.visual_failed:
            status = "FAIL"
            failure_class = self._classify_visual()
        elif self.vlm_required_blocked:
            status = "BLOCKED"
            block_reason = self.vlm_required_blocked
        vlm_real = [v for v in self.vlm_results
                    if v["verdict"] in ("PASS", "FAIL", "UNCERTAIN")]
        visual_status = "SKIPPED"
        if vlm_real:
            visual_status = "FAIL" if self.visual_failed else "PASS"
        return {
            "id": self.id,
            "title": self.sc["title"],
            "category": self.sc["category"],
            "stack": self.sc["stack"],
            "status": status,
            "failure_class": failure_class,
            "block_reason": block_reason,
            "duration_ms": self.duration_ms,
            "started_at": self.started_at,
            "finished_at": now_iso(),
            "deterministic": self.checks,
            "visual": {"status": visual_status, "anomalies": self._anomalies},
            "vlm_results": self.vlm_results,
            "screenshots": self.screenshots,
            "client_logs": self.client_logs,
            "compositor_errors": self._compositor_errors(),
            "notes": self.notes,
            "known_limitations": [
                "software rendering (llvmpipe/Xvfb) — visual findings are "
                "indicative, not native-DRM proof",
            ],
        }

    # ---------- top-level ----------

    def execute(self):
        from stack import Stack

        self.started_at = now_iso()
        self.started_mono = time.time()
        missing = self.check_preconditions()
        if missing:
            result = self._simple_result(
                "SKIPPED", note="unmet preconditions: %s" % ", ".join(missing))
            self._write_artifacts(result)
            return result

        ve = self.sc.get("veyra", {})
        self.stack = Stack(self.bin_dir, self.tmp_dir / "runtime",
                           self.test_dir / "compositor.log",
                           self.test_dir / "xvfb.log",
                           veyra_args=ve.get("args", ()),
                           rust_log=ve.get("rust_log",
                                           "veyra=info,veyra::compositor=debug"))
        for d in (self.shots_dir, self.clients_dir, self.tmp_dir):
            d.mkdir(parents=True, exist_ok=True)
        ok, err = self.stack.start()
        if not ok:
            result = self._simple_result(
                "BLOCKED", block_reason="stack startup failed: %s" % err)
            self._write_artifacts(result)
            self.stack.stop()
            return result
        self.seed_vars()
        try:
            self.run_steps()
        except TimeoutAbort as t:
            self.timeout = str(t)
        except CrashAbort as c:
            self.crash = str(c)
        finally:
            if not self.crash:
                try:
                    self.run_deferred()
                except Exception as e:
                    self.check("deferred_asserts", False, e)
            self._teardown()
        result = self.compose_result()
        self._write_artifacts(result)
        return result

    def _teardown(self):
        for proc in self.client_procs:
            if proc.poll() is None:
                try:
                    proc.kill()
                except OSError:
                    pass
        for proc in self.client_procs:
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass
        if self.stack:
            self.stack.stop()

    def _simple_result(self, status, block_reason=None, note=None):
        if note:
            self.note(note)
        return {
            "id": self.id,
            "title": self.sc["title"],
            "category": self.sc["category"],
            "stack": self.sc["stack"],
            "status": status,
            "failure_class": None,
            "block_reason": block_reason,
            "duration_ms": self.duration_ms if self.started_mono else 0,
            "started_at": self.started_at,
            "finished_at": now_iso(),
            "deterministic": self.checks,
            "visual": {"status": "SKIPPED", "anomalies": []},
            "vlm_results": self.vlm_results,
            "screenshots": self.screenshots,
            "client_logs": self.client_logs,
            "compositor_errors": [],
            "notes": self.notes,
            "known_limitations": [],
        }

    def _write_artifacts(self, result):
        with open(self.test_dir / "result.json", "w") as f:
            json.dump(result, f, indent=2)
        from report import render_report
        with open(self.test_dir / "report.md", "w") as f:
            f.write(render_report(self, result))

    @property
    def duration_ms(self):
        if self.started_mono is None:
            return 0
        return int((time.time() - self.started_mono) * 1000)

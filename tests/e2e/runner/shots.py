"""Screenshot capture, stability detection and deterministic image checks (ImageMagick)."""

import os
import subprocess
import time

DISPLAY = ":99"
FUZZ = "2%"


def _env():
    env = dict(os.environ)
    env["DISPLAY"] = DISPLAY
    return env


def _run(cmd, timeout=30):
    return subprocess.run(cmd, env=_env(), capture_output=True, timeout=timeout)


def capture(path):
    r = _run(["import", "-window", "root", str(path)], timeout=15)
    return r.returncode == 0 and os.path.exists(path)


def image_size(path):
    r = _run(["identify", "-format", "%w %h", str(path)], timeout=15)
    parts = r.stdout.decode().split()
    return int(parts[0]), int(parts[1])


def gray_stats(path, region=None):
    if region:
        x, y, w, h = region
        crop = ["-crop", "%dx%d+%d+%d" % (w, h, x, y), "+repage"]
    else:
        crop = []
    r = _run(["convert", str(path)] + crop + [
        "-format", "%[fx:mean] %[fx:standard_deviation]", "info:",
    ], timeout=20)
    parts = r.stdout.decode().split()
    mean = float(parts[0]) if parts else 0.0
    stddev = float(parts[1]) if len(parts) > 1 else 0.0
    return mean, stddev


def changed_pct(a, b, region=None):
    total = None
    try:
        a2, b2 = str(a), str(b)
        tmp_a = tmp_b = None
        if region:
            x, y, w, h = region
            base = os.path.splitext(str(a))[0]
            tmp_a = base + ".ca.png"
            tmp_b = base + ".cb.png"
            _run(["convert", str(a), "-crop", "%dx%d+%d+%d" % (w, h, x, y), "+repage", tmp_a])
            _run(["convert", str(b), "-crop", "%dx%d+%d+%d" % (w, h, x, y), "+repage", tmp_b])
            a2, b2 = tmp_a, tmp_b
            total = w * h
        r = subprocess.run(
            ["compare", "-metric", "AE", "-fuzz", FUZZ, a2, b2, "null:"],
            env=_env(), capture_output=True, timeout=30,
        )
        val = float(r.stderr.decode().strip().split()[0])
        if total is None:
            w, h = image_size(a)
            total = w * h
        return val / total * 100.0 if total else 0.0
    except Exception:
        return 100.0
    finally:
        for tmp in (tmp_a, tmp_b):
            if tmp:
                try:
                    os.remove(tmp)
                except OSError:
                    pass


def wait_stable(tmpdir, max_pct, interval_ms, timeout_s):
    a = os.path.join(str(tmpdir), "stab_a.png")
    b = os.path.join(str(tmpdir), "stab_b.png")
    deadline = time.time() + timeout_s
    attempts = 0
    last_pct = 100.0
    while time.time() < deadline:
        capture(a)
        time.sleep(interval_ms / 1000.0)
        capture(b)
        attempts += 1
        last_pct = changed_pct(a, b)
        if last_pct <= max_pct:
            return b, attempts, True, last_pct
        time.sleep(0.1)
    return b, attempts, False, last_pct

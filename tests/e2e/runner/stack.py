"""Xvfb + veyra stack lifecycle and compositor-log parsing for the E2E runner."""

import os
import re
import subprocess
import time

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
SOCKET_RE = re.compile(r"Listening on wayland socket: (wayland-[a-z0-9-]+)")
XWAYLAND_RE = re.compile(r"display\s*=\s*([0-9]+)")
POS_RE = re.compile(r"pos=Vector3 \[([^,\]]+), ([^,\]]+)")
DISPLAY_GEOMETRY = (1280, 720)


def strip_ansi(text):
    return ANSI_RE.sub("", text)


def user_runtime_dir():
    return "/run/user/%d" % os.getuid()


class Stack:
    def __init__(self, bin_dir, workdir, log_path, xvfb_log_path, veyra_args=(), rust_log="veyra=info,veyra::compositor=debug"):
        self.bin_dir = bin_dir
        self.workdir = str(workdir)
        self.log_path = str(log_path)
        self.xvfb_log_path = str(xvfb_log_path)
        self.veyra_args = [str(a) for a in veyra_args]
        self.rust_log = rust_log
        self.xvfb_proc = None
        self.veyra_proc = None
        self.socket = None
        self.win_w, self.win_h = DISPLAY_GEOMETRY
        self.wid0 = ""
        self._cache_key = None
        self._lines = []

    def _spawn(self, cmd, env, out_path):
        out = open(out_path, "wb")
        return subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env)

    def start(self):
        os.makedirs(self.workdir, exist_ok=True)
        if os.path.exists("/tmp/.X11-unix/X99"):
            subprocess.run(["pkill", "-x", "Xvfb"], capture_output=True)
            time.sleep(0.8)
        for stale in ("/tmp/.X99-lock", "/tmp/.X11-unix/X99"):
            try:
                os.remove(stale)
            except OSError:
                pass
        env = dict(os.environ)
        env["XDG_RUNTIME_DIR"] = user_runtime_dir()
        self.xvfb_proc = self._spawn(
            ["Xvfb", ":99", "-screen", "0", "%dx%dx24" % DISPLAY_GEOMETRY],
            env, self.xvfb_log_path,
        )
        deadline = time.time() + 10
        while time.time() < deadline:
            if os.path.exists("/tmp/.X11-unix/X99"):
                break
            if self.xvfb_proc.poll() is not None:
                return False, "Xvfb exited during startup (see %s)" % self.xvfb_log_path
            time.sleep(0.25)
        else:
            return False, "Xvfb socket did not appear"

        venv = dict(os.environ)
        venv.pop("WAYLAND_DISPLAY", None)
        venv["RUST_LOG"] = self.rust_log
        venv["XDG_RUNTIME_DIR"] = self.workdir
        venv["DISPLAY"] = ":99"
        self.veyra_proc = self._spawn(
            [os.path.join(self.bin_dir, "veyra")] + self.veyra_args,
            venv, self.log_path,
        )
        deadline = time.time() + 20
        while time.time() < deadline:
            self.socket = self._parse_socket()
            if self.socket and self.socket_path():
                break
            if self.veyra_proc.poll() is not None:
                return False, "veyra exited during startup (see %s)" % self.log_path
            time.sleep(0.25)
        if not self.socket:
            return False, "veyra never reported a Wayland socket (see %s)" % self.log_path
        self.win_w, self.win_h = self.render_size()
        self.wid0 = self._find_wid0()
        return True, ""

    def socket_path(self):
        for base in (self.workdir, user_runtime_dir()):
            p = os.path.join(base, self.socket or "")
            if self.socket and os.path.exists(p):
                return p
        return None

    def _parse_socket(self):
        for line in reversed(self.lines()):
            m = SOCKET_RE.search(line)
            if m:
                return m.group(1)
        return None

    def render_size(self):
        for line in reversed(self.lines()):
            if "render size" in line and "window_size" in line:
                m = re.search(r"\(([^)]*)\)", line)
                if m:
                    try:
                        parts = [round(float(x)) for x in m.group(1).split(",")[:2]]
                        if len(parts) == 2 and parts[0] > 0 and parts[1] > 0:
                            return parts[0], parts[1]
                    except ValueError:
                        pass
        return DISPLAY_GEOMETRY

    def xdisplay(self):
        for line in reversed(self.lines()):
            if "XWayland ready" in line:
                m = XWAYLAND_RE.search(line)
                if m:
                    return int(m.group(1))
        return 0

    def _find_wid0(self):
        env = dict(os.environ)
        env["DISPLAY"] = ":99"
        deadline = time.time() + 10
        while time.time() < deadline:
            r = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--name", "."],
                env=env, capture_output=True, timeout=10,
            )
            wids = r.stdout.decode().split()
            if wids:
                return wids[0]
            time.sleep(0.25)
        return ""

    def lines(self):
        try:
            st = os.stat(self.log_path)
            key = (st.st_mtime_ns, st.st_size)
        except OSError:
            return self._lines
        if key != self._cache_key:
            with open(self.log_path, "rb") as f:
                data = f.read()
            self._lines = strip_ansi(data.decode("utf-8", errors="replace")).splitlines()
            self._cache_key = key
        return self._lines

    def log_text(self):
        return "\n".join(self.lines())

    def win_center(self, token):
        for line in reversed(self.lines()):
            if "surface mapped" in line and token in line:
                m = POS_RE.search(line)
                if m:
                    wx, wy = float(m.group(1)), float(m.group(2))
                    return int(self.win_w / 2 + wx), int(self.win_h / 2 - wy)
        return None

    def mapped_count(self, token):
        return sum(1 for l in self.lines()
                   if "surface mapped" in l and token in l)

    def stop(self):
        for proc in (self.veyra_proc, self.xvfb_proc):
            if proc and proc.poll() is None:
                try:
                    proc.terminate()
                except OSError:
                    pass
        for proc in (self.veyra_proc, self.xvfb_proc):
            if proc:
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        proc.kill()
                    except OSError:
                        pass

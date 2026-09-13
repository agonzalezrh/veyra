# Veyra Local-AI E2E Validation System — Specification

This directory is the **normative specification** for Veyra's end-to-end
validation system. It validates the compositor as a *user-facing Linux
desktop*, not only as a Wayland protocol implementation. The first
implementation push (runner + infrastructure + the vertical slice in
`scenarios/`) must implement exactly what is written here; the wider
scenario inventory (E-A…E-AQ, H-GP) expands later **using this framework**,
never as parallel ad-hoc scripts.

The existing protocol/input harness in `tests/harness/` is unchanged and
remains the machine-evidence foundation. The E2E layer adds four evidence
sources per test:

1. **Machine evidence** — process/exit status, compositor log lines,
   client-kit JSON event logs, protocol/input assertions (deterministic).
2. **Screenshot evidence** — PNG captures at defined checkpoints with
   sidecar metadata.
3. **Local VLM evidence** — screenshots inspected by a local
   OpenAI-compatible vision endpoint (see `spec/vlm.md`). Images go
   straight to the endpoint from the runner process; **only structured
   verdicts enter logs/reports** — screenshots never enter an interactive
   AI agent's prompt context (same constraint as `tests/harness`).
4. **Human-readable reports** — one record per test, one aggregate per
   run, explicit PASS / FAIL / BLOCKED / SKIPPED / NOT_APPLICABLE.

A test is **never** PASS merely because the compositor did not crash, and
never PASS on VLM output alone. PASS requires the deterministic and
(required) visual evidence to agree (see *Evidence combination*).

---

## 1. Directory layout (target)

```text
tests/e2e/
  README.md            ← this spec (normative)
  spec/
    scenario.schema.json          ← scenario YAML contract (JSON Schema 2020-12)
    result.schema.json            ← per-test result JSON
    screenshot-meta.schema.json   ← per-screenshot sidecar metadata
    manifest.schema.json          ← run manifest + environment record
    vlm.md                        ← VLM request/response contract + prompts
    actions.md                    ← action/step vocabulary (normative semantics)
    bindings.md                   ← canonical bindings, geometry, log-pattern
                                    registry, client-kit event catalog
    invariants.md                 ← runtime invariants, failure classes,
                                    visual gate policy, false-positive control
  runner/               ← implementation (Python 3, stdlib + PyYAML)
    runner.py             CLI entry point
    ...                   one module per concern; no monolith
  scenarios/            ← scenario definitions (YAML, schema-validated)
  vlm/raw/<TEST-ID>/    ← raw VLM request/response pairs (evidence)
  runs/                 ← run artifacts (gitignored)
```

## 2. Runner CLI (normative)

```text
python3 tests/e2e/runner.py validate [--all | SCENARIO...]
python3 tests/e2e/runner.py run [--tags daily|nightly|soak] [--only ID[,ID...]]
                                [--list] [--dry-run] [--keep-all-shots] [--seed N]
python3 tests/e2e/runner.py report RUN_DIR
```

- `validate` schema-checks scenario YAML and cross-checks referenced
  capture names / anchors. Exit 0/1.
- `run` executes scenarios **in a fresh stack per scenario** (Xvfb +
  veyra), one run directory per invocation:
  `tests/e2e/runs/<UTC-timestamp>/`.
- Exit codes: `0` all tests PASS/SKIPPED/NOT_APPLICABLE; `1` any
  FAIL/BLOCKED; `2` environment/stack startup failure (nothing ran).
- One failing test never stops the suite unless the compositor itself is
  irrecoverable (then remaining tests are marked BLOCKED, class ENVIRONMENT).

## 3. Test-ID canon (frozen)

Numbering is derived from the inventory section headings and is **frozen
as of this spec** — never renumber after reports exist. (This canon
supersedes the draft numbering in the proposal's §AV/§BN, which shifted
letters by one in places; the mapping is listed so older references stay
resolvable.)

| Prefix   | Category                       | Draft-plan remap                          |
|----------|--------------------------------|-------------------------------------------|
| E-A      | Session startup                | — |
| E-B      | Keyboard input                 | — |
| E-C      | Spatial mode                   | — |
| E-D      | Camera (keyboard)              | — |
| E-E      | Camera (mouse orbit/pan/zoom)  | — |
| E-F      | Window selection               | — |
| E-G      | Window drag / manipulation     | — |
| E-H      | Window resize                  | draft "E-Gxx resize" → E-Hxx |
| E-I      | Maximize                       | draft "E-Hxx" → E-Ixx |
| E-J      | Fullscreen                     | draft "E-Ixx" → E-Jxx |
| E-K      | Minimize / restore             | draft "E-Jxx" → E-Kxx |
| E-L      | Close / reopen                 | draft "E-Kxx" → E-Lxx |
| E-M      | Workspace navigation           | draft "E-Lxx" → E-Mxx |
| E-N      | Workspace overview             | draft "E-Mxx" split |
| E-O      | Window overview                | draft "E-Mxx" split |
| E-P      | Focus mode                     | draft "E-Nxx" → E-Pxx |
| E-Q      | Escape chain                   | — |
| E-R      | Camera bookmarks               | — |
| E-S      | Arrangement                    | draft "E-Pxx" → E-Sxx |
| E-T      | Shelf                          | — |
| E-U      | Taskbar                        | draft "E-Rxx" → E-Uxx |
| E-V      | Context menu                   | draft "E-Sxx" → E-Vxx |
| E-W      | De-emphasis                    | — |
| E-X      | Reset transform                | — |
| E-Y      | Group / ungroup                | — |
| E-Z      | Pointer accuracy               | draft "E-Zxx XWayland" → E-AIxx |
| E-AA     | Browser E2E                    | — |
| E-AB     | Popups / menus (toolkits)      | — |
| E-AC     | Clipboard                      | draft "E-Txx" → E-ACxx |
| E-AD     | Primary selection              | draft "E-Uxx" → E-ADxx |
| E-AE     | Drag and drop                  | draft "E-Vxx" → E-AExx |
| E-AF     | IME / text input               | draft "E-Wxx" → E-AFxx |
| E-AG     | Subsurface / CSD               | draft "E-Xxx" → E-AGxx |
| E-AH     | Fractional scaling             | draft "E-Yxx" → E-AHxx |
| E-AI     | XWayland                       | draft "E-Zxx" → E-AIxx |
| E-AJ     | Pointer lock / constraints     | — |
| E-AK     | Combined-state interaction     | — |
| E-AL     | Full lifecycle sequences       | draft "E-ABxx" → E-ALxx |
| E-AM     | Persistence                    | draft "E-ACxx" → E-AMxx |
| E-AN     | Client destruction torture     | — |
| E-AO     | Workspace torture              | — |
| E-AP     | 3D visual corruption sweep     | draft "E-ADxx anomaly sweep" → E-APxx |
| E-AQ     | Screenshot pair regression     | — |
| H-GP-NNN | Hardware-gated                 | — |

Hardware-gated tests (`H-GP-001..008`: real GLES/dmabuf, DRM page flip,
VT switch, suspend/resume, hotplug, GPU recovery, multi-connector, 24h
soak) are declared `stack: hardware-gated` and produce **BLOCKED**, never
FAIL, when the hardware gate is unmet. Nested/Xvfb software rendering is
explicitly not proof of native DRM behavior.

## 4. Scenario model

A scenario is a YAML document validated against
`spec/scenario.schema.json`. Conceptually:

```text
Scenario → Steps (actions) → State observation → Screenshot
        → Deterministic assertions → VLM (when required) → Result
```

Key rules:

- **Stable-state rule**: every `capture` with a `stability` block takes
  two frames (default 150 ms apart) and requires the changed-pixel
  fraction below `max_changed_pct` (default 0.3%) before capture;
  otherwise wait+retry up to `timeout_s` (default 6). Captures without a
  stability block are immediate and never authoritative for visual
  verdicts. Animation tests declare `stability: null` explicitly.
- **Screenshot naming**: `<seq>-<name>.png` inside the test directory,
  sequence auto-assigned from capture order (`001-`, `002-`, …);
  never overwritten. A `.json` sidecar (screenshot-meta schema) sits
  beside every PNG.
- **Log scoping**: veyra's log is per-scenario (fresh stack), but
  assertions must still anchor: `after: <anchor>` restricts matches to
  lines after a recorded anchor (lesson from the shared-log suites:
  unscoped greps false-pass). Anchors are recorded by `wait_log`.
- **Modifier hygiene**: any scenario that types into a client first runs
  `release_mods` (XTEST duplicate-press history: stuck META).
- **Screenshot categories**: S0 baseline (before first action), S1
  immediately after an action, S2 after stabilization (the authoritative
  visual frame), S3 restoration (after returning to the prior state).

## 5. Evidence combination (normative)

Per checkpoint with VLM:

| Machine asserts | Visual (Stage B)                     | Test status |
|-----------------|--------------------------------------|-------------|
| PASS            | PASS                                 | PASS        |
| PASS            | FAIL (blocking class, conf ≥ 0.7)    | FAIL (VISUAL) |
| PASS            | UNCERTAIN / non-blocking anomaly     | PASS + notes |
| PASS            | VLM infra failure, `required: false` | PASS (visual SKIPPED, noted) |
| PASS            | VLM infra failure, `required: true`  | BLOCKED (ENVIRONMENT) |
| FAIL (any)      | *                                    | FAIL — deterministic evidence wins |

- A VLM **cannot** overturn a deterministic FAIL, and a VLM FAIL cannot
  be hidden by deterministic PASS when the anomaly is in the blocking
  class list (`spec/vlm.md`).
- VLM may only ever assert the *visible consequence* of state
  (e.g. "pasted text visible"); exact payloads, serials, event order
  remain deterministic-only.
- veyra process death at any point ⇒ FAIL, class CRASH, immediately.

## 6. First implementation batch (vertical slice)

Infrastructure: runner, screenshot capture, stable-state detector,
deterministic image checks, VLM adapter, per-screenshot sidecar,
per-test `result.json` + `report.md`, aggregate `manifest.json` +
`summary.md`. Scenarios in `scenarios/`:

| File | Covers |
|------|--------|
| `E-A01-clean-startup.yaml` | startup, taskbar, clean baseline |
| `E-B01-plain-typing.yaml`  | plain keys reach client, no binding theft |
| `E-C01-enter-spatial.yaml` | F5 spatial entry, camera-only change |
| `E-C02-leave-spatial.yaml` | spatial exit, content/focus retained |
| `E-F01-select-window.yaml` | click select, focus follows |
| `E-H01-resize-east.yaml`   | pointer resize end-to-end |
| `E-M01-workspace-switch.yaml` | Ctrl+Tab navigation, isolation |
| `E-V01-context-menu.yaml`  | Menu key opens/closes, no ghost |
| `E-AI01-xwayland-xterm.yaml` | X11 window as first-class visual |
| `E-AP01-anomaly-sweep.yaml` | VLM corruption sweep across modes |

Expansion beyond this batch uses the same framework: write scenario YAML,
no new harness code unless a genuinely new action/step is required.

## 7. Run artifacts

```text
runs/<UTC-timestamp>/
  manifest.json          ← manifest schema (counts, env, git SHAs, VLM)
  summary.md             ← aggregate report (AY format)
  environment.json       ← environment record (BA/BB discipline)
  logs/compositor.log    ← veyra log of the run (per scenario kept under test dir)
  tests/<TEST-ID>/
    result.json
    report.md
    shots/NNN-<name>.png + .json
    clients/*.json       ← client-kit event logs
    compositor.log       ← this scenario's veyra log
  vlm/raw/<TEST-ID>/<seq>-<name>.{request,response}.json
```

`environment.json` records: OS, kernel, GPU/GL renderer (glxinfo when
present; llvmpipe expected here), Xvfb version, screen size, output
scale, Mesa version, veyra git SHA + dirty flag, rustc version, runner
version, config SHA-256, VLM model + endpoint **host only** (never the
API key), stack mode. Visual comparisons never cross builds without
recording the SHA.

**Retention**: keep everything for FAIL/BLOCKED; for PASS keep S0+S2 (and
S1/S3 when defined) unless `--keep-all-shots`. Torture scenarios keep
first/middle/last/failure-adjacent captures.

## 8. Daily / nightly subsets

- `--tags daily` — the fast post-change gate (E-A01, E-B01, E-C01,
  E-C02, E-F01, E-H01, E-I01, E-J01, E-K01, E-L01, E-M01, E-O01, E-P01,
  E-S01, E-U01, E-V01, E-AC01, E-AD01, E-AE01, E-AF01, E-AG01, E-AH01,
  E-AI01). Target: minutes, runs on every push.
- `--tags nightly` — full deterministic suite + all arrangements +
  randomized exploration (seeded) + 200-client churn + E-AP anomaly
  sweep + persistence restart. Targets: 0 crashes, 0 protocol failures,
  0 input failures, 0 unexplained VLM anomalies.

## 9. Relationship to `tests/harness/`

The e2e runner **reuses** the established mechanics (documented in
`spec/bindings.md`): the Xvfb :99 1280x720 stack, `--normal` startup +
`WAYLAND_DISPLAY` unset for X11 mode, ortho 1:1 world↔screen mapping,
client-kit JSON events, first-visual geometry, taskbar geometry,
`start_veyra_x11`-style lifecycle, and the local VLM endpoint from
`tests/harness/scripts/visual_check.py` (same env vars, extended into
the structured contract in `spec/vlm.md`). It does not modify the
compositor or the existing suites.

## 10. Per-test report template (normative shape)

```markdown
## E-C01 — Enter spatial mode
Status: PASS  Duration: 2.84s
### Deterministic checks      ← table of assert results (PASS/FAIL/SKIP)
### Screenshots               ← file list w/ checkpoints
### VLM                       ← verdict, confidence, observations, anomalies
### Final verdict             ← PASS / FAIL (class) / BLOCKED (reason)
```

FAIL reports additionally carry: expected vs observed, the failing
deterministic checks, VLM anomalies with classes, relevant log lines
(ANSI-stripped), failure class, likely regression area, and a numbered
reproduction sequence — enough to reproduce without the full run.

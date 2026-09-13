# Runtime invariants, failure classes, gates (normative)

## 1. Runtime invariants

Checked automatically by the runner after every `capture` step and at
scenario end (cheap checks; the rest in torture/random scenarios on
their sampling cadence):

1. veyra process alive (`process_alive`) — violation ⇒ FAIL CRASH.
2. Wayland socket alive (`wayland_socket_alive`).
3. Compositor log contains no `panicked` line.
4. Every visible application has exactly one visual — via
   `surface mapped`/`surface destroyed` accounting per app_id/token.
5. Focused visual is live (focus-history vid appears in the mapped set).
6. No destroyed client remains referenced by later taskbar/state lines.
7. No popup survives its parent's destroy (`popup mapped` count ≤
   surviving parent count per client).
8. No active resize survives a destroyed window
   (`resize session started` without a matching `finished`).
9. No active drag survives a workspace switch (DnD grab lines closed).
10. Application content semantics unchanged by camera/spatial
    navigation — client commits keep flowing across mode toggles
    (`client_json` commit-count progress).

Invariant violations record a deterministic check entry
(`invariant:<n>`) and fail the scenario with the mapped class
(3→CRASH, 4/6→WINDOW_LIFECYCLE, 5→FOCUS, 7/8→WINDOW_LIFECYCLE,
9→DND, 10→CAMERA).

## 2. Failure classes and precedence

Classes (result.schema.json): CRASH, WAYLAND_PROTOCOL, INPUT, FOCUS,
GEOMETRY, RENDERING, VISUAL, WINDOW_LIFECYCLE, WORKSPACE, CAMERA, SHELL,
XWAYLAND, CLIPBOARD, DND, IME, SCALING, PERSISTENCE, TIMING,
PERFORMANCE, TEST_HARNESS, ENVIRONMENT, UNKNOWN.

Precedence when several apply:

1. CRASH (process death / panic) — always wins.
2. Deterministic assertion evidence (log/client/protocol) — the class
   matching the failing subsystem (mapping table below).
3. Visual/VLM evidence — VISUAL unless a blocking anomaly class maps
   more specifically (DETACHED_DECORATION→RENDERING,
   WRONG_POPUP_POSITION→GEOMETRY, WRONG_TASKBAR_STATE→SHELL,
   WRONG_FOCUS→FOCUS, BLACK/WHITE/MISSING_SURFACE→RENDERING).
4. VLM may *suggest* a class in `notes`; deterministic evidence takes
   precedence.

| Evidence | Class |
|---|---|
| protocol/xdg/wl_* misbehavior in client events | WAYLAND_PROTOCOL |
| key/pointer not delivered / stuck modifiers | INPUT |
| focus/MRU/activation wrong | FOCUS |
| size/position/configure geometry wrong | GEOMETRY |
| transform/rotation/scale/camera-mutation wrong | CAMERA |
| workspace membership/isolation wrong | WORKSPACE |
| taskbar/menu/shell UI wrong | SHELL |
| X11/XWayland-specific | XWAYLAND |
| selection/clipboard payload | CLIPBOARD |
| DnD grab/offer/drop | DND |
| text-input/IME | IME |
| fractional scale/viewport | SCALING |
| persistence restore | PERSISTENCE |
| injection landed but state never changed, injection verified working elsewhere | TIMING |
| injection itself did not land (F-key vanishing pattern) | TEST_HARNESS |
| stack/tool/environment unavailable | ENVIRONMENT |
| none of the above | UNKNOWN |

TEST_HARNESS vs TIMING: before classifying TIMING, the runner must show
the same injection mechanism succeeded in another checkpoint of the
same run (or re-verify once); otherwise classify TEST_HARNESS and
re-run the scenario once automatically.

## 3. Status semantics

- **PASS** — required deterministic checks PASS and (where required)
  visual PASS.
- **FAIL** — deterministic FAIL, blocking visual FAIL, crash, or
  timeout. Classified.
- **BLOCKED** — stack/tool unavailable mid-run (ENVIRONMENT), required
  VLM unavailable, or hardware gate unmet (`block_reason` states which).
- **SKIPPED** — precondition unmet (missing app/tool/env), reason
  recorded.
- **NOT_APPLICABLE** — scenario does not apply to the current
  configuration (e.g. single-output run vs multi-output test).

No "probably pass". Never infer PASS from absence of failure alone:
every PASS cites the deterministic checks that prove it.

## 4. Visual gate policy

Blocking visual failures (see vlm.md §4): missing surface, ghost
visual, duplicated decoration, wrong popup position, incorrect
fullscreen presentation, incorrect taskbar state, wrong focus
indicator, severe clipping, black/white/corrupt application surface,
wrong spatial pose. Low-confidence observations are recorded as
UNCERTAIN observations — never FAIL.

## 5. Randomized exploration (nightly; seeds required)

- State space: launch/close/focus/move/resize/rotate/scale/workspace
  switch/spatial toggle/overview/focus-mode/shelf/arrange/fullscreen/
  maximize/minimize/restore/camera move/zoom/scroll/context menu/
  Escape/app-switch — transitions restricted to safe combinations
  (e.g. no close during active DnD; Escape always legal).
- `--seed N` (recorded in manifest.suite.seed); default seeded from
  UTC time and recorded.
- VLM review cadence: every 20 operations, after major state
  transitions, and when a deterministic invariant trips.
- On failure: delta-debugging reduces the sequence to a minimal
  reproducer; the report carries Seed / original sequence / reduced
  sequence.

## 6. Torture scenarios (AN/AO)

Sampling cadence: periodic captures (every 10 clients / 10 workspace
switches / major transition), never per-operation VLM. Keep
first/middle/last/failure-adjacent screenshots. Client-kill variants
must cover: focused, unfocused, minimized, fullscreen, popup open,
IME open, DnD active, during drag, during resize.

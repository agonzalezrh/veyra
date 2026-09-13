# VLM Contract (normative)

The local vision-language model is **visual QA only**. It never owns exact
coordinates, protocol assertions, process success/failure, timing, or
numeric validation. It inspects screenshots and returns strict JSON.
Images are POSTed directly to the endpoint from the runner process;
only the structured verdict enters logs/reports/screenshots' sidecars —
never an interactive AI agent's prompt context.

## 1. Configuration

| Env var          | Default                          | Notes |
|------------------|----------------------------------|-------|
| `VLM_BASE_URL`   | `http://localhost:8888/v1`       | Legacy alias `VEYRA_VLM_URL` honored. |
| `VLM_API_KEY`    | fallback: `provider.vllm.options.apiKey` from `~/.config/opencode/opencode.json` | Same fallback as `tests/harness/scripts/visual_check.py`. |
| `VLM_MODEL`      | `GLM-5.3-Flash-EXL3`             | Legacy alias `VEYRA_VLM_MODEL`. |
| `VLM_TIMEOUT_S`  | `120`                            | Legacy alias `VEYRA_VLM_KEY` remains the key var. |
| `VLM_MAX_TOKENS` | `1200`                           | |

Endpoint: `POST {VLM_BASE_URL}/chat/completions` — OpenAI-compatible,
image as `data:image/png;base64,...` content part, `temperature: 0`,
`max_tokens` from env. One retry on invalid JSON with an appended
reminder ("Your previous reply was not valid JSON; reply with only the
JSON object."); still invalid ⇒ **infrastructure failure**.

## 2. Prompt templates

Rendered prompt = template text + `Expected visual state:` JSON block
(from the scenario's `vlm.expected`, when present) + optional
`question`. Non-negotiable closing line on every template:

> Return ONLY a strict JSON object matching the requested schema. Do not
> declare success merely because the screenshot "looks good".

### `base` — expected-state inspection

```text
You are inspecting a screenshot of the Veyra spatial Wayland desktop
(1280x720). Only report what is visually observable; do not guess hidden
application state.

Evaluate:
1. Whether the expected windows/elements are visible.
2. Whether their relative positions and geometry appear correct.
3. Whether focus/selection indicators match the expected state.
4. Whether the taskbar/shell state matches the expected state.
5. Whether there are ghost windows, duplicated decorations, clipping,
   tearing, black surfaces, missing surfaces, unexpected overlays, or
   visual discontinuities.
6. Whether text that should be visible is readable and approximately
   correct.
7. Whether this is a stable state or an in-between animation.

Expected visual state:
<expected-state JSON>

Schema:
{"verdict":"PASS|FAIL|UNCERTAIN","confidence":0.0-1.0,
 "expected_elements":[],"missing_elements":[],"unexpected_elements":[],
 "visual_anomalies":[{"class":"...","detail":"...","confidence":0.0-1.0}],
 "text_observations":[{"expected":"...","present":true,"corrupted":false,"detail":"..."}],
 "notes":[]}
```

### `compare` — before/after pair

```text
You are comparing two screenshots of the same Veyra desktop.
Screenshot A is BEFORE the action; screenshot B is AFTER.

Expected change:
<expected-state JSON (describe the exact intended transition)>

Determine:
1. Did the expected visual change occur?
2. Did anything unrelated unexpectedly change?
3. Did any window disappear or duplicate?
4. Did decorations remain attached?
5. Did the taskbar remain correct?
6. Did application content remain intact?
7. Did the desktop become visually corrupted?

Ignore harmless anti-aliasing and tiny text rasterization differences.

Schema:
{"verdict":"PASS|FAIL|UNCERTAIN","confidence":0.0-1.0,
 "expected_change_observed":true|false,
 "unexpected_changes":[],"missing_changes":[],
 "visual_anomalies":[{"class":"...","detail":"...","confidence":0.0-1.0}],
 "notes":[]}
```

### `anomaly` — generic defect sweep

```text
Inspect this screenshot specifically for compositor/rendering defects.

Look for:
- duplicated windows, ghost windows, orphaned decorations
- detached borders, floating title bars, orphaned shadows
- missing application surfaces, black or white application regions
- severe clipping, malformed taskbar, incorrectly layered shell UI
- impossible overlap, obvious rendering corruption
- unexpected window fragments, popup detached from its parent
- visibly corrupted text

Report every anomaly, including low-confidence ones. Do not invent
defects that cannot be seen.

Expected visual state:
<expected-state JSON>

Schema: (same as base)
```

### `text` — known-text verification

```text
Expected visible text: "<text>"
Determine: is the text visibly present, in the expected application
window, and uncorrupted? Do not fabricate OCR confidence.

Schema: (same as base; fill text_observations)
```

## 3. Response schema

```json
{
  "verdict": "PASS | FAIL | UNCERTAIN",
  "confidence": 0.0,
  "expected_change_observed": null,
  "expected_elements": [],
  "missing_elements": [],
  "unexpected_elements": [],
  "visual_anomalies": [{ "class": "GHOST", "detail": "...", "confidence": 0.9 }],
  "text_observations": [],
  "notes": []
}
```

Parsing: extract the first balanced `{...}` block. `verdict` and
`confidence` are required; unknown anomaly classes map to `UNKNOWN`;
all observations are stored even when the status is PASS (future
regression corpus).

## 4. Status mapping (normative)

With `min_confidence` = C (default 0.7) and the blocking-class list
below:

| Outcome                                                      | Checkpoint visual status |
|--------------------------------------------------------------|--------------------------|
| verdict PASS, confidence ≥ C                                  | PASS |
| verdict FAIL, confidence ≥ C                                  | **FAIL** |
| verdict FAIL, confidence < C                                  | PASS + recorded uncertain observation |
| verdict UNCERTAIN                                             | PASS + recorded uncertain observation |
| any anomaly of blocking class with confidence ≥ 0.7           | **FAIL** (regardless of verdict) |
| CLIPPING-class anomaly with confidence ≥ 0.8                  | **FAIL** (severity override) |
| anomalies ≤ `anomalies_max` (non-blocking)                    | PASS, anomalies recorded |
| infra failure, `required: false`                              | SKIPPED (test continues on machine evidence) |
| infra failure, `required: true`                               | BLOCKED (class ENVIRONMENT) |

### Blocking anomaly classes

`GHOST`, `DUPLICATE`, `DETACHED_DECORATION`, `MISSING_SURFACE`,
`BLACK_SURFACE`, `WHITE_SURFACE`, `WRONG_POPUP_POSITION`,
`WRONG_TASKBAR_STATE`, `WRONG_FOCUS`, `WRONG_SPATIAL_POSE`,
`TEXT_CORRUPTION` (conf ≥ 0.7), `CLIPPING` (conf ≥ 0.8).

Non-blocking (`WRONG_SCALE`, `WRONG_POSITION`, `WRONG_SHELL_STATE`,
`UNKNOWN`, etc.) are recorded in `result.visual.anomalies` and the run
summary's VLM-anomaly log; they do not fail a test.

## 5. False-positive control (BQ)

The `expected.ignore` list is appended to every prompt as:
`Ignore the following expected differences: <list>`. Standard entries:
font anti-aliasing, subpixel differences, cursor position (unless the
checkpoint is about the cursor), text caret blink, window-manager
shadows, animation micro-differences. The VLM prompt must never contain
leading language suggesting success (BG): prefer "Determine whether the
expected window is visible and whether any unexpected artifacts exist"
over "Check that the correct window is visible".

## 6. Raw evidence

Every request/response pair is stored verbatim:
`vlm/raw/<TEST-ID>/<seq>-<name>.request.json` and `.response.json`.
`result.vlm_results[].response_ref` points at the response file.

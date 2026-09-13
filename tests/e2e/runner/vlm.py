"""Local VLM adapter implementing tests/e2e/spec/vlm.md."""

import base64
import json
import os
import urllib.request

BLOCKING_07 = {
    "GHOST", "DUPLICATE", "DETACHED_DECORATION", "MISSING_SURFACE",
    "BLACK_SURFACE", "WHITE_SURFACE", "WRONG_POPUP_POSITION",
    "WRONG_TASKBAR_STATE", "WRONG_FOCUS", "WRONG_SPATIAL_POSE",
    "TEXT_CORRUPTION",
}
ALL_CLASSES = BLOCKING_07 | {
    "CLIPPING", "WRONG_SCALE", "WRONG_POSITION", "WRONG_SHELL_STATE", "UNKNOWN",
}

ANOMALY_SCHEMA = (
    '"visual_anomalies":[{"class":"GHOST|DUPLICATE|DETACHED_DECORATION|'
    'MISSING_SURFACE|BLACK_SURFACE|WHITE_SURFACE|CLIPPING|WRONG_SCALE|'
    'WRONG_POSITION|WRONG_FOCUS|WRONG_TASKBAR_STATE|WRONG_POPUP_POSITION|'
    'WRONG_SHELL_STATE|WRONG_SPATIAL_POSE|TEXT_CORRUPTION|UNKNOWN",'
    '"detail":"...","confidence":0.0}]'
)

CLOSING = (
    "Return ONLY a strict JSON object matching the requested schema. "
    'Do not declare success merely because the screenshot "looks good".'
)


def _load_key():
    key = os.environ.get("VLM_API_KEY") or os.environ.get("VEYRA_VLM_KEY")
    if key:
        return key
    cfg = os.path.expanduser("~/.config/opencode/opencode.json")
    try:
        with open(cfg) as f:
            return json.load(f)["provider"]["vllm"]["options"]["apiKey"]
    except Exception:
        return None


class Vlm:
    def __init__(self):
        base = (os.environ.get("VLM_BASE_URL")
                or os.environ.get("VEYRA_VLM_URL")
                or "http://localhost:8888/v1")
        self.url = base.rstrip("/") + "/chat/completions"
        self.key = _load_key()
        self.model = (os.environ.get("VLM_MODEL")
                      or os.environ.get("VEYRA_VLM_MODEL")
                      or "GLM-5.3-Flash-EXL3")
        self.timeout_s = int(os.environ.get("VLM_TIMEOUT_S", "120"))
        self.max_tokens = int(os.environ.get("VLM_MAX_TOKENS", "1200"))

    @property
    def available(self):
        return self.key is not None

    def _call(self, images, prompt):
        content = [{"type": "image_url",
                    "image_url": {"url": "data:image/png;base64,%s" % _b64(p)}}
                   for p in images]
        content.append({"type": "text", "text": prompt})
        body = {
            "model": self.model,
            "max_tokens": self.max_tokens,
            "temperature": 0,
            "messages": [{"role": "user", "content": content}],
        }
        req = urllib.request.Request(
            self.url, data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer %s" % self.key,
                     "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
            return json.load(resp)["choices"][0]["message"].get("content") or ""


def _b64(path):
    with open(path, "rb") as f:
        return base64.b64encode(f.read()).decode()


def extract_json(text):
    start = text.find("{")
    if start < 0:
        raise ValueError("no JSON object in response")
    depth = 0
    in_str = False
    esc = False
    for i in range(start, len(text)):
        ch = text[i]
        if esc:
            esc = False
            continue
        if ch == "\\":
            esc = True
            continue
        if ch == '"':
            in_str = not in_str
            continue
        if in_str:
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return json.loads(text[start:i + 1])
    raise ValueError("unbalanced JSON in response")


def _expected_block(expected, ignore):
    parts = []
    if expected:
        parts.append("Expected visual state:\n%s" % json.dumps(expected, indent=2))
    if ignore:
        parts.append("Ignore the following expected differences: %s"
                     % "; ".join(ignore))
    return "\n\n".join(parts)


def _schema_base():
    return ("Schema:\n"
            '{"verdict":"PASS|FAIL|UNCERTAIN","confidence":0.0,\n'
            ' "expected_elements":[],"missing_elements":[],"unexpected_elements":[],\n'
            " %s,\n"
            ' "text_observations":[{"expected":"...","present":true,'
            '"corrupted":false,"detail":"..."}],\n'
            ' "notes":[]}' % ANOMALY_SCHEMA)


def _schema_compare():
    return ("Schema:\n"
            '{"verdict":"PASS|FAIL|UNCERTAIN","confidence":0.0,\n'
            ' "expected_change_observed":true|false,\n'
            ' "unexpected_changes":[],"missing_changes":[],\n'
            " %s,\n"
            ' "notes":[]}' % ANOMALY_SCHEMA)


def build_prompt(template, expected=None, ignore=None, question=None, text=None):
    if template == "compare":
        body = (
            "You are comparing two screenshots of the same Veyra desktop.\n"
            "Screenshot A is BEFORE the action; screenshot B is AFTER.\n\n"
            "Expected change:\n%s\n\n"
            "Determine:\n"
            "1. Did the expected visual change occur?\n"
            "2. Did anything unrelated unexpectedly change?\n"
            "3. Did any window disappear or duplicate?\n"
            "4. Did decorations remain attached?\n"
            "5. Did the taskbar remain correct?\n"
            "6. Did application content remain intact?\n"
            "7. Did the desktop become visually corrupted?\n\n"
            "Ignore harmless anti-aliasing and tiny text rasterization differences.\n"
            % (_expected_block(expected, ignore) or "(none)")
        )
        closing_schema = _schema_compare()
    elif template == "anomaly":
        body = (
            "Inspect this screenshot specifically for compositor/rendering defects.\n\n"
            "Look for:\n"
            "- duplicated windows, ghost windows, orphaned decorations\n"
            "- detached borders, floating title bars, orphaned shadows\n"
            "- missing application surfaces, black or white application regions\n"
            "- severe clipping, malformed taskbar, incorrectly layered shell UI\n"
            "- impossible overlap, obvious rendering corruption\n"
            "- unexpected window fragments, popup detached from its parent\n"
            "- visibly corrupted text\n\n"
            "Report every anomaly, including low-confidence ones. Do not invent "
            "defects that cannot be seen.\n\n%s" % _expected_block(expected, ignore)
        )
        closing_schema = _schema_base()
    elif template == "text":
        body = (
            'Expected visible text: "%s"\n'
            "Determine: is the text visibly present, in the expected application "
            "window, and uncorrupted? Do not fabricate OCR confidence.\n\n%s"
            % (text or "", _expected_block(expected, ignore))
        )
        closing_schema = _schema_base()
    else:
        body = (
            "You are inspecting a screenshot of the Veyra spatial Wayland desktop "
            "(1280x720). Only report what is visually observable; do not guess "
            "hidden application state.\n\n"
            "Evaluate:\n"
            "1. Whether the expected windows/elements are visible.\n"
            "2. Whether their relative positions and geometry appear correct.\n"
            "3. Whether focus/selection indicators match the expected state.\n"
            "4. Whether the taskbar/shell state matches the expected state.\n"
            "5. Whether there are ghost windows, duplicated decorations, clipping, "
            "tearing, black surfaces, missing surfaces, unexpected overlays, or "
            "visual discontinuities.\n"
            "6. Whether text that should be visible is readable and approximately correct.\n"
            "7. Whether this is a stable state or an in-between animation.\n\n"
            "%s" % (_expected_block(expected, ignore) or "(no explicit expected state)")
        )
        closing_schema = _schema_base()
    if question:
        body += "\n\n%s" % question
    return "%s\n\n%s\n\n%s" % (body, closing_schema, CLOSING)


def evaluate(resp, min_conf, template):
    verdict = str(resp.get("verdict", "UNCERTAIN")).upper()
    try:
        conf = float(resp.get("confidence") or 0.0)
    except (TypeError, ValueError):
        conf = 0.0
    fail = verdict == "FAIL" and conf >= min_conf
    if template == "compare" and resp.get("expected_change_observed") is False:
        fail = True
    anomalies = []
    for a in resp.get("visual_anomalies") or []:
        cls = str(a.get("class", "UNKNOWN")).upper()
        try:
            ac = float(a.get("confidence") or 0.0)
        except (TypeError, ValueError):
            ac = 0.0
        anomalies.append({
            "class": cls if cls in ALL_CLASSES else "UNKNOWN",
            "detail": str(a.get("detail", "")),
            "confidence": ac,
        })
        if cls == "CLIPPING" and ac >= 0.8:
            fail = True
        elif cls in BLOCKING_07 and ac >= 0.7:
            fail = True
    status = "FAIL" if fail else "PASS"
    return status, anomalies, verdict, conf


class InspectResult:
    def __init__(self):
        self.status = "SKIPPED"
        self.verdict = "INFRA_SKIPPED"
        self.confidence = 0.0
        self.anomalies = []
        self.notes = []
        self.error = ""
        self.response_ref = ""
        self.required = False


def inspect(vlm, images, template, raw_dir, raw_name, expected=None, ignore=None,
            question=None, text=None, min_conf=0.7, required=False):
    res = InspectResult()
    res.required = required
    if vlm is None or not vlm.available:
        res.error = "VLM unavailable (no API key)"
        return res
    prompt = build_prompt(template, expected=expected, ignore=ignore,
                          question=question, text=text)
    request_body = {
        "template": template,
        "images": [os.path.basename(str(p)) for p in images],
        "prompt": prompt,
    }
    os.makedirs(raw_dir, exist_ok=True)
    req_path = os.path.join(raw_dir, raw_name + ".request.json")
    resp_path = os.path.join(raw_dir, raw_name + ".response.json")
    with open(req_path, "w") as f:
        json.dump(request_body, f, indent=2)
    try:
        content = vlm._call(images, prompt)
    except Exception as e:
        res.error = "endpoint failure: %s" % e
        with open(resp_path, "w") as f:
            json.dump({"error": res.error}, f, indent=2)
        res.response_ref = resp_path
        return res
    parsed = None
    parse_error = ""
    for attempt in range(2):
        try:
            parsed = extract_json(content)
            break
        except Exception as e:
            parse_error = str(e)
            if attempt == 0:
                try:
                    content = vlm._call(
                        images, prompt + "\n\nYour previous reply was not valid "
                        "JSON. Reply with ONLY the JSON object.")
                except Exception as e2:
                    res.error = "endpoint failure on retry: %s" % e2
                    break
    with open(resp_path, "w") as f:
        json.dump({"content": content}, f, indent=2)
    res.response_ref = resp_path
    if parsed is None:
        res.error = "invalid JSON after retry: %s" % parse_error
        return res
    status, anomalies, verdict, conf = evaluate(parsed, min_conf, template)
    res.status = status
    res.verdict = verdict
    res.confidence = conf
    res.anomalies = anomalies
    for key in ("missing_elements", "unexpected_elements", "notes"):
        for item in parsed.get(key) or []:
            res.notes.append("%s: %s" % (key, item))
    if template == "compare" and parsed.get("expected_change_observed") is False:
        res.notes.append("expected_change_observed=false")
    return res

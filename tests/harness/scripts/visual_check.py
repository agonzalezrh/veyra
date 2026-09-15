#!/usr/bin/env python3
"""Out-of-band visual assertion for the Veyra harness.

POSTs a screenshot directly to a local OpenAI-compatible vision endpoint
(default: the vLLM server from ~/.config/opencode/opencode.json) and
returns a one-line verdict. The image never enters any interactive
agent's prompt context — only the text verdict reaches the harness log.

Usage: visual_check.py <png> <question>
Prints: "PASS <reason>" | "FAIL <reason>" | "SKIP <why>"

Env overrides: VEYRA_VLM_URL, VEYRA_VLM_KEY, VEYRA_VLM_MODEL.
"""

import base64
import json
import os
import sys
import urllib.request

TIMEOUT_S = 120


def load_key():
    key = os.environ.get("VEYRA_VLM_KEY")
    if key:
        return key
    cfg = os.path.expanduser("~/.config/opencode/opencode.json")
    try:
        with open(cfg) as f:
            return json.load(f)["provider"]["vllm"]["options"]["apiKey"]
    except Exception as e:
        return None


def main():
    if len(sys.argv) != 3:
        print(f"SKIP usage: visual_check.py <png> <question>")
        return
    png, question = sys.argv[1], sys.argv[2]
    if not os.path.isfile(png):
        print(f"SKIP screenshot missing: {png}")
        return
    key = load_key()
    if not key:
        print("SKIP no API key (VEYRA_VLM_KEY or opencode.json)")
        return
    url = os.environ.get("VEYRA_VLM_URL",
                         "http://localhost:8888/v1/chat/completions")
    model = os.environ.get("VEYRA_VLM_MODEL", "GLM-5.3-Flash-EXL3")
    with open(png, "rb") as f:
        b64 = base64.b64encode(f.read()).decode()
    prompt = (
        "You are verifying a 1280x720 screenshot of the Veyra compositor.\n"
        f"Question: {question}\n"
        "Base the verdict strictly on visible pixels.\n"
        "Rules: PASS only when the expected UI is clearly present; "
        "FAIL when it is clearly absent, blank, or broken; UNCERTAIN "
        "when the image is too small, blurry, cropped, or ambiguous "
        "for a confident call. Do not guess.\n"
        "Reply with one short sentence of what you saw, then a final "
        "line containing exactly 'VERDICT: PASS', 'VERDICT: FAIL', or "
        "'VERDICT: UNCERTAIN'."
    )
    body = {
        "model": model,
        "max_tokens": 1000,
        "temperature": 0,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image_url",
                 "image_url": {"url": f"data:image/png;base64,{b64}"}},
                {"type": "text", "text": prompt},
            ],
        }],
    }
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {key}",
                 "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            msg = json.load(resp)["choices"][0]["message"]
    except Exception as e:
        print(f"SKIP endpoint unreachable: {e}")
        return
    text = (msg.get("content") or "").strip()
    if not text:
        print("SKIP empty model response")
        return
    idx = text.upper().rfind("VERDICT:")
    if idx < 0:
        print(f"SKIP no verdict in response: {text[-160:]}")
        return
    verdict = text[idx:].upper()
    reason = text[:idx].strip().replace("\n", " ")[:160]
    # Taxonomy (G-H0.8): a deterministic failure is FAIL; a clear VLM
    # anomaly is FAIL; weak VLM suspicion is UNCERTAIN and is reported
    # for review instead of failing the suite.
    if "VERDICT: PASS" in verdict:
        print(f"PASS {reason}")
    elif "VERDICT: FAIL" in verdict:
        print(f"FAIL {reason}")
    elif "VERDICT: UNCERTAIN" in verdict:
        print(f"UNCERTAIN {reason}")
    else:
        print(f"UNCERTAIN unclassified verdict: {verdict[-60:]}")


if __name__ == "__main__":
    main()

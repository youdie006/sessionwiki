#!/usr/bin/env python3
"""Generate a synthetic session store for screenshots and manual testing.

Creates fake-but-plausible Claude Code, Codex, and Gemini CLI sessions under
a throwaway HOME so the web UI can be demoed without exposing real history:

    python3 scripts/demo_data.py /tmp/sa-demo
    HOME=/tmp/sa-demo XDG_DATA_HOME=/tmp/sa-demo/.data sessionwiki web

No real session content is used; everything below is written for the demo.
"""

import json
import os
import sys
import uuid

BASE = sys.argv[1] if len(sys.argv) > 1 else "/tmp/sa-demo"


def ts(day, h, m, s=0):
    return f"2026-06-{day:02d}T{h:02d}:{m:02d}:{s:02d}.000Z"


def claude_session(proj_dir, cwd, summary, turns, day, h):
    d = f"{BASE}/.claude/projects/{proj_dir}"
    os.makedirs(d, exist_ok=True)
    sid = str(uuid.uuid4())
    lines = [{"type": "summary", "summary": summary, "leafUuid": str(uuid.uuid4())}]
    for i, (role, text) in enumerate(turns, 1):
        if role == "user":
            lines.append({"type": "user", "cwd": cwd, "sessionId": sid,
                          "timestamp": ts(day, h, i),
                          "message": {"role": "user", "content": text}})
        elif role == "assistant":
            lines.append({"type": "assistant", "cwd": cwd, "sessionId": sid,
                          "timestamp": ts(day, h, i),
                          "message": {"role": "assistant",
                                      "content": [{"type": "text", "text": text}]}})
        else:
            lines.append({"type": "assistant", "cwd": cwd, "sessionId": sid,
                          "timestamp": ts(day, h, i),
                          "message": {"role": "assistant",
                                      "content": [{"type": "tool_use", "name": role,
                                                   "input": {"command": text}}]}})
    with open(f"{d}/{sid}.jsonl", "w") as f:
        for line in lines:
            f.write(json.dumps(line) + "\n")


def codex_session(cwd, turns, day, h):
    d = f"{BASE}/.codex/sessions/2026/06/{day:02d}"
    os.makedirs(d, exist_ok=True)
    sid = str(uuid.uuid4())
    fn = f"{d}/rollout-2026-06-{day:02d}T{h:02d}-00-00-{sid}.jsonl"
    lines = [{"timestamp": ts(day, h, 0), "type": "session_meta",
              "payload": {"id": sid, "timestamp": ts(day, h, 0), "cwd": cwd,
                          "originator": "codex_cli_rs", "cli_version": "0.92.0"}}]
    for i, (role, text) in enumerate(turns, 1):
        kind = "input_text" if role == "user" else "output_text"
        lines.append({"timestamp": ts(day, h, i), "type": "response_item",
                      "payload": {"type": "message", "role": role,
                                  "content": [{"type": kind, "text": text}]}})
    with open(fn, "w") as f:
        for line in lines:
            f.write(json.dumps(line) + "\n")


def gemini_session(proj, turns, day, h):
    d = f"{BASE}/.gemini/tmp/{proj}/chats"
    os.makedirs(d, exist_ok=True)
    sid = str(uuid.uuid4())
    msgs = [{"id": str(uuid.uuid4()), "timestamp": ts(day, h, i + 1),
             "type": role, "content": [{"text": text}]}
            for i, (role, text) in enumerate(turns)]
    doc = {"sessionId": sid, "projectHash": "demo", "startTime": ts(day, h, 0),
           "lastUpdated": ts(day, h, len(turns)), "messages": msgs}
    with open(f"{d}/session-2026-06-{day:02d}T{h:02d}-00-{sid[:8]}.json", "w") as f:
        json.dump(doc, f)


os.system(f"rm -rf {BASE}")

claude_session(
    "-home-demo-projects-api-server", "/home/demo/projects/api-server",
    "Fix CORS preflight failing on /auth routes",
    [("user", "Preflight requests to /auth/login are failing with 403 in production, but only from the staging frontend. Can you find out why?"),
     ("assistant", "Let me check how the CORS middleware is registered relative to the auth guard."),
     ("Bash", "rg -n 'cors' src/middleware/"),
     ("assistant", "Found it. The CORS middleware runs **after** the auth guard, so OPTIONS preflight requests (which carry no token) get rejected with 403 before CORS headers are ever attached.\n\nThe fix is a two-line reorder in `src/middleware/mod.rs`:\n\n```rust\nrouter\n    .layer(CorsLayer::permissive())  // must come before auth\n    .layer(AuthLayer::new(jwks))\n```\n\nI also added a regression test that sends a preflight without credentials."),
     ("user", "Apply it and run the middleware tests."),
     ("Bash", "cargo test middleware:: -- --nocapture"),
     ("assistant", "Done. CORS middleware now runs first, `OPTIONS` is exempt from auth, and the new regression test passes: 14 tests, 0 failures.")],
    9, 14)

claude_session(
    "-home-demo-projects-api-server", "/home/demo/projects/api-server",
    "Add retry with backoff to the payment webhook handler",
    [("user", "Stripe webhooks sometimes fail when the DB connection pool is exhausted. Add retries with exponential backoff."),
     ("assistant", "I'll wrap the webhook handler in a retry policy: 3 attempts, 100ms base delay with jitter, and only on transient pool errors so we never double-process a charge."),
     ("Bash", "cargo test webhook_retry"),
     ("assistant", "Implemented and tested. Permanent failures still go straight to the dead-letter queue, so nothing is silently retried forever.")],
    8, 11)

claude_session(
    "-home-demo-projects-mobile-app", "/home/demo/projects/mobile-app",
    "JWT 토큰 만료 처리 버그 수정",
    [("user", "앱이 백그라운드에 오래 있다가 돌아오면 JWT 토큰이 만료돼서 API 호출이 전부 401로 떨어져. 자동 갱신 로직 좀 봐줘."),
     ("assistant", "토큰 갱신이 앱 포그라운드 전환 이벤트에만 걸려 있고, 만료 5분 전 선제 갱신이 없네요. 인터셉터에서 401을 받으면 refresh token으로 한 번 재시도하고, 그래도 실패하면 로그인 화면으로 보내는 구조로 고치겠습니다."),
     ("Bash", "npm test -- auth.interceptor"),
     ("assistant", "수정 완료했습니다. 만료 5분 전 선제 갱신 + 401 1회 재시도 + refresh 실패 시 세션 정리까지 테스트 통과합니다.")],
    10, 9)

claude_session(
    "-home-demo-projects-data-pipeline", "/home/demo/projects/data-pipeline",
    "Speed up the nightly ETL by parallelizing partition loads",
    [("user", "The nightly ETL takes 4 hours now. Most of it seems to be the partition loads. Can we parallelize?"),
     ("assistant", "The loader processes partitions sequentially. They are independent, so a worker pool sized to the warehouse connection limit (8) should cut wall time roughly by the partition count over eight."),
     ("Bash", "python bench/load_partitions.py --workers 8"),
     ("assistant", "With 8 workers the bench run drops from 212 to 31 minutes. I kept per-partition transactions so a failed partition retries alone without rolling back the others.")],
    7, 2)

claude_session(
    "-home-demo-projects-web-app", "/home/demo/projects/web-app",
    "Checkout flow: validation, race condition, and a11y pass",
    [("user", "The checkout form lets users submit twice if they double-click. Fix that first."),
     ("assistant", "Disabling the button on first submit and guarding the handler with an in-flight flag. Also debouncing at the API layer so a retry after a network error still works."),
     ("Bash", "npm test -- checkout.submit"),
     ("user", "Good. Next: the coupon field accepts expired codes and only fails at the payment step."),
     ("assistant", "Moved coupon validation to the field's blur event with a server check, so users see the error before they reach payment. Expired and exhausted codes now fail inline."),
     ("user", "QA says screen readers skip the error messages entirely."),
     ("assistant", "The errors were rendered outside the form's accessibility tree. Wrapped them in aria-live regions tied to each field with aria-describedby - VoiceOver and NVDA both announce them now."),
     ("user", "Run the full suite and summarize what changed."),
     ("Bash", "npm test"),
     ("assistant", "All 84 tests pass. Summary: double-submit guard on the button and handler, inline server-side coupon validation on blur, and aria-live error regions wired to each field.")],
    9, 10)

codex_session(
    "/home/demo/projects/web-app",
    [("user", "Migrate the dashboard route to React Server Components. Keep the chart widgets client-side."),
     ("assistant", "Plan: move data fetching into the server component, keep ChartPanel and DateRangePicker as client islands with 'use client', and pass serialized props across the boundary. Starting with the layout."),
     ("user", "Looks right. Watch out for the date objects crossing the boundary."),
     ("assistant", "Good catch - Dates serialize to strings across the RSC boundary, so I am passing ISO strings and parsing them inside the client components. Migration complete, build passes, no hydration warnings.")],
    9, 16)

codex_session(
    "/home/demo/projects/api-server",
    [("user", "Write property-based tests for the rate limiter. I want proof the token bucket never goes negative."),
     ("assistant", "Added proptest cases: random interleavings of acquire/refill across threads, asserting the bucket invariant `0 <= tokens <= capacity` holds after every step. 10,000 cases pass; also found and fixed an off-by-one when refill lands exactly on the window edge.")],
    11, 13)

gemini_session(
    "web-app",
    [("user", "Compare Playwright and Cypress for testing an app with heavy iframe usage. One paragraph, pick one."),
     ("gemini", "Playwright. Its frame locators treat iframes as first-class scopes, while Cypress needs plugins and manual frame switching that get brittle with nested frames. If your app embeds third-party widgets in iframes, Playwright's auto-waiting inside frames will save you the most flake.")],
    8, 10)

gemini_session(
    "data-pipeline",
    [("user", "Is zstd or lz4 a better default for Parquet files we read often but write once?"),
     ("gemini", "For write-once read-many, zstd at level 3: you pay compression cost once and get both smaller files and faster scans than lz4 in most columnar workloads. Keep lz4 only if your readers are CPU-bound.")],
    6, 18)

count = sum(len(files) for _, _, files in os.walk(BASE))
print(f"demo store ready at {BASE} ({count} files)")

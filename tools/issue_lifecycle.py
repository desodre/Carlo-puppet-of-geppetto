#!/usr/bin/env python3
"""Keep native parent issues open until children finish; require final acceptance."""

from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path

ACCEPTANCE_LABEL = "needs:acceptance"


def parent_action(state: str, child_states: list[str]) -> tuple[bool, bool]:
    """Return (reopen_parent, needs_acceptance), without auto-closing parents."""
    if not child_states:
        return False, False
    if state not in {"open", "closed"} or any(
        value not in {"open", "closed"} for value in child_states
    ):
        raise ValueError("unknown issue state")
    pending = any(value == "open" for value in child_states)
    return state == "closed" and pending, state == "open" and not pending


def api(method: str, endpoint: str, fields: dict | None = None, *, missing_ok: bool = False):
    args = ["gh", "api", "--method", method, endpoint]
    if fields is not None:
        args += ["--input", "-"]
    result = subprocess.run(
        args,
        input=json.dumps(fields) if fields is not None else None,
        text=True,
        capture_output=True,
        timeout=30,
    )
    if result.returncode:
        if missing_ok and "(HTTP 404)" in result.stderr:
            return None
        raise RuntimeError(f"GitHub request failed ({method} {endpoint}): {result.stderr.strip()}")
    return json.loads(result.stdout) if result.stdout.strip() else None


def children(repo: str, number: int) -> list[dict]:
    result = []
    for page in range(1, 102):
        batch = api("GET", f"repos/{repo}/issues/{number}/sub_issues?per_page=100&page={page}")
        result.extend(batch)
        if len(batch) < 100:
            return result
    raise RuntimeError("sub-issue traversal limit exceeded")


def reconcile(repo: str, number: int) -> None:
    issue = api("GET", f"repos/{repo}/issues/{number}")
    child_items = children(repo, number)
    if not child_items:
        return
    reopen, acceptance = parent_action(issue["state"], [child["state"] for child in child_items])
    if reopen:
        api("PATCH", f"repos/{repo}/issues/{number}", {"state": "open"})
        print(f"Reopened parent #{number}: pending native sub-issues")
    has_label = any(label["name"] == ACCEPTANCE_LABEL for label in issue["labels"])
    if acceptance and not has_label:
        api("POST", f"repos/{repo}/issues/{number}/labels", {"labels": [ACCEPTANCE_LABEL]})
        print(f"Parent #{number} requires final acceptance")
    elif not acceptance and has_label:
        api("DELETE", f"repos/{repo}/issues/{number}/labels/{ACCEPTANCE_LABEL}", missing_ok=True)


def main() -> None:
    repo = os.environ["GITHUB_REPOSITORY"]
    if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) is None:
        raise ValueError("invalid repository slug")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    number = event.get("issue", {}).get("number")
    if (
        not isinstance(number, int)
        or number < 1
        or event.get("action") not in {"closed", "reopened"}
    ):
        raise ValueError("expected an issue closed/reopened event")
    # Actions may coalesce pending events in a concurrency group. Reconcile every
    # epic so the last run also covers events from other parents that were coalesced.
    targets = {number}
    parent = api("GET", f"repos/{repo}/issues/{number}/parent", missing_ok=True)
    if parent is not None:
        targets.add(parent["number"])
    for page in range(1, 102):
        batch = api(
            "GET", f"repos/{repo}/issues?state=all&labels=type%3Aepic&per_page=100&page={page}"
        )
        targets.update(issue["number"] for issue in batch if "pull_request" not in issue)
        if len(batch) < 100:
            break
    else:
        raise RuntimeError("epic traversal limit exceeded")
    for target in sorted(targets):
        reconcile(repo, target)


if __name__ == "__main__":
    main()

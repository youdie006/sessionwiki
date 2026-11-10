#!/usr/bin/env python3
"""Fail-closed gate for privileged Dependabot merges after CI.

This file is executed only from a checkout of the repository's default branch.
It deliberately treats the workflow_run payload and every API response as data,
never as shell input.
"""

from __future__ import annotations

import base64
import binascii
import copy
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import NoReturn


EXPECTED_WORKFLOW = "CI"
EXPECTED_WORKFLOW_PATH = ".github/workflows/ci.yml"
EXPECTED_DEFAULT_BRANCH = "main"
DEPENDABOT_LOGIN = "dependabot[bot]"
DEPENDABOT_TYPE = "Bot"
MAX_EVENT_BYTES = 2 * 1024 * 1024
MAX_CARGO_FILE_BYTES = 2 * 1024 * 1024
MAX_PULL_REQUEST_FILES = 3_000
GH_TIMEOUT_SECONDS = 30

CARGO_PATHS = frozenset({"Cargo.toml", "Cargo.lock"})
REGULAR_FILE_MODE = "100644"
DEFAULT_CARGO_REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"

REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
SHA_RE = re.compile(r"[0-9a-f]{40}\Z")
CRATE_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
VERSION_RE = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z"
)
SEMVER_RE = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
TITLE_RE = re.compile(
    r"(?:Bump|chore\(deps(?:-dev)?\): bump) "
    r"(?P<name>[A-Za-z0-9][A-Za-z0-9_-]*) "
    r"from (?P<old>\S+) to (?P<new>\S+)\Z"
)
REQUIREMENT_RE = re.compile(
    r"(?P<prefix>\^|~|=)?"
    r"(?P<version>(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){0,2})\Z"
)
CHECKSUM_RE = re.compile(r"[0-9a-f]{64}\Z")
LOCK_DEPENDENCY_RE = re.compile(
    r"(?P<name>[A-Za-z0-9][A-Za-z0-9_-]*)"
    r"(?: (?P<version>\S+)(?: \((?P<source>[^\n()]+)\))?)?\Z"
)


class GateError(Exception):
    """Malformed or unavailable trusted input; fail the workflow."""


class NotEligible(Exception):
    """Valid input that is not safe to auto-merge."""


class Held(NotEligible):
    """A well-formed dependency update requiring human review."""


@dataclass(frozen=True)
class EventCandidate:
    repository: str
    repository_id: int
    run_id: int
    workflow_id: int
    run_attempt: int
    number: int
    pull_request_id: int
    head_sha: str
    head_ref: str
    base_sha: str


@dataclass(frozen=True)
class PullRequestSnapshot:
    pull_request_id: int
    number: int
    title: str
    changed_files: int
    head_sha: str
    head_ref: str
    base_sha: str


@dataclass(frozen=True)
class DependencyUpdate:
    name: str
    old_text: str
    old: tuple[int, int, int]
    new_text: str
    new: tuple[int, int, int]


@dataclass(frozen=True)
class GitBlobMetadata:
    sha: str
    size: int


def fail(message: str) -> NoReturn:
    raise GateError(message)


def reject(message: str) -> NoReturn:
    raise NotEligible(message)


def require_object(value: object, context: str) -> dict:
    if not isinstance(value, dict):
        fail(f"{context} is not an object")
    return value


def require_list(value: object, context: str) -> list:
    if not isinstance(value, list):
        fail(f"{context} is not an array")
    return value


def require_string(value: object, context: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{context} is not a non-empty string")
    return value


def require_bool(value: object, context: str) -> bool:
    if type(value) is not bool:
        fail(f"{context} is not a boolean")
    return value


def require_int(value: object, context: str, *, minimum: int = 0) -> int:
    if type(value) is not int or value < minimum:
        fail(f"{context} is not a valid integer")
    return value


def field(obj: dict, name: str, context: str) -> object:
    if name not in obj:
        fail(f"{context}.{name} is missing")
    return obj[name]


def object_field(obj: dict, name: str, context: str) -> dict:
    return require_object(field(obj, name, context), f"{context}.{name}")


def string_field(obj: dict, name: str, context: str) -> str:
    return require_string(field(obj, name, context), f"{context}.{name}")


def int_field(obj: dict, name: str, context: str, *, minimum: int = 0) -> int:
    return require_int(field(obj, name, context), f"{context}.{name}", minimum=minimum)


def validate_sha(value: object, context: str) -> str:
    sha = require_string(value, context)
    if not SHA_RE.fullmatch(sha):
        fail(f"{context} is not a full commit SHA")
    return sha


def load_event() -> dict:
    raw_path = os.environ.get("GITHUB_EVENT_PATH")
    if not raw_path:
        fail("GITHUB_EVENT_PATH is missing")
    path = Path(raw_path)
    try:
        size = path.stat().st_size
        if size > MAX_EVENT_BYTES:
            fail("workflow event is too large")
        payload = json.loads(path.read_text(encoding="utf-8"))
    except GateError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise GateError("workflow event cannot be read as JSON") from exc
    return require_object(payload, "workflow event")


def validate_event(event: dict, expected_repository: str) -> EventCandidate:
    if not REPOSITORY_RE.fullmatch(expected_repository):
        fail("GITHUB_REPOSITORY is malformed")
    if field(event, "action", "workflow event") != "completed":
        reject("workflow_run action is not completed")

    repository = object_field(event, "repository", "workflow event")
    repository_id = int_field(repository, "id", "workflow event.repository", minimum=1)
    if string_field(repository, "full_name", "workflow event.repository") != expected_repository:
        reject("event repository does not match the runner repository")
    if (
        string_field(repository, "default_branch", "workflow event.repository")
        != EXPECTED_DEFAULT_BRANCH
    ):
        reject("event repository default branch is not main")

    run = object_field(event, "workflow_run", "workflow event")
    run_id = int_field(run, "id", "workflow_run", minimum=1)
    workflow_id = int_field(run, "workflow_id", "workflow_run", minimum=1)
    run_attempt = int_field(run, "run_attempt", "workflow_run", minimum=1)
    if string_field(run, "name", "workflow_run") != EXPECTED_WORKFLOW:
        reject("triggering workflow is not CI")
    if string_field(run, "event", "workflow_run") != "pull_request":
        reject("CI run was not triggered by pull_request")
    if string_field(run, "status", "workflow_run") != "completed":
        reject("CI run is not completed")
    if string_field(run, "conclusion", "workflow_run") != "success":
        reject("CI run did not succeed")

    run_repository = object_field(run, "repository", "workflow_run")
    run_head_repository = object_field(run, "head_repository", "workflow_run")
    for context, candidate_repository in (
        ("workflow_run.repository", run_repository),
        ("workflow_run.head_repository", run_head_repository),
    ):
        if int_field(candidate_repository, "id", context, minimum=1) != repository_id:
            reject(f"{context} id does not match the event repository")
        if string_field(candidate_repository, "full_name", context) != expected_repository:
            reject(f"{context} name does not match the event repository")

    run_sha = validate_sha(field(run, "head_sha", "workflow_run"), "workflow_run.head_sha")
    head_branch = string_field(run, "head_branch", "workflow_run")
    if not head_branch.startswith("dependabot/"):
        reject("workflow branch is not a Dependabot branch")

    associations = require_list(
        field(run, "pull_requests", "workflow_run"), "workflow_run.pull_requests"
    )
    if len(associations) != 1:
        reject("workflow run does not have exactly one pull request association")
    association = require_object(associations[0], "workflow_run.pull_requests[0]")
    number = int_field(association, "number", "workflow_run.pull_requests[0]", minimum=1)
    pull_request_id = int_field(association, "id", "workflow_run.pull_requests[0]", minimum=1)

    association_head = object_field(association, "head", "workflow_run.pull_requests[0]")
    association_base = object_field(association, "base", "workflow_run.pull_requests[0]")
    association_head_repo = object_field(
        association_head, "repo", "workflow_run.pull_requests[0].head"
    )
    association_base_repo = object_field(
        association_base, "repo", "workflow_run.pull_requests[0].base"
    )
    if (
        int_field(
            association_head_repo,
            "id",
            "workflow_run.pull_requests[0].head.repo",
            minimum=1,
        )
        != repository_id
        or int_field(
            association_base_repo,
            "id",
            "workflow_run.pull_requests[0].base.repo",
            minimum=1,
        )
        != repository_id
    ):
        reject("embedded pull request is not wholly within the base repository")

    association_head_sha = validate_sha(
        field(association_head, "sha", "workflow_run.pull_requests[0].head"),
        "workflow_run.pull_requests[0].head.sha",
    )
    if association_head_sha != run_sha:
        # A synthetic refs/pull/.../merge SHA must never be translated into an
        # eligible source SHA. Only an exact run-on-head association is safe.
        reject("workflow run SHA is not the associated pull request head SHA")
    if string_field(association_head, "ref", "workflow_run.pull_requests[0].head") != head_branch:
        reject("workflow branch does not match the associated pull request head")
    if (
        string_field(association_base, "ref", "workflow_run.pull_requests[0].base")
        != EXPECTED_DEFAULT_BRANCH
    ):
        reject("associated pull request does not target main")

    base_sha = validate_sha(
        field(association_base, "sha", "workflow_run.pull_requests[0].base"),
        "workflow_run.pull_requests[0].base.sha",
    )
    return EventCandidate(
        repository=expected_repository,
        repository_id=repository_id,
        run_id=run_id,
        workflow_id=workflow_id,
        run_attempt=run_attempt,
        number=number,
        pull_request_id=pull_request_id,
        head_sha=run_sha,
        head_ref=head_branch,
        base_sha=base_sha,
    )


def gh_json(endpoint: str, *, paginate: bool = False) -> object:
    args = [
        "gh",
        "api",
        "--method",
        "GET",
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "X-GitHub-Api-Version: 2022-11-28",
    ]
    if paginate:
        args.extend(("--paginate", "--slurp"))
    args.append(endpoint)
    try:
        completed = subprocess.run(
            args,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=GH_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GateError("GitHub API command could not complete") from exc
    if completed.returncode != 0:
        fail("GitHub API command failed")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise GateError("GitHub API returned malformed JSON") from exc


def validate_current_repository(data: object, candidate: EventCandidate) -> None:
    repository = require_object(data, "repository API response")
    if int_field(repository, "id", "repository API response", minimum=1) != candidate.repository_id:
        reject("current repository id does not match the event")
    if string_field(repository, "full_name", "repository API response") != candidate.repository:
        reject("current repository name does not match the event")
    if (
        string_field(repository, "default_branch", "repository API response")
        != EXPECTED_DEFAULT_BRANCH
    ):
        reject("current default branch is not main")


def validate_current_workflow(data: object, candidate: EventCandidate) -> None:
    workflow = require_object(data, "workflow API response")
    if int_field(workflow, "id", "workflow API response", minimum=1) != candidate.workflow_id:
        reject("current CI workflow id does not match the triggering run")
    if string_field(workflow, "name", "workflow API response") != EXPECTED_WORKFLOW:
        reject("current CI workflow name is not CI")
    if string_field(workflow, "path", "workflow API response") != EXPECTED_WORKFLOW_PATH:
        reject("current CI workflow path is not the trusted CI workflow")
    if string_field(workflow, "state", "workflow API response") != "active":
        reject("current CI workflow is not active")


def validate_current_run(data: object, candidate: EventCandidate) -> None:
    run = require_object(data, "workflow run API response")
    if int_field(run, "id", "workflow run API response", minimum=1) != candidate.run_id:
        reject("current workflow run id does not match the event")
    if (
        int_field(run, "workflow_id", "workflow run API response", minimum=1)
        != candidate.workflow_id
    ):
        reject("current workflow run belongs to a different workflow")
    if (
        int_field(run, "run_attempt", "workflow run API response", minimum=1)
        != candidate.run_attempt
    ):
        reject("current workflow run attempt does not match the successful event")
    if string_field(run, "name", "workflow run API response") != EXPECTED_WORKFLOW:
        reject("current workflow run name is not CI")
    if string_field(run, "path", "workflow run API response") != EXPECTED_WORKFLOW_PATH:
        reject("current workflow run path is not the trusted CI workflow")
    if string_field(run, "event", "workflow run API response") != "pull_request":
        reject("current CI run was not triggered by pull_request")
    if string_field(run, "status", "workflow run API response") != "completed":
        reject("current CI run is not completed")
    if string_field(run, "conclusion", "workflow run API response") != "success":
        reject("current CI run did not succeed")

    run_repository = object_field(run, "repository", "workflow run API response")
    head_repository = object_field(run, "head_repository", "workflow run API response")
    for context, repository in (
        ("workflow run API response.repository", run_repository),
        ("workflow run API response.head_repository", head_repository),
    ):
        if int_field(repository, "id", context, minimum=1) != candidate.repository_id:
            reject(f"{context} id does not match the event repository")
        if string_field(repository, "full_name", context) != candidate.repository:
            reject(f"{context} name does not match the event repository")

    if (
        validate_sha(field(run, "head_sha", "workflow run API response"),
                     "workflow run API response.head_sha")
        != candidate.head_sha
    ):
        reject("current workflow run head is not the event source head")
    if string_field(run, "head_branch", "workflow run API response") != candidate.head_ref:
        reject("current workflow run branch does not match the event")

    associations = require_list(
        field(run, "pull_requests", "workflow run API response"),
        "workflow run API response.pull_requests",
    )
    if len(associations) != 1:
        reject("current workflow run does not have one pull request association")
    association = require_object(
        associations[0], "workflow run API response.pull_requests[0]"
    )
    context = "workflow run API response.pull_requests[0]"
    if (
        int_field(association, "id", context, minimum=1) != candidate.pull_request_id
        or int_field(association, "number", context, minimum=1) != candidate.number
    ):
        reject("current workflow run is associated with a different pull request")
    head = object_field(association, "head", context)
    base = object_field(association, "base", context)
    if (
        int_field(object_field(head, "repo", f"{context}.head"), "id",
                  f"{context}.head.repo", minimum=1)
        != candidate.repository_id
        or int_field(object_field(base, "repo", f"{context}.base"), "id",
                     f"{context}.base.repo", minimum=1)
        != candidate.repository_id
    ):
        reject("current workflow run pull request repositories do not match")
    if (
        validate_sha(field(head, "sha", f"{context}.head"), f"{context}.head.sha")
        != candidate.head_sha
        or string_field(head, "ref", f"{context}.head") != candidate.head_ref
    ):
        reject("current workflow run is not for the pull request source head")
    if (
        validate_sha(field(base, "sha", f"{context}.base"), f"{context}.base.sha")
        != candidate.base_sha
        or string_field(base, "ref", f"{context}.base") != EXPECTED_DEFAULT_BRANCH
    ):
        reject("current workflow run pull request base does not match")


def validate_commit_associations(data: object, candidate: EventCandidate) -> None:
    pages = require_list(data, "commit pull request associations API response")
    if not pages:
        fail("commit association API returned no pages")

    open_associations: list[tuple[int, int]] = []
    seen_ids: set[int] = set()
    for page_number, raw_page in enumerate(pages, start=1):
        page = require_list(raw_page, f"commit associations page {page_number}")
        if len(page) > 100:
            fail("commit association API returned an oversized page")
        for item_number, raw_association in enumerate(page, start=1):
            context = f"commit associations page {page_number} item {item_number}"
            association = require_object(raw_association, context)
            pull_request_id = int_field(association, "id", context, minimum=1)
            number = int_field(association, "number", context, minimum=1)
            state = string_field(association, "state", context)
            if state not in ("open", "closed"):
                fail("commit association API returned an unknown pull request state")
            if pull_request_id in seen_ids:
                fail("commit association pagination returned a duplicate pull request")
            seen_ids.add(pull_request_id)
            if state == "open":
                open_associations.append((pull_request_id, number))

    # With per_page=100, a full final page is indistinguishable from a response
    # whose next-page link was lost. The safe behavior is to decline the merge.
    if len(pages[-1]) == 100:
        fail("commit association pagination may be truncated")
    if open_associations != [(candidate.pull_request_id, candidate.number)]:
        reject("tested commit does not have one matching open pull request")


def validate_pull_request(data: object, candidate: EventCandidate) -> PullRequestSnapshot:
    pull_request = require_object(data, "pull request API response")
    pull_request_id = int_field(pull_request, "id", "pull request API response", minimum=1)
    number = int_field(pull_request, "number", "pull request API response", minimum=1)
    if pull_request_id != candidate.pull_request_id or number != candidate.number:
        reject("current pull request does not match the workflow association")
    if string_field(pull_request, "state", "pull request API response") != "open":
        reject("pull request is not open")
    if require_bool(
        field(pull_request, "draft", "pull request API response"),
        "pull request API response.draft",
    ):
        reject("pull request is a draft")

    user = object_field(pull_request, "user", "pull request API response")
    if string_field(user, "login", "pull request API response.user") != DEPENDABOT_LOGIN:
        reject("pull request author is not Dependabot")
    if string_field(user, "type", "pull request API response.user") != DEPENDABOT_TYPE:
        reject("Dependabot login is not a bot account")

    head = object_field(pull_request, "head", "pull request API response")
    base = object_field(pull_request, "base", "pull request API response")
    head_repository = object_field(head, "repo", "pull request API response.head")
    base_repository = object_field(base, "repo", "pull request API response.base")
    for context, repository in (
        ("pull request API response.head.repo", head_repository),
        ("pull request API response.base.repo", base_repository),
    ):
        if int_field(repository, "id", context, minimum=1) != candidate.repository_id:
            reject(f"{context} id does not match the base repository")
        if string_field(repository, "full_name", context) != candidate.repository:
            reject(f"{context} name does not match the base repository")

    head_sha = validate_sha(
        field(head, "sha", "pull request API response.head"),
        "pull request API response.head.sha",
    )
    if head_sha != candidate.head_sha:
        reject("current pull request head is not the CI-tested head")
    head_ref = string_field(head, "ref", "pull request API response.head")
    if head_ref != candidate.head_ref or not head_ref.startswith("dependabot/"):
        reject("current pull request head branch does not match the CI run")
    if string_field(base, "ref", "pull request API response.base") != EXPECTED_DEFAULT_BRANCH:
        reject("pull request does not target main")
    base_sha = validate_sha(
        field(base, "sha", "pull request API response.base"),
        "pull request API response.base.sha",
    )
    if base_sha != candidate.base_sha:
        reject("pull request base changed after the tested workflow was associated")

    changed_files = int_field(
        pull_request, "changed_files", "pull request API response", minimum=1
    )
    if changed_files >= MAX_PULL_REQUEST_FILES:
        reject("pull request file list could hit the API truncation limit")
    title = string_field(pull_request, "title", "pull request API response")
    return PullRequestSnapshot(
        pull_request_id=pull_request_id,
        number=number,
        title=title,
        changed_files=changed_files,
        head_sha=head_sha,
        head_ref=head_ref,
        base_sha=base_sha,
    )


def parse_version(value: str) -> tuple[int, int, int]:
    match = VERSION_RE.fullmatch(value)
    if not match:
        raise Held("dependency title does not contain a numeric stable version")
    return (int(match.group(1)), int(match.group(2)), int(match.group(3)))


def compatible_increment(title: str) -> DependencyUpdate:
    match = TITLE_RE.fullmatch(title)
    if not match:
        raise Held("dependency title is not a Dependabot bump title")
    name = match.group("name")
    old_text = match.group("old")
    new_text = match.group("new")
    old = parse_version(old_text)
    new = parse_version(new_text)
    if new <= old:
        raise Held("dependency version is not an increment")

    if old[0] != new[0]:
        raise Held("dependency update crosses a major compatibility boundary")
    if old[0] == 0 and old[1] != new[1]:
        raise Held("0.x dependency update crosses a minor compatibility boundary")
    if old[0] == 0 and old[1] == 0 and old[2] != new[2]:
        raise Held("0.0.x dependency update crosses a patch compatibility boundary")
    return DependencyUpdate(
        name=name,
        old_text=old_text,
        old=old,
        new_text=new_text,
        new=new,
    )


def validate_repository_path(value: object, context: str) -> str:
    path = require_string(value, context)
    parts = path.split("/")
    if (
        path.startswith("/")
        or "\\" in path
        or "\x00" in path
        or any(part in ("", ".", "..") for part in parts)
    ):
        fail(f"{context} is not a normalized repository path")
    return path


def validate_files(data: object, *, expected_count: int) -> dict[str, str]:
    pages = require_list(data, "pull request files API response")
    if not pages:
        fail("pull request files API returned no pages")

    files: dict[str, str] = {}
    seen: set[str] = set()
    for page_number, raw_page in enumerate(pages, start=1):
        page = require_list(raw_page, f"pull request files page {page_number}")
        if not page:
            fail("pull request files API returned an empty page")
        for file_number, raw_file in enumerate(page, start=1):
            context = f"pull request files page {page_number} item {file_number}"
            changed_file = require_object(raw_file, context)
            filename = validate_repository_path(
                field(changed_file, "filename", context), f"{context}.filename"
            )
            blob_sha = validate_sha(field(changed_file, "sha", context), f"{context}.sha")
            status = string_field(changed_file, "status", context)
            if status != "modified" or changed_file.get("previous_filename") is not None:
                reject("renamed, added, deleted, or copied files are not eligible")
            for counter in ("additions", "deletions", "changes"):
                int_field(changed_file, counter, context)
            if filename in seen:
                fail("pull request files API returned a duplicate path")
            seen.add(filename)
            if filename not in CARGO_PATHS:
                reject("pull request changes a file outside the root Cargo allowlist")
            files[filename] = blob_sha

    if len(files) != expected_count:
        fail("pull request file pagination did not return the advertised file count")
    if "Cargo.lock" not in files:
        raise Held("Cargo updates without a lockfile change require human review")
    return files


def load_root_tree(repository: str, commit_sha: str) -> dict[str, GitBlobMetadata]:
    commit_context = f"git commit {commit_sha} API response"
    commit = require_object(
        gh_json(f"repos/{repository}/git/commits/{commit_sha}"), commit_context
    )
    if validate_sha(field(commit, "sha", commit_context), f"{commit_context}.sha") != commit_sha:
        fail("git commit API returned a different commit")
    tree_ref = object_field(commit, "tree", commit_context)
    tree_sha = validate_sha(field(tree_ref, "sha", f"{commit_context}.tree"),
                            f"{commit_context}.tree.sha")

    tree_context = f"git tree {tree_sha} API response"
    tree = require_object(gh_json(f"repos/{repository}/git/trees/{tree_sha}"), tree_context)
    if validate_sha(field(tree, "sha", tree_context), f"{tree_context}.sha") != tree_sha:
        fail("git tree API returned a different tree")
    if require_bool(field(tree, "truncated", tree_context), f"{tree_context}.truncated"):
        fail("git tree API response was truncated")

    cargo_entries: dict[str, GitBlobMetadata] = {}
    for item_number, raw_entry in enumerate(
        require_list(field(tree, "tree", tree_context), f"{tree_context}.tree"), start=1
    ):
        context = f"{tree_context}.tree[{item_number}]"
        entry = require_object(raw_entry, context)
        path = validate_repository_path(field(entry, "path", context), f"{context}.path")
        if path not in CARGO_PATHS:
            continue
        if path in cargo_entries:
            fail("git tree API returned a duplicate root Cargo path")
        entry_type = string_field(entry, "type", context)
        mode = string_field(entry, "mode", context)
        if entry_type != "blob" or mode != REGULAR_FILE_MODE:
            raise Held(f"{path} is not an ordinary non-executable Git file")
        blob_sha = validate_sha(field(entry, "sha", context), f"{context}.sha")
        size = int_field(entry, "size", context)
        if size > MAX_CARGO_FILE_BYTES:
            fail(f"{path} exceeds the Cargo file size limit")
        cargo_entries[path] = GitBlobMetadata(sha=blob_sha, size=size)

    if set(cargo_entries) != CARGO_PATHS:
        fail("git tree is missing a root Cargo manifest or lockfile")
    return cargo_entries


def load_blob_text(
    repository: str,
    metadata: GitBlobMetadata,
    context: str,
    cache: dict[str, str],
) -> str:
    if metadata.sha in cache:
        cached = cache[metadata.sha]
        if len(cached.encode("utf-8")) != metadata.size:
            fail("cached Git blob size does not match trusted tree metadata")
        return cached
    response = require_object(
        gh_json(f"repos/{repository}/git/blobs/{metadata.sha}"), context
    )
    if validate_sha(field(response, "sha", context), f"{context}.sha") != metadata.sha:
        fail("Git blob API returned a different blob")
    size = int_field(response, "size", context)
    if size != metadata.size or size > MAX_CARGO_FILE_BYTES:
        fail("Git blob size does not match trusted tree metadata")
    if string_field(response, "encoding", context) != "base64":
        fail("Git blob API did not return base64 content")
    encoded = require_string(field(response, "content", context), f"{context}.content")
    compact = encoded.replace("\n", "").replace("\r", "")
    try:
        raw = base64.b64decode(compact, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise GateError("Git blob API returned malformed base64 content") from exc
    if len(raw) != size:
        fail("decoded Git blob size does not match its metadata")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError("Cargo file blob is not valid UTF-8") from exc
    if "\x00" in text:
        fail("Cargo file blob contains a NUL byte")
    cache[metadata.sha] = text
    return text


def load_cargo_contents(
    candidate: EventCandidate, changed_files: dict[str, str]
) -> tuple[dict[str, str], dict[str, str]]:
    base_tree = load_root_tree(candidate.repository, candidate.base_sha)
    head_tree = load_root_tree(candidate.repository, candidate.head_sha)
    actual_changes = {
        path for path in CARGO_PATHS if base_tree[path].sha != head_tree[path].sha
    }
    if actual_changes != set(changed_files):
        fail("pull request file list does not match immutable Cargo blobs")
    for path, advertised_sha in changed_files.items():
        if head_tree[path].sha != advertised_sha:
            fail("pull request file metadata does not match the head tree blob")

    cache: dict[str, str] = {}
    base_contents = {
        path: load_blob_text(
            candidate.repository,
            base_tree[path],
            f"base {path} blob API response",
            cache,
        )
        for path in sorted(CARGO_PATHS)
    }
    head_contents = {
        path: load_blob_text(
            candidate.repository,
            head_tree[path],
            f"head {path} blob API response",
            cache,
        )
        for path in sorted(CARGO_PATHS)
    }
    return base_contents, head_contents


def parse_toml_document(text: str, context: str) -> dict:
    try:
        document = tomllib.loads(text)
    except (tomllib.TOMLDecodeError, ValueError) as exc:
        raise GateError(f"{context} is malformed TOML") from exc
    return require_object(document, context)


def dependency_tables(document: dict) -> list[tuple[tuple[str, ...], dict]]:
    tables: list[tuple[tuple[str, ...], dict]] = []
    dependency_kinds = ("dependencies", "dev-dependencies", "build-dependencies")
    for kind in dependency_kinds:
        if kind in document:
            table = document[kind]
            if not isinstance(table, dict):
                raise Held(f"Cargo.toml {kind} is not a supported dependency table")
            tables.append(((kind,), table))

    workspace = document.get("workspace")
    if workspace is not None:
        if not isinstance(workspace, dict):
            raise Held("Cargo.toml workspace configuration is unfamiliar")
        if "dependencies" in workspace:
            table = workspace["dependencies"]
            if not isinstance(table, dict):
                raise Held("Cargo.toml workspace.dependencies is not a table")
            tables.append((("workspace", "dependencies"), table))

    targets = document.get("target")
    if targets is not None:
        if not isinstance(targets, dict):
            raise Held("Cargo.toml target configuration is unfamiliar")
        for target_name, target in targets.items():
            if not isinstance(target_name, str) or not isinstance(target, dict):
                raise Held("Cargo.toml target dependency configuration is unfamiliar")
            for kind in dependency_kinds:
                if kind in target:
                    table = target[kind]
                    if not isinstance(table, dict):
                        raise Held(f"Cargo.toml target {kind} is not a table")
                    tables.append((("target", target_name, kind), table))
    return tables


def dependency_requirements(
    document: dict, dependency_name: str
) -> dict[tuple[str, ...], str]:
    requirements: dict[tuple[str, ...], str] = {}
    for table_path, table in dependency_tables(document):
        for alias, declaration in table.items():
            if not isinstance(alias, str):
                raise Held("Cargo.toml contains a non-string dependency name")
            if isinstance(declaration, str):
                package_name = alias
                version_path = table_path + (alias,)
                requirement = declaration
            elif isinstance(declaration, dict):
                package_name = declaration.get("package", alias)
                if not isinstance(package_name, str):
                    raise Held("Cargo.toml dependency package mapping is unfamiliar")
                version_path = table_path + (alias, "version")
                requirement = declaration.get("version")
            else:
                if alias == dependency_name:
                    raise Held("announced Cargo dependency has an unfamiliar declaration")
                continue
            if package_name != dependency_name:
                continue
            if not isinstance(requirement, str):
                raise Held("announced Cargo dependency has no supported version requirement")
            requirements[version_path] = requirement
    if not requirements:
        raise Held("announced crate is not a root Cargo dependency")
    return requirements


def document_differences(base: object, head: object, path: tuple[str, ...] = ()) -> set[tuple[str, ...]]:
    if type(base) is not type(head):
        return {path}
    if isinstance(base, dict):
        differences: set[tuple[str, ...]] = set()
        for key in set(base) | set(head):
            if key not in base or key not in head:
                differences.add(path + (str(key),))
            else:
                differences.update(document_differences(base[key], head[key], path + (str(key),)))
        return differences
    if base != head:
        return {path}
    return set()


def parse_requirement(requirement: str) -> tuple[str, tuple[int, int, int], int]:
    match = REQUIREMENT_RE.fullmatch(requirement)
    if not match:
        raise Held("announced dependency uses an unfamiliar Cargo version constraint")
    prefix = match.group("prefix") or ""
    components = tuple(int(part) for part in match.group("version").split("."))
    if prefix == "=" and len(components) != 3:
        raise Held("partial exact Cargo version constraints require human review")
    floor = components + (0,) * (3 - len(components))
    return prefix, floor, len(components)


def requirement_allows(
    prefix: str,
    floor: tuple[int, int, int],
    precision: int,
    version: tuple[int, int, int],
) -> bool:
    if version < floor:
        return False
    if prefix == "=":
        return version == floor
    if prefix == "~":
        return version[0] == floor[0] if precision == 1 else version[:2] == floor[:2]
    if precision == 1:
        return version[0] == floor[0]
    if floor[0] > 0:
        return version[0] == floor[0]
    if floor[1] > 0:
        return version[:2] == floor[:2]
    return version == floor


def root_package_identity(document: dict) -> tuple[str, str] | None:
    package = document.get("package")
    if package is None:
        return None
    if not isinstance(package, dict):
        raise Held("Cargo.toml package configuration is unfamiliar")
    name = package.get("name")
    version = package.get("version")
    if not isinstance(name, str) or not CRATE_NAME_RE.fullmatch(name):
        raise Held("Cargo.toml root package name is unfamiliar")
    if not isinstance(version, str) or not SEMVER_RE.fullmatch(version):
        raise Held("Cargo.toml root package version is unfamiliar")
    return name, version


def validate_manifest_update(
    base_text: str,
    head_text: str,
    update: DependencyUpdate,
    manifest_changed: bool,
) -> tuple[str, str]:
    base = parse_toml_document(base_text, "base Cargo.toml")
    head = parse_toml_document(head_text, "head Cargo.toml")
    base_requirements = dependency_requirements(base, update.name)
    head_requirements = dependency_requirements(head, update.name)
    if set(base_requirements) != set(head_requirements):
        raise Held("announced dependency declarations changed shape")

    differences = document_differences(base, head)
    if differences - set(base_requirements):
        raise Held("Cargo.toml changes more than the announced version requirement")
    if manifest_changed and not differences:
        raise Held("Cargo.toml contains only non-structural changes")
    if not manifest_changed and differences:
        fail("unchanged Cargo.toml blob parsed differently")

    for path in sorted(base_requirements):
        base_prefix, base_version, base_precision = parse_requirement(
            base_requirements[path]
        )
        head_prefix, head_version, head_precision = parse_requirement(
            head_requirements[path]
        )
        if path in differences:
            if (
                base_prefix != head_prefix
                or base_precision != 3
                or head_precision != 3
                or base_version != update.old
                or head_version != update.new
            ):
                raise Held("Cargo.toml version change does not match the Dependabot title")
        elif (
            base_prefix != head_prefix
            or base_precision != head_precision
            or base_version != head_version
            or not requirement_allows(
                base_prefix, base_version, base_precision, update.old
            )
            or not requirement_allows(
                base_prefix, base_version, base_precision, update.new
            )
        ):
            raise Held("unchanged Cargo requirement does not allow the announced update")

    identity = root_package_identity(base)
    if identity is None or root_package_identity(head) != identity:
        raise Held("a single unchanged root Cargo package is required")
    return identity


def validate_lock_packages(document: dict, context: str) -> dict[tuple[str, str, str | None], dict]:
    packages = require_list(field(document, "package", context), f"{context}.package")
    if not packages:
        fail(f"{context}.package is empty")
    validated: dict[tuple[str, str, str | None], dict] = {}
    allowed_keys = {"name", "version", "source", "checksum", "dependencies"}
    for index, raw_package in enumerate(packages, start=1):
        package_context = f"{context}.package[{index}]"
        package = require_object(raw_package, package_context)
        if set(package) - allowed_keys:
            raise Held("Cargo.lock package contains unfamiliar fields")
        name = string_field(package, "name", package_context)
        if not CRATE_NAME_RE.fullmatch(name):
            fail("Cargo.lock contains a malformed package name")
        version = string_field(package, "version", package_context)
        if not SEMVER_RE.fullmatch(version):
            fail("Cargo.lock contains a malformed package version")
        source_value = package.get("source")
        if source_value is not None and not isinstance(source_value, str):
            fail("Cargo.lock package source is not a string")
        source = source_value

        checksum = package.get("checksum")
        if source is not None and source.startswith("registry+"):
            if not isinstance(checksum, str) or not CHECKSUM_RE.fullmatch(checksum):
                fail("Cargo.lock registry package checksum is malformed")
        elif checksum is not None:
            fail("Cargo.lock non-registry package unexpectedly has a checksum")

        dependencies = package.get("dependencies", [])
        if not isinstance(dependencies, list):
            fail("Cargo.lock package dependencies is not an array")
        seen_dependencies: set[str] = set()
        for dependency in dependencies:
            if not isinstance(dependency, str) or not LOCK_DEPENDENCY_RE.fullmatch(dependency):
                fail("Cargo.lock contains a malformed dependency reference")
            if dependency in seen_dependencies:
                fail("Cargo.lock contains a duplicate dependency reference")
            seen_dependencies.add(dependency)

        identity = (name, version, source)
        if identity in validated:
            fail("Cargo.lock contains a duplicate package identity")
        validated[identity] = package
    return validated


def validate_lock_references(
    packages: dict[tuple[str, str, str | None], dict]
) -> None:
    identities = tuple(packages)
    for package in packages.values():
        for dependency in package.get("dependencies", []):
            match = LOCK_DEPENDENCY_RE.fullmatch(dependency)
            if match is None:
                fail("Cargo.lock dependency reference could not be parsed")
            name = match.group("name")
            version = match.group("version")
            source = match.group("source")
            matches = [
                identity
                for identity in identities
                if identity[0] == name
                and (version is None or identity[1] == version)
                and (source is None or identity[2] == source)
            ]
            if len(matches) != 1:
                fail("Cargo.lock dependency reference is missing or ambiguous")


def validate_lock_update(
    base_text: str,
    head_text: str,
    update: DependencyUpdate,
    root_identity: tuple[str, str],
) -> None:
    base = parse_toml_document(base_text, "base Cargo.lock")
    head = parse_toml_document(head_text, "head Cargo.lock")
    for context, document in (("base Cargo.lock", base), ("head Cargo.lock", head)):
        lock_version = document.get("version")
        if type(lock_version) is not int or lock_version not in (3, 4):
            fail(f"{context} has an unsupported lockfile version")

    base_metadata = copy.deepcopy(base)
    head_metadata = copy.deepcopy(head)
    base_metadata.pop("package", None)
    head_metadata.pop("package", None)
    if base_metadata != head_metadata:
        raise Held("Cargo.lock metadata or format version changed")
    if base == head:
        raise Held("Cargo.lock contains no structural dependency update")

    base_packages = validate_lock_packages(base, "base Cargo.lock")
    head_packages = validate_lock_packages(head, "head Cargo.lock")
    validate_lock_references(base_packages)
    validate_lock_references(head_packages)

    root_key = (root_identity[0], root_identity[1], None)
    if root_key not in base_packages or root_key not in head_packages:
        raise Held("Cargo.lock does not contain the unchanged local root package")

    base_nondefault = {
        identity: package
        for identity, package in base_packages.items()
        if identity[2] != DEFAULT_CARGO_REGISTRY
    }
    head_nondefault = {
        identity: package
        for identity, package in head_packages.items()
        if identity[2] != DEFAULT_CARGO_REGISTRY
    }
    if set(base_nondefault) != set(head_nondefault):
        raise Held("Cargo.lock adds, removes, or changes a non-default source")
    for identity, base_package in base_nondefault.items():
        head_package = head_nondefault[identity]
        if identity == root_key:
            base_without_dependencies = copy.deepcopy(base_package)
            head_without_dependencies = copy.deepcopy(head_package)
            base_without_dependencies.pop("dependencies", None)
            head_without_dependencies.pop("dependencies", None)
            if base_without_dependencies != head_without_dependencies:
                raise Held("Cargo.lock changes local root package metadata")
        elif base_package != head_package:
            raise Held("Cargo.lock modifies an existing non-default source package")

    base_named_versions = sorted(
        version
        for name, version, source in base_packages
        if name == update.name and source == DEFAULT_CARGO_REGISTRY
    )
    head_named_versions = sorted(
        version
        for name, version, source in head_packages
        if name == update.name and source == DEFAULT_CARGO_REGISTRY
    )
    if base_named_versions != [update.old_text] or head_named_versions != [update.new_text]:
        raise Held("Cargo.lock does not represent the single announced crate update")


def validate_cargo_update(
    candidate: EventCandidate,
    changed_files: dict[str, str],
    update: DependencyUpdate,
) -> None:
    base_contents, head_contents = load_cargo_contents(candidate, changed_files)
    root_identity = validate_manifest_update(
        base_contents["Cargo.toml"],
        head_contents["Cargo.toml"],
        update,
        "Cargo.toml" in changed_files,
    )
    validate_lock_update(
        base_contents["Cargo.lock"],
        head_contents["Cargo.lock"],
        update,
        root_identity,
    )


def merge(candidate: EventCandidate) -> None:
    args = [
        "gh",
        "pr",
        "merge",
        str(candidate.number),
        "--repo",
        candidate.repository,
        "--squash",
        "--delete-branch",
        "--match-head-commit",
        candidate.head_sha,
    ]
    try:
        completed = subprocess.run(
            args,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=GH_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GateError("merge command could not complete") from exc
    if completed.returncode != 0:
        fail("SHA-pinned merge command failed")


def gate() -> None:
    expected_repository = os.environ.get("GITHUB_REPOSITORY", "")
    candidate = validate_event(load_event(), expected_repository)

    repository_data = gh_json(f"repos/{candidate.repository}")
    validate_current_repository(repository_data, candidate)

    workflow_data = gh_json(
        f"repos/{candidate.repository}/actions/workflows/ci.yml"
    )
    validate_current_workflow(workflow_data, candidate)

    run_endpoint = f"repos/{candidate.repository}/actions/runs/{candidate.run_id}"
    validate_current_run(gh_json(run_endpoint), candidate)

    association_pages = gh_json(
        f"repos/{candidate.repository}/commits/{candidate.head_sha}/pulls?per_page=100",
        paginate=True,
    )
    validate_commit_associations(association_pages, candidate)

    pull_request_endpoint = f"repos/{candidate.repository}/pulls/{candidate.number}"
    initial = validate_pull_request(gh_json(pull_request_endpoint), candidate)
    update = compatible_increment(initial.title)

    file_pages = gh_json(f"{pull_request_endpoint}/files?per_page=100", paginate=True)
    changed_files = validate_files(
        file_pages, expected_count=initial.changed_files
    )
    validate_cargo_update(candidate, changed_files, update)

    # Re-read the exact attempt immediately before the final PR snapshot. The
    # merge command then atomically refuses any head SHA change.
    validate_current_run(gh_json(run_endpoint), candidate)
    current = validate_pull_request(gh_json(pull_request_endpoint), candidate)
    if current != initial:
        reject("pull request metadata changed while the gate was running")
    if compatible_increment(current.title) != update:
        reject("dependency title changed while the gate was running")
    merge(candidate)
    print(
        f"merged dependency pull request #{candidate.number}: "
        f"{update.old_text} -> {update.new_text} at {candidate.head_sha}"
    )


def main() -> int:
    try:
        gate()
    except Held as exc:
        print(f"held for review: {exc}")
        return 0
    except NotEligible as exc:
        print(f"not eligible for automatic merge: {exc}")
        return 0
    except GateError as exc:
        print(f"dependency auto-merge gate failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Regression tests for the privileged Dependabot merge gate."""

from __future__ import annotations

import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPT = Path(__file__).with_name("deps-automerge.py")
REPOSITORY = "example/sessionwiki"
HEAD_SHA = "a" * 40
OTHER_SHA = "b" * 40
BASE_SHA = "c" * 40
RUN_ID = 9001
WORKFLOW_ID = 5001
DEFAULT_REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"

BASE_MANIFEST = """\
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1.0.0"
"""

HEAD_MANIFEST = BASE_MANIFEST.replace('anyhow = "1.0.0"', 'anyhow = "1.0.1"')

BASE_LOCK = f"""\
version = 4

[[package]]
name = "anyhow"
version = "1.0.0"
source = "{DEFAULT_REGISTRY}"
checksum = "{'1' * 64}"

[[package]]
name = "fixture"
version = "0.1.0"
dependencies = ["anyhow"]
"""

HEAD_LOCK = BASE_LOCK.replace('version = "1.0.0"', 'version = "1.0.1"', 1).replace(
    f'checksum = "{"1" * 64}"', f'checksum = "{"2" * 64}"'
)


def git_blob_sha(content: str) -> str:
    raw = content.encode()
    return hashlib.sha1(f"blob {len(raw)}\0".encode() + raw).hexdigest()


def git_blob(content: str) -> dict:
    raw = content.encode()
    return {
        "sha": git_blob_sha(content),
        "size": len(raw),
        "encoding": "base64",
        "content": base64.b64encode(raw).decode(),
    }


def cargo_git_state(
    *,
    base_manifest: str = BASE_MANIFEST,
    head_manifest: str = HEAD_MANIFEST,
    base_lock: str = BASE_LOCK,
    head_lock: str = HEAD_LOCK,
    changed_paths: tuple[str, ...] = ("Cargo.toml", "Cargo.lock"),
    base_modes: dict[str, tuple[str, str]] | None = None,
    head_modes: dict[str, tuple[str, str]] | None = None,
) -> dict:
    contents = {
        BASE_SHA: {"Cargo.toml": base_manifest, "Cargo.lock": base_lock},
        HEAD_SHA: {"Cargo.toml": head_manifest, "Cargo.lock": head_lock},
    }
    modes = {
        BASE_SHA: base_modes or {},
        HEAD_SHA: head_modes or {},
    }
    tree_shas = {BASE_SHA: "1" * 40, HEAD_SHA: "2" * 40}
    commits = {
        commit_sha: {"sha": commit_sha, "tree": {"sha": tree_sha}}
        for commit_sha, tree_sha in tree_shas.items()
    }
    trees = {}
    blobs = {}
    head_blob_shas = {}
    for commit_sha, files in contents.items():
        entries = []
        for path, content in files.items():
            blob = git_blob(content)
            blobs[blob["sha"]] = blob
            mode, entry_type = modes[commit_sha].get(path, ("100644", "blob"))
            entries.append(
                {
                    "path": path,
                    "mode": mode,
                    "type": entry_type,
                    "sha": blob["sha"],
                    "size": blob["size"],
                }
            )
            if commit_sha == HEAD_SHA:
                head_blob_shas[path] = blob["sha"]
        trees[tree_shas[commit_sha]] = {
            "sha": tree_shas[commit_sha],
            "truncated": False,
            "tree": entries,
        }

    changed_files = [
        {
            "sha": head_blob_shas[path],
            "filename": path,
            "status": "modified",
            "additions": 1,
            "deletions": 1,
            "changes": 2,
        }
        for path in changed_paths
    ]
    return {
        "commits": commits,
        "trees": trees,
        "blobs": blobs,
        "changed_files": changed_files,
    }


FAKE_GH = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

scenario = json.loads(Path(os.environ["FAKE_GH_SCENARIO"]).read_text())
state_path = Path(os.environ["FAKE_GH_STATE"])
state = (
    json.loads(state_path.read_text())
    if state_path.exists()
    else {"pr_calls": 0, "run_calls": 0}
)
argv = sys.argv[1:]

with Path(os.environ["FAKE_GH_LOG"]).open("a") as log:
    log.write(json.dumps(argv) + "\n")

if not argv:
    sys.exit(90)

if argv[0] == "api":
    endpoint = argv[-1]
    if endpoint.endswith("/actions/workflows/ci.yml"):
        operation = "workflow"
        response = scenario["workflow"]
    elif "/actions/runs/" in endpoint:
        operation = "run"
        responses = scenario.get("runs") or [scenario["run"]]
        response = responses[min(state["run_calls"], len(responses) - 1)]
        state["run_calls"] += 1
        state_path.write_text(json.dumps(state))
    elif "/git/commits/" in endpoint:
        operation = "commit"
        response = scenario["commits"][endpoint.rsplit("/", 1)[-1]]
    elif "/git/trees/" in endpoint:
        operation = "tree"
        response = scenario["trees"][endpoint.rsplit("/", 1)[-1]]
    elif "/git/blobs/" in endpoint:
        operation = "blob"
        response = scenario["blobs"][endpoint.rsplit("/", 1)[-1]]
    elif endpoint.endswith("/files?per_page=100"):
        operation = "files"
        response = scenario["files"]
    elif "/commits/" in endpoint and endpoint.endswith("/pulls?per_page=100"):
        operation = "associations"
        response = scenario["associations"]
    elif "/pulls/" in endpoint:
        operation = "pr"
        responses = scenario.get("prs") or [scenario["pr"]]
        response = responses[min(state["pr_calls"], len(responses) - 1)]
        state["pr_calls"] += 1
        state_path.write_text(json.dumps(state))
    else:
        operation = "repo"
        response = scenario["repo"]

    if operation in scenario.get("fail_on", []):
        print("simulated gh api failure", file=sys.stderr)
        sys.exit(91)
    if operation in scenario.get("raw", {}):
        sys.stdout.write(scenario["raw"][operation])
    else:
        print(json.dumps(response))
    sys.exit(0)

if argv[:2] == ["pr", "merge"]:
    if "merge" in scenario.get("fail_on", []):
        print("simulated merge failure", file=sys.stderr)
        sys.exit(92)
    sys.exit(0)

sys.exit(93)
"""


def event_payload() -> dict:
    repo_ref = {
        "id": 101,
        "url": f"https://api.github.test/repos/{REPOSITORY}",
        "name": "sessionwiki",
    }
    return {
        "action": "completed",
        "repository": {
            "id": 101,
            "full_name": REPOSITORY,
            "default_branch": "main",
        },
        "workflow_run": {
            "id": RUN_ID,
            "workflow_id": WORKFLOW_ID,
            "run_attempt": 1,
            "name": "CI",
            "event": "pull_request",
            "status": "completed",
            "conclusion": "success",
            "head_branch": "dependabot/cargo/anyhow-1.0.1",
            "head_sha": HEAD_SHA,
            "repository": {"id": 101, "full_name": REPOSITORY},
            "head_repository": {"id": 101, "full_name": REPOSITORY},
            "pull_requests": [
                {
                    "id": 7001,
                    "number": 7,
                    "url": f"https://api.github.test/repos/{REPOSITORY}/pulls/7",
                    "head": {
                        "ref": "dependabot/cargo/anyhow-1.0.1",
                        "sha": HEAD_SHA,
                        "repo": copy.deepcopy(repo_ref),
                    },
                    "base": {
                        "ref": "main",
                        "sha": BASE_SHA,
                        "repo": copy.deepcopy(repo_ref),
                    },
                }
            ],
        },
    }


def pull_request() -> dict:
    return {
        "id": 7001,
        "number": 7,
        "state": "open",
        "draft": False,
        "title": "Bump anyhow from 1.0.0 to 1.0.1",
        "changed_files": 2,
        "user": {"login": "dependabot[bot]", "type": "Bot"},
        "head": {
            "ref": "dependabot/cargo/anyhow-1.0.1",
            "sha": HEAD_SHA,
            "repo": {"id": 101, "full_name": REPOSITORY},
        },
        "base": {
            "ref": "main",
            "sha": BASE_SHA,
            "repo": {"id": 101, "full_name": REPOSITORY},
        },
    }


def file_list() -> list[dict]:
    return cargo_git_state()["changed_files"]


def current_run() -> dict:
    run = copy.deepcopy(event_payload()["workflow_run"])
    run["path"] = ".github/workflows/ci.yml"
    return run


def scenario_payload() -> dict:
    git_state = cargo_git_state()
    return {
        "repo": {"id": 101, "full_name": REPOSITORY, "default_branch": "main"},
        "workflow": {
            "id": WORKFLOW_ID,
            "name": "CI",
            "path": ".github/workflows/ci.yml",
            "state": "active",
        },
        "run": current_run(),
        "pr": pull_request(),
        "associations": [[{"id": 7001, "number": 7, "state": "open"}]],
        # gh api --paginate --slurp returns one list per response page.
        "files": [[file_list()[0]], [file_list()[1]]],
        "commits": git_state["commits"],
        "trees": git_state["trees"],
        "blobs": git_state["blobs"],
    }


def configure_cargo(scenario: dict, **kwargs: object) -> None:
    git_state = cargo_git_state(**kwargs)
    scenario["commits"] = git_state["commits"]
    scenario["trees"] = git_state["trees"]
    scenario["blobs"] = git_state["blobs"]
    scenario["files"] = [[item] for item in git_state["changed_files"]]
    scenario["pr"]["changed_files"] = len(git_state["changed_files"])


class DependabotGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.workspace = self.root / "trusted-checkout"
        self.workspace.mkdir()
        (self.workspace / "Cargo.toml").write_text("[package]\nname='fixture'\n")
        (self.workspace / "Cargo.lock").write_text("# fixture\n")

        fake_bin = self.root / "bin"
        fake_bin.mkdir()
        fake_gh = fake_bin / "gh"
        fake_gh.write_text(textwrap.dedent(FAKE_GH))
        fake_gh.chmod(0o755)

        self.event_path = self.root / "event.json"
        self.scenario_path = self.root / "scenario.json"
        self.state_path = self.root / "state.json"
        self.log_path = self.root / "gh.log"
        self.marker_path = self.root / "shell-was-run"
        self.env = os.environ.copy()
        for credential_name in (
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ):
            self.env.pop(credential_name, None)
        self.env.update(
            {
                "PATH": f"{fake_bin}{os.pathsep}{self.env.get('PATH', '')}",
                "GH_TOKEN": "fake-token-for-tests",
                "GH_CONFIG_DIR": str(self.root / "fake-gh-config"),
                "GITHUB_EVENT_PATH": str(self.event_path),
                "GITHUB_REPOSITORY": REPOSITORY,
                "GITHUB_WORKSPACE": str(self.workspace),
                "FAKE_GH_SCENARIO": str(self.scenario_path),
                "FAKE_GH_STATE": str(self.state_path),
                "FAKE_GH_LOG": str(self.log_path),
            }
        )

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def run_gate(
        self,
        *,
        event: dict | str | None = None,
        scenario: dict | None = None,
    ) -> subprocess.CompletedProcess[str]:
        payload = event_payload() if event is None else event
        api = scenario_payload() if scenario is None else scenario
        self.event_path.write_text(payload if isinstance(payload, str) else json.dumps(payload))
        self.scenario_path.write_text(json.dumps(api))
        self.state_path.write_text(json.dumps({"pr_calls": 0, "run_calls": 0}))
        self.log_path.write_text("")
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            cwd=self.workspace,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def gh_commands(self) -> list[list[str]]:
        return [json.loads(line) for line in self.log_path.read_text().splitlines()]

    def merge_commands(self) -> list[list[str]]:
        return [cmd for cmd in self.gh_commands() if cmd[:2] == ["pr", "merge"]]

    def assert_rejected(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual([], self.merge_commands(), result.stdout + result.stderr)

    def test_valid_trusted_compatible_bump_merges_with_pinned_head(self) -> None:
        result = self.run_gate()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            [
                [
                    "pr",
                    "merge",
                    "7",
                    "--repo",
                    REPOSITORY,
                    "--squash",
                    "--delete-branch",
                    "--match-head-commit",
                    HEAD_SHA,
                ]
            ],
            self.merge_commands(),
        )
        commands = self.gh_commands()
        self.assertEqual(
            [
                "api",
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github+json",
                "--header",
                "X-GitHub-Api-Version: 2022-11-28",
                f"repos/{REPOSITORY}/actions/runs/{RUN_ID}",
            ],
            commands[-3],
        )
        self.assertEqual(
            f"repos/{REPOSITORY}/pulls/7",
            commands[-2][-1],
        )
        self.assertEqual(4, sum("/git/blobs/" in cmd[-1] for cmd in commands if cmd[0] == "api"))

    def test_rejects_wrong_current_run_or_workflow_identity(self) -> None:
        cases = {}

        event_workflow = (event_payload(), scenario_payload())
        event_workflow[0]["workflow_run"]["workflow_id"] = WORKFLOW_ID + 1
        cases["event workflow id"] = event_workflow

        run_workflow = (event_payload(), scenario_payload())
        run_workflow[1]["run"]["workflow_id"] = WORKFLOW_ID + 1
        cases["current run workflow id"] = run_workflow

        run_path = (event_payload(), scenario_payload())
        run_path[1]["run"]["path"] = ".github/workflows/not-ci.yml"
        cases["current run workflow path"] = run_path

        workflow_id = (event_payload(), scenario_payload())
        workflow_id[1]["workflow"]["id"] = WORKFLOW_ID + 1
        cases["trusted workflow id"] = workflow_id

        workflow_path = (event_payload(), scenario_payload())
        workflow_path[1]["workflow"]["path"] = ".github/workflows/not-ci.yml"
        cases["trusted workflow path"] = workflow_path

        run_repository = (event_payload(), scenario_payload())
        run_repository[1]["run"]["repository"]["id"] = 202
        cases["current run repository"] = run_repository

        run_id = (event_payload(), scenario_payload())
        run_id[1]["run"]["id"] = RUN_ID + 1
        cases["current run id"] = run_id

        run_head = (event_payload(), scenario_payload())
        run_head[1]["run"]["head_sha"] = OTHER_SHA
        cases["current run source head"] = run_head

        for name, (event, scenario) in cases.items():
            with self.subTest(name=name):
                self.assert_rejected(self.run_gate(event=event, scenario=scenario))

    def test_rejects_stale_success_after_failed_or_running_rerun(self) -> None:
        for status, conclusion in (("completed", "failure"), ("in_progress", None)):
            with self.subTest(status=status):
                scenario = scenario_payload()
                scenario["run"]["run_attempt"] = 2
                scenario["run"]["status"] = status
                scenario["run"]["conclusion"] = conclusion
                self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rechecks_run_attempt_immediately_before_final_pr_read(self) -> None:
        scenario = scenario_payload()
        newer_attempt = copy.deepcopy(scenario["run"])
        newer_attempt.update(
            run_attempt=2,
            status="completed",
            conclusion="failure",
        )
        scenario["runs"] = [scenario["run"], newer_attempt]

        result = self.run_gate(scenario=scenario)

        self.assert_rejected(result)
        commands = self.gh_commands()
        run_endpoint = f"repos/{REPOSITORY}/actions/runs/{RUN_ID}"
        self.assertEqual(2, sum(cmd[-1] == run_endpoint for cmd in commands if cmd[0] == "api"))
        self.assertEqual(run_endpoint, commands[-1][-1])

    def test_valid_lock_only_update_with_unchanged_constraint_merges(self) -> None:
        scenario = scenario_payload()
        shorthand_manifest = BASE_MANIFEST.replace('anyhow = "1.0.0"', 'anyhow = "1"')
        configure_cargo(
            scenario,
            base_manifest=shorthand_manifest,
            head_manifest=shorthand_manifest,
            changed_paths=("Cargo.lock",),
        )

        result = self.run_gate(scenario=scenario)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(1, len(self.merge_commands()))

    def test_valid_dependency_alias_update_merges(self) -> None:
        base_manifest = BASE_MANIFEST.replace(
            'anyhow = "1.0.0"',
            'error-stack = { package = "anyhow", version = "~1.0.0", features = ["std"] }',
        )
        head_manifest = base_manifest.replace('version = "~1.0.0"', 'version = "~1.0.1"')
        scenario = scenario_payload()
        configure_cargo(
            scenario,
            base_manifest=base_manifest,
            head_manifest=head_manifest,
        )

        result = self.run_gate(scenario=scenario)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(1, len(self.merge_commands()))

    def test_valid_updates_in_supported_dependency_tables_merge(self) -> None:
        table_headers = (
            "[dev-dependencies]",
            "[build-dependencies]",
            "[workspace.dependencies]",
            "[target.'cfg(unix)'.dependencies]",
        )
        for table_header in table_headers:
            with self.subTest(table=table_header):
                base_manifest = BASE_MANIFEST.replace("[dependencies]", table_header)
                head_manifest = base_manifest.replace(
                    'anyhow = "1.0.0"', 'anyhow = "1.0.1"'
                )
                scenario = scenario_payload()
                configure_cargo(
                    scenario,
                    base_manifest=base_manifest,
                    head_manifest=head_manifest,
                )

                result = self.run_gate(scenario=scenario)

                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                self.assertEqual(1, len(self.merge_commands()))

    def test_valid_lock_update_preserves_existing_git_package_and_root_metadata(self) -> None:
        unchanged_git_package = """
[[package]]
name = "existing-git-dependency"
version = "0.5.0"
source = "git+https://example.test/existing#0123456789abcdef"
"""
        base_lock = BASE_LOCK.replace(
            'dependencies = ["anyhow"]',
            'dependencies = ["anyhow 1.0.0"]',
        ) + unchanged_git_package
        head_lock = HEAD_LOCK.replace(
            'dependencies = ["anyhow"]',
            'dependencies = ["anyhow 1.0.1"]',
        ) + unchanged_git_package
        scenario = scenario_payload()
        configure_cargo(
            scenario,
            base_lock=base_lock,
            head_lock=head_lock,
        )

        result = self.run_gate(scenario=scenario)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(1, len(self.merge_commands()))

    def test_rejects_nonbot_and_bot_type_spoof(self) -> None:
        cases = [("octocat", "User"), ("dependabot[bot]", "User")]
        for login, account_type in cases:
            with self.subTest(login=login, account_type=account_type):
                scenario = scenario_payload()
                scenario["pr"]["user"] = {"login": login, "type": account_type}
                self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rejects_repository_and_base_trust_mismatches(self) -> None:
        mutations = {
            "event repository": lambda event, api: event["repository"].update(
                full_name="attacker/repo"
            ),
            "run repository": lambda event, api: event["workflow_run"][
                "repository"
            ].update(full_name="attacker/repo"),
            "run head repository": lambda event, api: event["workflow_run"][
                "head_repository"
            ].update(full_name="attacker/repo"),
            "embedded fork": lambda event, api: event["workflow_run"]["pull_requests"][
                0
            ]["head"]["repo"].update(id=202),
            "api head fork": lambda event, api: api["pr"]["head"]["repo"].update(
                full_name="attacker/repo"
            ),
            "api base repo": lambda event, api: api["pr"]["base"]["repo"].update(
                full_name="attacker/repo"
            ),
            "other base": lambda event, api: api["pr"]["base"].update(ref="develop"),
            "default not main": lambda event, api: api["repo"].update(
                default_branch="develop"
            ),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                event = event_payload()
                scenario = scenario_payload()
                mutate(event, scenario)
                self.assert_rejected(self.run_gate(event=event, scenario=scenario))

    def test_rejects_failed_or_wrong_workflow_and_ineligible_pr_state(self) -> None:
        mutations = {
            "failed CI": lambda event, api: event["workflow_run"].update(
                conclusion="failure"
            ),
            "wrong event": lambda event, api: event["workflow_run"].update(event="push"),
            "wrong workflow": lambda event, api: event["workflow_run"].update(
                name="Not CI"
            ),
            "unfinished run": lambda event, api: event["workflow_run"].update(
                status="in_progress"
            ),
            "wrong action": lambda event, api: event.update(action="requested"),
            "closed": lambda event, api: api["pr"].update(state="closed"),
            "draft": lambda event, api: api["pr"].update(draft=True),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                event = event_payload()
                scenario = scenario_payload()
                mutate(event, scenario)
                self.assert_rejected(self.run_gate(event=event, scenario=scenario))

    def test_rejects_stale_or_ambiguous_pr_association(self) -> None:
        cases: list[tuple[str, object]] = [
            ("none", []),
            (
                "multiple",
                event_payload()["workflow_run"]["pull_requests"] * 2,
            ),
            ("malformed", {"number": 7}),
        ]
        for name, associations in cases:
            with self.subTest(name=name):
                event = event_payload()
                event["workflow_run"]["pull_requests"] = associations
                self.assert_rejected(self.run_gate(event=event))

        scenario = scenario_payload()
        scenario["pr"]["head"]["sha"] = OTHER_SHA
        self.assert_rejected(self.run_gate(scenario=scenario))

        event = event_payload()
        event["workflow_run"]["pull_requests"][0]["head"]["sha"] = OTHER_SHA
        self.assert_rejected(self.run_gate(event=event))

        ambiguous = scenario_payload()
        ambiguous["associations"][0].append(
            {"id": 7002, "number": 8, "state": "open"}
        )
        self.assert_rejected(self.run_gate(scenario=ambiguous))

    def test_rejects_unknown_files_rename_and_truncated_pagination(self) -> None:
        cases = {}

        unknown = scenario_payload()
        unknown["pr"]["changed_files"] = 1
        unknown["files"] = [[{**file_list()[0], "filename": "src/lib.rs"}]]
        cases["unknown file"] = unknown

        renamed = scenario_payload()
        renamed["pr"]["changed_files"] = 1
        renamed["files"] = [
            [
                {
                    **file_list()[0],
                    "filename": "Cargo.lock",
                    "status": "renamed",
                    "previous_filename": "README.md",
                }
            ]
        ]
        cases["renamed disallowed source"] = renamed

        truncated = scenario_payload()
        truncated["pr"]["changed_files"] = 3
        cases["pagination count mismatch"] = truncated

        duplicate = scenario_payload()
        duplicate["pr"]["changed_files"] = 3
        duplicate["files"].append([copy.deepcopy(file_list()[0])])
        cases["duplicate across pages"] = duplicate

        absent_manifest = scenario_payload()
        absent_manifest["pr"]["changed_files"] = 1
        absent_manifest["files"] = [
            [{**file_list()[0], "filename": "nested/Cargo.toml"}]
        ]
        cases["manifest absent from trusted checkout"] = absent_manifest

        for name, scenario in cases.items():
            with self.subTest(name=name):
                self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rejects_non_cargo_ecosystem_even_when_file_exists(self) -> None:
        (self.workspace / "package.json").write_text('{"dependencies": {"x": "1.0.0"}}')
        scenario = scenario_payload()
        scenario["pr"]["changed_files"] = 1
        scenario["pr"]["title"] = "Bump x from 1.0.0 to 1.0.1"
        scenario["files"] = [
            [
                {
                    **file_list()[0],
                    "sha": "d" * 40,
                    "filename": "package.json",
                }
            ]
        ]

        self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rejects_manifest_changes_beyond_announced_version_requirement(self) -> None:
        head_manifests = {
            "git source": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"',
                'anyhow = { version = "1.0.1", git = "https://example.test/evil" }',
            ),
            "path source": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"',
                'anyhow = { version = "1.0.1", path = "vendor/anyhow" }',
            ),
            "registry source": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"',
                'anyhow = { version = "1.0.1", registry = "private" }',
            ),
            "features": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"',
                'anyhow = { version = "1.0.1", features = ["backtrace"] }',
            ),
            "optional": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"',
                'anyhow = { version = "1.0.1", optional = true }',
            ),
            "patch": HEAD_MANIFEST
            + '\n[patch.crates-io]\nanyhow = { git = "https://example.test/evil" }\n',
            "package build script": HEAD_MANIFEST.replace(
                'edition = "2021"', 'edition = "2021"\nbuild = "build.rs"'
            ),
            "unannounced dependency": HEAD_MANIFEST
            + '\nserde = "1.0.999"\n',
            "compound constraint": BASE_MANIFEST.replace(
                'anyhow = "1.0.0"', 'anyhow = ">=1.0.1, <2.0.0"'
            ),
        }
        for name, head_manifest in head_manifests.items():
            with self.subTest(name=name):
                scenario = scenario_payload()
                configure_cargo(scenario, head_manifest=head_manifest)
                self.assert_rejected(self.run_gate(scenario=scenario))

        alias_base = BASE_MANIFEST.replace(
            'anyhow = "1.0.0"',
            'error-stack = { package = "anyhow", version = "1.0.0" }',
        )
        alias_head = alias_base.replace(
            'package = "anyhow", version = "1.0.0"',
            'package = "other-crate", version = "1.0.1"',
        )
        scenario = scenario_payload()
        configure_cargo(
            scenario,
            base_manifest=alias_base,
            head_manifest=alias_head,
        )
        self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rejects_symlink_type_and_mode_changes(self) -> None:
        modes = {
            "symlink": {"Cargo.toml": ("120000", "blob")},
            "gitlink": {"Cargo.toml": ("160000", "commit")},
            "executable": {"Cargo.toml": ("100755", "blob")},
        }
        for name, head_modes in modes.items():
            with self.subTest(name=name):
                scenario = scenario_payload()
                configure_cargo(scenario, head_modes=head_modes)
                self.assert_rejected(self.run_gate(scenario=scenario))

    def test_rejects_nondefault_lock_sources_and_malformed_checksums(self) -> None:
        additions = {
            "git source": """
[[package]]
name = "evil"
version = "1.0.0"
source = "git+https://example.test/evil#0123456789abcdef"
""",
            "path source": """
[[package]]
name = "evil"
version = "1.0.0"
""",
            "custom registry": """
[[package]]
name = "evil"
version = "1.0.0"
source = "registry+https://example.test/index"
checksum = "3333333333333333333333333333333333333333333333333333333333333333"
""",
        }
        lock_cases = {
            name: HEAD_LOCK + addition for name, addition in additions.items()
        }
        lock_cases["malformed checksum"] = HEAD_LOCK.replace(
            f'checksum = "{"2" * 64}"', 'checksum = "not-a-checksum"'
        )
        lock_cases["lock version"] = HEAD_LOCK.replace("version = 4", "version = 3", 1)
        lock_cases["wrong announced version"] = HEAD_LOCK.replace(
            'version = "1.0.1"', 'version = "1.0.2"', 1
        )

        for name, head_lock in lock_cases.items():
            with self.subTest(name=name):
                scenario = scenario_payload()
                configure_cargo(scenario, head_lock=head_lock)
                self.assert_rejected(self.run_gate(scenario=scenario))

    def test_fails_closed_for_missing_oversize_or_malformed_git_content(self) -> None:
        cases = {}

        missing_base = scenario_payload()
        base_tree_sha = missing_base["commits"][BASE_SHA]["tree"]["sha"]
        missing_base["trees"][base_tree_sha]["tree"] = [
            entry
            for entry in missing_base["trees"][base_tree_sha]["tree"]
            if entry["path"] != "Cargo.toml"
        ]
        cases["missing base tree entry"] = missing_base

        missing_head_blob = scenario_payload()
        head_manifest_sha = missing_head_blob["files"][0][0]["sha"]
        del missing_head_blob["blobs"][head_manifest_sha]
        cases["missing head blob"] = missing_head_blob

        oversize = scenario_payload()
        head_tree_sha = oversize["commits"][HEAD_SHA]["tree"]["sha"]
        for entry in oversize["trees"][head_tree_sha]["tree"]:
            if entry["path"] == "Cargo.lock":
                entry["size"] = 10 * 1024 * 1024
        cases["oversize head blob"] = oversize

        inconsistent_cached_blob = scenario_payload()
        configure_cargo(
            inconsistent_cached_blob,
            head_manifest=BASE_MANIFEST,
            changed_paths=("Cargo.lock",),
        )
        cached_head_tree_sha = inconsistent_cached_blob["commits"][HEAD_SHA]["tree"][
            "sha"
        ]
        for entry in inconsistent_cached_blob["trees"][cached_head_tree_sha]["tree"]:
            if entry["path"] == "Cargo.toml":
                entry["size"] += 1
        cases["cached blob tree size mismatch"] = inconsistent_cached_blob

        malformed_toml = scenario_payload()
        configure_cargo(malformed_toml, head_manifest="[dependencies\nanyhow =")
        cases["malformed head TOML"] = malformed_toml

        malformed_blob = scenario_payload()
        malformed_sha = malformed_blob["files"][0][0]["sha"]
        malformed_blob["blobs"][malformed_sha]["content"] = "not base64!"
        cases["malformed blob encoding"] = malformed_blob

        for name, scenario in cases.items():
            with self.subTest(name=name):
                result = self.run_gate(scenario=scenario)
                self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
                self.assert_rejected(result)

    def test_rejects_malformed_payloads_and_api_errors(self) -> None:
        malformed_event = self.run_gate(event="{not json")
        self.assertNotEqual(0, malformed_event.returncode)
        self.assert_rejected(malformed_event)

        cases = {}
        bad_repo = scenario_payload()
        bad_repo["repo"] = []
        cases["malformed repo"] = bad_repo

        bad_pr = scenario_payload()
        bad_pr["pr"] = {"number": 7}
        cases["malformed pr"] = bad_pr

        bad_files = scenario_payload()
        bad_files["files"] = {"filename": "Cargo.lock"}
        cases["malformed files"] = bad_files

        bad_associations = scenario_payload()
        bad_associations["associations"] = [[{"number": 7}]]
        cases["malformed associations"] = bad_associations

        invalid_json = scenario_payload()
        invalid_json["raw"] = {"pr": "not-json"}
        cases["invalid API JSON"] = invalid_json

        for operation in (
            "repo",
            "workflow",
            "run",
            "associations",
            "pr",
            "files",
            "commit",
            "tree",
            "blob",
        ):
            failed = scenario_payload()
            failed["fail_on"] = [operation]
            cases[f"{operation} API error"] = failed

        for name, scenario in cases.items():
            with self.subTest(name=name):
                result = self.run_gate(scenario=scenario)
                self.assertNotEqual(0, result.returncode)
                self.assert_rejected(result)

    def test_version_gate_accepts_only_compatible_numeric_increments(self) -> None:
        compatible = [
            "Bump anyhow from 1.0.0 to 1.0.1",
            # Dependabot's titles once a repository uses Conventional Commits.
            "chore(deps): bump anyhow from 1.0.0 to 1.0.1",
            "chore(deps-dev): bump anyhow from 1.0.0 to 1.0.1",
        ]
        for title in compatible:
            with self.subTest(title=title):
                scenario = scenario_payload()
                scenario["pr"]["title"] = title
                result = self.run_gate(scenario=scenario)
                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                self.assertEqual(1, len(self.merge_commands()))

        rejected = [
            "Bump crate from 1.2.3 to 2.0.0",
            "Bump crate from 0.2.3 to 0.3.0",
            "Bump crate from 0.0.1 to 0.0.2",
            "Bump crate from 1.2.3 to 1.2.3",
            "Bump crate from 1.2.3 to 1.2.2",
            "Bump crate from 1.2.3-alpha to 1.2.4",
            "Bump crate from 1.2.3 to 1.2.4-beta",
            "Bump crate from 1 to 1.1",
            "Bump crate from one to two",
            "Bump dependencies in /crate",
            "Bump a from 1.0.0 to 1.0.1 and b from 2.0.0 to 2.0.1",
            "chore(deps): bump crate from 0.2.3 to 0.3.0",
            "fix(deps): bump anyhow from 1.0.0 to 1.0.1",
            "chore(deps): Bump anyhow from 1.0.0 to 1.0.1",
            "bump anyhow from 1.0.0 to 1.0.1",
        ]
        for title in rejected:
            with self.subTest(title=title):
                scenario = scenario_payload()
                scenario["pr"]["title"] = title
                result = self.run_gate(scenario=scenario)
                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                self.assert_rejected(result)
                self.assertEqual(
                    [],
                    [cmd for cmd in self.gh_commands() if cmd and cmd[0] == "pr"],
                    result.stdout + result.stderr,
                )

    def test_refetch_immediately_before_merge_rejects_midflight_head_change(self) -> None:
        scenario = scenario_payload()
        changed = copy.deepcopy(scenario["pr"])
        changed["head"]["sha"] = OTHER_SHA
        scenario["prs"] = [scenario["pr"], changed]

        result = self.run_gate(scenario=scenario)

        self.assert_rejected(result)
        pr_api_calls = [
            cmd
            for cmd in self.gh_commands()
            if cmd[0] == "api" and cmd[-1] == f"repos/{REPOSITORY}/pulls/7"
        ]
        self.assertEqual(2, len(pr_api_calls))

    def test_event_content_never_becomes_shell_code(self) -> None:
        event = event_payload()
        scenario = scenario_payload()
        tainted_ref = f"dependabot/$(touch${{IFS}}{self.marker_path})"
        event["workflow_run"]["head_branch"] = tainted_ref
        event["workflow_run"]["pull_requests"][0]["head"]["ref"] = tainted_ref
        scenario["pr"]["head"]["ref"] = tainted_ref
        scenario["run"]["head_branch"] = tainted_ref
        scenario["run"]["pull_requests"][0]["head"]["ref"] = tainted_ref

        result = self.run_gate(event=event, scenario=scenario)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(1, len(self.merge_commands()))
        self.assertFalse(self.marker_path.exists())


if __name__ == "__main__":
    unittest.main()

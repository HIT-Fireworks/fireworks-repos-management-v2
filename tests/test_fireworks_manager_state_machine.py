import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[1]
CORE_PATH = ROOT / "scripts" / "fireworks_manager_core.py"
SPEC = importlib.util.spec_from_file_location("fireworks_manager_core_integration", CORE_PATH)
assert SPEC and SPEC.loader
core = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(core)


def run_git(repository: Path, *arguments: str) -> str:
    process = subprocess.run(
        ["git", *arguments],
        cwd=repository,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode:
        raise AssertionError(
            f"git {' '.join(arguments)} failed:\n{process.stdout}\n{process.stderr}"
        )
    return process.stdout.strip()


def create_remote(root: Path, repo_id: str, files=None):
    remote = root / "remotes" / f"{repo_id}.git"
    remote.parent.mkdir(parents=True, exist_ok=True)
    run_git(remote.parent, "init", "--bare", str(remote))
    if files is None:
        return remote, None
    work = root / f"{repo_id}-work"
    work.mkdir()
    run_git(work, "init")
    run_git(work, "config", "user.name", "Core Integration Test")
    run_git(work, "config", "user.email", "core@example.invalid")
    for relative, content in files.items():
        path = work / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    run_git(work, "add", ".")
    run_git(work, "commit", "-m", "test: seed")
    run_git(work, "branch", "-M", "main")
    run_git(work, "remote", "add", "origin", str(remote))
    run_git(work, "push", "origin", "main")
    return remote, run_git(work, "rev-parse", "HEAD")


def write_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


class CoreStateMachineIntegrationTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source_remote, self.source_head = create_remote(
            self.root,
            "COURSE-A",
            {"README.md": "source", "notes/a.txt": "A", "notes/b.txt": "B"},
        )
        assert self.source_head
        self.workspace = self.root / "workspace"
        self.topology = {
            "schema_version": 1,
            "generation": 1,
            "organization": "LOCAL",
            "repositories": {
                "COURSE-A": {
                    "repo_id": "COURSE-A",
                    "repo_type": "course",
                    "display_name": "课程 A",
                    "physical_repository_id": "physical-a",
                    "member_resource_group_ids": ["group-a", "group-b"],
                    "lineage": {"kind": "fixture", "source_repo_ids": ["COURSE-A"]},
                }
            },
        }
        self.routes = {
            "schema_version": 1,
            "generation": 1,
            "source_plan_identity_sha256": "fixture-plan",
            "inventory_complete_repositories": ["COURSE-A"],
            "repository_heads": {"COURSE-A": self.source_head},
            "files": [
                {
                    "repo_id": "COURSE-A",
                    "path": "README.md",
                    "resource_group_id": None,
                    "sha256": None,
                    "size": 6,
                },
                {
                    "repo_id": "COURSE-A",
                    "path": "notes/a.txt",
                    "resource_group_id": "group-a",
                    "sha256": None,
                    "size": 1,
                },
                {
                    "repo_id": "COURSE-A",
                    "path": "notes/b.txt",
                    "resource_group_id": "group-b",
                    "sha256": None,
                    "size": 1,
                },
            ],
            "course_code_routes": [],
        }
        self.manifest = {
            "schema_version": 1,
            "organization": "LOCAL",
            "sources": {
                "curriculum": {"metadata_repo_id": "fireworks-course-registry-v2"}
            },
            "repositories": [
                {
                    "repo_id": "COURSE-A",
                    "repo_type": "course",
                    "display_name": "课程 A",
                    "description": "课程 A",
                    "member_resource_group_ids": ["group-a", "group-b"],
                }
            ],
            "course_descriptors": [],
            "virtual_collections": [],
        }
        write_json(self.workspace / "data/repository-manifest.no-collection.v4.json", self.manifest)
        write_json(self.workspace / "config/repository-topology.v4.json", self.topology)
        write_json(self.workspace / "config/repository-file-routes.v4.json", self.routes)
        self.remote_template = str(self.root / "remotes" / "{repo_id}.git")

    def tearDown(self):
        self.temporary.cleanup()

    def request(self, family, kind, arguments=None, confirmation=None):
        return {
            "schema_version": 1,
            "request_id": "integration",
            "command": {"family": family, "kind": kind, "arguments": arguments or {}},
            "context": {
                "workspace": str(self.workspace),
                "organization": "LOCAL",
                "actor": "integration-test",
            },
            "confirmation": confirmation,
        }

    def plan_split(self):
        response = core.invoke(
            self.request(
                "plan",
                "split",
                {
                    "source_repo_id": "COURSE-A",
                    "targets": [
                        {
                            "repo_id": "COURSE-A1",
                            "display_name": "课程 A 一",
                            "resource_group_ids": ["group-a"],
                            "paths": ["README.md"],
                        },
                        {
                            "repo_id": "COURSE-A2",
                            "display_name": "课程 A 二",
                            "resource_group_ids": ["group-b"],
                            "paths": [],
                        },
                    ],
                    "remote_url_template": self.remote_template,
                },
            )
        )
        self.assertTrue(response["ok"], response)
        return response["result"]

    def execute_arguments(self, planned):
        return {
            "plan": planned["path"],
            "plan_identity_sha256": planned["plan"]["core"][
                "plan_identity_sha256"
            ],
        }

    def test_plan_apply_provisions_missing_targets_and_verifies(self):
        planned = self.plan_split()
        frozen = json.loads(Path(planned["path"]).read_text(encoding="utf-8"))
        self.assertFalse(frozen["core"]["remote_baseline"]["targets"]["COURSE-A1"]["exists"])
        response = core.invoke(
            self.request(
                "execute",
                "apply",
                self.execute_arguments(planned),
                confirmation=frozen["core"]["confirmation_phrase"],
            )
        )
        self.assertTrue(response["ok"], response)
        journal_path = Path(response["result"]["journal_path"])
        journal = json.loads(journal_path.read_text(encoding="utf-8"))
        self.assertEqual(journal["status"], "completed")
        self.assertEqual(journal["plan"], frozen)
        self.assertIn("unresolved_repository_heads", frozen["after"]["routes"])
        self.assertNotIn("unresolved_repository_heads", journal["resolved_after_routes"])
        for repo_id in ("COURSE-A1", "COURSE-A2"):
            self.assertTrue((self.root / "remotes" / f"{repo_id}.git").exists())
            self.assertTrue(core.engine.remote_head(self.remote_template.format(repo_id=repo_id)))
        duplicate = core.invoke(
            self.request(
                "execute",
                "apply",
                self.execute_arguments(planned),
                confirmation=frozen["core"]["confirmation_phrase"],
            )
        )
        self.assertFalse(duplicate["ok"])
        self.assertEqual(duplicate["errors"][0]["code"], "workspace_drifted")
        verify = core.invoke(
            self.request(
                "execute",
                "verify",
                {
                    "journal": str(journal_path),
                    "plan_identity_sha256": frozen["core"]["plan_identity_sha256"],
                },
            )
        )
        self.assertTrue(verify["ok"], verify)
        self.assertTrue(verify["result"]["valid"])

    def test_tampered_journal_remote_url_is_invalid(self):
        planned = self.plan_split()
        frozen = planned["plan"]
        applied = core.invoke(
            self.request(
                "execute",
                "apply",
                self.execute_arguments(planned),
                confirmation=frozen["core"]["confirmation_phrase"],
            )
        )
        self.assertTrue(applied["ok"], applied)
        journal_path = Path(applied["result"]["journal_path"])
        journal = json.loads(journal_path.read_text(encoding="utf-8"))
        journal["status"] = "failed"
        journal["git"]["targets"]["COURSE-A1"]["remote_url"] = str(
            self.root / "attacker.git"
        )
        write_json(journal_path, journal)
        response = core.invoke(
            self.request(
                "execute",
                "resume",
                {
                    "journal": str(journal_path),
                    "plan_identity_sha256": frozen["core"]["plan_identity_sha256"],
                },
                confirmation=f"RESUME {frozen['operation_id']}",
            )
        )
        self.assertFalse(response["ok"])
        self.assertEqual(response["errors"][0]["code"], "journal_invalid")

    def test_failed_journal_resumes_from_topology_boundary(self):
        planned = self.plan_split()
        operation = planned["plan"]
        identity = operation["core"]["plan_identity_sha256"]
        adapter = core.adapter_for_operation(operation)
        core.validate_remote_baseline(operation, adapter)
        journal_path = (
            self.workspace
            / "data/repository-management-operations"
            / f"{operation['operation_id']}.json"
        )

        def interrupt(stage):
            if stage == "topology":
                raise RuntimeError("interrupt after topology")

        with self.assertRaisesRegex(RuntimeError, "interrupt after topology"):
            core.engine.apply(
                operation,
                topology_path=self.workspace
                / "config/repository-topology.v4.json",
                routes_path=self.workspace
                / "config/repository-file-routes.v4.json",
                journal_path=journal_path,
                remote_url_for=adapter.url,
                ensure_target_repository=adapter.ensure_repository,
                stage_hook=interrupt,
            )
        failed = json.loads(journal_path.read_text(encoding="utf-8"))
        self.assertEqual(failed["status"], "failed")
        self.assertIn("topology", failed["completed_stages"])
        listed = core.invoke(self.request("query", "journals"))
        self.assertTrue(listed["ok"], listed)
        row = next(
            item
            for item in listed["result"]["journals"]
            if item.get("operation_id") == operation["operation_id"]
        )
        self.assertEqual(row["recovery_state"], "resumable")
        resumed = core.invoke(
            self.request(
                "execute",
                "resume",
                {
                    "journal": str(journal_path),
                    "plan_identity_sha256": identity,
                },
                confirmation=f"RESUME {operation['operation_id']}",
            )
        )
        self.assertTrue(resumed["ok"], resumed)
        self.assertEqual(resumed["result"]["journal"]["status"], "completed")
        verified = core.invoke(
            self.request(
                "execute",
                "verify",
                {"journal": str(journal_path), "plan_identity_sha256": identity},
            )
        )
        self.assertTrue(verified["ok"], verified)



if __name__ == "__main__":
    unittest.main()

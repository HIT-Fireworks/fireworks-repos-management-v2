import importlib.util
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

ROOT = Path(__file__).parents[1]
MODULE_PATH = ROOT / "scripts" / "repository_management.py"
SPEC = importlib.util.spec_from_file_location("repository_management_engine", MODULE_PATH)
assert SPEC and SPEC.loader
management = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(management)


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


def create_bare_remote(
    root: Path, repo_id: str, files: dict[str, str] | None = None
) -> tuple[Path, str | None]:
    remote = root / f"{repo_id}.git"
    remote.mkdir()
    run_git(remote, "init", "--bare")
    if files is None:
        return remote, None
    work = root / f"{repo_id}-work"
    work.mkdir()
    run_git(work, "init")
    run_git(work, "config", "user.name", "Repository Management Test")
    run_git(work, "config", "user.email", "repository-test@example.invalid")
    for relative, content in files.items():
        path = work / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    run_git(work, "add", ".")
    run_git(work, "commit", "-m", "test: seed repository")
    run_git(work, "branch", "-M", "main")
    run_git(work, "remote", "add", "origin", str(remote))
    run_git(work, "push", "origin", "main")
    return remote, run_git(work, "rev-parse", "HEAD")


def split_state(source_head: str) -> tuple[dict, dict]:
    topology = {
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
    routes = {
        "schema_version": 1,
        "generation": 1,
        "inventory_complete_repositories": ["COURSE-A"],
        "repository_heads": {"COURSE-A": source_head},
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
    }
    management.validate_state(topology, routes)
    return topology, routes


class RepositoryManagementEngineIntegrationTest(unittest.TestCase):
    def test_split_preserves_frozen_plan_and_resumes_after_topology_boundary(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_remote, source_head = create_bare_remote(
                root,
                "COURSE-A",
                {
                    "README.md": "source",
                    "notes/a.txt": "A",
                    "notes/b.txt": "B",
                },
            )
            assert source_head
            target_one, _ = create_bare_remote(root, "COURSE-A1")
            target_two, _ = create_bare_remote(root, "COURSE-A2")
            remotes = {
                "COURSE-A": source_remote,
                "COURSE-A1": target_one,
                "COURSE-A2": target_two,
            }
            topology, routes = split_state(source_head)
            plan = management.plan_split(
                topology,
                routes,
                source_repo_id="COURSE-A",
                targets=[
                    {
                        "repo_id": "COURSE-A1",
                        "resource_group_ids": ["group-a"],
                        "paths": ["README.md"],
                    },
                    {
                        "repo_id": "COURSE-A2",
                        "resource_group_ids": ["group-b"],
                        "paths": [],
                    },
                ],
            )
            frozen_identity = management.canonical_sha256(plan)
            topology_path = root / "topology.json"
            routes_path = root / "routes.json"
            journal_path = root / "journal.json"
            management.atomic_json(topology_path, topology)
            management.atomic_json(routes_path, routes)

            def interrupt(stage: str) -> None:
                if stage == "topology":
                    raise RuntimeError("interrupt after topology")

            with self.assertRaisesRegex(RuntimeError, "interrupt after topology"):
                management.apply(
                    plan,
                    topology_path=topology_path,
                    routes_path=routes_path,
                    journal_path=journal_path,
                    remote_url_for=lambda repo_id: str(remotes[repo_id]),
                    stage_hook=interrupt,
                )
            self.assertEqual(management.canonical_sha256(plan), frozen_identity)
            failed = management.load_json(journal_path)
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["plan"], plan)
            self.assertIn("unresolved_repository_heads", failed["plan"]["after"]["routes"])
            self.assertNotIn(
                "unresolved_repository_heads", failed["resolved_after_routes"]
            )

            completed = management.resume(
                journal_path,
                topology_path=topology_path,
                routes_path=routes_path,
            )
            self.assertEqual(completed["status"], "completed")
            final_topology = management.load_json(topology_path)
            final_routes = management.load_json(routes_path)
            management.validate_state(final_topology, final_routes)
            self.assertNotIn("unresolved_repository_heads", final_routes)
            self.assertEqual(
                run_git(target_one, "ls-tree", "-r", "--name-only", "main").splitlines(),
                ["README.md", "notes/a.txt"],
            )
            self.assertEqual(
                run_git(target_two, "ls-tree", "-r", "--name-only", "main").splitlines(),
                ["notes/b.txt"],
            )
            self.assertEqual(run_git(target_one, "show", "main:notes/a.txt"), "A")
            self.assertEqual(run_git(target_two, "show", "main:notes/b.txt"), "B")
            self.assertEqual(management.remote_head(str(source_remote)), source_head)

    def test_merge_preserves_files_and_relocates_path_collision(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            remote_a, head_a = create_bare_remote(
                root,
                "COURSE-A",
                {"README.md": "A readme", "notes/a.txt": "A"},
            )
            remote_b, head_b = create_bare_remote(
                root,
                "COURSE-B",
                {"README.md": "B readme", "notes/b.txt": "B"},
            )
            assert head_a and head_b
            topology = {
                "schema_version": 1,
                "generation": 1,
                "organization": "LOCAL",
                "repositories": {
                    repo_id: {
                        "repo_id": repo_id,
                        "repo_type": "course",
                        "display_name": f"课程 {repo_id[-1]}",
                        "physical_repository_id": f"physical-{repo_id[-1].lower()}",
                        "member_resource_group_ids": [f"group-{repo_id[-1].lower()}"],
                        "lineage": {"kind": "fixture", "source_repo_ids": [repo_id]},
                    }
                    for repo_id in ("COURSE-A", "COURSE-B")
                },
            }
            routes = {
                "schema_version": 1,
                "generation": 1,
                "inventory_complete_repositories": ["COURSE-A", "COURSE-B"],
                "repository_heads": {"COURSE-A": head_a, "COURSE-B": head_b},
                "files": [
                    {
                        "repo_id": repo_id,
                        "path": path,
                        "resource_group_id": f"group-{repo_id[-1].lower()}",
                        "sha256": None,
                        "size": len(content),
                    }
                    for repo_id, path, content in (
                        ("COURSE-A", "README.md", "A readme"),
                        ("COURSE-A", "notes/a.txt", "A"),
                        ("COURSE-B", "README.md", "B readme"),
                        ("COURSE-B", "notes/b.txt", "B"),
                    )
                ],
            }
            management.validate_state(topology, routes)
            plan = management.plan_merge(
                topology,
                routes,
                source_repo_ids=["COURSE-A", "COURSE-B"],
                target_repo_id="COURSE-A",
                display_name="课程 A/B",
            )
            topology_path = root / "topology.json"
            routes_path = root / "routes.json"
            journal_path = root / "journal.json"
            management.atomic_json(topology_path, topology)
            management.atomic_json(routes_path, routes)
            remotes = {"COURSE-A": remote_a, "COURSE-B": remote_b}

            completed = management.apply(
                plan,
                topology_path=topology_path,
                routes_path=routes_path,
                journal_path=journal_path,
                remote_url_for=lambda repo_id: str(remotes[repo_id]),
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(
                run_git(remote_a, "ls-tree", "-r", "--name-only", "main").splitlines(),
                [
                    "README.md",
                    "merged-from/COURSE-B/README.md",
                    "notes/a.txt",
                    "notes/b.txt",
                ],
            )
            self.assertEqual(run_git(remote_a, "show", "main:README.md"), "A readme")
            self.assertEqual(
                run_git(remote_a, "show", "main:merged-from/COURSE-B/README.md"),
                "B readme",
            )
            self.assertEqual(management.remote_head(str(remote_b)), head_b)


if __name__ == "__main__":
    unittest.main()

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[1]
CORE_PATH = ROOT / "scripts" / "fireworks_manager_core.py"
SPEC = importlib.util.spec_from_file_location("fireworks_manager_core", CORE_PATH)
assert SPEC and SPEC.loader
core = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(core)


class FireworksManagerCoreTest(unittest.TestCase):
    def request(
        self,
        family,
        kind,
        arguments=None,
        confirmation=None,
        workspace=None,
        organization="HIT-Fireworks",
    ):
        return {
            "schema_version": 1,
            "request_id": "test",
            "command": {"family": family, "kind": kind, "arguments": arguments or {}},
            "context": {
                "workspace": str(workspace or ROOT),
                "organization": organization,
                "actor": "agent",
            },
            "confirmation": confirmation,
        }

    def test_inspect_and_search_current_v4_state(self):
        inspect = core.invoke(self.request("query", "inspect"))
        self.assertTrue(inspect["ok"], inspect)
        self.assertEqual(inspect["mode"], "read_only")
        self.assertEqual(inspect["result"]["repository_count"], 100)
        self.assertEqual(inspect["result"]["course_route_count"], 2618)
        self.assertEqual(inspect["result"]["file_route_count"], 3857)

        search = core.invoke(
            self.request("query", "search", {"term": "数理逻辑", "limit": 20})
        )
        self.assertTrue(search["ok"], search)
        ids = {row["repo_id"] for row in search["result"]["repositories"]}
        self.assertIn("COURSES-RA-ED4F15651BB9", ids)

    def test_repository_detail_exposes_domain_view(self):
        response = core.invoke(
            self.request(
                "query",
                "repository",
                {"repo_id": "COURSES-RA-ED4F15651BB9"},
            )
        )
        self.assertTrue(response["ok"], response)
        detail = response["result"]
        self.assertEqual(detail["display_name"], "数理逻辑与近世代数")
        self.assertEqual(
            detail["course_codes"], ["22CS22002", "22CS22038", "CS31114"]
        )
        self.assertEqual(detail["file_count"], 29)

    def state_arguments(self, *, topology=None, operations=None):
        return {
            "manifest": str(ROOT / "data/repository-manifest.no-collection.v4.json"),
            "topology": str(
                topology or ROOT / "config/repository-topology.v4.json"
            ),
            "routes": str(ROOT / "config/repository-file-routes.v4.json"),
            "operations": str(
                operations or ROOT / "data/repository-management-operations"
            ),
        }

    def write_fake_plan(
        self,
        operations,
        *,
        workspace_identity,
        organization="HIT-Fireworks",
    ):
        operations.mkdir(parents=True)
        operation = {
            "schema_version": 1,
            "operation_id": "fake",
            "created_at": "2026-08-29T00:00:00+00:00",
            "kind": "merge",
            "before": {
                "topology_sha256": workspace_identity["topology_sha256"],
                "routes_sha256": workspace_identity["routes_sha256"],
            },
            "after": {
                "topology": json.loads(
                    (ROOT / "config/repository-topology.v4.json").read_text(
                        encoding="utf-8"
                    )
                ),
                "routes": json.loads(
                    (ROOT / "config/repository-file-routes.v4.json").read_text(
                        encoding="utf-8"
                    )
                ),
                "topology_sha256": workspace_identity["topology_sha256"],
                "routes_sha256": workspace_identity["routes_sha256"],
            },
            "details": {"source_repository_heads": {}, "file_moves": []},
            "core": {
                "organization": organization,
                "workspace_identity": workspace_identity,
                "request_actor": "agent",
                "confirmation_phrase": "APPLY fake",
                "remote_baseline": {
                    "kind": "local",
                    "remote_url_template": "unused/{repo_id}.git",
                    "github_actor": "local-test",
                    "registry": {
                        "repo_id": "fireworks-course-registry-v2",
                        "manifest_sha256": workspace_identity["manifest_sha256"],
                        "source_plan_identity_sha256": None,
                    },
                    "sources": {},
                    "targets": {},
                },
            },
        }
        operation["core"]["plan_identity_sha256"] = core.plan_identity_sha256(
            operation
        )
        path = operations / "fake.plan.json"
        path.write_text(json.dumps(operation), encoding="utf-8")
        return path, operation["core"]["plan_identity_sha256"]

    def baseline_identity(self):
        response = core.invoke(self.request("query", "inspect"))
        self.assertTrue(response["ok"], response)
        return response["result"]["identity"]

    def assert_error(self, response, code):
        self.assertFalse(response["ok"], response)
        self.assertEqual(response["errors"][0]["code"], code)

    def test_read_only_query_does_not_write_workspace(self):
        state_paths = [
            ROOT / "data/repository-manifest.no-collection.v4.json",
            ROOT / "config/repository-topology.v4.json",
            ROOT / "config/repository-file-routes.v4.json",
        ]
        before = {
            path: hashlib.sha256(path.read_bytes()).hexdigest() for path in state_paths
        }
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "operations"
            arguments = self.state_arguments(operations=operations)
            arguments.update({"term": "数理逻辑", "limit": 20})
            response = core.invoke(self.request("query", "search", arguments))
            self.assertTrue(response["ok"], response)
            self.assertEqual(response["mode"], "read_only")
            self.assertFalse(operations.exists())
        after = {
            path: hashlib.sha256(path.read_bytes()).hexdigest() for path in state_paths
        }
        self.assertEqual(after, before)

    def test_routes_limit_zero_returns_complete_snapshot(self):
        response = core.invoke(
            self.request("query", "routes", {"limit": 0})
        )
        self.assertTrue(response["ok"], response)
        result = response["result"]
        self.assertEqual(result["file_total"], 3857)
        self.assertEqual(result["course_code_total"], 2618)
        self.assertEqual(len(result["file_routes"]), result["file_total"])
        self.assertEqual(
            len(result["course_code_routes"]), result["course_code_total"]
        )

    def test_missing_operations_directory_returns_empty_lists(self):
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "missing-operations"
            arguments = self.state_arguments(operations=operations)
            plans = core.invoke(self.request("query", "plan", arguments))
            journals = core.invoke(self.request("query", "journals", arguments))
            self.assertTrue(plans["ok"], plans)
            self.assertTrue(journals["ok"], journals)
            self.assertEqual(plans["result"]["plans"], [])
            self.assertEqual(journals["result"]["journals"], [])
            self.assertFalse(operations.exists())

    def test_mutation_rejects_wrong_plan_identity_before_journal(self):
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "operations"
            plan, identity = self.write_fake_plan(
                operations, workspace_identity=self.baseline_identity()
            )
            arguments = self.state_arguments(operations=operations)
            arguments.update(
                {"plan": str(plan), "plan_identity_sha256": "wrong-identity"}
            )
            response = core.invoke(self.request("execute", "apply", arguments))
            self.assert_error(response, "plan_identity_mismatch")
            self.assertFalse((operations / "fake.json").exists())

    def test_mutation_rejects_plan_for_another_organization(self):
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "operations"
            plan, identity = self.write_fake_plan(
                operations,
                workspace_identity=self.baseline_identity(),
                organization="Another-Organization",
            )
            arguments = self.state_arguments(operations=operations)
            arguments.update({"plan": str(plan), "plan_identity_sha256": identity})
            response = core.invoke(self.request("execute", "apply", arguments))
            self.assert_error(response, "organization_mismatch")
            self.assertFalse((operations / "fake.json").exists())

    def test_mutation_rejects_workspace_drift_before_journal(self):
        baseline_identity = self.baseline_identity()
        with tempfile.TemporaryDirectory() as temporary:
            temporary = Path(temporary)
            operations = temporary / "operations"
            topology_path = temporary / "repository-topology.v4.json"
            topology = json.loads(
                (ROOT / "config/repository-topology.v4.json").read_text(
                    encoding="utf-8"
                )
            )
            first_repo_id = next(iter(topology["repositories"]))
            topology["repositories"][first_repo_id]["display_name"] += "（漂移）"
            topology_path.write_text(
                json.dumps(topology, ensure_ascii=False), encoding="utf-8"
            )
            plan, identity = self.write_fake_plan(
                operations, workspace_identity=baseline_identity
            )
            arguments = self.state_arguments(
                topology=topology_path, operations=operations
            )
            arguments.update({"plan": str(plan), "plan_identity_sha256": identity})
            response = core.invoke(self.request("execute", "apply", arguments))
            self.assert_error(response, "workspace_drifted")
            self.assertFalse((operations / "fake.json").exists())

    def test_mutation_requires_exact_confirmation_before_journal(self):
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "operations"
            plan, identity = self.write_fake_plan(
                operations, workspace_identity=self.baseline_identity()
            )
            arguments = self.state_arguments(operations=operations)
            arguments.update({"plan": str(plan), "plan_identity_sha256": identity})
            response = core.invoke(
                self.request(
                    "execute",
                    "apply",
                    arguments,
                    confirmation="APPLY another-operation",
                )
            )
            self.assert_error(response, "confirmation_required")
            self.assertIn("APPLY fake", response["errors"][0]["message"])
            self.assertFalse((operations / "fake.json").exists())

    def test_mutation_rejects_tampered_persisted_plan(self):
        with tempfile.TemporaryDirectory() as temporary:
            operations = Path(temporary) / "operations"
            plan, identity = self.write_fake_plan(
                operations, workspace_identity=self.baseline_identity()
            )
            operation = json.loads(plan.read_text(encoding="utf-8"))
            operation["details"]["file_moves"].append(
                {"source_repo_id": "A", "target_repo_id": "B", "path": "x"}
            )
            plan.write_text(json.dumps(operation), encoding="utf-8")
            arguments = self.state_arguments(operations=operations)
            arguments.update({"plan": str(plan), "plan_identity_sha256": identity})
            response = core.invoke(self.request("execute", "apply", arguments))
            self.assert_error(response, "plan_identity_mismatch")
            self.assertFalse((operations / "fake.json").exists())

    def test_incompatible_schema_is_machine_readable_failure(self):
        request = self.request("query", "inspect")
        request["schema_version"] = 2
        response = core.invoke(request)
        self.assertFalse(response["ok"])
        self.assertEqual(response["errors"][0]["code"], "schema_incompatible")


if __name__ == "__main__":
    unittest.main()

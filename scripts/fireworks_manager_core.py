#!/usr/bin/env python3
"""HIT-Fireworks 仓库管理共享 JSON 核心。"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.parse
from datetime import UTC, datetime
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any, Mapping, Sequence

try:
    import repository_management as engine
except ModuleNotFoundError:
    from scripts import repository_management as engine

SCHEMA_VERSION = 1
DEFAULT_ORGANIZATION = "HIT-Fireworks"
DEFAULT_MANIFEST = Path("data/repository-manifest.no-collection.v4.json")
DEFAULT_TOPOLOGY = Path("config/repository-topology.v4.json")
DEFAULT_ROUTES = Path("config/repository-file-routes.v4.json")
DEFAULT_OPERATIONS = Path("data/repository-management-operations")
DEFAULT_REGISTRY_REPO_ID = "fireworks-course-registry-v2"
_SHA1 = re.compile(r"^[0-9a-f]{40}$")


class CoreError(Exception):
    def __init__(self, code: str, message: str, *, retryable: bool = False) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


def now() -> str:
    return datetime.now(UTC).isoformat()


def normalize(text: Any) -> str:
    return str(text or "").strip()


def canonical_sha256(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def plan_identity_sha256(operation: Mapping[str, Any]) -> str:
    """计算冻结计划身份；排除生成时间和 identity 字段自身。"""
    payload = {
        key: copy.deepcopy(item)
        for key, item in operation.items()
        if key != "created_at"
    }
    metadata = dict(payload.get("core") or {})
    metadata.pop("plan_identity_sha256", None)
    payload["core"] = metadata
    return canonical_sha256(payload)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise CoreError("source_missing", f"状态文件不存在：{path}") from error
    except json.JSONDecodeError as error:
        raise CoreError("source_invalid", f"状态文件不是合法 JSON：{path}: {error}") from error
    if not isinstance(value, dict):
        raise CoreError("source_invalid", f"状态文件根不是对象：{path}")
    return value


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    os.replace(temporary, path)


def success(
    request_id: str,
    mode: str,
    result: Any,
    *,
    warnings=None,
    evidence=None,
    next_actions=None,
):
    return {
        "schema_version": SCHEMA_VERSION,
        "request_id": request_id,
        "ok": True,
        "mode": mode,
        "result": result,
        "warnings": list(warnings or []),
        "errors": [],
        "evidence": list(evidence or []),
        "next_actions": list(next_actions or []),
    }


def failure(request_id: str, error: Exception):
    if isinstance(error, CoreError):
        code, message, retryable = error.code, error.message, error.retryable
    elif isinstance(error, engine.ManagementError):
        code, message, retryable = "contract_violation", str(error), False
    else:
        code, message, retryable = "internal_error", str(error), False
    mode = "drifted" if code in {"workspace_drifted", "remote_drifted"} else "failed"
    return {
        "schema_version": SCHEMA_VERSION,
        "request_id": request_id,
        "ok": False,
        "mode": mode,
        "result": None,
        "warnings": [],
        "errors": [{"code": code, "message": message, "retryable": retryable}],
        "evidence": [],
        "next_actions": [],
    }


def _run(
    arguments: Sequence[str],
    *,
    cwd: Path | None = None,
    input_text: str | None = None,
    allow_failure: bool = False,
) -> subprocess.CompletedProcess[str]:
    process = subprocess.run(
        list(arguments),
        cwd=cwd,
        input=input_text,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode and not allow_failure:
        raise CoreError(
            "remote_unavailable",
            f"命令失败：{' '.join(arguments)}\n{process.stderr.strip()}",
            retryable=True,
        )
    return process


class Workspace:
    def __init__(self, root: Path, arguments: Mapping[str, Any]) -> None:
        self.root = root.resolve()
        self.manifest_path = self.resolve(arguments.get("manifest"), DEFAULT_MANIFEST)
        self.topology_path = self.resolve(arguments.get("topology"), DEFAULT_TOPOLOGY)
        self.routes_path = self.resolve(arguments.get("routes"), DEFAULT_ROUTES)
        self.operations_path = self.resolve(arguments.get("operations"), DEFAULT_OPERATIONS)
        self.manifest = load_json(self.manifest_path)
        self.topology = load_json(self.topology_path)
        self.routes = load_json(self.routes_path)
        try:
            engine.validate_state(self.topology, self.routes)
            self.state_valid = True
            self.state_error = None
        except Exception as error:
            self.state_valid = False
            self.state_error = str(error)

    def resolve(self, value: Any, default: Path) -> Path:
        path = Path(str(value)) if value else default
        return path if path.is_absolute() else self.root / path

    @property
    def organization(self) -> str:
        return normalize(self.topology.get("organization") or self.manifest.get("organization"))

    @property
    def identity(self) -> dict[str, str]:
        return {
            "manifest_sha256": canonical_sha256(self.manifest),
            "topology_sha256": canonical_sha256(self.topology),
            "routes_sha256": canonical_sha256(self.routes),
        }

    def repository_rows(self) -> list[dict[str, Any]]:
        manifest_by_id = {
            row["repo_id"]: row for row in self.manifest.get("repositories", [])
        }
        descriptors = {
            row["course_code"]: row
            for row in self.manifest.get("course_descriptors", [])
        }
        files_by_repo: dict[str, list[dict[str, Any]]] = {}
        for row in self.routes.get("files", []):
            files_by_repo.setdefault(row["repo_id"], []).append(row)
        rows: list[dict[str, Any]] = []
        for repo_id, topology_row in sorted(self.topology.get("repositories", {}).items()):
            manifest_row = manifest_by_id.get(repo_id, {})
            codes = list(
                topology_row.get("course_codes")
                or manifest_row.get("course_codes")
                or []
            )
            files = files_by_repo.get(repo_id, [])
            rows.append(
                {
                    "repo_id": repo_id,
                    "repo_type": topology_row.get("repo_type")
                    or manifest_row.get("repo_type"),
                    "display_name": topology_row.get("display_name")
                    or manifest_row.get("display_name")
                    or repo_id,
                    "description": manifest_row.get("description") or "",
                    "physical_repository_id": topology_row.get("physical_repository_id"),
                    "course_codes": codes,
                    "course_names": sorted(
                        {
                            descriptors[code]["course_name"]
                            for code in codes
                            if code in descriptors
                        }
                    ),
                    "member_resource_group_ids": list(
                        topology_row.get("member_resource_group_ids")
                        or manifest_row.get("member_resource_group_ids")
                        or []
                    ),
                    "file_count": len(files),
                    "bytes": sum(int(row.get("size") or 0) for row in files),
                    "route_kinds": dict(
                        sorted(
                            __import__("collections").Counter(
                                row.get("route_kind") or "unclassified" for row in files
                            ).items()
                        )
                    ),
                    "unowned_paths": sorted(
                        row["path"] for row in files if not row.get("resource_group_id")
                    ),
                    "head": self.routes.get("repository_heads", {}).get(repo_id),
                    "inventory_complete": repo_id
                    in set(self.routes.get("inventory_complete_repositories", [])),
                }
            )
        return rows


class RemoteAdapter:
    """GitHub production and local bare-remote adapter used by one safety model."""

    def __init__(
        self,
        organization: str,
        *,
        template: str = engine.DEFAULT_REMOTE_URL_TEMPLATE,
    ) -> None:
        self.organization = organization
        self.template = template
        self.kind = "github" if template == engine.DEFAULT_REMOTE_URL_TEMPLATE else "local"

    def url(self, repo_id: str) -> str:
        return engine.format_remote_url(
            self.template,
            organization=self.organization,
            repo_id=engine.safe_repo_id(repo_id),
        )

    def actor(self) -> str:
        if self.kind == "local":
            return "local-test"
        process = _run(["gh", "api", "user", "--jq", ".login"])
        actor = process.stdout.strip()
        if not actor:
            raise CoreError("github_identity_missing", "无法读取 GitHub 登录身份")
        return actor

    def _local_path(self, repo_id: str) -> Path:
        value = self.url(repo_id)
        if value.startswith("file://"):
            value = urllib.parse.unquote(urllib.parse.urlparse(value).path)
            if os.name == "nt" and value.startswith("/") and len(value) > 2:
                value = value[1:]
        return Path(value)

    def repository(self, repo_id: str) -> dict[str, Any] | None:
        repo_id = engine.safe_repo_id(repo_id)
        if self.kind == "local":
            path = self._local_path(repo_id)
            return {"name": repo_id, "empty": True} if path.exists() else None
        process = _run(
            ["gh", "api", f"repos/{self.organization}/{repo_id}"],
            allow_failure=True,
        )
        if process.returncode:
            combined = f"{process.stdout}\n{process.stderr}"
            if "HTTP 404" in combined or "Not Found" in combined:
                return None
            raise CoreError(
                "remote_unavailable",
                f"无法读取 GitHub 仓库 {self.organization}/{repo_id}：{process.stderr.strip()}",
                retryable=True,
            )
        value = json.loads(process.stdout)
        if not isinstance(value, dict):
            raise CoreError("remote_invalid", f"GitHub 仓库响应无效：{repo_id}")
        return value

    def revision(self, repo_id: str) -> dict[str, Any]:
        repository = self.repository(repo_id)
        url = self.url(repo_id)
        if repository is None:
            return {
                "exists": False,
                "head": None,
                "tree": None,
                "remote_url": url,
            }
        head = engine.remote_head(url)
        tree = None
        if head:
            if not _SHA1.fullmatch(head):
                raise CoreError("remote_invalid", f"远端 HEAD 非法：{repo_id}={head}")
            with TemporaryDirectory(prefix="fireworks-remote-tree-") as temporary:
                bare = Path(temporary)
                _run(["git", "init", "--bare"], cwd=bare)
                _run(
                    [
                        "git",
                        "fetch",
                        "--no-tags",
                        "--depth=1",
                        url,
                        f"{head}:refs/fireworks/preflight",
                    ],
                    cwd=bare,
                )
                tree = _run(
                    ["git", "rev-parse", "refs/fireworks/preflight^{tree}"], cwd=bare
                ).stdout.strip()
        return {
            "exists": True,
            "head": head,
            "tree": tree,
            "remote_url": url,
        }

    def ensure_repository(self, repo_id: str, baseline: Mapping[str, Any]) -> None:
        current = self.repository(repo_id)
        if baseline.get("exists"):
            if current is None:
                raise CoreError("remote_drifted", f"目标仓库已消失：{repo_id}")
            return
        if current is not None:
            revision = self.revision(repo_id)
            if revision["head"] is not None:
                raise CoreError("remote_drifted", f"新目标仓库已被占用：{repo_id}")
            return
        if self.kind == "local":
            path = self._local_path(repo_id)
            path.parent.mkdir(parents=True, exist_ok=True)
            _run(["git", "init", "--bare", str(path)])
        else:
            body = {
                "name": repo_id,
                "description": normalize(baseline.get("description"))[:350],
                "private": False,
                "has_issues": True,
                "has_projects": False,
                "has_wiki": False,
                "auto_init": False,
            }
            _run(
                ["gh", "api", "--method", "POST", f"orgs/{self.organization}/repos", "--input", "-"],
                input_text=json.dumps(body, ensure_ascii=False),
            )
        revision = self.revision(repo_id)
        if not revision["exists"] or revision["head"] is not None:
            raise CoreError("remote_drifted", f"目标仓库创建后不是空仓库：{repo_id}")


def _target_repo_ids(operation: Mapping[str, Any]) -> list[str]:
    unresolved = (
        operation.get("after", {})
        .get("routes", {})
        .get("unresolved_repository_heads", [])
    )
    return sorted({engine.safe_repo_id(item) for item in unresolved})


def _source_repo_ids(operation: Mapping[str, Any]) -> list[str]:
    heads = operation.get("details", {}).get("source_repository_heads", {})
    return sorted(engine.safe_repo_id(item) for item in heads)


def freeze_remote_baseline(
    workspace: Workspace,
    operation: Mapping[str, Any],
    adapter: RemoteAdapter,
) -> dict[str, Any]:
    sources: dict[str, Any] = {}
    expected_heads = operation.get("details", {}).get("source_repository_heads", {})
    for repo_id in _source_repo_ids(operation):
        revision = adapter.revision(repo_id)
        if not revision["exists"] or not revision["head"]:
            raise CoreError("remote_drifted", f"源仓库 main 不存在：{repo_id}")
        if revision["head"] != expected_heads.get(repo_id):
            raise CoreError(
                "remote_drifted",
                f"源仓库 HEAD 偏离库存：{repo_id}: {expected_heads.get(repo_id)} -> {revision['head']}",
            )
        sources[repo_id] = revision
    after_repositories = operation.get("after", {}).get("topology", {}).get("repositories", {})
    targets: dict[str, Any] = {}
    for repo_id in _target_repo_ids(operation):
        revision = adapter.revision(repo_id)
        revision["description"] = normalize(
            after_repositories.get(repo_id, {}).get("display_name") or repo_id
        )
        targets[repo_id] = revision
    registry_repo_id = normalize(
        workspace.manifest.get("sources", {})
        .get("curriculum", {})
        .get("metadata_repo_id")
    ) or DEFAULT_REGISTRY_REPO_ID
    registry: dict[str, Any] = {
        "repo_id": registry_repo_id,
        "manifest_sha256": workspace.identity["manifest_sha256"],
        "source_plan_identity_sha256": workspace.routes.get(
            "source_plan_identity_sha256"
        ),
    }
    if adapter.kind == "github":
        registry["remote"] = adapter.revision(registry_repo_id)
        if not registry["remote"]["exists"] or not registry["remote"]["head"]:
            raise CoreError("remote_drifted", "Registry main 不存在")
    return {
        "kind": adapter.kind,
        "remote_url_template": adapter.template,
        "github_actor": adapter.actor(),
        "registry": registry,
        "sources": sources,
        "targets": targets,
    }


def validate_plan(operation: Mapping[str, Any], workspace: Workspace) -> str:
    metadata = operation.get("core")
    if not isinstance(metadata, Mapping):
        raise CoreError("plan_identity_mismatch", "冻结计划缺少 core 元数据")
    identity = normalize(metadata.get("plan_identity_sha256"))
    if not identity or plan_identity_sha256(operation) != identity:
        raise CoreError("plan_identity_mismatch", "冻结计划内容与 plan identity 不一致")
    if normalize(metadata.get("organization")) != workspace.organization:
        raise CoreError("organization_mismatch", "计划属于另一 organization")
    if not isinstance(metadata.get("workspace_identity"), Mapping):
        raise CoreError("plan_identity_mismatch", "冻结计划缺少 workspace identity")
    if not isinstance(metadata.get("remote_baseline"), Mapping):
        raise CoreError("plan_identity_mismatch", "冻结计划缺少远端基线")
    expected_phrase = f"APPLY {operation.get('operation_id')}"
    if metadata.get("confirmation_phrase") != expected_phrase:
        raise CoreError("plan_identity_mismatch", "冻结计划确认短语无效")
    return identity


def validate_journal(journal: Mapping[str, Any], workspace: Workspace) -> tuple[Mapping[str, Any], str]:
    operation = journal.get("plan")
    if not isinstance(operation, Mapping):
        raise CoreError("journal_invalid", "journal 缺少冻结计划")
    identity = validate_plan(operation, workspace)
    if journal.get("operation_id") != operation.get("operation_id"):
        raise CoreError("journal_invalid", "journal operation_id 与计划不一致")
    if journal.get("kind") != operation.get("kind"):
        raise CoreError("journal_invalid", "journal kind 与计划不一致")
    if journal.get("status") not in {"planned", "applying", "failed", "completed"}:
        raise CoreError("journal_invalid", "journal status 非法")
    resolved = journal.get("resolved_after_routes")
    if resolved is not None:
        if not isinstance(resolved, Mapping):
            raise CoreError("journal_invalid", "journal resolved_after_routes 非法")
        resolved_hash = canonical_sha256(resolved)
        if journal.get("resolved_after_routes_sha256") != resolved_hash:
            raise CoreError("journal_invalid", "journal 已解析 routes identity 不一致")
        try:
            engine.validate_state(operation["after"]["topology"], resolved)
        except Exception as error:
            raise CoreError("journal_invalid", f"journal 最终状态无效：{error}") from error
    _validate_provisioning(journal, operation)
    _validate_git_journal(journal, operation)
    return operation, identity


def _validate_provisioning(journal: Mapping[str, Any], operation: Mapping[str, Any]) -> None:
    provisioning = journal.get("provisioning")
    if provisioning is None:
        return
    baselines = operation["core"]["remote_baseline"]["targets"]
    if not isinstance(provisioning, Mapping) or set(provisioning.get("targets", {})) != set(baselines):
        raise CoreError("journal_invalid", "journal 建仓目标集合不一致")
    if provisioning.get("status") not in {"pending", "completed"}:
        raise CoreError("journal_invalid", "journal 建仓状态非法")
    for repo_id, record in provisioning["targets"].items():
        if not isinstance(record, Mapping) or record.get("status") not in {
            "pending",
            "creating",
            "existing",
            "completed",
        }:
            raise CoreError("journal_invalid", f"journal 建仓目标状态非法：{repo_id}")
        if baselines[repo_id].get("exists") and record.get("status") not in {
            "existing",
            "completed",
        }:
            raise CoreError("journal_invalid", f"既有目标仓库被标记为待创建：{repo_id}")


def _validate_git_journal(journal: Mapping[str, Any], operation: Mapping[str, Any]) -> None:
    git_state = journal.get("git")
    if git_state is None:
        return
    if not isinstance(git_state, Mapping) or git_state.get("status") not in {
        "pending",
        "completed",
    }:
        raise CoreError("journal_invalid", "journal Git 状态非法")
    baseline = operation["core"]["remote_baseline"]
    expected_sources = baseline["sources"]
    expected_targets = baseline["targets"]
    sources = git_state.get("sources")
    targets = git_state.get("targets")
    if not isinstance(sources, Mapping) or set(sources) != set(expected_sources):
        raise CoreError("journal_invalid", "journal Git 源集合不一致")
    if not isinstance(targets, Mapping) or set(targets) != set(expected_targets):
        raise CoreError("journal_invalid", "journal Git 目标集合不一致")
    for repo_id, record in sources.items():
        expected = expected_sources[repo_id]
        if (
            record.get("remote_url") != expected["remote_url"]
            or record.get("expected_head") != expected["head"]
        ):
            raise CoreError("journal_invalid", f"journal Git 源基线被篡改：{repo_id}")
    for repo_id, record in targets.items():
        expected = expected_targets[repo_id]
        if (
            record.get("remote_url") != expected["remote_url"]
            or record.get("expected_head") != expected["head"]
        ):
            raise CoreError("journal_invalid", f"journal Git 目标基线被篡改：{repo_id}")
        status = record.get("status")
        commit = record.get("commit")
        if status not in {"pending", "prepared", "completed"}:
            raise CoreError("journal_invalid", f"journal Git 目标状态非法：{repo_id}")
        if commit is not None and not _SHA1.fullmatch(normalize(commit)):
            raise CoreError("journal_invalid", f"journal Git 目标 commit 非法：{repo_id}")
        if status in {"prepared", "completed"} and not commit:
            raise CoreError("journal_invalid", f"journal Git 目标缺少 commit：{repo_id}")
    if git_state.get("status") == "completed" and any(
        record.get("status") != "completed" for record in targets.values()
    ):
        raise CoreError("journal_invalid", "journal Git 完成状态与目标不一致")

def validate_journal_target_commits(
    journal: Mapping[str, Any], adapter: RemoteAdapter
) -> None:
    git_state = journal.get("git")
    if not isinstance(git_state, Mapping):
        return
    targets = git_state.get("targets", {})
    dynamic_targets = {
        repo_id: record
        for repo_id, record in targets.items()
        if isinstance(record, Mapping) and record.get("commit")
    }
    if not dynamic_targets:
        return
    operation = journal["plan"]
    source_heads = {
        repo_id: record["expected_head"]
        for repo_id, record in git_state["sources"].items()
    }
    moves_by_target = {repo_id: [] for repo_id in targets}
    for move in operation.get("details", {}).get("file_moves", []):
        moves_by_target[move["target_repo_id"]].append(move)
    with TemporaryDirectory(prefix="fireworks-journal-commit-") as temporary:
        bare = Path(temporary)
        _run(["git", "init", "--bare"], cwd=bare)
        fetched: set[tuple[str, str]] = set()
        for repo_id, expected_head in source_heads.items():
            key = (adapter.url(repo_id), expected_head)
            if key in fetched:
                continue
            _run(
                [
                    "git",
                    "fetch",
                    "--no-tags",
                    adapter.url(repo_id),
                    f"{expected_head}:refs/fireworks/source/{repo_id}",
                ],
                cwd=bare,
            )
            fetched.add(key)
        for repo_id, record in dynamic_targets.items():
            expected_parent = record.get("expected_head")
            if expected_parent:
                key = (adapter.url(repo_id), expected_parent)
                if key not in fetched:
                    _run(
                        [
                            "git",
                            "fetch",
                            "--no-tags",
                            adapter.url(repo_id),
                            f"{expected_parent}:refs/fireworks/target/{repo_id}",
                        ],
                        cwd=bare,
                    )
                    fetched.add(key)
            rebuilt = engine._build_target_commit(
                bare,
                operation_id=operation["operation_id"],
                created_at=operation["created_at"],
                target_repo_id=repo_id,
                expected_target_head=expected_parent,
                file_moves=moves_by_target.get(repo_id, []),
                source_heads=source_heads,
            )
            if rebuilt != record.get("commit"):
                raise CoreError(
                    "journal_invalid",
                    f"journal Git 目标 commit 无法由冻结计划重建：{repo_id}",
                )


def workspace_phase(
    workspace: Workspace,
    operation: Mapping[str, Any],
    journal: Mapping[str, Any] | None = None,
) -> str:
    before_identity = operation["core"]["workspace_identity"]
    if workspace.identity["manifest_sha256"] != before_identity["manifest_sha256"]:
        return "drifted"
    before_topology = operation["before"]["topology_sha256"]
    before_routes = operation["before"]["routes_sha256"]
    after_topology = operation["after"]["topology_sha256"]
    if journal and journal.get("resolved_after_routes_sha256"):
        after_routes = journal["resolved_after_routes_sha256"]
    else:
        after_routes = operation["after"]["routes_sha256"]
    current = workspace.identity
    if (
        current["topology_sha256"] == before_topology
        and current["routes_sha256"] == before_routes
    ):
        return "before"
    if (
        current["topology_sha256"] == after_topology
        and current["routes_sha256"] == before_routes
    ):
        return "topology-applied"
    if (
        current["topology_sha256"] == after_topology
        and current["routes_sha256"] == after_routes
    ):
        return "after"
    return "drifted"


def adapter_for_operation(operation: Mapping[str, Any]) -> RemoteAdapter:
    metadata = operation["core"]["remote_baseline"]
    return RemoteAdapter(
        operation["core"]["organization"],
        template=metadata["remote_url_template"],
    )



def _same_revision(current: Mapping[str, Any], expected: Mapping[str, Any]) -> bool:
    return all(
        current.get(key) == expected.get(key)
        for key in ("exists", "head", "tree", "remote_url")
    )
def validate_remote_baseline(
    operation: Mapping[str, Any],
    adapter: RemoteAdapter,
    *,
    journal: Mapping[str, Any] | None = None,
) -> None:
    baseline = operation["core"]["remote_baseline"]
    if adapter.actor() != baseline.get("github_actor"):
        raise CoreError("github_identity_mismatch", "当前 GitHub 登录身份与计划不一致")
    registry = baseline["registry"]
    if adapter.kind == "github":
        current_registry = adapter.revision(registry["repo_id"])
        frozen_registry = registry.get("remote")
        if not frozen_registry or current_registry != frozen_registry:
            raise CoreError("remote_drifted", "Registry baseline 已漂移")
    git_state = (journal or {}).get("git", {})
    target_records = git_state.get("targets", {}) if isinstance(git_state, Mapping) else {}
    known_target_commits = {
        record.get("commit")
        for record in target_records.values()
        if isinstance(record, Mapping) and record.get("commit")
    }
    for repo_id, expected in baseline["sources"].items():
        current = adapter.revision(repo_id)
        if _same_revision(current, expected):
            continue
        if current.get("head") in known_target_commits:
            continue
        raise CoreError("remote_drifted", f"源仓库远端基线已漂移：{repo_id}")
    provisioning = (journal or {}).get("provisioning", {})
    provision_records = (
        provisioning.get("targets", {}) if isinstance(provisioning, Mapping) else {}
    )
    for repo_id, expected in baseline["targets"].items():
        current = adapter.revision(repo_id)
        if _same_revision(current, expected):
            continue
        record = target_records.get(repo_id, {})
        if isinstance(record, Mapping) and record.get("commit") == current.get("head"):
            continue
        provision = provision_records.get(repo_id, {})
        if (
            not expected.get("exists")
            and current.get("exists")
            and current.get("head") is None
            and isinstance(provision, Mapping)
            and provision.get("status") in {"creating", "completed"}
        ):
            continue
        raise CoreError("remote_drifted", f"目标仓库远端基线已漂移：{repo_id}")


def query(workspace: Workspace, kind: str, arguments: Mapping[str, Any]):
    rows = workspace.repository_rows()
    if kind == "inspect":
        types: dict[str, int] = {}
        for row in rows:
            types[row["repo_type"]] = types.get(row["repo_type"], 0) + 1
        return {
            "organization": workspace.organization,
            "health": "healthy" if workspace.state_valid else "invalid",
            "health_message": workspace.state_error,
            "identity": workspace.identity,
            "repository_count": len(rows),
            "repository_types": dict(sorted(types.items())),
            "course_route_count": len(workspace.routes.get("course_code_routes", [])),
            "file_route_count": len(workspace.routes.get("files", [])),
            "inventory_complete_repository_count": len(
                workspace.routes.get("inventory_complete_repositories", [])
            ),
            "virtual_collection_count": len(
                workspace.manifest.get("virtual_collections", [])
            ),
            "special_topic_route_count": len(
                {
                    key
                    for row in workspace.routes.get("files", [])
                    if row.get("route_kind") == "special-topic"
                    for key in row.get("route_keys", [])
                }
            ),
        }
    if kind == "validate":
        if not workspace.state_valid:
            raise CoreError("state_invalid", workspace.state_error or "状态无效")
        return {"valid": True, "identity": workspace.identity}
    if kind == "search":
        term = normalize(arguments.get("term")).casefold()
        repo_type = normalize(arguments.get("repo_type"))
        has_content = arguments.get("has_content")
        limit = min(max(int(arguments.get("limit") or 50), 1), 500)
        matches = []
        for row in rows:
            haystack = " ".join(
                [
                    row["repo_id"],
                    row["display_name"],
                    row["description"],
                    *row["course_codes"],
                    *row["course_names"],
                ]
            ).casefold()
            if term and term not in haystack:
                continue
            if repo_type and row["repo_type"] != repo_type:
                continue
            if has_content is not None and bool(row["file_count"]) != bool(has_content):
                continue
            matches.append(row)
        return {"total": len(matches), "repositories": matches[:limit]}
    if kind == "repository":
        repo_id = engine.safe_repo_id(arguments.get("repo_id", ""))
        row = next((item for item in rows if item["repo_id"] == repo_id), None)
        if not row:
            raise CoreError("repository_not_found", f"仓库不存在：{repo_id}")
        return {
            **row,
            "file_routes": [
                route
                for route in workspace.routes.get("files", [])
                if route["repo_id"] == repo_id
            ],
            "course_routes": [
                route
                for route in workspace.routes.get("course_code_routes", [])
                if route["repo_id"] == repo_id
            ],
            "topology": workspace.topology["repositories"][repo_id],
        }
    if kind == "routes":
        route_kind = normalize(arguments.get("route_kind"))
        repo_id = normalize(arguments.get("repo_id"))
        raw_limit = arguments.get("limit")
        requested_limit = 200 if raw_limit is None else int(raw_limit)
        limit = None if requested_limit == 0 else min(max(requested_limit, 1), 10000)
        files = list(workspace.routes.get("files", []))
        codes = list(workspace.routes.get("course_code_routes", []))
        if route_kind:
            files = [row for row in files if row.get("route_kind") == route_kind]
        if repo_id:
            files = [row for row in files if row.get("repo_id") == repo_id]
            codes = [row for row in codes if row.get("repo_id") == repo_id]
        return {
            "file_total": len(files),
            "course_code_total": len(codes),
            "file_routes": files if limit is None else files[:limit],
            "course_code_routes": codes if limit is None else codes[:limit],
        }
    if kind == "plan":
        operation_id = normalize(arguments.get("operation_id"))
        if operation_id:
            path = workspace.operations_path / f"{operation_id}.plan.json"
            operation = load_json(path)
            identity = validate_plan(operation, workspace)
            return {
                "path": str(path),
                "plan": operation,
                "plan_identity_sha256": identity,
                "state": workspace_phase(workspace, operation),
            }
        plans = []
        for path in sorted(workspace.operations_path.glob("*.plan.json")):
            try:
                operation = load_json(path)
                identity = validate_plan(operation, workspace)
                plans.append(
                    {
                        "path": str(path),
                        "operation_id": operation.get("operation_id"),
                        "kind": operation.get("kind"),
                        "created_at": operation.get("created_at"),
                        "plan_identity_sha256": identity,
                        "confirmation_phrase": operation["core"]["confirmation_phrase"],
                        "state": workspace_phase(workspace, operation),
                        "valid": True,
                    }
                )
            except Exception as error:
                plans.append({"path": str(path), "valid": False, "error": str(error)})
        return {"plans": plans}
    if kind == "journals":
        journals = []
        for path in sorted(workspace.operations_path.glob("*.json")):
            if path.name.endswith(".plan.json"):
                continue
            try:
                journal = load_json(path)
                operation, identity = validate_journal(journal, workspace)
                phase = workspace_phase(workspace, operation, journal)
                status = journal.get("status")
                if phase == "drifted":
                    recovery_state = "drifted"
                elif status == "completed" and phase == "after":
                    recovery_state = "completed"
                elif status in {"planned", "applying", "failed"}:
                    recovery_state = "resumable"
                else:
                    recovery_state = "invalid"
                journals.append(
                    {
                        "path": str(path),
                        "operation_id": journal.get("operation_id"),
                        "kind": journal.get("kind"),
                        "status": status,
                        "recovery_state": recovery_state,
                        "plan_identity_sha256": identity,
                        "confirmation_phrase": f"RESUME {journal.get('operation_id')}",
                        "error": journal.get("error"),
                        "updated_at": journal.get("updated_at"),
                    }
                )
            except Exception as error:
                journals.append(
                    {
                        "path": str(path),
                        "status": "invalid",
                        "recovery_state": "invalid",
                        "error": str(error),
                    }
                )
        return {"journals": journals}
    raise CoreError("unsupported_query", f"不支持的 query.kind：{kind}")


def plan(
    workspace: Workspace,
    kind: str,
    arguments: Mapping[str, Any],
    context: Mapping[str, Any],
):
    if not workspace.state_valid:
        raise CoreError("state_invalid", workspace.state_error or "状态无效")
    if kind == "split":
        operation = engine.plan_split(
            workspace.topology,
            workspace.routes,
            source_repo_id=arguments.get("source_repo_id", ""),
            targets=arguments.get("targets", []),
        )
    elif kind == "merge":
        operation = engine.plan_merge(
            workspace.topology,
            workspace.routes,
            source_repo_ids=arguments.get("source_repo_ids", []),
            target_repo_id=arguments.get("target_repo_id", ""),
            display_name=arguments.get("display_name"),
        )
    else:
        raise CoreError("unsupported_plan", f"不支持的 plan.kind：{kind}")
    template = normalize(arguments.get("remote_url_template")) or engine.DEFAULT_REMOTE_URL_TEMPLATE
    adapter = RemoteAdapter(workspace.organization, template=template)
    operation["core"] = {
        "organization": workspace.organization,
        "workspace_identity": workspace.identity,
        "request_actor": normalize(context.get("actor")),
        "confirmation_phrase": f"APPLY {operation['operation_id']}",
        "remote_baseline": freeze_remote_baseline(workspace, operation, adapter),
    }
    operation["core"]["plan_identity_sha256"] = plan_identity_sha256(operation)
    path = workspace.operations_path / f"{operation['operation_id']}.plan.json"
    atomic_json(path, operation)
    return {
        "path": str(path),
        "plan": operation,
        "risk": {
            "remote_mutation": True,
            "file_move_count": len(operation.get("details", {}).get("file_moves", [])),
            "source_repo_ids": _source_repo_ids(operation),
            "target_repo_ids": _target_repo_ids(operation),
            "new_target_repo_ids": [
                repo_id
                for repo_id, baseline in operation["core"]["remote_baseline"]["targets"].items()
                if not baseline["exists"]
            ],
        },
    }


def execute(
    workspace: Workspace,
    kind: str,
    arguments: Mapping[str, Any],
    confirmation: Any,
):
    if kind not in {"apply", "resume", "verify"}:
        raise CoreError("unsupported_execute", f"不支持的 execute.kind：{kind}")
    if kind == "apply":
        plan_path = workspace.resolve(arguments.get("plan"), Path(""))
        operation = load_json(plan_path)
        identity = validate_plan(operation, workspace)
        if normalize(arguments.get("plan_identity_sha256")) != identity:
            raise CoreError("plan_identity_mismatch", "调用未绑定当前 plan identity")
        if workspace_phase(workspace, operation) != "before":
            raise CoreError("workspace_drifted", "工作区不是冻结计划的 before 状态")
        if confirmation != operation["core"]["confirmation_phrase"]:
            raise CoreError(
                "confirmation_required",
                f"需要确认短语：{operation['core']['confirmation_phrase']}",
            )
        adapter = adapter_for_operation(operation)
        validate_remote_baseline(operation, adapter)
        journal_path = workspace.operations_path / f"{operation['operation_id']}.json"
        if journal_path.exists():
            raise CoreError(
                "journal_exists",
                f"操作 journal 已存在；使用 resume 或 verify：{journal_path}",
            )
        result = engine.apply(
            operation,
            topology_path=workspace.topology_path,
            routes_path=workspace.routes_path,
            journal_path=journal_path,
            remote_url_for=adapter.url,
            ensure_target_repository=adapter.ensure_repository,
        )
        return {
            "journal": result,
            "journal_path": str(journal_path),
            "plan_identity_sha256": identity,
        }
    journal_path = workspace.resolve(arguments.get("journal"), Path(""))
    journal = load_json(journal_path)
    operation, identity = validate_journal(journal, workspace)
    if normalize(arguments.get("plan_identity_sha256")) != identity:
        raise CoreError("plan_identity_mismatch", "调用未绑定 journal 的 plan identity")
    phase = workspace_phase(workspace, operation, journal)
    if phase == "drifted":
        raise CoreError("workspace_drifted", "工作区不属于 journal 的合法阶段状态")
    adapter = adapter_for_operation(operation)
    validate_remote_baseline(operation, adapter, journal=journal)
    validate_journal_target_commits(journal, adapter)
    if kind == "resume":
        expected_phrase = f"RESUME {journal.get('operation_id')}"
        if confirmation != expected_phrase:
            raise CoreError("confirmation_required", f"需要确认短语：{expected_phrase}")
        if journal.get("status") == "completed":
            raise CoreError("journal_not_resumable", "已完成 journal 不得 resume")
        result = engine.resume(
            journal_path,
            topology_path=workspace.topology_path,
            routes_path=workspace.routes_path,
            remote_url_for=adapter.url,
            ensure_target_repository=adapter.ensure_repository,
        )
        return {
            "journal": result,
            "journal_path": str(journal_path),
            "plan_identity_sha256": identity,
        }
    if journal.get("status") != "completed" or phase != "after":
        raise CoreError("verification_incomplete", "只有 completed 且处于最终状态的 journal 可验证")
    engine.validate_state(workspace.topology, workspace.routes)
    if operation.get("kind") in {"split", "merge"}:
        engine._verify_final_remote_heads(journal)
    return {
        "valid": True,
        "operation_id": operation["operation_id"],
        "plan_identity_sha256": identity,
        "workspace_identity": workspace.identity,
    }


def invoke(request: Mapping[str, Any]) -> dict[str, Any]:
    request_id = normalize(request.get("request_id")) or "request"
    try:
        if request.get("schema_version") != SCHEMA_VERSION:
            raise CoreError("schema_incompatible", "仅支持 schema_version=1")
        command = request.get("command")
        context = request.get("context") or {}
        if not isinstance(command, Mapping):
            raise CoreError("invalid_request", "command 必须是对象")
        if not isinstance(context, Mapping):
            raise CoreError("invalid_request", "context 必须是对象")
        root = Path(normalize(context.get("workspace")) or ".")
        arguments = command.get("arguments") or {}
        if not isinstance(arguments, Mapping):
            raise CoreError("invalid_request", "command.arguments 必须是对象")
        workspace = Workspace(root, arguments)
        expected_org = normalize(context.get("organization")) or DEFAULT_ORGANIZATION
        if workspace.organization != expected_org:
            raise CoreError(
                "organization_mismatch",
                f"当前 organization={workspace.organization!r}，期望={expected_org!r}",
            )
        family = normalize(command.get("family"))
        kind = normalize(command.get("kind"))
        if family == "query":
            result = query(workspace, kind, arguments)
            return success(
                request_id, "read_only", result, evidence=[workspace.identity]
            )
        if family == "plan":
            result = plan(workspace, kind, arguments, context)
            return success(
                request_id,
                "planned",
                result,
                evidence=[workspace.identity],
                next_actions=[
                    {
                        "family": "execute",
                        "kind": "apply",
                        "plan_identity_sha256": result["plan"]["core"][
                            "plan_identity_sha256"
                        ],
                        "confirmation_phrase": result["plan"]["core"][
                            "confirmation_phrase"
                        ],
                    }
                ],
            )
        if family == "execute":
            result = execute(
                workspace, kind, arguments, request.get("confirmation")
            )
            mode = result.get("journal", {}).get("status") or "completed"
            return success(request_id, mode, result)
        raise CoreError("unsupported_family", f"不支持的 command.family：{family}")
    except Exception as error:
        return failure(request_id, error)


def build_request_from_cli(args: argparse.Namespace) -> dict[str, Any]:
    family, kind = args.family, args.kind
    arguments: dict[str, Any] = {}
    if family == "query":
        if kind == "search":
            arguments = {
                "term": args.term,
                "repo_type": args.repo_type,
                "limit": args.limit,
            }
        elif kind == "repository":
            arguments = {"repo_id": args.repo_id}
    return {
        "schema_version": SCHEMA_VERSION,
        "request_id": "cli",
        "command": {"family": family, "kind": kind, "arguments": arguments},
        "context": {
            "workspace": str(args.workspace),
            "organization": args.organization,
            "actor": "human",
        },
        "confirmation": None,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--workspace", type=Path, default=Path("."))
    result.add_argument("--organization", default=DEFAULT_ORGANIZATION)
    sub = result.add_subparsers(dest="family", required=True)
    sub.add_parser("invoke", help="从 stdin 读取 CommandEnvelope JSON")
    query_parser = sub.add_parser("query", help="只读查询")
    query_sub = query_parser.add_subparsers(dest="kind", required=True)
    query_sub.add_parser("inspect")
    query_sub.add_parser("validate")
    search = query_sub.add_parser("search")
    search.add_argument("term", nargs="?", default="")
    search.add_argument("--repo-type", default="")
    search.add_argument("--limit", type=int, default=50)
    repository = query_sub.add_parser("repository")
    repository.add_argument("repo_id")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    request = json.load(sys.stdin) if args.family == "invoke" else build_request_from_cli(args)
    response = invoke(request)
    print(json.dumps(response, ensure_ascii=False, indent=2))
    if response["ok"]:
        return 0
    code = response["errors"][0]["code"] if response["errors"] else ""
    return 2 if code in {"invalid_request", "schema_incompatible"} else 3


if __name__ == "__main__":
    raise SystemExit(main())

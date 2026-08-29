#!/usr/bin/env python3
"""薪火课程资料仓库管理状态引擎与命令入口。

拓扑与文件路由是仓库拆分/合并的显式真源。所有操作先生成可审计计划，
再通过 operation journal 应用；中断后可幂等续跑。
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from tempfile import TemporaryDirectory
import tomllib
import unicodedata
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable, Mapping, Sequence

TOPOLOGY_SCHEMA_VERSION = 1
ROUTES_SCHEMA_VERSION = 1
SUPPORTED_TOPOLOGY_SCHEMA_VERSIONS = {TOPOLOGY_SCHEMA_VERSION, 2, 3, 4}
SUPPORTED_ROUTES_SCHEMA_VERSIONS = {ROUTES_SCHEMA_VERSION, 2, 3, 4}
OPERATION_SCHEMA_VERSION = 1
COURSE_DIFF_SCHEMA_VERSION = 1
DECISION_SCHEMA_VERSION = 1
CANONICAL_STATE_SCHEMA_VERSION = 3
CANONICAL_STATE_GENERATION = 3

DEFAULT_BOOTSTRAP_PLAN = Path("data/course-code-atomic-content-migration-plan.v1.json")
DEFAULT_TOPOLOGY = Path("config/repository-topology.v3.json")
DEFAULT_ROUTES = Path("config/repository-file-routes.v3.json")
DEFAULT_OPERATIONS = Path("data/repository-management-operations")
DEFAULT_MANIFEST = Path("data/repository-manifest.json")
DEFAULT_PREPARED_MIGRATION = Path(
    "data/legacy-content-migration-approved-candidates-prepared.v2.json"
)
DEFAULT_APPROVED_MANIFEST = Path("data/repository-manifest.approved-candidates.v2.json")
DEFAULT_MIGRATION_EXECUTION = Path(
    "data/legacy-content-migration-approved-candidates-execution.v2.json"
)
DEFAULT_MIGRATION_VERIFICATION = Path(
    "data/legacy-content-migration-approved-candidates-verification.v2.json"
)
DEFAULT_PROVISION_DRY_RUN = Path(
    "data/repository-provision-approved-candidates-dry-run.v2.json"
)
DEFAULT_PROVISION_EXECUTION = Path(
    "data/repository-provision-approved-candidates-execution.v2.json"
)
DEFAULT_COURSE_FAMILIES = Path("config/course-resource-families.v1.json")
DEFAULT_COURSE_FAMILY_MIGRATION = Path(
    "config/course-resource-family-migration.v1.json"
)
DEFAULT_CANDIDATE_DATA = Path(".workspaces/curriculum-candidate")
DEFAULT_COURSE_DIFF = Path("data/curriculum-course-diff.v1.json")
DEFAULT_COURSE_DECISIONS = Path("config/curriculum-course-decisions.v1.json")
DEFAULT_ADJUDICATED_PLANS = Path(".workspaces/curriculum-adjudicated/plans")
DEFAULT_HIT_BASE_URL = "http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080"
DEFAULT_REMOTE_URL_TEMPLATE = "https://github.com/{organization}/{repo_id}.git"
DEFAULT_CURRICULUM_WRITER_TIMEOUT_SECONDS = 120

COURSE_FIELDS = (
    "course_code",
    "course_name",
    "credit",
    "assessment_method",
    "recommended_year_semester",
    "course_nature",
    "course_category",
    "offering_college",
    "total_hours",
    "hours",
)


class ManagementError(ValueError):
    """仓库管理契约被违反。"""


def now() -> str:
    return datetime.now(UTC).isoformat()


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(
        value, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def normalize(value: Any) -> str:
    return unicodedata.normalize("NFKC", str(value or "")).strip()


def natural_key(value: str) -> tuple[Any, ...]:
    import re

    return tuple(
        int(part) if part.isdigit() else part.casefold()
        for part in re.split(r"(\d+)", value)
    )


def unique(values: Iterable[str]) -> list[str]:
    return sorted(set(values), key=natural_key)


def safe_repo_id(value: str) -> str:
    result = normalize(value)
    if not result or len(result) > 100:
        raise ManagementError(f"非法 repo_id：{value!r}")
    allowed = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.")
    if any(character not in allowed for character in result):
        raise ManagementError(f"repo_id 含非法字符：{value!r}")
    if result in {".", ".."}:
        raise ManagementError(f"非法 repo_id：{value!r}")
    return result


def safe_path(value: str) -> str:
    result = normalize(value).replace("\\", "/")
    path = PurePosixPath(result)
    if (
        not result
        or result.startswith("/")
        or path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
        or path.parts[0] == ".git"
    ):
        raise ManagementError(f"非法仓库相对路径：{value!r}")
    return path.as_posix()


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ManagementError(f"文件不存在：{path}") from error
    except json.JSONDecodeError as error:
        raise ManagementError(f"JSON 无效：{path}: {error}") from error
    if not isinstance(value, dict):
        raise ManagementError(f"JSON 根必须是对象：{path}")
    return value


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()

def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def atomic_bytes(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(handle, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()

def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _atomic_replace_many(replacements: Mapping[Path, bytes]) -> None:
    """预写全部内容；切换中断时按逆序恢复所有原文件。"""
    ordered = [(Path(path), bytes(content)) for path, content in replacements.items()]
    resolved = [path.resolve() for path, _ in ordered]
    if len(resolved) != len(set(resolved)):
        raise ManagementError("多文件切换包含重复目标")

    backups = {
        path: path.with_name(f".{path.name}.promotion-backup") for path, _ in ordered
    }
    ambiguous = [
        path for path, backup in backups.items() if path.exists() and backup.exists()
    ]
    if ambiguous:
        raise ManagementError(
            "检测到目标与 promotion 备份并存，拒绝猜测文件版本："
            + "、".join(str(path) for path in ambiguous)
        )
    recovered = []
    for path, backup in backups.items():
        if backup.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            os.replace(backup, path)
            recovered.append(path)
    if recovered:
        raise ManagementError(
            "已恢复上次中断的 promotion，拒绝在同一次调用继续切换："
            + "、".join(str(path) for path in recovered)
        )
    missing = [path for path, _ in ordered if not path.is_file()]
    if missing:
        raise ManagementError(
            "production promotion 目标文件不存在："
            + "、".join(str(path) for path in missing)
        )

    staged: dict[Path, Path] = {}
    backed_up: list[Path] = []
    try:
        for path, content in ordered:
            handle, temporary_name = tempfile.mkstemp(
                prefix=f".{path.name}.promotion-", suffix=".tmp", dir=path.parent
            )
            temporary = Path(temporary_name)
            staged[path] = temporary
            with os.fdopen(handle, "wb") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())

        try:
            for path, _ in ordered:
                os.replace(path, backups[path])
                backed_up.append(path)
                os.replace(staged[path], path)
        except BaseException as error:
            rollback_errors = []
            for path in reversed(backed_up):
                try:
                    os.replace(backups[path], path)
                except OSError as rollback_error:
                    rollback_errors.append(f"{path}: {rollback_error}")
            if rollback_errors:
                raise ManagementError(
                    "production promotion 失败且回滚不完整："
                    + "；".join(rollback_errors)
                ) from error
            if isinstance(error, (KeyboardInterrupt, SystemExit)):
                raise
            raise ManagementError("production promotion 失败，原文件已全部恢复") from error

        cleanup_errors = []
        for backup in backups.values():
            try:
                backup.unlink()
            except OSError as cleanup_error:
                cleanup_errors.append(f"{backup}: {cleanup_error}")
        if cleanup_errors:
            raise ManagementError(
                "production promotion 已一致切换，但备份清理失败："
                + "；".join(cleanup_errors)
            )
    finally:
        for temporary in staged.values():
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass




def _repository_index(topology: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    repositories = topology.get("repositories")
    if not isinstance(repositories, dict):
        raise ManagementError("topology.repositories 必须是对象")
    return repositories  # type: ignore[return-value]


def validate_topology(topology: Mapping[str, Any]) -> None:
    if topology.get("schema_version") not in SUPPORTED_TOPOLOGY_SCHEMA_VERSIONS:
        raise ManagementError("不支持的 topology schema_version")
    generation = topology.get("generation")
    if not isinstance(generation, int) or generation < 1:
        raise ManagementError("topology.generation 必须是正整数")
    repositories = _repository_index(topology)
    seen_physical: set[str] = set()
    group_owner: dict[str, str] = {}
    for key, repository in repositories.items():
        if not isinstance(repository, dict):
            raise ManagementError(f"仓库记录不是对象：{key}")
        repo_id = safe_repo_id(repository.get("repo_id", ""))
        if key != repo_id:
            raise ManagementError(f"仓库索引键与 repo_id 不一致：{key}")
        physical_id = normalize(repository.get("physical_repository_id"))
        if not physical_id or physical_id in seen_physical:
            raise ManagementError(f"physical_repository_id 缺失或重复：{repo_id}")
        seen_physical.add(physical_id)
        members = repository.get("member_resource_group_ids", [])
        if not isinstance(members, list) or len(members) != len(set(members)):
            raise ManagementError(f"仓库逻辑资源组成员无效：{repo_id}")
        for group_id in members:
            group_id = normalize(group_id)
            if not group_id:
                raise ManagementError(f"仓库含空逻辑资源组：{repo_id}")
            prior = group_owner.setdefault(group_id, repo_id)
            if prior != repo_id:
                raise ManagementError(
                    f"逻辑资源组 {group_id} 同时属于 {prior} 与 {repo_id}"
                )


def validate_routes(
    routes: Mapping[str, Any],
    topology: Mapping[str, Any],
    *,
    allow_unresolved_heads: bool = False,
) -> None:
    if routes.get("schema_version") not in SUPPORTED_ROUTES_SCHEMA_VERSIONS:
        raise ManagementError("不支持的 routes schema_version")
    if routes.get("schema_version") != topology.get("schema_version"):
        raise ManagementError("topology 与 routes schema_version 不一致")
    if routes.get("generation") != topology.get("generation"):
        raise ManagementError("topology 与 routes generation 不一致")
    repositories = _repository_index(topology)
    files = routes.get("files")
    if not isinstance(files, list):
        raise ManagementError("routes.files 必须是数组")
    seen: set[tuple[str, str]] = set()
    group_owner = {
        group_id: repo_id
        for repo_id, repository in repositories.items()
        for group_id in repository.get("member_resource_group_ids", [])
    }
    for record in files:
        if not isinstance(record, dict):
            raise ManagementError("文件路由记录必须是对象")
        repo_id = safe_repo_id(record.get("repo_id", ""))
        path = safe_path(record.get("path", ""))
        if repo_id not in repositories:
            raise ManagementError(f"文件路由引用未知仓库：{repo_id}/{path}")
        identity = (repo_id.casefold(), path.casefold())
        if identity in seen:
            raise ManagementError(f"仓库路径重复（忽略大小写）：{repo_id}/{path}")
        seen.add(identity)
        group_id = normalize(record.get("resource_group_id"))
        if group_id and group_owner.get(group_id) != repo_id:
            raise ManagementError(
                f"文件路由的逻辑资源组不属于目标仓库：{repo_id}/{path}"
            )
    complete = routes.get("inventory_complete_repositories", [])
    if not isinstance(complete, list) or len(complete) != len(set(complete)):
        raise ManagementError("inventory_complete_repositories 无效")
    unknown = set(complete) - set(repositories)
    if unknown:
        raise ManagementError(f"完整清单引用未知仓库：{sorted(unknown)}")
    repository_heads = routes.get("repository_heads", {})
    unresolved_heads = routes.get("unresolved_repository_heads", [])
    if not isinstance(repository_heads, dict) or not isinstance(unresolved_heads, list):
        raise ManagementError("repository_heads 或 unresolved_repository_heads 类型无效")
    if len(unresolved_heads) != len(set(unresolved_heads)):
        raise ManagementError("unresolved_repository_heads 含重复仓库")
    if unresolved_heads and not allow_unresolved_heads:
        raise ManagementError("持久路由不允许未解析的仓库 HEAD")
    if set(repository_heads) & set(unresolved_heads):
        raise ManagementError("仓库 HEAD 不能同时为冻结和未解析状态")
    if set(repository_heads) | set(unresolved_heads) != set(complete):
        raise ManagementError("完整库存仓库必须恰有一个冻结或未解析 HEAD")
    if set(unresolved_heads) - set(repositories):
        raise ManagementError("未解析 HEAD 引用未知仓库")
    for repo_id, head in repository_heads.items():
        if repo_id not in repositories or not isinstance(head, str) or len(head) != 40:
            raise ManagementError(f"非法库存仓库 HEAD：{repo_id}={head!r}")
        try:
            int(head, 16)
        except ValueError as error:
            raise ManagementError(f"非法库存仓库 HEAD：{repo_id}={head!r}") from error


def validate_state(
    topology: Mapping[str, Any],
    routes: Mapping[str, Any],
    *,
    allow_unresolved_heads: bool = False,
) -> None:
    validate_topology(topology)
    validate_routes(
        routes, topology, allow_unresolved_heads=allow_unresolved_heads
    )


def bootstrap_state(
    manifest: Mapping[str, Any], prepared: Mapping[str, Any]
) -> tuple[dict[str, Any], dict[str, Any]]:
    """从当前 manifest 与已核验历史迁移计划建立显式状态。"""
    repositories: dict[str, dict[str, Any]] = {}
    for source in manifest.get("repositories", []):
        if not isinstance(source, dict):
            raise ManagementError("manifest.repositories 含非对象记录")
        repo_id = safe_repo_id(source.get("repo_id", ""))
        physical_id = normalize(source.get("physical_repository_id")) or (
            "physical-repository-" + canonical_sha256({"repo_id": repo_id})[:16]
        )
        repositories[repo_id] = {
            "repo_id": repo_id,
            "repo_type": normalize(source.get("repo_type")) or "collection",
            "display_name": normalize(source.get("display_name")) or repo_id,
            "physical_repository_id": physical_id,
            "member_resource_group_ids": unique(
                source.get("member_resource_group_ids", [])
            ),
            "lineage": {"kind": "bootstrap", "source_repo_ids": [repo_id]},
        }
    topology = {
        "schema_version": CANONICAL_STATE_SCHEMA_VERSION,
        "generation": CANONICAL_STATE_GENERATION,
        "organization": normalize(manifest.get("organization")),
        "source_manifest_sha256": canonical_sha256(manifest),
        "resource_aware_policy_identity_sha256": (
            manifest.get("sources", {})
            .get("physical_repositories", {})
            .get("resource_aware_policy_identity_sha256")
        ),
        "repositories": repositories,
    }
    route_files: list[dict[str, Any]] = []
    for index, source in enumerate(prepared.get("files", [])):
        if not isinstance(source, dict):
            raise ManagementError(f"prepared.files[{index}] 不是对象")
        assignment_status = normalize(source.get("assignment_status")) or "assigned"
        if assignment_status == "reviewed-excluded":
            if source.get("repo_id") is not None or source.get("target_path") is not None:
                raise ManagementError(
                    f"prepared.files[{index}] reviewed-excluded 仍声明目标"
                )
            continue
        if assignment_status != "assigned":
            raise ManagementError(
                f"prepared.files[{index}] assignment_status 非法：{assignment_status!r}"
            )
        repo_id = safe_repo_id(source.get("repo_id", ""))
        if repo_id not in repositories:
            raise ManagementError(f"prepared 文件引用未知仓库：{repo_id}")
        route_files.append(
            {
                "repo_id": repo_id,
                "path": safe_path(source.get("target_path", "")),
                "resource_group_id": normalize(source.get("resource_group_id")) or None,
                "sha256": normalize(source.get("sha256")) or None,
                "size": source.get("size"),
                "origin": source.get("source_uri"),
            }
        )
    route_files.sort(key=lambda item: (natural_key(item["repo_id"]), natural_key(item["path"])))
    routes = {
        "schema_version": CANONICAL_STATE_SCHEMA_VERSION,
        "generation": CANONICAL_STATE_GENERATION,
        "source_plan_identity_sha256": prepared.get("plan_identity_sha256"),
        "inventory_complete_repositories": [],
        "repository_heads": {},
        "files": route_files,
    }
    validate_state(topology, routes)
    return topology, routes

def _unique_repo_ids(value: Any, *, label: str, records: bool) -> set[str]:
    if not isinstance(value, list):
        raise ManagementError(f"{label} 必须是数组")
    result: set[str] = set()
    identities: set[str] = set()
    for index, item in enumerate(value):
        if records:
            if not isinstance(item, dict):
                raise ManagementError(f"{label}[{index}] 不是对象")
            raw_repo_id = item.get("repo_id")
        else:
            raw_repo_id = item
        repo_id = safe_repo_id(raw_repo_id)
        identity = repo_id.casefold()
        if identity in identities:
            raise ManagementError(f"{label} 含重复 repo_id：{repo_id}")
        identities.add(identity)
        result.add(repo_id)
    return result


def validate_promotion_inputs(
    *,
    approved_manifest_path: Path,
    prepared_path: Path,
    migration_execution_path: Path,
    migration_verification_path: Path,
    provision_dry_run_path: Path,
    provision_execution_path: Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """验证 approved manifest、建仓和内容迁移属于同一已完成快照。"""
    approved = load_json(approved_manifest_path)
    prepared = load_json(prepared_path)
    migration_execution = load_json(migration_execution_path)
    migration_verification = load_json(migration_verification_path)
    provision_dry_run = load_json(provision_dry_run_path)
    provision_execution = load_json(provision_execution_path)

    approved_sha256 = file_sha256(approved_manifest_path)
    if approved.get("schema_version") != 2:
        raise ManagementError("approved manifest schema_version 不是 2")
    if (
        approved.get("sources", {}).get("course_families", {}).get("stage_id")
        != "approved-candidates"
        or approved.get("sources", {})
        .get("physical_repositories", {})
        .get("stage_id")
        != "approved-candidates"
    ):
        raise ManagementError("promotion 输入不是 approved-candidates 阶段")
    repositories = approved.get("repositories")
    approved_repo_ids = _unique_repo_ids(
        repositories, label="approved manifest repositories", records=True
    )
    summary = approved.get("summary", {})
    if summary.get("repository_count") != len(approved_repo_ids):
        raise ManagementError("approved manifest 仓库计数不闭合")

    if prepared.get("mode") != "prepared" or prepared.get("assignment_mode") != "production":
        raise ManagementError("迁移计划不是 production prepared")
    if prepared.get("manifest", {}).get("sha256") != approved_sha256:
        raise ManagementError("prepared 计划未绑定 approved manifest 字节 SHA-256")
    prepared_identity = normalize(prepared.get("plan_identity_sha256"))
    try:
        if len(prepared_identity) != 64:
            raise ValueError
        int(prepared_identity, 16)
    except ValueError as error:
        raise ManagementError("prepared 计划 identity 非法") from error
    prepared_repo_ids = _unique_repo_ids(
        prepared.get("repositories"), label="prepared repositories", records=True
    )
    prepared_files = prepared.get("files")
    if not isinstance(prepared_files, list):
        raise ManagementError("prepared files 必须是数组")
    assigned_count = 0
    excluded_count = 0
    for index, record in enumerate(prepared_files):
        if not isinstance(record, dict):
            raise ManagementError(f"prepared files[{index}] 不是对象")
        status = normalize(record.get("assignment_status")) or "assigned"
        if status == "assigned":
            assigned_count += 1
        elif status == "reviewed-excluded":
            excluded_count += 1
        else:
            raise ManagementError(
                f"prepared files[{index}] assignment_status 非法：{status!r}"
            )
    prepared_summary = prepared.get("summary", {})
    if (
        prepared_summary.get("file_count") != len(prepared_files)
        or prepared_summary.get("assigned_file_count") != assigned_count
        or prepared_summary.get("pending_file_count") != 0
        or prepared_summary.get("excluded_file_count") != excluded_count
        or len(prepared_files) != 3857
        or assigned_count != 3830
        or excluded_count != 27
    ):
        raise ManagementError("prepared 迁移文件计数不符合冻结范围")

    if (
        migration_execution.get("plan_identity_sha256") != prepared_identity
        or not migration_execution.get("completed_at")
    ):
        raise ManagementError("迁移 execution 未按 prepared identity 完成")
    execution_repositories = migration_execution.get("repositories")
    if not isinstance(execution_repositories, dict) or not execution_repositories:
        raise ManagementError("迁移 execution 缺少仓库结果")
    execution_repo_ids: set[str] = set()
    execution_identities: set[str] = set()
    for raw_repo_id, record in execution_repositories.items():
        repo_id = safe_repo_id(raw_repo_id)
        identity = repo_id.casefold()
        if identity in execution_identities:
            raise ManagementError(f"迁移 execution 含重复 repo_id：{repo_id}")
        if not isinstance(record, dict):
            raise ManagementError(f"迁移 execution 仓库结果不是对象：{repo_id}")
        if safe_repo_id(record.get("repo_id")) != repo_id:
            raise ManagementError(f"迁移 execution 仓库 key 与记录不一致：{repo_id}")
        if record.get("status") != "completed":
            raise ManagementError(f"迁移 execution 仓库未完成：{repo_id}")
        execution_identities.add(identity)
        execution_repo_ids.add(repo_id)
    if migration_execution.get("summary", {}).get("completed") != len(execution_repo_ids):
        raise ManagementError("迁移 execution 完成仓库计数不闭合")

    verification_summary = migration_verification.get("summary", {})
    verification_records = migration_verification.get("repositories")
    verification_repo_ids = _unique_repo_ids(
        verification_records, label="迁移 verification repositories", records=True
    )
    if isinstance(verification_records, list):
        for record in verification_records:
            if record.get("valid") is not True or record.get("errors") != []:
                raise ManagementError(
                    f"迁移 verification 仓库未通过：{record.get('repo_id')}"
                )
    if (
        migration_verification.get("plan_identity_sha256") != prepared_identity
        or migration_verification.get("scope") != "full"
        or migration_verification.get("valid") is not True
        or migration_verification.get("errors") != []
        or verification_summary.get("repository_count") != len(verification_repo_ids)
        or verification_summary.get("file_count") != assigned_count
        or verification_summary.get("invalid_repository_count") != 0
    ):
        raise ManagementError("迁移 verification 未全量通过")
    if not (
        prepared_repo_ids == execution_repo_ids == verification_repo_ids
    ):
        raise ManagementError("prepared、迁移 execution 与 verification 仓库集合不一致")

    dry_selection = provision_dry_run.get("selection", {})
    dry_selected_repo_ids = _unique_repo_ids(
        dry_selection.get("selected_repo_ids"),
        label="建仓 dry-run selection",
        records=False,
    )
    dry_items = provision_dry_run.get("items")
    dry_item_repo_ids = _unique_repo_ids(
        dry_items, label="建仓 dry-run items", records=True
    )
    if isinstance(dry_items, list):
        for item in dry_items:
            if item.get("action") != "reuse" or item.get("reasons") != []:
                raise ManagementError(f"建仓 dry-run 仓库不可安全复用：{item.get('repo_id')}")
    provision_summary = provision_dry_run.get("summary", {})
    if (
        provision_dry_run.get("manifest_sha256") != approved_sha256
        or dry_selection.get("mode") != "all"
        or provision_summary.get("total") != len(approved_repo_ids)
        or provision_summary.get("create") != 0
        or provision_summary.get("reuse") != len(approved_repo_ids)
        or provision_summary.get("conflict") != 0
        or provision_summary.get("invalid") != 0
        or dry_selected_repo_ids != approved_repo_ids
        or dry_item_repo_ids != approved_repo_ids
    ):
        raise ManagementError("建仓 dry-run 未确认全部 approved 仓库可安全复用")

    provision_verification = provision_execution.get("verification", {})
    execution_selection = provision_verification.get("selection", {})
    provision_repo_ids = _unique_repo_ids(
        execution_selection.get("selected_repo_ids"),
        label="建仓 execution verification selection",
        records=False,
    )
    if (
        provision_execution.get("manifest_sha256") != approved_sha256
        or not provision_execution.get("completed_at")
        or provision_verification.get("scope") != "full"
        or provision_verification.get("valid") is not True
        or execution_selection.get("mode") != "all"
        or provision_verification.get("expected_repository_count")
        != len(approved_repo_ids)
        or provision_verification.get("matched_repository_count")
        != len(approved_repo_ids)
        or provision_verification.get("initialized_repository_count")
        != len(approved_repo_ids)
        or provision_verification.get("errors") != []
        or provision_repo_ids != approved_repo_ids
    ):
        raise ManagementError("建仓 execution 未全量核验 approved 仓库")

    topology, routes = bootstrap_state(approved, prepared)
    if len(topology["repositories"]) != len(approved_repo_ids):
        raise ManagementError("promotion topology 仓库数量不一致")
    if len(routes["files"]) != assigned_count:
        raise ManagementError("promotion routes 未覆盖全部 assigned 文件")
    return topology, routes


def promote_approved_snapshot(
    *,
    approved_manifest_path: Path,
    prepared_path: Path,
    migration_execution_path: Path,
    migration_verification_path: Path,
    provision_dry_run_path: Path,
    provision_execution_path: Path,
    manifest_path: Path,
    topology_path: Path,
    routes_path: Path,
    course_families_path: Path,
    course_family_migration_path: Path,
) -> dict[str, Any]:
    """在所有远端闸门通过后原子切换本地生产 manifest 与状态。"""
    topology, routes = validate_promotion_inputs(
        approved_manifest_path=approved_manifest_path,
        prepared_path=prepared_path,
        migration_execution_path=migration_execution_path,
        migration_verification_path=migration_verification_path,
        provision_dry_run_path=provision_dry_run_path,
        provision_execution_path=provision_execution_path,
    )
    families = load_json(course_families_path)
    family_migration = load_json(course_family_migration_path)
    if families.get("default_stage") != "aggressive-policy":
        raise ManagementError("课程族默认阶段不是预期的 aggressive-policy")
    if family_migration.get("default_stage") != "aggressive-policy":
        raise ManagementError("课程族迁移默认阶段不是预期的 aggressive-policy")
    families["default_stage"] = "approved-candidates"
    family_migration["default_stage"] = "approved-candidates"

    approved_bytes = approved_manifest_path.read_bytes()
    _atomic_replace_many(
        {
            manifest_path: approved_bytes,
            topology_path: _json_bytes(topology),
            routes_path: _json_bytes(routes),
            course_families_path: _json_bytes(families),
            course_family_migration_path: _json_bytes(family_migration),
        }
    )
    return {
        "manifest_sha256": file_sha256(manifest_path),
        "repository_count": len(topology["repositories"]),
        "routed_file_count": len(routes["files"]),
        "prepared_identity": routes.get(
            "source_prepared_plan_identity_sha256",
            routes.get("source_plan_identity_sha256"),
        ),
        "default_stage": "approved-candidates",
    }



def _assert_complete_inventory(routes: Mapping[str, Any], repo_ids: Iterable[str]) -> None:
    complete = set(routes.get("inventory_complete_repositories", []))
    missing = sorted(set(repo_ids) - complete, key=natural_key)
    if missing:
        raise ManagementError(
            "以下仓库尚未建立完整文件路由，禁止拆分/合并：" + ", ".join(missing)
        )


def _target_physical_id(repo_id: str, operation: str) -> str:
    return "physical-managed-" + canonical_sha256(
        {"repo_id": repo_id, "operation": operation}
    )[:16]


def _operation_plan(
    kind: str,
    before_topology: Mapping[str, Any],
    before_routes: Mapping[str, Any],
    after_topology: Mapping[str, Any],
    after_routes: Mapping[str, Any],
    details: Mapping[str, Any],
) -> dict[str, Any]:
    validate_state(before_topology, before_routes)
    validate_state(
        after_topology, after_routes, allow_unresolved_heads=True
    )
    body = {
        "kind": kind,
        "before": {
            "topology_sha256": canonical_sha256(before_topology),
            "routes_sha256": canonical_sha256(before_routes),
        },
        "after": {
            "topology": after_topology,
            "routes": after_routes,
            "topology_sha256": canonical_sha256(after_topology),
            "routes_sha256": canonical_sha256(after_routes),
        },
        "details": dict(details),
    }
    return {
        "schema_version": OPERATION_SCHEMA_VERSION,
        "operation_id": "operation-" + canonical_sha256(body)[:20],
        "created_at": now(),
        **body,
    }


def plan_split(
    topology: Mapping[str, Any],
    routes: Mapping[str, Any],
    *,
    source_repo_id: str,
    targets: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """规划仓库拆分；资源组和无归属文件都必须形成完整互斥分区。"""
    validate_state(topology, routes)
    source_repo_id = safe_repo_id(source_repo_id)
    repositories = _repository_index(topology)
    source = repositories.get(source_repo_id)
    if source is None:
        raise ManagementError(f"待拆分仓库不存在：{source_repo_id}")
    _assert_complete_inventory(routes, [source_repo_id])
    if len(targets) < 2:
        raise ManagementError("拆分至少需要两个目标仓库")

    target_by_id: dict[str, dict[str, Any]] = {}
    group_target: dict[str, str] = {}
    path_target: dict[str, str] = {}
    source_groups = set(source.get("member_resource_group_ids", []))
    for target_source in targets:
        target_id = safe_repo_id(target_source.get("repo_id", ""))
        if target_id in target_by_id:
            raise ManagementError(f"拆分目标仓库重复：{target_id}")
        if target_id in repositories and target_id != source_repo_id:
            raise ManagementError(f"拆分目标仓库已存在：{target_id}")
        groups = unique(target_source.get("resource_group_ids", []))
        paths = unique(safe_path(path) for path in target_source.get("paths", []))
        if not groups and not paths:
            raise ManagementError(f"拆分目标没有成员或文件：{target_id}")
        for group_id in groups:
            if group_id not in source_groups:
                raise ManagementError(f"拆分目标含非源仓库资源组：{group_id}")
            if group_id in group_target:
                raise ManagementError(f"资源组被分配到多个拆分目标：{group_id}")
            group_target[group_id] = target_id
        for path in paths:
            if path in path_target:
                raise ManagementError(f"路径被分配到多个拆分目标：{path}")
            path_target[path] = target_id
        target_by_id[target_id] = {
            "repo_id": target_id,
            "resource_group_ids": groups,
            "paths": paths,
            "display_name": normalize(target_source.get("display_name")) or target_id,
        }
    if set(group_target) != source_groups:
        missing = sorted(source_groups - set(group_target), key=natural_key)
        extra = sorted(set(group_target) - source_groups, key=natural_key)
        raise ManagementError(f"资源组拆分不是完整分区；缺少={missing}，多余={extra}")

    after_routes = copy.deepcopy(routes)
    source_file_paths: set[str] = set()
    routed_counts = {target_id: 0 for target_id in target_by_id}
    file_moves: list[dict[str, str]] = []
    for record in after_routes["files"]:
        if record["repo_id"] != source_repo_id:
            continue
        path = record["path"]
        source_file_paths.add(path)
        group_id = normalize(record.get("resource_group_id"))
        explicit_target = path_target.get(path)
        semantic_target = group_target.get(group_id) if group_id else None
        if explicit_target and semantic_target and explicit_target != semantic_target:
            raise ManagementError(
                f"文件显式路由与资源组目标冲突：{source_repo_id}/{path}"
            )
        target_id = explicit_target or semantic_target
        if not target_id:
            raise ManagementError(
                f"文件没有资源组归属且未显式裁决：{source_repo_id}/{path}"
            )
        record["repo_id"] = target_id
        file_moves.append(
            {
                "source_repo_id": source_repo_id,
                "source_path": path,
                "target_repo_id": target_id,
                "target_path": path,
            }
        )
        routed_counts[target_id] += 1
    unknown_paths = sorted(set(path_target) - source_file_paths, key=natural_key)
    if unknown_paths:
        raise ManagementError(f"显式文件路由含源仓库不存在路径：{unknown_paths}")
    empty = sorted(
        target_id
        for target_id, target in target_by_id.items()
        if not target["resource_group_ids"] and routed_counts[target_id] == 0
    )
    if empty:
        raise ManagementError(f"拆分产生空目标仓库：{empty}")

    after_topology = copy.deepcopy(topology)
    del after_topology["repositories"][source_repo_id]
    for target_id, target in target_by_id.items():
        physical_id = (
            source["physical_repository_id"]
            if target_id == source_repo_id
            else _target_physical_id(target_id, "split")
        )
        after_topology["repositories"][target_id] = {
            "repo_id": target_id,
            "repo_type": source.get("repo_type", "collection"),
            "display_name": target["display_name"],
            "physical_repository_id": physical_id,
            "member_resource_group_ids": target["resource_group_ids"],
            "lineage": {
                "kind": "split",
                "source_repo_ids": [source_repo_id],
                "source_physical_repository_id": source["physical_repository_id"],
            },
        }
    generation = int(topology["generation"]) + 1
    after_topology["generation"] = generation
    after_routes["generation"] = generation
    complete = set(after_routes.get("inventory_complete_repositories", []))
    complete.discard(source_repo_id)
    complete.update(target_by_id)
    repository_heads = after_routes.setdefault("repository_heads", {})
    repository_heads.pop(source_repo_id)
    after_routes["unresolved_repository_heads"] = unique(target_by_id)
    after_routes["inventory_complete_repositories"] = unique(complete)
    after_routes["files"].sort(
        key=lambda item: (natural_key(item["repo_id"]), natural_key(item["path"]))
    )
    return _operation_plan(
        "split",
        topology,
        routes,
        after_topology,
        after_routes,
        {
            "source_repo_id": source_repo_id,
            "targets": list(target_by_id.values()),
            "routed_file_counts": routed_counts,
            "source_repository_heads": {
                source_repo_id: routes["repository_heads"][source_repo_id]
            },
            "file_moves": file_moves,
        },
    )


def plan_merge(
    topology: Mapping[str, Any],
    routes: Mapping[str, Any],
    *,
    source_repo_ids: Sequence[str],
    target_repo_id: str,
    display_name: str | None = None,
) -> dict[str, Any]:
    """规划整仓合并；路径冲突被重定位并记录，不静默覆盖。"""
    validate_state(topology, routes)
    sources = unique(safe_repo_id(repo_id) for repo_id in source_repo_ids)
    if len(sources) < 2:
        raise ManagementError("合并至少需要两个不同源仓库")
    target_repo_id = safe_repo_id(target_repo_id)
    repositories = _repository_index(topology)
    missing = [repo_id for repo_id in sources if repo_id not in repositories]
    if missing:
        raise ManagementError(f"待合并仓库不存在：{missing}")
    if target_repo_id in repositories and target_repo_id not in sources:
        raise ManagementError(f"合并目标仓库已存在且不在源集合：{target_repo_id}")
    _assert_complete_inventory(routes, sources)

    source_records = [repositories[repo_id] for repo_id in sources]
    repo_types = unique(record.get("repo_type", "collection") for record in source_records)
    target_type = repo_types[0] if len(repo_types) == 1 else "collection"
    member_groups = unique(
        group_id
        for record in source_records
        for group_id in record.get("member_resource_group_ids", [])
    )
    after_routes = copy.deepcopy(routes)
    source_route_records = [
        record for record in after_routes["files"] if record["repo_id"] in sources
    ]
    by_path: dict[str, list[dict[str, Any]]] = {}
    for record in source_route_records:
        by_path.setdefault(record["path"].casefold(), []).append(record)
    relocations: list[dict[str, str]] = []
    occupied: set[str] = set()
    file_moves: list[dict[str, str]] = []
    preferred_source = target_repo_id if target_repo_id in sources else sources[0]
    for folded_path in sorted(by_path):
        records = sorted(
            by_path[folded_path],
            key=lambda item: (item["repo_id"] != preferred_source, natural_key(item["repo_id"])),
        )
        for index, record in enumerate(records):
            original_repo = record["repo_id"]
            original_path = record["path"]
            target_path = original_path
            if index > 0 or target_path.casefold() in occupied:
                target_path = safe_path(f"merged-from/{original_repo}/{original_path}")
                suffix = 1
                base = target_path
                while target_path.casefold() in occupied:
                    suffix += 1
                    target_path = safe_path(f"{base}.conflict-{suffix}")
                relocations.append(
                    {
                        "source_repo_id": original_repo,
                        "source_path": original_path,
                        "target_path": target_path,
                    }
                )
            occupied.add(target_path.casefold())
            record["repo_id"] = target_repo_id
            record["path"] = target_path
            file_moves.append(
                {
                    "source_repo_id": original_repo,
                    "source_path": original_path,
                    "target_repo_id": target_repo_id,
                    "target_path": target_path,
                }
            )

    after_topology = copy.deepcopy(topology)
    for source_id in sources:
        del after_topology["repositories"][source_id]
    preserved = repositories.get(target_repo_id)
    physical_id = (
        preserved["physical_repository_id"]
        if preserved
        else _target_physical_id(target_repo_id, "merge")
    )
    after_topology["repositories"][target_repo_id] = {
        "repo_id": target_repo_id,
        "repo_type": target_type,
        "display_name": normalize(display_name) or target_repo_id,
        "physical_repository_id": physical_id,
        "member_resource_group_ids": member_groups,
        "lineage": {
            "kind": "merge",
            "source_repo_ids": sources,
            "source_physical_repository_ids": unique(
                record["physical_repository_id"] for record in source_records
            ),
        },
    }
    generation = int(topology["generation"]) + 1
    after_topology["generation"] = generation
    after_routes["generation"] = generation
    complete = set(after_routes.get("inventory_complete_repositories", []))
    complete.difference_update(sources)
    complete.add(target_repo_id)
    repository_heads = after_routes.setdefault("repository_heads", {})
    for source_id in sources:
        repository_heads.pop(source_id)
    after_routes["unresolved_repository_heads"] = [target_repo_id]
    after_routes["inventory_complete_repositories"] = unique(complete)
    after_routes["files"].sort(
        key=lambda item: (natural_key(item["repo_id"]), natural_key(item["path"]))
    )
    return _operation_plan(
        "merge",
        topology,
        routes,
        after_topology,
        after_routes,
        {
            "source_repo_ids": sources,
            "target_repo_id": target_repo_id,
            "relocations": relocations,
            "file_moves": file_moves,
            "source_repository_heads": {
                repo_id: routes["repository_heads"][repo_id] for repo_id in sources
            },
        },
    )

def format_remote_url(
    template: str, *, organization: str, repo_id: str
) -> str:
    try:
        return template.format(organization=organization, repo_id=repo_id)
    except (KeyError, ValueError) as error:
        raise ManagementError(f"非法 remote URL 模板：{template!r}") from error

def _remote_url_for_state(
    topology: Mapping[str, Any], template: str
) -> Callable[[str], str]:
    organization = normalize(topology.get("organization"))
    if not organization:
        raise ManagementError("topology 缺少 organization")

    def resolve(repo_id: str) -> str:
        return format_remote_url(
            template,
            organization=organization,
            repo_id=safe_repo_id(repo_id),
        )

    return resolve


def _run_git(
    repository: Path,
    *args: str,
    env: Mapping[str, str] | None = None,
    input_bytes: bytes | None = None,
) -> bytes:
    process_env = os.environ.copy()
    if env:
        process_env.update(env)
    process = subprocess.run(
        ["git", *args],
        cwd=repository,
        env=process_env,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode:
        stderr = process.stderr.decode("utf-8", errors="replace")
        stdout = process.stdout.decode("utf-8", errors="replace")
        raise RuntimeError(
            f"git {' '.join(args)} 失败：\nstdout={stdout}\nstderr={stderr}"
        )
    return process.stdout


def remote_head(remote_url: str) -> str | None:
    process = subprocess.run(
        ["git", "ls-remote", remote_url, "refs/heads/main"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode:
        raise RuntimeError(
            f"无法读取远端 main：{remote_url}："
            + process.stderr.decode("utf-8", errors="replace")
        )
    output = process.stdout.decode("ascii", errors="replace").strip()
    if not output:
        return None
    return output.split()[0]


def _fetch_remote_head(
    object_repository: Path, remote_url: str, ref_name: str
) -> str | None:
    head = remote_head(remote_url)
    if head is None:
        return None
    _run_git(
        object_repository,
        "fetch",
        "--no-tags",
        "--force",
        remote_url,
        f"refs/heads/main:{ref_name}",
    )
    fetched = _run_git(object_repository, "rev-parse", ref_name).decode("ascii").strip()
    if fetched != head:
        raise RuntimeError(f"远端 main 在读取期间变化：{remote_url}")
    return head


def _tree_blob(
    object_repository: Path, commit: str, path: str
) -> tuple[str, str]:
    output = _run_git(
        object_repository,
        "ls-tree",
        "-z",
        commit,
        "--",
        path,
    )
    if not output:
        raise ManagementError(f"源提交缺少计划文件：{commit}:{path}")
    record = output.rstrip(b"\0")
    metadata, encoded_path = record.split(b"\t", 1)
    mode, object_type, object_id = metadata.decode("ascii").split()
    actual_path = encoded_path.decode("utf-8")
    if object_type != "blob" or actual_path != path:
        raise ManagementError(f"源路径不是普通 Git blob：{commit}:{path}")
    return mode, object_id


def _build_target_commit(
    object_repository: Path,
    *,
    operation_id: str,
    created_at: str,
    target_repo_id: str,
    expected_target_head: str | None,
    file_moves: Sequence[Mapping[str, str]],
    source_heads: Mapping[str, str],
) -> str:
    index_path = object_repository / f"index-{target_repo_id}"
    index_path.unlink(missing_ok=True)
    index_env = {"GIT_INDEX_FILE": str(index_path.resolve())}
    _run_git(object_repository, "read-tree", "--empty", env=index_env)
    seen_paths: set[str] = set()
    for move in sorted(file_moves, key=lambda item: natural_key(item["target_path"])):
        target_path = safe_path(move["target_path"])
        folded = target_path.casefold()
        if folded in seen_paths:
            raise ManagementError(
                f"Git 目标树含大小写冲突路径：{target_repo_id}/{target_path}"
            )
        seen_paths.add(folded)
        source_repo_id = safe_repo_id(move["source_repo_id"])
        source_head = source_heads.get(source_repo_id)
        if not source_head:
            raise ManagementError(f"缺少源仓库冻结 HEAD：{source_repo_id}")
        mode, object_id = _tree_blob(
            object_repository, source_head, safe_path(move["source_path"])
        )
        _run_git(
            object_repository,
            "update-index",
            "--add",
            "--cacheinfo",
            mode,
            object_id,
            target_path,
            env=index_env,
        )
    tree = _run_git(object_repository, "write-tree", env=index_env).decode("ascii").strip()
    identity_env = {
        "GIT_AUTHOR_NAME": "HIT Fireworks Repository Manager",
        "GIT_AUTHOR_EMAIL": "repository-manager@hit-fireworks.invalid",
        "GIT_COMMITTER_NAME": "HIT Fireworks Repository Manager",
        "GIT_COMMITTER_EMAIL": "repository-manager@hit-fireworks.invalid",
        "GIT_AUTHOR_DATE": created_at,
        "GIT_COMMITTER_DATE": created_at,
    }
    args = ["commit-tree", tree]
    if expected_target_head:
        args.extend(["-p", expected_target_head])
    message = (
        f"chore(repository-management): apply {operation_id} to {target_repo_id}\n"
    ).encode("utf-8")
    return _run_git(
        object_repository, *args, env=identity_env, input_bytes=message
    ).decode("ascii").strip()


def _initialize_git_journal(
    journal: dict[str, Any],
    *,
    remote_url_for: Callable[[str], str],
) -> None:
    if journal.get("git"):
        return
    plan = journal["plan"]
    details = plan.get("details", {})
    source_ids = unique(details.get("source_repository_heads", {}))
    target_ids = unique(
        plan.get("after", {})
        .get("routes", {})
        .get("unresolved_repository_heads", [])
    )
    expected_source_heads = details.get("source_repository_heads", {})
    if set(expected_source_heads) != set(source_ids):
        raise ManagementError("操作计划缺少同步时冻结的源仓库 HEAD")
    sources: dict[str, dict[str, Any]] = {}
    targets: dict[str, dict[str, Any]] = {}
    for repo_id in source_ids:
        remote_url = remote_url_for(repo_id)
        expected_head = expected_source_heads[repo_id]
        actual_head = remote_head(remote_url)
        if actual_head != expected_head:
            raise ManagementError(
                f"源仓库 main 已偏离库存快照：{repo_id}: "
                f"{expected_head} -> {actual_head}"
            )
        sources[repo_id] = {
            "remote_url": remote_url,
            "expected_head": expected_head,
        }
    for repo_id in target_ids:
        remote_url = remote_url_for(repo_id)
        actual_head = remote_head(remote_url)
        if repo_id in expected_source_heads:
            expected_head = expected_source_heads[repo_id]
            if actual_head != expected_head:
                raise ManagementError(
                    f"目标仓库 main 已偏离库存快照：{repo_id}: "
                    f"{expected_head} -> {actual_head}"
                )
        else:
            if actual_head is not None:
                raise ManagementError(
                    f"新目标仓库不是空仓库，拒绝覆盖：{repo_id}={actual_head}"
                )
            expected_head = None
        targets[repo_id] = {
            "remote_url": remote_url,
            "expected_head": expected_head,
            "status": "pending",
            "commit": None,
        }
    journal["git"] = {
        "status": "pending",
        "sources": sources,
        "targets": targets,
    }


def _effective_after_routes(journal: Mapping[str, Any]) -> dict[str, Any]:
    resolved = journal.get("resolved_after_routes")
    if isinstance(resolved, Mapping):
        return copy.deepcopy(dict(resolved))
    return copy.deepcopy(dict(journal["plan"]["after"]["routes"]))


def _resolve_final_routes(journal: dict[str, Any]) -> None:
    targets = journal.get("git", {}).get("targets", {})
    if not targets or any(
        record.get("status") != "completed" or not record.get("commit")
        for record in targets.values()
    ):
        raise ManagementError("目标仓库尚未全部完成，不能解析最终路由 HEAD")
    routes = copy.deepcopy(journal["plan"]["after"]["routes"])
    unresolved = set(routes.get("unresolved_repository_heads", []))
    if unresolved and unresolved != set(targets):
        raise ManagementError("未解析 HEAD 集合与 Git 目标仓库不一致")
    heads = routes.setdefault("repository_heads", {})
    for repo_id, record in targets.items():
        existing = heads.get(repo_id)
        if existing and existing != record["commit"]:
            raise ManagementError(f"最终路由 HEAD 与 journal 冲突：{repo_id}")
        heads[repo_id] = record["commit"]
    routes.pop("unresolved_repository_heads", None)
    validate_state(journal["plan"]["after"]["topology"], routes)
    resolved_sha256 = canonical_sha256(routes)
    existing_resolved = journal.get("resolved_after_routes")
    if existing_resolved is not None and canonical_sha256(existing_resolved) != resolved_sha256:
        raise ManagementError("journal 已解析 routes 与远端目标提交不一致")
    journal["resolved_after_routes"] = routes
    journal["resolved_after_routes_sha256"] = resolved_sha256


def _verify_final_remote_heads(journal: Mapping[str, Any]) -> None:
    routes = _effective_after_routes(journal)
    for repo_id, record in journal.get("git", {}).get("targets", {}).items():
        expected = routes.get("repository_heads", {}).get(repo_id)
        actual = remote_head(record["remote_url"])
        if not expected or actual != expected:
            raise ManagementError(
                f"目标仓库 main 与最终路由 HEAD 不一致：{repo_id}: "
                f"{expected} != {actual}"
            )


def _execute_git_operation(
    journal: dict[str, Any],
    *,
    journal_path: Path,
    remote_url_for: Callable[[str], str],
    stage_hook: Callable[[str], None] | None,
) -> None:
    _initialize_git_journal(journal, remote_url_for=remote_url_for)
    atomic_json(journal_path, journal)
    git_state = journal["git"]
    if git_state.get("status") == "completed":
        _resolve_final_routes(journal)
        _verify_final_remote_heads(journal)
        atomic_json(journal_path, journal)
        return
    moves = journal["plan"]["details"].get("file_moves", [])
    moves_by_target: dict[str, list[dict[str, str]]] = {
        repo_id: [] for repo_id in git_state["targets"]
    }
    for move in moves:
        moves_by_target[move["target_repo_id"]].append(move)
    with TemporaryDirectory(prefix="repository-management-git-") as temporary:
        object_repository = Path(temporary) / "objects.git"
        object_repository.mkdir()
        _run_git(object_repository, "init", "--bare")
        source_heads = {
            repo_id: record["expected_head"]
            for repo_id, record in git_state["sources"].items()
        }
        known_target_commits = {
            record.get("commit")
            for record in git_state["targets"].values()
            if record.get("commit")
        }
        for repo_id, source in git_state["sources"].items():
            current = _fetch_remote_head(
                object_repository,
                source["remote_url"],
                f"refs/repository-management/source/{repo_id}",
            )
            expected = source["expected_head"]
            if current != expected and current not in known_target_commits:
                raise ManagementError(
                    f"源仓库 main 已漂移：{repo_id}: {expected} -> {current}"
                )
            _run_git(object_repository, "cat-file", "-e", f"{expected}^{{commit}}")

        for target_repo_id in sorted(git_state["targets"], key=natural_key):
            target = git_state["targets"][target_repo_id]
            current = _fetch_remote_head(
                object_repository,
                target["remote_url"],
                f"refs/repository-management/target/{target_repo_id}",
            )
            if target.get("status") == "completed":
                if current != target.get("commit"):
                    raise ManagementError(
                        f"已完成目标仓库 main 漂移：{target_repo_id}"
                    )
                continue
            expected_target_head = target.get("expected_head")
            if current != expected_target_head:
                if current == target.get("commit") and current:
                    target["status"] = "completed"
                    atomic_json(journal_path, journal)
                    continue
                raise ManagementError(
                    f"目标仓库 main 已漂移：{target_repo_id}: "
                    f"{expected_target_head} -> {current}"
                )
            commit = _build_target_commit(
                object_repository,
                operation_id=journal["operation_id"],
                created_at=journal["plan"]["created_at"],
                target_repo_id=target_repo_id,
                expected_target_head=expected_target_head,
                file_moves=moves_by_target.get(target_repo_id, []),
                source_heads=source_heads,
            )
            if target.get("commit") and target["commit"] != commit:
                raise ManagementError(f"目标提交重建不一致：{target_repo_id}")
            target["commit"] = commit
            target["status"] = "prepared"
            atomic_json(journal_path, journal)
            _run_git(
                object_repository,
                "push",
                "--porcelain",
                target["remote_url"],
                f"{commit}:refs/heads/main",
            )
            if remote_head(target["remote_url"]) != commit:
                raise RuntimeError(f"目标仓库 push 后 main 未到预期提交：{target_repo_id}")
            target["status"] = "completed"
            target["completed_at"] = now()
            atomic_json(journal_path, journal)
            if stage_hook:
                stage_hook(f"git:{target_repo_id}")
    git_state["status"] = "completed"
    git_state["completed_at"] = now()
    atomic_json(journal_path, journal)
    _resolve_final_routes(journal)
    _verify_final_remote_heads(journal)
    atomic_json(journal_path, journal)


def sync_repository_inventory(
    topology: Mapping[str, Any],
    routes: Mapping[str, Any],
    *,
    repo_ids: Sequence[str],
    remote_url_for: Callable[[str], str],
) -> dict[str, Any]:
    """从远端 main 补齐仓库完整文件清单，新增文件保持未归属待裁决。"""
    validate_state(topology, routes)
    repositories = _repository_index(topology)
    selected = unique(safe_repo_id(repo_id) for repo_id in repo_ids)
    unknown = sorted(set(selected) - set(repositories), key=natural_key)
    if unknown:
        raise ManagementError(f"库存同步含未知仓库：{unknown}")
    result = copy.deepcopy(routes)
    prior = {
        (record["repo_id"], record["path"]): record
        for record in result["files"]
        if record["repo_id"] in selected
    }
    result["files"] = [
        record for record in result["files"] if record["repo_id"] not in selected
    ]
    with TemporaryDirectory(prefix="repository-inventory-") as temporary:
        object_repository = Path(temporary) / "objects.git"
        object_repository.mkdir()
        _run_git(object_repository, "init", "--bare")
        for repo_id in selected:
            head = _fetch_remote_head(
                object_repository,
                remote_url_for(repo_id),
                f"refs/repository-management/inventory/{repo_id}",
            )
            if not head:
                raise ManagementError(f"仓库 main 不存在：{repo_id}")
            result.setdefault("repository_heads", {})[repo_id] = head
            result.pop("unresolved_repository_heads", None)
            raw = _run_git(object_repository, "ls-tree", "-rlz", head)
            for item in raw.split(b"\0"):
                if not item:
                    continue
                metadata, encoded_path = item.split(b"\t", 1)
                mode, object_type, object_id, size = metadata.decode("ascii").split()
                if object_type != "blob":
                    continue
                path = safe_path(encoded_path.decode("utf-8"))
                existing = prior.get((repo_id, path), {})
                result["files"].append(
                    {
                        "repo_id": repo_id,
                        "path": path,
                        "resource_group_id": existing.get("resource_group_id"),
                        "sha256": existing.get("sha256"),
                        "size": int(size),
                        "git_blob_sha1": object_id,
                        "git_mode": mode,
                        "origin": existing.get("origin") or f"git:{head}/{path}",
                    }
                )
    result["inventory_complete_repositories"] = unique(
        [*result.get("inventory_complete_repositories", []), *selected]
    )
    result["files"].sort(
        key=lambda item: (natural_key(item["repo_id"]), natural_key(item["path"]))
    )
    validate_state(topology, result)
    return result


def _state_hash(path: Path) -> str | None:
    return canonical_sha256(load_json(path)) if path.exists() else None


def _journal_for_plan(plan: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": OPERATION_SCHEMA_VERSION,
        "operation_id": plan["operation_id"],
        "kind": plan["kind"],
        "status": "planned",
        "created_at": now(),
        "updated_at": now(),
        "plan": plan,
        "completed_stages": [],
        "error": None,
    }

def _provision_target_repositories(
    journal: dict[str, Any],
    *,
    journal_path: Path,
    ensure_target_repository: Callable[[str, Mapping[str, Any]], None] | None,
) -> None:
    if ensure_target_repository is None:
        return
    targets = (
        journal.get("plan", {})
        .get("core", {})
        .get("remote_baseline", {})
        .get("targets", {})
    )
    if not isinstance(targets, Mapping):
        raise ManagementError("冻结计划缺少目标仓库远端基线")
    provisioning = journal.setdefault(
        "provisioning",
        {
            "status": "pending",
            "targets": {
                repo_id: {
                    "status": "existing" if baseline.get("exists") else "pending",
                    "created": False,
                }
                for repo_id, baseline in targets.items()
            },
        },
    )
    if set(provisioning.get("targets", {})) != set(targets):
        raise ManagementError("journal 建仓目标集合与冻结计划不一致")
    for repo_id, baseline in targets.items():
        record = provisioning["targets"][repo_id]
        if record.get("status") in {"existing", "completed"}:
            continue
        record["status"] = "creating"
        journal["updated_at"] = now()
        atomic_json(journal_path, journal)
        ensure_target_repository(repo_id, baseline)
        record["status"] = "completed"
        record["created"] = True
        record["completed_at"] = now()
        journal["updated_at"] = record["completed_at"]
        atomic_json(journal_path, journal)
    provisioning["status"] = "completed"
    provisioning["completed_at"] = now()
    journal["updated_at"] = provisioning["completed_at"]
    atomic_json(journal_path, journal)


def apply(
    plan: Mapping[str, Any],
    *,
    topology_path: Path = DEFAULT_TOPOLOGY,
    routes_path: Path = DEFAULT_ROUTES,
    journal_path: Path | None = None,
    remote_url_for: Callable[[str], str] | None = None,
    ensure_target_repository: Callable[[str, Mapping[str, Any]], None] | None = None,
    stage_hook: Callable[[str], None] | None = None,
) -> dict[str, Any]:
    """先迁移远端 Git 内容，再原子切换拓扑与路由；每阶段可续跑。"""
    if plan.get("schema_version") != OPERATION_SCHEMA_VERSION:
        raise ManagementError("不支持的 operation schema_version")
    journal_path = journal_path or (
        DEFAULT_OPERATIONS / f"{plan['operation_id']}.json"
    )
    if journal_path.exists():
        journal = load_json(journal_path)
        if journal.get("operation_id") != plan.get("operation_id"):
            raise ManagementError("journal 与计划 operation_id 不一致")
        if canonical_sha256(journal.get("plan")) != canonical_sha256(plan):
            raise ManagementError("journal 内嵌计划与调用计划不一致")
    else:
        journal = _journal_for_plan(plan)
        atomic_json(journal_path, journal)
    return _resume_journal(
        journal,
        journal_path=journal_path,
        topology_path=topology_path,
        routes_path=routes_path,
        remote_url_for=remote_url_for,
        ensure_target_repository=ensure_target_repository,
        stage_hook=stage_hook,
    )


def _resume_journal(
    journal: dict[str, Any],
    *,
    journal_path: Path,
    topology_path: Path,
    routes_path: Path,
    remote_url_for: Callable[[str], str] | None,
    ensure_target_repository: Callable[[str, Mapping[str, Any]], None] | None,
    stage_hook: Callable[[str], None] | None,
) -> dict[str, Any]:
    plan = journal.get("plan", {})
    before = plan.get("before", {})
    if journal.get("git", {}).get("status") == "completed":
        _resolve_final_routes(journal)
        _verify_final_remote_heads(journal)
        atomic_json(journal_path, journal)
    after = plan.get("after", {})
    effective_after_routes = _effective_after_routes(journal)
    effective_after_routes_sha256 = canonical_sha256(effective_after_routes)
    topology_hash = _state_hash(topology_path)
    routes_hash = _state_hash(routes_path)
    allowed_topology = {before.get("topology_sha256"), after.get("topology_sha256")}
    allowed_routes = {
        before.get("routes_sha256"),
        after.get("routes_sha256"),
        journal.get("resolved_after_routes_sha256"),
        effective_after_routes_sha256,
    }
    if topology_hash not in allowed_topology:
        raise ManagementError("当前 topology 既不是计划前状态也不是计划后状态")
    if routes_hash not in allowed_routes:
        raise ManagementError("当前 routes 既不是计划前状态也不是计划后状态")
    journal["status"] = "applying"
    journal["error"] = None
    journal["updated_at"] = now()
    atomic_json(journal_path, journal)
    try:
        _provision_target_repositories(
            journal,
            journal_path=journal_path,
            ensure_target_repository=ensure_target_repository,
        )
        if plan.get("kind") in {"split", "merge"}:
            if remote_url_for is None and journal.get("git", {}).get("status") != "completed":
                raise ManagementError("含文件迁移的操作必须提供 remote_url_for")
            if journal.get("git", {}).get("status") != "completed":
                assert remote_url_for is not None
                _execute_git_operation(
                    journal,
                    journal_path=journal_path,
                    remote_url_for=remote_url_for,
                    stage_hook=stage_hook,
                )
            else:
                _resolve_final_routes(journal)
                _verify_final_remote_heads(journal)
            after = plan["after"]
            effective_after_routes = _effective_after_routes(journal)
            effective_after_routes_sha256 = canonical_sha256(effective_after_routes)
            topology_hash = _state_hash(topology_path)
            routes_hash = _state_hash(routes_path)
            journal["completed_stages"] = unique(
                [*journal.get("completed_stages", []), "git"]
            )
            journal["updated_at"] = now()
            atomic_json(journal_path, journal)
        if topology_hash != after.get("topology_sha256"):
            atomic_json(topology_path, after["topology"])
            journal["completed_stages"] = unique(
                [*journal.get("completed_stages", []), "topology"]
            )
            journal["updated_at"] = now()
            atomic_json(journal_path, journal)
            if stage_hook:
                stage_hook("topology")
        routes_hash = _state_hash(routes_path)
        effective_after_routes = _effective_after_routes(journal)
        effective_after_routes_sha256 = canonical_sha256(effective_after_routes)
        if routes_hash != effective_after_routes_sha256:
            atomic_json(routes_path, effective_after_routes)
            journal["completed_stages"] = unique(
                [*journal.get("completed_stages", []), "routes"]
            )
            journal["updated_at"] = now()
            atomic_json(journal_path, journal)
            if stage_hook:
                stage_hook("routes")
        validate_state(load_json(topology_path), load_json(routes_path))
        if plan.get("kind") in {"split", "merge"}:
            _verify_final_remote_heads(journal)
        journal["status"] = "completed"
        journal["completed_at"] = now()
        journal["updated_at"] = journal["completed_at"]
        journal["error"] = None
        atomic_json(journal_path, journal)
        return journal
    except Exception as error:
        journal["status"] = "failed"
        journal["error"] = str(error)
        journal["updated_at"] = now()
        atomic_json(journal_path, journal)
        raise

def resume(
    journal_path: Path,
    *,
    topology_path: Path = DEFAULT_TOPOLOGY,
    routes_path: Path = DEFAULT_ROUTES,
    remote_url_for: Callable[[str], str] | None = None,
    ensure_target_repository: Callable[[str, Mapping[str, Any]], None] | None = None,
    stage_hook: Callable[[str], None] | None = None,
) -> dict[str, Any]:
    journal = load_json(journal_path)
    if journal.get("status") == "completed":
        if journal.get("git", {}).get("status") == "completed":
            _resolve_final_routes(journal)
            _verify_final_remote_heads(journal)
        return journal
    return _resume_journal(
        journal,
        journal_path=journal_path,
        topology_path=topology_path,
        routes_path=routes_path,
        remote_url_for=remote_url_for,
        ensure_target_repository=ensure_target_repository,
        stage_hook=stage_hook,
    )


def _canonical_course(course: Mapping[str, Any]) -> dict[str, Any]:
    """保留课程原始字段；身份归一化不得改变最终写回内容。"""
    return copy.deepcopy(dict(course))


def _course_identity(course: Mapping[str, Any]) -> dict[str, str]:
    """返回课程的稳定匹配身份；可变教学属性不参与身份。"""
    course_code = normalize(course.get("course_code"))
    if course_code:
        return {"kind": "coded", "course_code": course_code}
    return {
        "kind": "uncoded",
        "course_name": normalize(course.get("course_name")),
    }


def _course_key(plan_id: str, identity: Mapping[str, str]) -> str:
    """返回供人阅读的非唯一课程标签。"""
    if identity["kind"] == "coded":
        return f"course-code:{identity['course_code']}"
    name = identity.get("course_name") or "未命名培养要求"
    return f"uncoded:{plan_id}:{name}"


def _course_occurrences(
    plan_id: str, source_file: str, raw_courses: Sequence[Any]
) -> list[dict[str, Any]]:
    """加载源序列记录；跨快照 occurrence_key 只能在序列对齐后生成。"""
    result: list[dict[str, Any]] = []
    for source_ordinal, raw_course in enumerate(raw_courses):
        if not isinstance(raw_course, dict):
            raise ManagementError(
                f"课程记录不是对象：{source_file}#{source_ordinal}"
            )
        course = _canonical_course(raw_course)
        identity = _course_identity(course)
        result.append(
            {
                "record_key": "course-record-"
                + canonical_sha256(
                    {"plan_id": plan_id, "source_ordinal": source_ordinal}
                )[:24],
                "course_key": _course_key(plan_id, identity),
                "identity": identity,
                "plan_id": plan_id,
                "source_file": source_file,
                "source_ordinal": source_ordinal,
                "course": course,
            }
        )
    return result


def _course_similarity(
    before: Mapping[str, Any],
    after: Mapping[str, Any],
    *,
    before_index: int,
    after_index: int,
    sequence_width: int,
) -> int:
    """为同身份重复项选择字段最相近且位置最接近的单调配对。"""
    before_course = before["course"]
    after_course = after["course"]
    fields = set(before_course) | set(after_course)
    equal_fields = sum(
        before_course.get(field) == after_course.get(field) for field in fields
    )
    position_bonus = max(0, sequence_width - abs(before_index - after_index))
    return equal_fields * (sequence_width + 1) + position_bonus


def _align_course_sequences(
    plan_id: str,
    before_records: Sequence[Mapping[str, Any]],
    after_records: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    """先最大化完整记录匹配，再按稳定身份与相似度对齐重复项。"""
    before_count = len(before_records)
    after_count = len(after_records)
    sequence_width = max(before_count, after_count, 1)
    pair_similarity: dict[tuple[int, int], int] = {}
    for before_index, before in enumerate(before_records):
        for after_index, after in enumerate(after_records):
            if before["identity"] != after["identity"]:
                continue
            pair_similarity[(before_index, after_index)] = _course_similarity(
                before,
                after,
                before_index=before_index,
                after_index=after_index,
                sequence_width=sequence_width,
            )
    match_limit = min(before_count, after_count)
    maximum_similarity = max(pair_similarity.values(), default=0)
    similarity_bound = match_limit * maximum_similarity
    match_weight = similarity_bound + 1
    exact_weight = match_limit * match_weight + similarity_bound + 1
    prior_scores = [0] * (after_count + 1)
    decisions = [bytearray(after_count + 1) for _ in range(before_count + 1)]
    for before_offset in range(1, before_count + 1):
        current_scores = [0] * (after_count + 1)
        current_scores[0] = prior_scores[0]
        decisions[before_offset][0] = 1
        for after_offset in range(1, after_count + 1):
            best = prior_scores[after_offset]
            direction = 1
            if current_scores[after_offset - 1] > best:
                best = current_scores[after_offset - 1]
                direction = 2
            similarity = pair_similarity.get(
                (before_offset - 1, after_offset - 1)
            )
            if similarity is not None:
                exact = (
                    before_records[before_offset - 1]["course"]
                    == after_records[after_offset - 1]["course"]
                )
                pair_score = match_weight + similarity
                if exact:
                    pair_score += exact_weight
                diagonal = prior_scores[after_offset - 1] + pair_score
                if diagonal >= best:
                    best = diagonal
                    direction = 3
            current_scores[after_offset] = best
            decisions[before_offset][after_offset] = direction
        prior_scores = current_scores

    steps: list[tuple[Mapping[str, Any] | None, Mapping[str, Any] | None]] = []
    before_offset = before_count
    after_offset = after_count
    while before_offset or after_offset:
        if before_offset == 0:
            steps.append((None, after_records[after_offset - 1]))
            after_offset -= 1
            continue
        if after_offset == 0:
            steps.append((before_records[before_offset - 1], None))
            before_offset -= 1
            continue
        direction = decisions[before_offset][after_offset]
        if direction == 3:
            steps.append(
                (
                    before_records[before_offset - 1],
                    after_records[after_offset - 1],
                )
            )
            before_offset -= 1
            after_offset -= 1
        elif direction == 1:
            steps.append((before_records[before_offset - 1], None))
            before_offset -= 1
        elif direction == 2:
            steps.append((None, after_records[after_offset - 1]))
            after_offset -= 1
        else:
            raise AssertionError("课程序列对齐回溯无方向")
    steps.reverse()

    counters: dict[str, int] = {}
    result: list[dict[str, Any]] = []
    for before, after in steps:
        representative = after or before
        if representative is None:
            raise AssertionError("空课程序列对齐槽")
        identity = representative["identity"]
        identity_digest = canonical_sha256(identity)
        occurrence_index = counters.get(identity_digest, 0)
        counters[identity_digest] = occurrence_index + 1
        occurrence_key = "course-occurrence-" + canonical_sha256(
            {
                "plan_id": plan_id,
                "identity": identity,
                "occurrence_index": occurrence_index,
            }
        )[:24]

        def annotate(record: Mapping[str, Any] | None) -> dict[str, Any] | None:
            if record is None:
                return None
            annotated = copy.deepcopy(dict(record))
            annotated["occurrence_key"] = occurrence_key
            annotated["occurrence_index"] = occurrence_index
            return annotated

        result.append(
            {
                "occurrence_key": occurrence_key,
                "occurrence_index": occurrence_index,
                "course_key": representative["course_key"],
                "identity": copy.deepcopy(identity),
                "before": annotate(before),
                "after": annotate(after),
            }
        )
    return result


def load_course_snapshot(plan_dir: Path) -> dict[str, Any]:
    """加载完整培养方案；课程原始序列与未知字段均完整保留。"""
    plans: dict[str, dict[str, Any]] = {}
    all_courses: list[dict[str, Any]] = []
    for path in sorted(plan_dir.glob("*.toml"), key=lambda item: natural_key(item.name)):
        try:
            document = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            raise ManagementError(f"培养方案 TOML 无效：{path}: {error}") from error
        info = document.get("info", {})
        if not isinstance(info, dict):
            raise ManagementError(f"培养方案 info 不是对象：{path}")
        plan_id = normalize(info.get("plan_ID"))
        if not plan_id:
            raise ManagementError(f"培养方案缺少 info.plan_ID：{path}")
        if plan_id in plans:
            raise ManagementError(f"培养方案 plan_ID 重复：{plan_id}")
        raw_courses = document.get("courses", [])
        if not isinstance(raw_courses, list):
            raise ManagementError(f"培养方案 courses 不是数组：{path}")
        occurrences = _course_occurrences(plan_id, path.name, raw_courses)
        plans[plan_id] = {
            "plan_id": plan_id,
            "file": path.name,
            "info": copy.deepcopy(info),
            "document": copy.deepcopy(document),
            "courses": occurrences,
        }
        all_courses.extend(occurrences)
    return {"plans": plans, "courses": all_courses}


def _plan_metadata(plan: Mapping[str, Any] | None) -> dict[str, Any] | None:
    """提取方案中除课程序列外的完整内容，供独立差异与裁决使用。"""
    if plan is None:
        return None
    document = plan.get("document")
    if not isinstance(document, dict):
        raise ManagementError(f"方案文档不是对象：{plan.get('plan_id')}")
    return {
        key: copy.deepcopy(value)
        for key, value in document.items()
        if key != "courses"
    }


def diff_courses(current_plan_dir: Path, candidate_plan_dir: Path) -> dict[str, Any]:
    """逐方案比较元数据，并对齐课程序列后生成逐记录差异。"""
    current = load_course_snapshot(current_plan_dir)
    candidate = load_course_snapshot(candidate_plan_dir)
    plan_ids = sorted(
        set(current["plans"]) | set(candidate["plans"]), key=natural_key
    )
    changes: list[dict[str, Any]] = []
    for plan_id in plan_ids:
        current_plan = current["plans"].get(plan_id)
        candidate_plan = candidate["plans"].get(plan_id)
        before_metadata = _plan_metadata(current_plan)
        after_metadata = _plan_metadata(candidate_plan)
        if before_metadata != after_metadata:
            metadata_identity = {
                "plan_id": plan_id,
                "before": before_metadata,
                "after": after_metadata,
            }
            changes.append(
                {
                    "change_id": "plan-metadata-change-"
                    + canonical_sha256(metadata_identity)[:20],
                    "change_type": "plan-metadata",
                    "plan_id": plan_id,
                    "metadata_key": f"plan-metadata:{plan_id}",
                    "kind": "added"
                    if before_metadata is None
                    else "removed"
                    if after_metadata is None
                    else "changed",
                    "before": before_metadata,
                    "after": after_metadata,
                    "before_file": current_plan["file"] if current_plan else None,
                    "after_file": candidate_plan["file"] if candidate_plan else None,
                    "affected_plan_ids": [plan_id],
                }
            )

        before_records = current_plan["courses"] if current_plan else []
        after_records = candidate_plan["courses"] if candidate_plan else []
        for slot in _align_course_sequences(plan_id, before_records, after_records):
            before = slot["before"]
            after = slot["after"]
            if before and after and before["course"] == after["course"]:
                continue
            kind = "added" if before is None else "removed" if after is None else "changed"
            representative = after or before
            if representative is None:
                raise AssertionError(slot["occurrence_key"])
            identity = {
                "occurrence_key": slot["occurrence_key"],
                "before": before["course"] if before else None,
                "after": after["course"] if after else None,
            }
            course = representative["course"]
            changes.append(
                {
                    "change_id": "course-change-"
                    + canonical_sha256(identity)[:20],
                    "change_type": "course-occurrence",
                    "plan_id": plan_id,
                    "occurrence_key": slot["occurrence_key"],
                    "course_key": slot["course_key"],
                    "occurrence_index": slot["occurrence_index"],
                    "course_code": normalize(course.get("course_code")) or None,
                    "course_name": normalize(course.get("course_name")),
                    "kind": kind,
                    "before": before,
                    "after": after,
                    "affected_plan_ids": [plan_id],
                }
            )
    source_identity = {
        "current": canonical_sha256(current),
        "candidate": canonical_sha256(candidate),
    }
    return {
        "schema_version": COURSE_DIFF_SCHEMA_VERSION,
        "generated_at": now(),
        "current_plan_dir": current_plan_dir.as_posix(),
        "candidate_plan_dir": candidate_plan_dir.as_posix(),
        "source_identity": source_identity,
        "diff_identity_sha256": canonical_sha256(
            {"source_identity": source_identity, "changes": changes}
        ),
        "summary": {
            "change_count": len(changes),
            "added": sum(change["kind"] == "added" for change in changes),
            "removed": sum(change["kind"] == "removed" for change in changes),
            "changed": sum(change["kind"] == "changed" for change in changes),
        },
        "changes": changes,
    }


def record_decision(
    diff: Mapping[str, Any],
    decisions: Mapping[str, Any] | None,
    *,
    change_id: str,
    decision: str,
    note: str = "",
) -> dict[str, Any]:
    """记录课程出现或方案元数据的 accept/reject 裁决。"""
    if decision not in {"accept", "reject"}:
        raise ManagementError("差异裁决只能是 accept 或 reject")
    changes = {
        change["change_id"]: change for change in diff.get("changes", [])
    }
    change = changes.get(change_id)
    if change is None:
        raise ManagementError(f"差异不存在：{change_id}")
    if decisions:
        result = copy.deepcopy(decisions)
        if result.get("diff_identity_sha256") != diff.get("diff_identity_sha256"):
            raise ManagementError("裁决文件属于另一份课程差异")
    else:
        result = {
            "schema_version": DECISION_SCHEMA_VERSION,
            "diff_identity_sha256": diff.get("diff_identity_sha256"),
            "decisions": {},
        }
    change_type = change.get("change_type", "course-occurrence")
    result["decisions"][change_id] = {
        "change_id": change_id,
        "change_type": change_type,
        "occurrence_key": change.get("occurrence_key"),
        "course_key": change.get("course_key"),
        "metadata_key": change.get("metadata_key"),
        "decision": decision,
        "note": normalize(note),
        "decided_at": now(),
    }
    total = len(changes)
    accepted = sum(
        record.get("decision") == "accept"
        for record in result["decisions"].values()
    )
    rejected = sum(
        record.get("decision") == "reject"
        for record in result["decisions"].values()
    )
    result["summary"] = {
        "total_changes": total,
        "decided": accepted + rejected,
        "pending": total - accepted - rejected,
        "accepted": accepted,
        "rejected": rejected,
    }
    return result

def _write_curriculum_documents(
    documents: Sequence[Mapping[str, Any]],
    output_plan_dir: Path,
    *,
    hoa_project: Path,
) -> None:
    """通过 hoa-cli 的 TOML 依赖写入完整文档并执行 round-trip 校验。"""
    if not (hoa_project / "pyproject.toml").exists():
        raise ManagementError(f"未找到教务爬虫项目：{hoa_project}")
    writer = Path(__file__).with_name("write-curriculum-toml.py").resolve()
    if not writer.exists():
        raise ManagementError(f"未找到培养方案写入器：{writer}")
    with TemporaryDirectory(prefix="curriculum-materialization-") as temporary:
        payload = Path(temporary) / "documents.json"
        atomic_json(payload, {"documents": list(documents)})
        command = [
            "uv",
            "run",
            "--project",
            str(hoa_project.resolve()),
            "python",
            str(writer),
            "--input",
            str(payload),
            "--output",
            str(output_plan_dir.resolve()),
        ]
        try:
            process = subprocess.run(
                command,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=DEFAULT_CURRICULUM_WRITER_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired as error:
            raise ManagementError(
                "培养方案写入器在 "
                f"{DEFAULT_CURRICULUM_WRITER_TIMEOUT_SECONDS} 秒内未完成"
            ) from error
        except OSError as error:
            raise ManagementError(f"无法启动培养方案写入器：{error}") from error
        if process.returncode:
            raise RuntimeError(
                "培养方案写入失败：\n"
                f"stdout={process.stdout}\n"
                f"stderr={process.stderr}"
            )


def _replace_directory(staging: Path, destination: Path) -> None:
    """切换完整目录；保留并恢复上次中断留下的唯一快照。"""
    backup = destination.parent / f".{destination.name}.backup"
    if backup.exists():
        if destination.exists():
            raise ManagementError(
                f"目标目录与中断备份同时存在，拒绝删除备份：{destination} / {backup}"
            )
        backup.replace(destination)
        raise ManagementError(
            f"发现上次中断留下的备份，已恢复目标目录；请核验后重试：{destination}"
        )
    if destination.exists():
        destination.replace(backup)
    try:
        staging.replace(destination)
    except Exception:
        if backup.exists() and not destination.exists():
            backup.replace(destination)
        raise
    if backup.exists():
        shutil.rmtree(backup)


def materialize_course_decisions(
    diff: Mapping[str, Any],
    decisions: Mapping[str, Any],
    *,
    hoa_project: Path,
    output_plan_dir: Path = DEFAULT_ADJUDICATED_PLANS,
) -> dict[str, Any]:
    """按 occurrence_key 应用每条课程裁决，并保留重复及无代码课程。"""
    if decisions.get("diff_identity_sha256") != diff.get("diff_identity_sha256"):
        raise ManagementError("裁决文件属于另一份课程差异")
    changes = diff.get("changes", [])
    decision_index = decisions.get("decisions", {})
    if not isinstance(changes, list) or not isinstance(decision_index, dict):
        raise ManagementError("课程差异或裁决结构无效")
    change_index: dict[str, Mapping[str, Any]] = {}
    occurrence_changes: dict[str, Mapping[str, Any]] = {}
    metadata_changes: dict[str, Mapping[str, Any]] = {}
    for change in changes:
        if not isinstance(change, dict):
            raise ManagementError("差异记录不是对象")
        change_id = normalize(change.get("change_id"))
        if not change_id or change_id in change_index:
            raise ManagementError(f"差异 change_id 缺失或重复：{change_id!r}")
        change_type = change.get("change_type", "course-occurrence")
        if change_type == "course-occurrence":
            occurrence_key = normalize(change.get("occurrence_key"))
            if not occurrence_key or occurrence_key in occurrence_changes:
                raise ManagementError(
                    f"课程差异 occurrence_key 缺失或重复：{occurrence_key!r}"
                )
            occurrence_changes[occurrence_key] = change
        elif change_type == "plan-metadata":
            plan_id = normalize(change.get("plan_id"))
            metadata_key = normalize(change.get("metadata_key"))
            if not plan_id or metadata_key != f"plan-metadata:{plan_id}":
                raise ManagementError(f"方案元数据差异身份无效：{change_id}")
            if plan_id in metadata_changes:
                raise ManagementError(f"方案元数据差异重复：{plan_id}")
            metadata_changes[plan_id] = change
        else:
            raise ManagementError(f"不支持的差异类型：{change_type!r}")
        change_index[change_id] = change
    unknown = sorted(set(decision_index) - set(change_index), key=natural_key)
    if unknown:
        raise ManagementError(f"裁决文件含未知差异：{unknown}")
    pending = sorted(set(change_index) - set(decision_index), key=natural_key)
    if pending:
        raise ManagementError(f"仍有 {len(pending)} 条差异未裁决")
    for change_id, record in decision_index.items():
        if not isinstance(record, dict) or record.get("decision") not in {
            "accept",
            "reject",
        }:
            raise ManagementError(f"差异裁决无效：{change_id}")
        expected = change_index[change_id]
        expected_type = expected.get("change_type", "course-occurrence")
        if record.get("change_type", expected_type) != expected_type:
            raise ManagementError(f"差异裁决类型不一致：{change_id}")
        if expected_type == "course-occurrence":
            expected_occurrence = expected["occurrence_key"]
            recorded_occurrence = record.get("occurrence_key")
            if recorded_occurrence not in {None, expected_occurrence}:
                raise ManagementError(f"课程裁决 occurrence_key 不一致：{change_id}")
        elif expected_type == "plan-metadata":
            expected_metadata = expected["metadata_key"]
            if record.get("metadata_key") != expected_metadata:
                raise ManagementError(f"方案元数据裁决 metadata_key 不一致：{change_id}")

    current_dir = Path(diff["current_plan_dir"])
    candidate_dir = Path(diff["candidate_plan_dir"])
    current = load_course_snapshot(current_dir)
    candidate = load_course_snapshot(candidate_dir)
    expected_identity = diff.get("source_identity", {})
    if canonical_sha256(current) != expected_identity.get("current"):
        raise ManagementError("当前培养方案自生成差异后已变化")
    if canonical_sha256(candidate) != expected_identity.get("candidate"):
        raise ManagementError("候选培养方案自生成差异后已变化")

    selected_sources: dict[str, str] = {}
    documents: list[dict[str, Any]] = []
    plan_ids = sorted(
        set(current["plans"]) | set(candidate["plans"]), key=natural_key
    )
    for plan_id in plan_ids:
        current_plan = current["plans"].get(plan_id)
        candidate_plan = candidate["plans"].get(plan_id)
        before_records = current_plan["courses"] if current_plan else []
        after_records = candidate_plan["courses"] if candidate_plan else []
        slots = _align_course_sequences(plan_id, before_records, after_records)
        selected_courses: list[dict[str, Any]] = []
        for slot in slots:
            occurrence_key = slot["occurrence_key"]
            before = slot["before"]
            after = slot["after"]
            change = occurrence_changes.get(occurrence_key)
            if change is None:
                if before is None or after is None or before["course"] != after["course"]:
                    raise ManagementError(
                        f"差异缺少课程出现记录：{plan_id}/{occurrence_key}"
                    )
                selected_courses.append(copy.deepcopy(after["course"]))
                continue
            decision = decision_index[change["change_id"]]["decision"]
            chosen = after if decision == "accept" else before
            if chosen is not None:
                selected_courses.append(copy.deepcopy(chosen["course"]))

        metadata_change = metadata_changes.get(plan_id)
        if metadata_change is None:
            before_metadata = _plan_metadata(current_plan)
            after_metadata = _plan_metadata(candidate_plan)
            if before_metadata != after_metadata:
                raise ManagementError(f"差异缺少方案元数据记录：{plan_id}")
            selected_metadata = after_metadata
            metadata_source_plan = current_plan or candidate_plan
        else:
            metadata_decision = decision_index[metadata_change["change_id"]]["decision"]
            selected_metadata = (
                metadata_change["after"]
                if metadata_decision == "accept"
                else metadata_change["before"]
            )
            metadata_source_plan = (
                candidate_plan if metadata_decision == "accept" else current_plan
            )
        if selected_metadata is None:
            if selected_courses:
                raise ManagementError(
                    f"方案元数据裁决删除了方案但仍保留课程记录：{plan_id}"
                )
            continue
        if metadata_source_plan is None:
            raise ManagementError(f"方案元数据裁决缺少来源方案：{plan_id}")
        document = copy.deepcopy(selected_metadata)
        document["courses"] = selected_courses
        file_name = metadata_source_plan["file"]
        documents.append({"file": file_name, "data": document})
        current_document = current_plan and current_plan["document"]
        candidate_document = candidate_plan and candidate_plan["document"]
        if current_document == document:
            selected_sources[plan_id] = str(current_dir / current_plan["file"])
        elif candidate_document == document:
            selected_sources[plan_id] = str(candidate_dir / candidate_plan["file"])
        else:
            selected_sources[plan_id] = "generated:mixed-course-decisions"

    staging = output_plan_dir.parent / f".{output_plan_dir.name}.staging"
    if staging.exists():
        shutil.rmtree(staging)
    staging.parent.mkdir(parents=True, exist_ok=True)
    _write_curriculum_documents(documents, staging, hoa_project=hoa_project)
    _replace_directory(staging, output_plan_dir)
    accepted_course = sum(
        record["decision"] == "accept"
        and change_index[change_id].get("change_type", "course-occurrence")
        == "course-occurrence"
        for change_id, record in decision_index.items()
    )
    rejected_course = sum(
        record["decision"] == "reject"
        and change_index[change_id].get("change_type", "course-occurrence")
        == "course-occurrence"
        for change_id, record in decision_index.items()
    )
    accepted_metadata = sum(
        record["decision"] == "accept"
        and change_index[change_id].get("change_type") == "plan-metadata"
        for change_id, record in decision_index.items()
    )
    rejected_metadata = sum(
        record["decision"] == "reject"
        and change_index[change_id].get("change_type") == "plan-metadata"
        for change_id, record in decision_index.items()
    )
    result = {
        "schema_version": 1,
        "materialized_at": now(),
        "diff_identity_sha256": diff["diff_identity_sha256"],
        "plan_count": len(documents),
        "accepted_course_count": accepted_course,
        "rejected_course_count": rejected_course,
        "accepted_plan_metadata_count": accepted_metadata,
        "rejected_plan_metadata_count": rejected_metadata,
        "output_plan_dir": output_plan_dir.as_posix(),
        "selected_sources": selected_sources,
    }
    atomic_json(output_plan_dir.parent / "materialization.v1.json", result)
    return result


def run_curriculum_crawl(
    *,
    hoa_project: Path,
    candidate_data_dir: Path = DEFAULT_CANDIDATE_DATA,
    base_url: str = DEFAULT_HIT_BASE_URL,
    grades: Sequence[str] | None = None,
    plan_ids: Sequence[str] | None = None,
) -> dict[str, Any]:
    """在同盘临时目录抓取并校验，成功后原子替换候选数据快照。"""
    if not (hoa_project / "pyproject.toml").exists():
        raise ManagementError(f"未找到教务爬虫项目：{hoa_project}")
    candidate_data_dir.parent.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(
        prefix=f".{candidate_data_dir.name}.crawl-",
        dir=candidate_data_dir.parent,
    ) as temporary:
        staging = Path(temporary) / candidate_data_dir.name
        command = [
            "uv",
            "run",
            "--project",
            str(hoa_project.resolve()),
            "hoa",
            "crawl",
            "--campus",
            "hit",
            "--base-url",
            base_url,
            "--data-dir",
            str(staging.resolve()),
        ]
        if grades:
            command.extend(["--grades", *grades])
        for plan_id in plan_ids or []:
            command.extend(["--plan-id", plan_id])
        try:
            process = subprocess.run(
                command,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as error:
            raise ManagementError(f"无法启动教务方案抓取器：{error}") from error
        if process.returncode:
            raise RuntimeError(
                f"教务方案抓取失败，退出码 {process.returncode}\n"
                f"stdout={process.stdout}\n"
                f"stderr={process.stderr}"
            )
        snapshot = load_course_snapshot(staging / "plans")
        if not snapshot["plans"]:
            raise ManagementError("教务方案抓取未生成任何培养方案，拒绝替换候选快照")
        _replace_directory(staging, candidate_data_dir)
    return {
        "candidate_data_dir": candidate_data_dir.as_posix(),
        "plan_count": len(snapshot["plans"]),
        "curriculum_record_count": len(snapshot["courses"]),
        "base_url": base_url,
        "grades": list(grades or []),
        "plan_ids": list(plan_ids or []),
    }


def _parse_target(value: str) -> dict[str, Any]:
    """解析 REPO=GROUP1,GROUP2；无组时用 REPO=-。"""
    repo_id, separator, groups = value.partition("=")
    if not separator:
        raise argparse.ArgumentTypeError("目标格式必须是 REPO=GROUP1,GROUP2")
    return {
        "repo_id": repo_id,
        "resource_group_ids": []
        if groups.strip() in {"", "-"}
        else [item.strip() for item in groups.split(",") if item.strip()],
        "paths": [],
    }


def _print_json(value: Any) -> None:
    print(json.dumps(value, ensure_ascii=False, indent=2))


def _common_state_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--topology", type=Path, default=DEFAULT_TOPOLOGY)
    parser.add_argument("--routes", type=Path, default=DEFAULT_ROUTES)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    bootstrap = subparsers.add_parser("bootstrap", help="从当前 manifest 建立显式状态")
    bootstrap.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    bootstrap.add_argument("--prepared", type=Path, default=DEFAULT_BOOTSTRAP_PLAN)
    _common_state_args(bootstrap)
    promote = subparsers.add_parser(
        "promote-approved",
        help="在远端建仓与内容迁移全量核验后切换 approved 生产快照",
    )
    promote.add_argument(
        "--approved-manifest", type=Path, default=DEFAULT_APPROVED_MANIFEST
    )
    promote.add_argument("--prepared", type=Path, default=DEFAULT_PREPARED_MIGRATION)
    promote.add_argument(
        "--migration-execution", type=Path, default=DEFAULT_MIGRATION_EXECUTION
    )
    promote.add_argument(
        "--migration-verification", type=Path, default=DEFAULT_MIGRATION_VERIFICATION
    )
    promote.add_argument(
        "--provision-dry-run", type=Path, default=DEFAULT_PROVISION_DRY_RUN
    )
    promote.add_argument(
        "--provision-execution", type=Path, default=DEFAULT_PROVISION_EXECUTION
    )
    promote.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    promote.add_argument("--topology", type=Path, default=DEFAULT_TOPOLOGY)
    promote.add_argument("--routes", type=Path, default=DEFAULT_ROUTES)
    promote.add_argument(
        "--course-families", type=Path, default=DEFAULT_COURSE_FAMILIES
    )
    promote.add_argument(
        "--course-family-migration",
        type=Path,
        default=DEFAULT_COURSE_FAMILY_MIGRATION,
    )


    validate = subparsers.add_parser("validate", help="校验显式状态")
    _common_state_args(validate)

    sync = subparsers.add_parser("sync-inventory", help="从远端 main 同步完整文件路由")
    _common_state_args(sync)
    sync.add_argument("--repo", action="append", required=True)
    sync.add_argument(
        "--remote-url-template", default=DEFAULT_REMOTE_URL_TEMPLATE
    )

    split = subparsers.add_parser("plan-split", help="规划仓库拆分")
    _common_state_args(split)
    split.add_argument("--source", required=True)
    split.add_argument("--target", action="append", type=_parse_target, required=True)
    split.add_argument(
        "--path",
        action="append",
        default=[],
        help="无资源组文件裁决，格式 TARGET_REPO=relative/path",
    )
    split.add_argument("--output", type=Path)

    merge = subparsers.add_parser("plan-merge", help="规划仓库合并")
    _common_state_args(merge)
    merge.add_argument("--source", action="append", required=True)
    merge.add_argument("--target", required=True)
    merge.add_argument("--display-name")
    merge.add_argument("--output", type=Path)

    apply_parser = subparsers.add_parser("apply", help="应用拆分/合并计划")
    _common_state_args(apply_parser)
    apply_parser.add_argument("plan", type=Path)
    apply_parser.add_argument("--journal", type=Path)
    apply_parser.add_argument(
        "--remote-url-template", default=DEFAULT_REMOTE_URL_TEMPLATE
    )

    resume_parser = subparsers.add_parser("resume", help="续跑中断操作")
    _common_state_args(resume_parser)
    resume_parser.add_argument("journal", type=Path)
    resume_parser.add_argument(
        "--remote-url-template", default=DEFAULT_REMOTE_URL_TEMPLATE
    )

    crawl = subparsers.add_parser("crawl", help="抓取 HIT 培养方案到候选目录")
    crawl.add_argument("--hoa-project", type=Path, required=True)
    crawl.add_argument("--candidate-data", type=Path, default=DEFAULT_CANDIDATE_DATA)
    crawl.add_argument("--base-url", default=DEFAULT_HIT_BASE_URL)
    crawl.add_argument("--grade", action="append")
    crawl.add_argument("--plan-id", action="append")

    diff = subparsers.add_parser("diff-courses", help="生成逐课程方案差异")
    diff.add_argument("--current", type=Path, required=True)
    diff.add_argument(
        "--candidate", type=Path, default=DEFAULT_CANDIDATE_DATA / "plans"
    )
    diff.add_argument("--output", type=Path, default=DEFAULT_COURSE_DIFF)

    decide = subparsers.add_parser("decide", help="裁决单门课程变化")
    decide.add_argument("change_id")
    decide.add_argument("decision", choices=("accept", "reject"))
    decide.add_argument("--note", default="")
    decide.add_argument("--diff", type=Path, default=DEFAULT_COURSE_DIFF)
    decide.add_argument("--decisions", type=Path, default=DEFAULT_COURSE_DECISIONS)

    materialize = subparsers.add_parser(
        "materialize", help="按逐记录裁决物化培养方案"
    )
    materialize.add_argument("--diff", type=Path, default=DEFAULT_COURSE_DIFF)
    materialize.add_argument(
        "--decisions", type=Path, default=DEFAULT_COURSE_DECISIONS
    )
    materialize.add_argument(
        "--output", type=Path, default=DEFAULT_ADJUDICATED_PLANS
    )
    materialize.add_argument("--hoa-project", type=Path, required=True)

    tui = subparsers.add_parser(
        "tui",
        help="启动 Rust ratatui 本地仓库状态界面",
        description="启动 Rust ratatui repository-tui；仅读取当前仓库的 manifest、topology、routes 与 operations。",
    )
    tui.add_argument("--check", action="store_true", help="非交互输出本地状态摘要")
    return parser

def _initial_course_decisions(diff: Mapping[str, Any]) -> dict[str, Any]:
    total = len(diff.get("changes", []))
    return {
        "schema_version": DECISION_SCHEMA_VERSION,
        "diff_identity_sha256": diff.get("diff_identity_sha256"),
        "decisions": {},
        "summary": {
            "total_changes": total,
            "decided": 0,
            "pending": total,
            "accepted": 0,
            "rejected": 0,
        },
    }


def review_course_changes(
    diff: Mapping[str, Any],
    decisions: Mapping[str, Any] | None,
    *,
    decisions_path: Path,
) -> dict[str, Any]:
    """逐条展示方案元数据或课程记录，并在裁决后立即持久化。"""
    result = copy.deepcopy(decisions) if decisions else _initial_course_decisions(diff)
    if result.get("diff_identity_sha256") != diff.get("diff_identity_sha256"):
        raise ManagementError("裁决文件属于另一份课程差异")
    atomic_json(decisions_path, result)
    changes = diff.get("changes", [])
    for index, change in enumerate(changes, start=1):
        prior = result.get("decisions", {}).get(change["change_id"], {}).get("decision")
        change_type = change.get("change_type", "course-occurrence")
        if change_type == "plan-metadata":
            subject = f"方案元数据 {change['plan_id']}"
        elif change_type == "course-occurrence":
            code = change.get("course_code") or "（无课程代码）"
            occurrence = int(change.get("occurrence_index", 0)) + 1
            subject = (
                f"课程记录 {code} {change.get('course_name', '')} "
                f"出现 #{occurrence}"
            )
        else:
            raise ManagementError(f"不支持的差异类型：{change_type!r}")
        print(
            f"\n[{index}/{len(changes)}] {change['kind']} {subject}；"
            f"当前裁决：{prior or '未裁决'}"
        )
        print("before=" + json.dumps(change.get("before"), ensure_ascii=False, indent=2))
        print("after=" + json.dumps(change.get("after"), ensure_ascii=False, indent=2))
        choice = input("[a]ccept / [r]eject / [s]kip / [q]uit：").strip().casefold()
        if choice == "q":
            break
        if choice in {"", "s"}:
            continue
        if choice not in {"a", "r"}:
            print("无效裁决，本条跳过")
            continue
        result = record_decision(
            diff,
            result,
            change_id=change["change_id"],
            decision="accept" if choice == "a" else "reject",
        )
        atomic_json(decisions_path, result)
    _print_json(result["summary"])
    return result
def _launch_rust_tui(*, check: bool = False) -> int:
    """从当前脚本位置定位 Rust crate；Python 不参与 TUI 运行逻辑。"""
    repo_root = Path(__file__).resolve().parents[1]
    manifest = repo_root / "repository-tui" / "Cargo.toml"
    if not manifest.is_file():
        raise ManagementError(f"Rust TUI Cargo.toml 不存在：{manifest}")
    command = [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        str(manifest),
        "--",
    ]
    if check:
        command.append("--check")
    command.extend(["--root", str(repo_root)])
    try:
        process = subprocess.run(command, cwd=repo_root)
    except OSError as error:
        raise ManagementError(f"无法启动 Rust TUI：{error}") from error
    return process.returncode



def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "bootstrap":
        topology, routes = bootstrap_state(
            load_json(args.manifest), load_json(args.prepared)
        )
        atomic_json(args.topology, topology)
        atomic_json(args.routes, routes)
        _print_json(
            {
                "repository_count": len(topology["repositories"]),
                "routed_file_count": len(routes["files"]),
                "inventory_complete_repository_count": 0,
            }
        )
        return 0
    if args.command == "promote-approved":
        result = promote_approved_snapshot(
            approved_manifest_path=args.approved_manifest,
            prepared_path=args.prepared,
            migration_execution_path=args.migration_execution,
            migration_verification_path=args.migration_verification,
            provision_dry_run_path=args.provision_dry_run,
            provision_execution_path=args.provision_execution,
            manifest_path=args.manifest,
            topology_path=args.topology,
            routes_path=args.routes,
            course_families_path=args.course_families,
            course_family_migration_path=args.course_family_migration,
        )
        _print_json(result)
        return 0
    if args.command == "validate":
        validate_state(load_json(args.topology), load_json(args.routes))
        print("仓库拓扑与文件路由有效")
        return 0
    if args.command == "sync-inventory":
        topology = load_json(args.topology)
        routes = sync_repository_inventory(
            topology,
            load_json(args.routes),
            repo_ids=args.repo,
            remote_url_for=_remote_url_for_state(
                topology, args.remote_url_template
            ),
        )
        atomic_json(args.routes, routes)
        _print_json(
            {
                "repository_count": len(args.repo),
                "routed_file_count": len(routes["files"]),
            }
        )
        return 0
    if args.command == "plan-split":
        targets = args.target
        target_index = {target["repo_id"]: target for target in targets}
        for raw in args.path:
            repo_id, separator, path = raw.partition("=")
            if not separator or repo_id not in target_index:
                raise ManagementError(f"非法 --path 裁决：{raw}")
            target_index[repo_id]["paths"].append(safe_path(path))
        plan = plan_split(
            load_json(args.topology),
            load_json(args.routes),
            source_repo_id=args.source,
            targets=targets,
        )
        output = args.output or DEFAULT_OPERATIONS / f"{plan['operation_id']}.plan.json"
        atomic_json(output, plan)
        print(output)
        return 0
    if args.command == "plan-merge":
        plan = plan_merge(
            load_json(args.topology),
            load_json(args.routes),
            source_repo_ids=args.source,
            target_repo_id=args.target,
            display_name=args.display_name,
        )
        output = args.output or DEFAULT_OPERATIONS / f"{plan['operation_id']}.plan.json"
        atomic_json(output, plan)
        print(output)
        return 0
    if args.command == "apply":
        plan = load_json(args.plan)
        topology = load_json(args.topology)
        journal = apply(
            plan,
            topology_path=args.topology,
            routes_path=args.routes,
            journal_path=args.journal,
            remote_url_for=_remote_url_for_state(
                topology, args.remote_url_template
            ),
        )
        _print_json({"operation_id": journal["operation_id"], "status": journal["status"]})
        return 0
    if args.command == "resume":
        topology = load_json(args.topology)
        journal = resume(
            args.journal,
            topology_path=args.topology,
            routes_path=args.routes,
            remote_url_for=_remote_url_for_state(
                topology, args.remote_url_template
            ),
        )
        _print_json({"operation_id": journal["operation_id"], "status": journal["status"]})
        return 0
    if args.command == "crawl":
        result = run_curriculum_crawl(
            hoa_project=args.hoa_project,
            candidate_data_dir=args.candidate_data,
            base_url=args.base_url,
            grades=args.grade,
            plan_ids=args.plan_id,
        )
        _print_json(result)
        return 0
    if args.command == "diff-courses":
        report = diff_courses(args.current, args.candidate)
        atomic_json(args.output, report)
        _print_json(report["summary"])
        return 0
    if args.command == "decide":
        diff = load_json(args.diff)
        decisions = load_json(args.decisions) if args.decisions.exists() else None
        result = record_decision(
            diff,
            decisions,
            change_id=args.change_id,
            decision=args.decision,
            note=args.note,
        )
        atomic_json(args.decisions, result)
        _print_json(result["summary"])
        return 0
    if args.command == "materialize":
        result = materialize_course_decisions(
            load_json(args.diff),
            load_json(args.decisions),
            output_plan_dir=args.output,
            hoa_project=args.hoa_project,
        )
        _print_json(result)
        return 0
    if args.command == "tui":
        return _launch_rust_tui(check=args.check)
    raise AssertionError(args.command)


if __name__ == "__main__":
    raise SystemExit(main())

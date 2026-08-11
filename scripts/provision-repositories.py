#!/usr/bin/env python3
"""按冻结 manifest 幂等创建并初始化 HIT-Fireworks 全量仓库。"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Iterable

DEFAULT_MANIFEST = Path("data/repository-manifest.json")
DEFAULT_DRY_RUN_REPORT = Path("data/repository-provision-dry-run.v2.json")
DEFAULT_EXECUTION_REPORT = Path("data/repository-provision-execution.v2.json")
DEFAULT_CANARY_REPORT = Path("data/repository-provision-canary.v2.json")
MANAGED_TEMPLATE_TYPES = {"template"}
SEED_REPOSITORY_TYPES = {"control", "template", "requirement-set"}
VALID_REPO_TYPES = {
    "course",
    "requirement-set",
    "collection",
    "shared",
    "competition",
    "software",
    "control",
    "template",
}
REGISTRY_REPO_ID = "fireworks-course-registry-v2"
MANAGEMENT_REPO_ID = "fireworks-repos-management-v2"
COURSE_TEMPLATE_REPO_ID = "fireworks-course-template-v2"
REQUIREMENT_TEMPLATE_REPO_ID = "fireworks-requirement-template-v2"
COLLECTION_TEMPLATE_REPO_ID = "fireworks-collection-template-v2"
SEED_ORDER = (
    REGISTRY_REPO_ID,
    MANAGEMENT_REPO_ID,
    COURSE_TEMPLATE_REPO_ID,
    REQUIREMENT_TEMPLATE_REPO_ID,
    COLLECTION_TEMPLATE_REPO_ID,
)
V2_CONTROL_REPOSITORY_TYPES = {
    REGISTRY_REPO_ID: "control",
    MANAGEMENT_REPO_ID: "control",
    COURSE_TEMPLATE_REPO_ID: "template",
    REQUIREMENT_TEMPLATE_REPO_ID: "template",
    COLLECTION_TEMPLATE_REPO_ID: "template",
}
COMPLETED_STATUSES = {"created", "reused"}
FROZEN_LEGACY_REPOSITORIES = {
    "fireworks-course-registry": {
        "commit": "29491c2d8a19e80293e3b07a0399667a46929e39",
        "tree": "66b8eaac5b59d74b9beb82da6b9c0e058851a89e",
        "is_template": False,
    },
    "fireworks-repos-management": {
        "commit": "1b1fc9309f1944e7e4da502b97f6ba4321e3f676",
        "tree": "c4450d08e4f5fe78b5afeae82a3cf28d690fd5a6",
        "is_template": False,
    },
    "fireworks-course-template": {
        "commit": "73ac884969d1cf365e2995608ab6cde91bf56c9e",
        "tree": "825ec918ace3f4ef3f32740a41258c9320c7e753",
        "is_template": True,
    },
    "22AD11001": {
        "commit": "828b3b586aad439de72a93a2288201ec827990ba",
        "tree": "825ec918ace3f4ef3f32740a41258c9320c7e753",
        "is_template": False,
    },
}


class ApiError(RuntimeError):
    def __init__(self, status: int, message: str, body: Any = None) -> None:
        super().__init__(f"GitHub API {status}: {message}")
        self.status = status
        self.body = body


class GitHubClient:
    def __init__(self, token: str, mutation_interval: float) -> None:
        self.token = token
        self.mutation_interval = mutation_interval
        self.last_mutation = 0.0

    def _wait_for_mutation_slot(self) -> None:
        delay = self.mutation_interval - (time.monotonic() - self.last_mutation)
        if delay > 0:
            time.sleep(delay)
        self.last_mutation = time.monotonic()

    def request(
        self,
        method: str,
        path: str,
        payload: dict[str, Any] | None = None,
        *,
        mutation: bool = False,
        max_attempts: int = 20,
    ) -> tuple[Any, dict[str, str]]:
        if mutation:
            self._wait_for_mutation_slot()
        url = path if path.startswith("https://") else f"https://api.github.com{path}"
        data = None
        headers = {
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {self.token}",
            "User-Agent": "HIT-Fireworks-repository-provisioner",
            "X-GitHub-Api-Version": "2022-11-28",
        }
        if payload is not None:
            data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
            headers["Content-Type"] = "application/json"

        for attempt in range(1, max_attempts + 1):
            request = urllib.request.Request(url, data=data, headers=headers, method=method)
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    raw = response.read()
                    body = json.loads(raw) if raw else None
                    return body, {key.lower(): value for key, value in response.headers.items()}
            except urllib.error.HTTPError as error:
                raw = error.read().decode("utf-8", errors="replace")
                try:
                    body = json.loads(raw) if raw else None
                except json.JSONDecodeError:
                    body = raw
                response_headers = {
                    key.lower(): value for key, value in error.headers.items()
                }
                message = (
                    body.get("message", raw)
                    if isinstance(body, dict)
                    else str(body or error.reason)
                )
                retryable = (
                    error.code in {429, 502, 503, 504}
                    or (
                        error.code == 403
                        and (
                            "secondary rate limit" in message.lower()
                            or response_headers.get("x-ratelimit-remaining") == "0"
                            or "retry-after" in response_headers
                        )
                    )
                )
                if not retryable or attempt == max_attempts:
                    raise ApiError(error.code, message, body) from error

                retry_after = response_headers.get("retry-after")
                reset = response_headers.get("x-ratelimit-reset")
                if retry_after:
                    wait_seconds = max(float(retry_after), 1.0)
                elif reset and response_headers.get("x-ratelimit-remaining") == "0":
                    wait_seconds = max(float(reset) - time.time() + 3.0, 3.0)
                elif "secondary rate limit" in message.lower():
                    wait_seconds = min(60.0 * attempt, 900.0)
                else:
                    wait_seconds = min(2.0**attempt, 120.0)
                print(
                    f"GitHub API 暂时拒绝请求（{error.code}，第 {attempt} 次），"
                    f"{wait_seconds:.1f} 秒后重试：{message}",
                    flush=True,
                )
                time.sleep(wait_seconds)
            except urllib.error.URLError as error:
                if attempt == max_attempts:
                    raise RuntimeError(f"GitHub API 网络失败：{error}") from error
                time.sleep(min(2.0**attempt, 60.0))
        raise AssertionError("unreachable")

    def graphql(
        self, query: str, variables: dict[str, Any], *, mutation: bool = False
    ) -> dict[str, Any]:
        body, _ = self.request(
            "POST",
            "/graphql",
            {"query": query, "variables": variables},
            mutation=mutation,
        )
        if not isinstance(body, dict):
            raise RuntimeError("GitHub GraphQL 返回非对象")
        return body


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--apply", action="store_true")
    mode.add_argument("--verify", action="store_true")
    mode.add_argument("--canary-test", action="store_true")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--dry-run-report", type=Path, default=DEFAULT_DRY_RUN_REPORT)
    parser.add_argument("--execution-report", type=Path, default=DEFAULT_EXECUTION_REPORT)
    parser.add_argument("--canary-report", type=Path, default=DEFAULT_CANARY_REPORT)
    parser.add_argument("--organization")
    parser.add_argument("--batch-size", type=int, default=10)
    parser.add_argument("--mutation-interval", type=float, default=2.0)
    parser.add_argument(
        "--repo-id",
        dest="repo_ids",
        action="append",
        default=[],
        help="仅处理指定仓库；可重复传入",
    )
    parser.add_argument(
        "--max-repositories",
        type=int,
        help="本次最多处理的未完成仓库数",
    )
    parser.add_argument(
        "--all-repositories",
        action="store_true",
        help="显式允许处理全部未完成仓库",
    )
    return parser.parse_args()


def now() -> str:
    return datetime.now(UTC).isoformat()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    os.replace(temporary, path)


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def assert_physical_repository_contract(
    manifest: dict[str, Any],
    repositories: list[dict[str, Any]],
    resource_groups: list[dict[str, Any]],
) -> None:
    policy = manifest.get("policy", {})
    if (
        policy.get("physical_repository_cardinality")
        != "many-resource-groups-to-one-repository"
        or policy.get("physical_repository_grouping")
        != "versioned-exclusive-canonical-owner-policy-only"
        or policy.get("course_groups_are_navigation_evidence_only") is not True
    ):
        raise RuntimeError("manifest 缺少互斥物理仓库策略契约")

    family_source = manifest.get("sources", {}).get("course_families", {})
    physical_source = manifest.get("sources", {}).get("physical_repositories", {})
    if (
        physical_source.get("schema_version") != 1
        or physical_source.get("role") != "exclusive-physical-repository-policy"
        or physical_source.get("stage_id") != family_source.get("stage_id")
        or physical_source.get("course_groups_are_navigation_evidence_only") is not True
        or physical_source.get("canonical_owner_algorithm")
        != "longest-common-prefix-over-curriculum-records"
        or not isinstance(physical_source.get("hierarchy_fields"), list)
        or not physical_source.get("hierarchy_fields")
        or not isinstance(physical_source.get("shared_frontier_depth"), int)
    ):
        raise RuntimeError("manifest 物理仓库来源契约缺失或与课程族阶段不一致")

    group_ids = [group.get("resource_group_id") for group in resource_groups]
    if None in group_ids or len(group_ids) != len(set(group_ids)):
        raise RuntimeError("manifest resource_group_id 缺失或不唯一")
    group_index = {
        group["resource_group_id"]: group for group in resource_groups
    }
    physical_repositories = [
        repository
        for repository in repositories
        if repository.get("repo_type") == "course"
        and repository.get("physical_repository_id")
    ]
    if any(
        repository.get("physical_repository_id")
        and repository.get("repo_type") != "course"
        for repository in repositories
    ):
        raise RuntimeError("非 course 仓库不得携带 physical_repository_id")
    physical_ids = [
        repository["physical_repository_id"] for repository in physical_repositories
    ]
    if len(physical_ids) != len(set(physical_ids)):
        raise RuntimeError("manifest physical_repository_id 不唯一")

    physical_count = len(physical_repositories)
    group_count = len(resource_groups)
    summary = manifest.get("summary", {})
    if (
        physical_source.get("resource_group_count") != group_count
        or physical_source.get("physical_course_repository_count") != physical_count
        or summary.get("resource_group_count") != group_count
        or summary.get("physical_course_repository_count") != physical_count
        or summary.get("physical_repository_reduction")
        != group_count - physical_count
        or summary.get("repository_count") != len(repositories)
    ):
        raise RuntimeError("manifest 物理仓库数量与减量契约不一致")

    represented_group_ids: list[str] = []
    physical_by_id: dict[str, dict[str, Any]] = {}
    for repository in physical_repositories:
        physical_id = repository["physical_repository_id"]
        physical_by_id[physical_id] = repository
        if "resource_group_id" in repository:
            raise RuntimeError(
                f"物理课程仓库不得携带单值 resource_group_id：{repository['repo_id']}"
            )
        member_ids = repository.get("member_resource_group_ids")
        if (
            not isinstance(member_ids, list)
            or not member_ids
            or len(member_ids) != len(set(member_ids))
            or any(group_id not in group_index for group_id in member_ids)
        ):
            raise RuntimeError(f"物理课程仓库成员无效：{repository['repo_id']}")
        represented_group_ids.extend(member_ids)

        if repository.get("materialization_kind") not in {
            "dedicated-resource-group",
            "hierarchy-node",
        } or not isinstance(repository.get("canonical_owner"), dict):
            raise RuntimeError(f"物理课程仓库物化元数据无效：{repository['repo_id']}")
        expected_codes = {
            code
            for group_id in member_ids
            for code in group_index[group_id].get("course_codes", [])
        }
        actual_codes = repository.get("course_codes")
        if (
            not isinstance(actual_codes, list)
            or len(actual_codes) != len(set(actual_codes))
            or set(actual_codes) != expected_codes
        ):
            raise RuntimeError(f"物理课程仓库课程代码并集错误：{repository['repo_id']}")
        expected_preferred_ids = {
            group_index[group_id].get("preferred_repo_id") for group_id in member_ids
        }
        actual_preferred_ids = repository.get("preferred_source_repo_ids")
        if (
            None in expected_preferred_ids
            or not isinstance(actual_preferred_ids, list)
            or len(actual_preferred_ids) != len(set(actual_preferred_ids))
            or set(actual_preferred_ids) != expected_preferred_ids
        ):
            raise RuntimeError(f"物理课程仓库首选来源集合错误：{repository['repo_id']}")
        expected_lineage = {
            course_group_id
            for group_id in member_ids
            for course_group_id in group_index[group_id]
            .get("split_lineage", {})
            .get("course_group_ids", [])
        }
        actual_lineage = repository.get("split_lineage_course_group_ids")
        if (
            not isinstance(actual_lineage, list)
            or len(actual_lineage) != len(set(actual_lineage))
            or set(actual_lineage) != expected_lineage
        ):
            raise RuntimeError(f"物理课程仓库拆分血缘错误：{repository['repo_id']}")

    if (
        len(represented_group_ids) != len(set(represented_group_ids))
        or set(represented_group_ids) != set(group_ids)
    ):
        raise RuntimeError("物理课程仓库未将每个 ResourceGroup 恰好覆盖一次")

    for group in resource_groups:
        group_id = group["resource_group_id"]
        physical_id = group.get("physical_repository_id")
        repository = physical_by_id.get(physical_id)
        lineage = group.get("split_lineage", {})
        if (
            not repository
            or repository.get("repo_id") != group.get("repo_id")
            or group_id not in repository["member_resource_group_ids"]
        ):
            raise RuntimeError(f"ResourceGroup 物理仓库绑定错误：{group_id}")
        if (
            lineage.get("semantics") != "navigation-evidence-only"
            or not isinstance(lineage.get("course_group_ids"), list)
            or len(lineage["course_group_ids"])
            != len(set(lineage["course_group_ids"]))
            or not isinstance(group.get("canonical_owner"), dict)
        ):
            raise RuntimeError(f"ResourceGroup 拆分血缘或 owner 无效：{group_id}")

    for section_name in ("course_descriptors", "curriculum_records", "legacy_units"):
        for item in manifest.get(section_name, []):
            group_id = item.get("resource_group_id")
            if group_id is None:
                if item.get("physical_repository_id") is not None:
                    raise RuntimeError(f"{section_name} 无逻辑分组却绑定物理仓库")
                continue
            group = group_index.get(group_id)
            if (
                not group
                or item.get("physical_repository_id")
                != group.get("physical_repository_id")
                or item.get("repo_id") != group.get("repo_id")
            ):
                raise RuntimeError(f"{section_name} 物理仓库绑定错误：{group_id}")


def assert_v2_manifest_contract(manifest: dict[str, Any]) -> None:
    if manifest.get("schema_version") != 2:
        raise RuntimeError(
            f"建仓器只接受 manifest schema v2，当前为 {manifest.get('schema_version')}"
        )
    authority = manifest.get("policy", {}).get("authority")
    if authority != REGISTRY_REPO_ID:
        raise RuntimeError(
            f"manifest 权威注册表不是 {REGISTRY_REPO_ID}：{authority!r}"
        )
    repositories = manifest.get("repositories", [])
    repo_ids = {repository.get("repo_id") for repository in repositories}
    template_ids = {
        repository.get("template_id")
        for repository in repositories
        if repository.get("template_id") is not None
    }
    forbidden = sorted(
        (repo_ids | template_ids) & set(FROZEN_LEGACY_REPOSITORIES)
    )
    if forbidden:
        raise RuntimeError(
            f"manifest v2 禁止复用冻结旧世代仓库 ID：{forbidden}"
        )
    actual_control_types = {
        repository.get("repo_id"): repository.get("repo_type")
        for repository in repositories
        if repository.get("repo_type") in {"control", "template"}
    }
    if actual_control_types != V2_CONTROL_REPOSITORY_TYPES:
        raise RuntimeError(
            "manifest v2 控制面仓库不完整或类型错误："
            f"{actual_control_types!r}"
        )
    family_source = manifest.get("sources", {}).get("course_families", {})
    if family_source.get("schema_version") != 1:
        raise RuntimeError("manifest 缺少 schema v1 课程族来源契约")
    if family_source.get("stage_id") not in {
        "approved-candidates",
        "aggressive-policy",
    }:
        raise RuntimeError("manifest 课程族阶段未知")
    base_count = family_source.get("base_resource_group_count")
    result_count = family_source.get("result_resource_group_count")
    reduction = family_source.get("course_repository_reduction")
    if (
        not isinstance(base_count, int)
        or not isinstance(result_count, int)
        or not isinstance(reduction, int)
        or base_count - result_count != reduction
        or manifest.get("summary", {}).get("base_resource_group_count")
        != base_count
        or manifest.get("summary", {}).get("resource_group_count")
        != result_count
        or manifest.get("summary", {}).get("course_repository_reduction")
        != reduction
    ):
        raise RuntimeError("manifest 课程族数量与减量契约不一致")
    resource_groups = manifest.get("resource_groups", [])
    if len(resource_groups) != result_count:
        raise RuntimeError("manifest 课程族结果数量与 resource_groups 不一致")
    family_group_count = sum(
        group.get("grouping_rule") == "materialized-course-family"
        for group in resource_groups
    )
    if (
        family_source.get("family_group_count") != family_group_count
        or manifest.get("summary", {}).get("course_family_group_count")
        != family_group_count
    ):
        raise RuntimeError("manifest 课程族分组数量不一致")
    assert_physical_repository_contract(manifest, repositories, resource_groups)


def gh_token() -> str:
    result = subprocess.run(
        ["gh", "auth", "token"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
        check=True,
    )
    token = result.stdout.strip()
    if not token:
        raise RuntimeError("gh auth token 返回空 token")
    return token


def list_org_repositories(client: GitHubClient, organization: str) -> list[dict[str, Any]]:
    repositories: list[dict[str, Any]] = []
    page = 1
    while True:
        body, _ = client.request(
            "GET",
            f"/orgs/{urllib.parse.quote(organization)}/repos?per_page=100&page={page}&type=all",
        )
        if not isinstance(body, list):
            raise RuntimeError("组织仓库列表返回非数组")
        repositories.extend(body)
        if len(body) < 100:
            return repositories
        page += 1


def get_identity(client: GitHubClient, organization: str) -> dict[str, Any]:
    user, _ = client.request("GET", "/user")
    login = user["login"]
    membership, _ = client.request(
        "GET",
        f"/orgs/{urllib.parse.quote(organization)}/memberships/{urllib.parse.quote(login)}",
    )
    organization_record, _ = client.request(
        "GET", f"/orgs/{urllib.parse.quote(organization)}"
    )
    return {
        "login": login,
        "membership_state": membership.get("state"),
        "membership_role": membership.get("role"),
        "organization_node_id": organization_record.get("node_id"),
    }

def load_execution_for_selection(
    path: Path, manifest_path: Path
) -> dict[str, Any]:
    if not path.exists():
        return {"repositories": {}}
    execution = load_json(path)
    if execution.get("manifest_sha256") != sha256(manifest_path):
        raise RuntimeError("execution report 属于另一份 manifest")
    if not isinstance(execution.get("repositories"), dict):
        raise RuntimeError("execution report repositories 不是对象")
    return execution


def select_repositories(
    manifest: dict[str, Any],
    execution: dict[str, Any],
    *,
    repo_ids: list[str],
    max_repositories: int | None,
    all_repositories: bool,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if all_repositories and (repo_ids or max_repositories is not None):
        raise ValueError("--all-repositories 不能与 --repo-id 或 --max-repositories 同时使用")
    if not all_repositories and not repo_ids and max_repositories is None:
        raise ValueError(
            "必须明确指定 --repo-id、--max-repositories 或 --all-repositories"
        )
    if max_repositories is not None and max_repositories < 1:
        raise ValueError("--max-repositories 必须大于 0")

    repositories = manifest.get("repositories", [])
    repository_ids = [repository.get("repo_id", "") for repository in repositories]
    repository_set = set(repository_ids)
    requested_ids = list(repo_ids)
    requested_folded = [repo_id.casefold() for repo_id in requested_ids]
    if len(requested_folded) != len(set(requested_folded)):
        raise ValueError("--repo-id 不能重复，且大小写视为相同")
    unknown = [repo_id for repo_id in requested_ids if repo_id not in repository_set]
    if unknown:
        raise ValueError(f"--repo-id 不在 manifest：{unknown}")

    requested_set = set(requested_ids)
    candidates = (
        repositories
        if all_repositories or not requested_ids
        else [
            repository
            for repository in repositories
            if repository["repo_id"] in requested_set
        ]
    )
    execution_records = execution.get("repositories", {})
    completed_ids = {
        repo_id
        for repo_id, record in execution_records.items()
        if record.get("status") in COMPLETED_STATUSES
        and record.get("initialized_commit")
    }
    if max_repositories is not None:
        selected = [
            repository
            for repository in candidates
            if repository["repo_id"] not in completed_ids
        ][:max_repositories]
    else:
        selected = list(candidates)

    selected_ids = [repository["repo_id"] for repository in selected]
    selection = {
        "mode": (
            "all"
            if all_repositories
            else "allowlist-limit"
            if requested_ids and max_repositories is not None
            else "allowlist"
            if requested_ids
            else "limit"
        ),
        "requested_repo_ids": requested_ids,
        "max_repositories": max_repositories,
        "all_repositories": all_repositories,
        "manifest_repository_count": len(repositories),
        "candidate_repository_count": len(candidates),
        "selected_repository_count": len(selected_ids),
        "selected_repo_ids": selected_ids,
        "already_completed_repo_ids": [
            repo_id for repo_id in selected_ids if repo_id in completed_ids
        ],
        "pending_repo_ids": [
            repo_id for repo_id in selected_ids if repo_id not in completed_ids
        ],
    }
    return selected, selection


def selection_request(selection: dict[str, Any]) -> dict[str, Any]:
    return {
        key: selection.get(key)
        for key in (
            "mode",
            "requested_repo_ids",
            "max_repositories",
            "all_repositories",
            "manifest_repository_count",
            "candidate_repository_count",
        )
    }


def selection_from_args(
    manifest: dict[str, Any],
    manifest_path: Path,
    execution_path: Path,
    args: argparse.Namespace,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    execution = load_execution_for_selection(execution_path, manifest_path)
    return select_repositories(
        manifest,
        execution,
        repo_ids=args.repo_ids,
        max_repositories=args.max_repositories,
        all_repositories=args.all_repositories,
    )


def classify(
    manifest: dict[str, Any],
    remote_repositories: list[dict[str, Any]],
    repositories: list[dict[str, Any]] | None = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    expected = (
        repositories
        if repositories is not None
        else manifest.get("repositories", [])
    )
    expected_templates = {
        repo["repo_id"]
        for repo in manifest.get("repositories", [])
        if repo.get("repo_type") in MANAGED_TEMPLATE_TYPES
    }
    remote_by_fold: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for repository in remote_repositories:
        remote_by_fold[repository["name"].casefold()].append(repository)

    name_counts = Counter(
        repo.get("repo_id", "").casefold()
        for repo in manifest.get("repositories", [])
    )
    selected_repo_ids = {repo.get("repo_id") for repo in expected}
    items: list[dict[str, Any]] = []
    for index, repository in enumerate(expected):
        repo_id = repository.get("repo_id", "")
        repo_type = repository.get("repo_type")
        template_id = repository.get("template_id")
        reasons: list[str] = []
        action = "create"

        if not repo_id or len(repo_id) > 100:
            reasons.append("repo_id 为空或超过 100 字符")
        if not all(character.isalnum() or character in "._-" for character in repo_id):
            reasons.append("repo_id 含 GitHub 不允许的字符")
        if repo_type not in VALID_REPO_TYPES:
            reasons.append(f"未知 repo_type：{repo_type}")
        if name_counts[repo_id.casefold()] > 1:
            reasons.append("manifest 中 repo_id 大小写不唯一")
        if repo_type not in SEED_REPOSITORY_TYPES:
            if not template_id:
                reasons.append("需要初始化模板但 template_id 为空")
            elif template_id not in expected_templates:
                reasons.append(f"template_id 不存在或不是模板：{template_id}")
            elif template_id not in selected_repo_ids:
                template_matches = remote_by_fold.get(template_id.casefold(), [])
                if len(template_matches) != 1:
                    reasons.append(
                        f"未选择且远端不可唯一复用初始化模板：{template_id}"
                    )
                else:
                    template_metadata = template_matches[0]
                    if (
                        template_metadata.get("name") != template_id
                        or template_metadata.get("visibility") != "public"
                        or template_metadata.get("archived")
                        or not template_metadata.get("is_template")
                    ):
                        reasons.append(f"远端初始化模板状态冲突：{template_id}")
        elif repo_type in {"control", "template"} and template_id is not None:
            reasons.append("控制面或模板仓库不应再引用模板")

        matches = remote_by_fold.get(repo_id.casefold(), [])
        existing = None
        if reasons:
            action = "invalid"
        elif len(matches) > 1:
            action = "conflict"
            reasons.append("组织内存在大小写冲突的多个仓库")
        elif len(matches) == 1:
            existing = matches[0]
            if existing["name"] != repo_id:
                action = "conflict"
                reasons.append(f"已有仓库仅大小写不同：{existing['name']}")
            elif existing.get("visibility") != "public" or existing.get("archived"):
                action = "conflict"
                reasons.append("已有仓库不是未归档公开仓库")
            elif repo_type == "template" and not existing.get("is_template"):
                action = "conflict"
                reasons.append("已有模板仓库未标记为 template")
            elif repo_type != "template" and existing.get("is_template"):
                action = "conflict"
                reasons.append("已有普通仓库被标记为 template")
            else:
                action = "reuse"

        items.append(
            {
                "index": index,
                "repo_id": repo_id,
                "repo_type": repo_type,
                "template_id": template_id,
                "action": action,
                "reasons": reasons,
                "existing": (
                    {
                        "name": existing.get("name"),
                        "url": existing.get("html_url"),
                        "node_id": existing.get("node_id"),
                        "default_branch": existing.get("default_branch"),
                        "is_template": existing.get("is_template"),
                    }
                    if existing
                    else None
                ),
            }
        )

    expected_names = {
        repo["repo_id"].casefold()
        for repo in manifest.get("repositories", [])
    }
    unrelated = [
        {
            "name": repository["name"],
            "url": repository["html_url"],
            "visibility": repository.get("visibility"),
        }
        for repository in remote_repositories
        if repository["name"].casefold() not in expected_names
    ]
    return items, unrelated


def build_dry_run(
    *,
    manifest_path: Path,
    manifest: dict[str, Any],
    identity: dict[str, Any],
    remote_repositories: list[dict[str, Any]],
    selected_repositories: list[dict[str, Any]],
    selection: dict[str, Any],
    frozen_legacy_repositories: dict[str, Any],
) -> dict[str, Any]:
    items, unrelated = classify(manifest, remote_repositories, selected_repositories)
    summary = Counter(item["action"] for item in items)
    return {
        "schema_version": 2,
        "generated_at": now(),
        "manifest_path": manifest_path.as_posix(),
        "manifest_sha256": sha256(manifest_path),
        "organization": manifest["organization"],
        "identity": identity,
        "selection": selection,
        "summary": {
            "total": len(items),
            "create": summary["create"],
            "reuse": summary["reuse"],
            "conflict": summary["conflict"],
            "invalid": summary["invalid"],
        },
        "items": items,
        "unrelated_existing_repositories": unrelated,
        "frozen_legacy_repositories": frozen_legacy_repositories,
    }


def validate_frozen_legacy_snapshot(snapshot: dict[str, Any]) -> None:
    repositories = snapshot.get("repositories", [])
    if not isinstance(repositories, list):
        raise RuntimeError("dry-run 冻结旧世代仓库快照不是数组")
    by_id = {repository.get("repo_id"): repository for repository in repositories}
    if len(by_id) != len(repositories) or set(by_id) != set(FROZEN_LEGACY_REPOSITORIES):
        raise RuntimeError("dry-run 未包含完整且唯一的冻结旧世代仓库快照")
    errors = snapshot.get("errors")
    if snapshot.get("valid") is not True or errors != []:
        raise RuntimeError("dry-run 冻结旧世代仓库快照不是有效状态")
    for repo_id, expected in FROZEN_LEGACY_REPOSITORIES.items():
        repository = by_id[repo_id]
        if (
            repository.get("valid") is not True
            or repository.get("errors") != []
            or repository.get("remote_name") != repo_id
            or repository.get("visibility") != "public"
            or repository.get("archived") is not False
            or repository.get("is_template") is not expected["is_template"]
            or repository.get("default_branch") != "main"
            or repository.get("expected_commit") != expected["commit"]
            or repository.get("expected_tree") != expected["tree"]
            or repository.get("expected_is_template") is not expected["is_template"]
            or repository.get("commit") != expected["commit"]
            or repository.get("tree") != expected["tree"]
        ):
            raise RuntimeError(f"dry-run 冻结旧世代仓库快照被篡改：{repo_id}")


def assert_safe_dry_run(
    report: dict[str, Any], manifest_path: Path, selection: dict[str, Any]
) -> None:
    if report.get("schema_version") != 2:
        raise RuntimeError("dry-run report 不是 schema v2；请重新执行 dry-run")
    if report.get("manifest_sha256") != sha256(manifest_path):
        raise RuntimeError("dry-run 使用的 manifest 与当前文件不一致")
    if selection_request(report.get("selection", {})) != selection_request(selection):
        raise RuntimeError("dry-run 的目标参数与当前参数不一致")
    identity = report.get("identity", {})
    if identity.get("membership_state") != "active" or identity.get(
        "membership_role"
    ) not in {"admin"}:
        raise RuntimeError(
            "当前 GitHub 身份没有经确认的组织建仓权限："
            f"{identity.get('membership_state')}/{identity.get('membership_role')}"
        )
    summary = report.get("summary", {})
    if summary.get("conflict") or summary.get("invalid"):
        raise RuntimeError(
            f"dry-run 存在 conflict={summary.get('conflict')}、"
            f"invalid={summary.get('invalid')}，禁止写入"
        )
    validate_frozen_legacy_snapshot(report.get("frozen_legacy_repositories", {}))


def repositories_from_dry_run(
    manifest: dict[str, Any], report: dict[str, Any]
) -> list[dict[str, Any]]:
    selected_ids = report.get("selection", {}).get("selected_repo_ids")
    if not isinstance(selected_ids, list) or any(
        not isinstance(repo_id, str) or not repo_id for repo_id in selected_ids
    ):
        raise RuntimeError("dry-run 缺少有效的 selected_repo_ids 快照")
    if len(selected_ids) != len(set(repo_id.casefold() for repo_id in selected_ids)):
        raise RuntimeError("dry-run selected_repo_ids 存在大小写重复")
    item_ids = [item.get("repo_id") for item in report.get("items", [])]
    if item_ids != selected_ids:
        raise RuntimeError("dry-run items 与 selected_repo_ids 快照不一致")

    repository_index = {
        repository["repo_id"]: repository
        for repository in manifest.get("repositories", [])
    }
    unknown = [repo_id for repo_id in selected_ids if repo_id not in repository_index]
    if unknown:
        raise RuntimeError(f"dry-run 引用 manifest 之外的仓库：{unknown}")
    return [repository_index[repo_id] for repo_id in selected_ids]


def load_or_create_execution(
    path: Path, manifest_path: Path, manifest: dict[str, Any]
) -> dict[str, Any]:
    manifest_digest = sha256(manifest_path)
    if path.exists():
        report = load_json(path)
        if report.get("manifest_sha256") != manifest_digest:
            raise RuntimeError("execution report 属于另一份 manifest")
        return report
    report = {
        "schema_version": 2,
        "started_at": now(),
        "updated_at": now(),
        "completed_at": None,
        "manifest_path": manifest_path.as_posix(),
        "manifest_sha256": manifest_digest,
        "organization": manifest["organization"],
        "summary": {},
        "repositories": {},
        "errors": [],
    }
    atomic_json(path, report)
    return report


def save_execution(path: Path, report: dict[str, Any]) -> None:
    counts = Counter(
        value.get("status", "unknown") for value in report["repositories"].values()
    )
    report["updated_at"] = now()
    report["summary"] = dict(sorted(counts.items()))
    atomic_json(path, report)


def record_execution(
    path: Path,
    report: dict[str, Any],
    repo_id: str,
    **fields: Any,
) -> None:
    prior = report["repositories"].get(repo_id, {})
    prior.update(fields)
    prior["repo_id"] = repo_id
    prior["updated_at"] = now()
    report["repositories"][repo_id] = prior
    save_execution(path, report)


def run_git(args: list[str], cwd: Path, timeout: int = 300) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=timeout,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} 失败：\nstdout={result.stdout}\nstderr={result.stderr}"
        )
    return result.stdout.strip()


def write_file(root: Path, relative: str, content: str | bytes) -> None:
    destination = root / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, bytes):
        destination.write_bytes(content)
    else:
        destination.write_text(content, encoding="utf-8")


def common_repository_toml() -> str:
    return f"""schema_version = 2
registry = "https://github.com/HIT-Fireworks/{REGISTRY_REPO_ID}"
repo_id_source = "github.repository"
repo_type_source = "registry"
"""


def sync_workflow() -> str:
    return """name: Sync managed repository

on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  validate:
    uses: HIT-Fireworks/fireworks-repos-management-v2/.github/workflows/reusable-repository-sync.yml@main
    permissions:
      contents: read
"""


def reusable_workflow() -> str:
    return """name: Validate managed repository

on:
  workflow_call:
  workflow_dispatch:

jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Validate base metadata
        run: test -f repository.toml
"""


def template_seed(repo_id: str) -> dict[str, str | bytes]:
    base: dict[str, str | bytes] = {
        "README.md": (
            "# 薪火笔记社托管仓库 v2\n\n"
            "本仓库由 [HIT-Fireworks 课程注册表 v2]"
            f"(https://github.com/HIT-Fireworks/{REGISTRY_REPO_ID}) "
            "统一维护身份与索引。\n"
        ),
        "repository.toml": common_repository_toml(),
        ".github/workflows/sync.yml": sync_workflow(),
    }
    if repo_id == COURSE_TEMPLATE_REPO_ID:
        directories = (
            "notes",
            "slides",
            "assignments",
            "exams",
            "labs",
            "textbooks",
            "tutorials",
            "projects",
            "software",
            "variants",
            "legacy",
        )
    elif repo_id == REQUIREMENT_TEMPLATE_REPO_ID:
        directories = ("records", "mappings", "legacy")
    elif repo_id == COLLECTION_TEMPLATE_REPO_ID:
        directories = ("shared", "members", "legacy")
    else:
        raise ValueError(f"未知模板：{repo_id}")
    for directory in directories:
        base[f"{directory}/.gitkeep"] = ""
    return base


def control_seed(repo_id: str, manifest_path: Path) -> dict[str, str | bytes]:
    root_license = Path("LICENSE")
    license_content = root_license.read_bytes() if root_license.exists() else b""
    if repo_id == REGISTRY_REPO_ID:
        return {
            "README.md": (
                "# 薪火课程注册表 v2\n\n"
                "本仓库是 HIT 全量课程、培养方案记录、目标仓库和历史资料映射的 v2 权威源。\n"
            ),
            "repository-manifest.json": manifest_path.read_bytes(),
            "resource-repository-groups.v1.json": Path(
                "config/resource-repository-groups.v1.json"
            ).read_bytes(),
            "LICENSE": license_content,
        }
    if repo_id == MANAGEMENT_REPO_ID:
        return {
            "README.md": (
                "# 薪火课程仓库管理 v2\n\n"
                "本仓库维护 v2 全量仓库生成、校验、初始化和可复用工作流。\n"
            ),
            "scripts/generate-repository-manifest.py": Path(
                "scripts/generate-repository-manifest.py"
            ).read_bytes(),
            "scripts/validate-repository-manifest.py": Path(
                "scripts/validate-repository-manifest.py"
            ).read_bytes(),
            "scripts/provision-repositories.py": Path(__file__).read_bytes(),
            "config/resource-repository-groups.v1.json": Path(
                "config/resource-repository-groups.v1.json"
            ).read_bytes(),
            "docs/full-repository-structure-design.md": Path(
                "docs/superpowers/specs/2026-08-09-full-repository-structure-design.md"
            ).read_bytes(),
            ".github/workflows/reusable-repository-sync.yml": reusable_workflow(),
            "repository.toml": common_repository_toml(),
            "LICENSE": license_content,
        }
    raise ValueError(f"未知控制面仓库：{repo_id}")


def requirement_seed(
    repository: dict[str, Any], records: list[dict[str, Any]]
) -> dict[str, str | bytes]:
    files: dict[str, str | bytes] = {
        "README.md": (
            f"# {repository['display_name']}\n\n"
            "本仓库逐项维护培养方案中尚无稳定课程代码的课程或培养要求。"
            "每个 `record_id` 是独立维护单元；同仓不代表课程等价。\n"
        ),
        "repository.toml": common_repository_toml(),
        "requirements.json": json.dumps(
            {
                "schema_version": 1,
                "repo_id": repository["repo_id"],
                "records": records,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        ".github/workflows/sync.yml": sync_workflow(),
        "mappings/.gitkeep": "",
        "legacy/.gitkeep": "",
    }
    for record in records:
        record_id = record["record_id"]
        files[f"records/{record_id}/README.md"] = (
            f"# {record['course_name']}\n\n"
            f"- 记录 ID：`{record_id}`\n"
            f"- 来源培养方案：`{record['source_plan']}`\n"
            f"- 专业：{record['major_name']}（`{record['major_code']}`）\n"
            f"- 建议学期：{record['recommended_year_semester'] or '未标注'}\n"
            f"- 学分：{record.get('credit')}\n"
            f"- 总学时：{record.get('total_hours')}\n"
            f"- 状态：`{record['identity_status']}`\n"
        )
        files[f"records/{record_id}/notes/.gitkeep"] = ""
        files[f"records/{record_id}/resources/.gitkeep"] = ""
    return files


def create_remote_repository(
    client: GitHubClient, organization: str, repository: dict[str, Any]
) -> dict[str, Any]:
    body, _ = client.request(
        "POST",
        f"/orgs/{urllib.parse.quote(organization)}/repos",
        {
            "name": repository["repo_id"],
            "description": repository.get("description", "")[:350],
            "private": False,
            "has_issues": True,
            "has_projects": False,
            "has_wiki": False,
            "auto_init": False,
        },
        mutation=True,
    )
    return body


def get_repository(
    client: GitHubClient, organization: str, repo_id: str
) -> dict[str, Any] | None:
    try:
        body, _ = client.request(
            "GET",
            f"/repos/{urllib.parse.quote(organization)}/{urllib.parse.quote(repo_id)}",
        )
        return body
    except ApiError as error:
        if error.status == 404:
            return None
        raise


def get_head_commit(
    client: GitHubClient, organization: str, repo_id: str
) -> str | None:
    try:
        body, _ = client.request(
            "GET",
            f"/repos/{urllib.parse.quote(organization)}/{urllib.parse.quote(repo_id)}/git/ref/heads/main",
        )
        return body["object"]["sha"]
    except ApiError as error:
        if error.status in {404, 409}:
            return None
        raise


def get_head_revision(
    client: GitHubClient, organization: str, repo_id: str
) -> dict[str, str]:
    body, _ = client.request(
        "GET",
        f"/repos/{urllib.parse.quote(organization)}/{urllib.parse.quote(repo_id)}/commits/main",
    )
    commit = body.get("sha") if isinstance(body, dict) else None
    tree = (
        body.get("commit", {}).get("tree", {}).get("sha")
        if isinstance(body, dict)
        else None
    )
    if not commit or not tree:
        raise RuntimeError(f"无法读取仓库 main 的 commit/tree：{organization}/{repo_id}")
    return {"commit": commit, "tree": tree}


def inspect_frozen_legacy_repositories(
    client: GitHubClient,
    organization: str,
    remote_repositories: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    remote = (
        remote_repositories
        if remote_repositories is not None
        else list_org_repositories(client, organization)
    )
    remote_by_fold: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for repository in remote:
        remote_by_fold[repository.get("name", "").casefold()].append(repository)

    errors: list[str] = []
    repositories: list[dict[str, Any]] = []
    for repo_id, expected in FROZEN_LEGACY_REPOSITORIES.items():
        matches = remote_by_fold.get(repo_id.casefold(), [])
        item: dict[str, Any] = {
            "repo_id": repo_id,
            "expected_commit": expected["commit"],
            "expected_tree": expected["tree"],
            "expected_is_template": expected["is_template"],
        }
        item_errors: list[str] = []
        if len(matches) != 1:
            item_errors.append(f"远程匹配数为 {len(matches)}，预期为 1")
        else:
            metadata = matches[0]
            item.update(
                {
                    "remote_name": metadata.get("name"),
                    "remote_url": metadata.get("html_url"),
                    "visibility": metadata.get("visibility"),
                    "archived": metadata.get("archived"),
                    "is_template": metadata.get("is_template"),
                    "default_branch": metadata.get("default_branch"),
                }
            )
            if metadata.get("name") != repo_id:
                item_errors.append(f"远程名称为 {metadata.get('name')!r}")
            if metadata.get("visibility") != "public" or metadata.get("private"):
                item_errors.append("不是公开仓库")
            if metadata.get("archived"):
                item_errors.append("意外处于归档状态")
            if bool(metadata.get("is_template")) != expected["is_template"]:
                item_errors.append(
                    "template 标记错误："
                    f"{metadata.get('is_template')} != {expected['is_template']}"
                )
            if metadata.get("default_branch") != "main":
                item_errors.append(
                    f"默认分支不是 main：{metadata.get('default_branch')!r}"
                )
            try:
                revision = get_head_revision(client, organization, repo_id)
            except Exception as error:
                item_errors.append(f"无法读取 main commit/tree：{error}")
            else:
                item.update(revision)
                if revision["commit"] != expected["commit"]:
                    item_errors.append(
                        f"commit 已漂移：{revision['commit']} != {expected['commit']}"
                    )
                if revision["tree"] != expected["tree"]:
                    item_errors.append(
                        f"tree 已漂移：{revision['tree']} != {expected['tree']}"
                    )
        item["valid"] = not item_errors
        item["errors"] = item_errors
        repositories.append(item)
        errors.extend(f"{repo_id}: {error}" for error in item_errors)

    return {
        "valid": not errors,
        "repositories": repositories,
        "errors": errors,
    }


def assert_frozen_legacy_repositories(
    client: GitHubClient,
    organization: str,
    remote_repositories: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    snapshot = inspect_frozen_legacy_repositories(
        client, organization, remote_repositories
    )
    if not snapshot["valid"]:
        raise RuntimeError(
            "冻结旧世代仓库前置条件失败：" + "; ".join(snapshot["errors"])
        )
    return snapshot


def expected_seed_tree(files: dict[str, str | bytes]) -> str:
    with tempfile.TemporaryDirectory(prefix="fireworks-seed-tree-") as temporary:
        root = Path(temporary)
        for relative, content in files.items():
            write_file(root, relative, content)
        run_git(["init"], root)
        run_git(["add", "."], root)
        return run_git(["write-tree"], root)


def validate_existing_seed(
    *,
    client: GitHubClient,
    organization: str,
    repository: dict[str, Any],
    files: dict[str, str | bytes],
    execution: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, str]]:
    repo_id = repository["repo_id"]
    metadata = get_repository(client, organization, repo_id)
    if not metadata:
        raise RuntimeError(f"种子仓库不存在：{repo_id}")
    if metadata.get("name") != repo_id:
        raise RuntimeError(f"种子仓库大小写不一致：{repo_id} -> {metadata.get('name')}")
    if metadata.get("visibility") != "public" or metadata.get("archived"):
        raise RuntimeError(f"种子仓库不是未归档公开仓库：{repo_id}")
    if metadata.get("default_branch") != "main":
        raise RuntimeError(
            f"种子仓库默认分支不是 main：{repo_id}: {metadata.get('default_branch')}"
        )
    if repository["repo_type"] == "template" and not metadata.get("is_template"):
        raise RuntimeError(f"种子仓库未标记为 template：{repo_id}")
    if repository["repo_type"] != "template" and metadata.get("is_template"):
        raise RuntimeError(f"普通种子仓库意外标记为 template：{repo_id}")

    revision = get_head_revision(client, organization, repo_id)
    expected = None
    if expected is None:
        prior = execution.get("repositories", {}).get(repo_id, {})
        prior_commit = prior.get("initialized_commit")
        prior_tree = prior.get("initialized_tree")
        if prior_commit and prior_tree:
            expected = {"commit": prior_commit, "tree": prior_tree}
        else:
            expected = {"tree": expected_seed_tree(files)}
    if expected.get("commit") and revision["commit"] != expected["commit"]:
        raise RuntimeError(
            f"种子仓库 commit 已漂移：{repo_id}: "
            f"{revision['commit']} != {expected['commit']}"
        )
    if revision["tree"] != expected["tree"]:
        raise RuntimeError(
            f"种子仓库 tree 已漂移：{repo_id}: "
            f"{revision['tree']} != {expected['tree']}"
        )
    return metadata, revision


def push_seed(
    organization: str, repo_id: str, files: dict[str, str | bytes]
) -> str:
    with tempfile.TemporaryDirectory(prefix=f"fireworks-{repo_id[:20]}-") as temporary:
        root = Path(temporary)
        for relative, content in files.items():
            write_file(root, relative, content)
        run_git(["init", "-b", "main"], root)
        run_git(["config", "user.name", "HIT Fireworks Automation"], root)
        run_git(["config", "user.email", "actions@users.noreply.github.com"], root)
        run_git(["add", "."], root)
        run_git(["commit", "-m", "chore: 初始化仓库结构"], root)
        commit = run_git(["rev-parse", "HEAD"], root)
        run_git(
            [
                "remote",
                "add",
                "origin",
                f"https://github.com/{organization}/{repo_id}.git",
            ],
            root,
        )
        run_git(["push", "-u", "origin", "main"], root, timeout=600)
        return commit


def set_template(
    client: GitHubClient, organization: str, repo_id: str
) -> dict[str, Any]:
    body, _ = client.request(
        "PATCH",
        f"/repos/{urllib.parse.quote(organization)}/{urllib.parse.quote(repo_id)}",
        {"is_template": True},
        mutation=True,
    )
    return body


def seed_repository(
    *,
    client: GitHubClient,
    organization: str,
    repository: dict[str, Any],
    files: dict[str, str | bytes],
    execution_path: Path,
    execution: dict[str, Any],
) -> None:
    repo_id = repository["repo_id"]
    prior = execution["repositories"].get(repo_id, {})
    existing = get_repository(client, organization, repo_id)
    if existing and get_head_commit(client, organization, repo_id):
        existing, revision = validate_existing_seed(
            client=client,
            organization=organization,
            repository=repository,
            files=files,
            execution=execution,
        )
        record_execution(
            execution_path,
            execution,
            repo_id,
            status="reused",
            action="reused",
            repo_type=repository["repo_type"],
            template_id=repository.get("template_id"),
            template_version=None,
            remote_url=existing.get("html_url"),
            initialized_commit=revision["commit"],
            initialized_tree=revision["tree"],
            error=None,
        )
        return

    if existing:
        resumable = (
            prior.get("action") == "created"
            and prior.get("status") in {"remote-created", "failed"}
            and prior.get("remote_url") == existing.get("html_url")
            and not prior.get("initialized_commit")
        )
        if not resumable:
            raise RuntimeError(
                f"拒绝初始化无法由 execution report 证明归属的空仓库：{repo_id}"
            )
        if (
            existing.get("name") != repo_id
            or existing.get("visibility") != "public"
            or existing.get("archived")
        ):
            raise RuntimeError(f"待续跑空仓库状态冲突：{repo_id}")
    else:
        existing = create_remote_repository(client, organization, repository)
        record_execution(
            execution_path,
            execution,
            repo_id,
            status="remote-created",
            action="created",
            repo_type=repository["repo_type"],
            template_id=repository.get("template_id"),
            remote_url=existing.get("html_url"),
            error=None,
        )

    try:
        push_seed(organization, repo_id, files)
        if repository["repo_type"] == "template":
            existing = set_template(client, organization, repo_id)
        revision = get_head_revision(client, organization, repo_id)
        expected_tree = expected_seed_tree(files)
        if revision["tree"] != expected_tree:
            raise RuntimeError(
                f"新种子仓库 tree 与本地种子不一致：{repo_id}: "
                f"{revision['tree']} != {expected_tree}"
            )
    except Exception as error:
        record_execution(
            execution_path,
            execution,
            repo_id,
            status="failed",
            action="created",
            repo_type=repository["repo_type"],
            template_id=repository.get("template_id"),
            remote_url=existing.get("html_url"),
            error=str(error),
        )
        raise

    record_execution(
        execution_path,
        execution,
        repo_id,
        status="created",
        action="created",
        repo_type=repository["repo_type"],
        template_id=repository.get("template_id"),
        template_version=None,
        remote_url=existing.get("html_url"),
        initialized_commit=revision["commit"],
        initialized_tree=revision["tree"],
        error=None,
    )


def chunks(values: list[Any], size: int) -> Iterable[list[Any]]:
    for index in range(0, len(values), size):
        yield values[index : index + size]


def wait_for_head_commit(
    client: GitHubClient,
    organization: str,
    repo_id: str,
    *,
    attempts: int = 30,
) -> str:
    for attempt in range(1, attempts + 1):
        commit = get_head_commit(client, organization, repo_id)
        if commit:
            return commit
        if attempt < attempts:
            time.sleep(min(float(attempt), 5.0))
    raise RuntimeError(f"模板仓库生成后仍没有 main 初始化 commit：{repo_id}")


def generate_from_template(
    client: GitHubClient,
    *,
    template_owner: str,
    template_repo: str,
    target_owner: str,
    repository: dict[str, Any],
) -> tuple[dict[str, Any], str]:
    repo_id = repository["repo_id"]
    existing = get_repository(client, target_owner, repo_id)
    if existing:
        if existing.get("name") != repo_id:
            raise RuntimeError(
                f"模板目标仓库仅大小写匹配：期望 {repo_id}，实际 {existing.get('name')}"
            )
        if existing.get("visibility") != "public" or existing.get("archived"):
            raise RuntimeError(f"模板目标仓库状态冲突：{repo_id}")
        return existing, "reused"

    try:
        body, _ = client.request(
            "POST",
            (
                f"/repos/{urllib.parse.quote(template_owner)}/"
                f"{urllib.parse.quote(template_repo)}/generate"
            ),
            {
                "owner": target_owner,
                "name": repo_id,
                "description": repository.get("description", "")[:350],
                "include_all_branches": False,
                "private": False,
            },
            mutation=True,
        )
    except ApiError as error:
        if error.status == 422:
            existing = get_repository(client, target_owner, repo_id)
            if existing and existing.get("name") == repo_id:
                return existing, "reused-after-race"
        raise
    if not isinstance(body, dict) or body.get("name") != repo_id:
        raise RuntimeError(f"REST 模板生成返回异常：{repo_id}: {body!r}")
    if body.get("visibility") != "public" or body.get("private"):
        raise RuntimeError(f"REST 模板生成的仓库不是公开仓库：{repo_id}")
    return body, "created"


def repository_content(
    client: GitHubClient,
    organization: str,
    repo_id: str,
    path: str,
) -> str:
    body, _ = client.request(
        "GET",
        (
            f"/repos/{urllib.parse.quote(organization)}/{urllib.parse.quote(repo_id)}/"
            f"contents/{urllib.parse.quote(path, safe='/')}?ref=main"
        ),
    )
    if not isinstance(body, dict) or body.get("type") != "file" or not body.get("content"):
        raise RuntimeError(f"模板目标缺少文件：{repo_id}/{path}")
    return base64.b64decode(body["content"].replace("\n", "")).decode("utf-8")






def canary_test_template_generation(
    *,
    client: GitHubClient,
    manifest_path: Path,
    manifest: dict[str, Any],
    dry_run_path: Path,
    execution_path: Path,
    canary_report_path: Path,
    selection: dict[str, Any],
) -> dict[str, Any]:
    dry_run = load_json(dry_run_path)
    assert_safe_dry_run(dry_run, manifest_path, selection)
    selected = repositories_from_dry_run(manifest, dry_run)
    if len(selected) != 1:
        raise RuntimeError("canary-test 必须通过统一选择器恰好选择一个仓库")
    target = selected[0]
    target_repo_id = target["repo_id"]
    template_id = target.get("template_id")
    if template_id != COURSE_TEMPLATE_REPO_ID:
        raise RuntimeError(
            f"canary 目标必须使用课程模板：{target_repo_id} -> {template_id}"
        )

    organization = manifest["organization"]
    identity = get_identity(client, organization)
    if identity != dry_run["identity"]:
        raise RuntimeError("GitHub 身份或组织成员状态自 dry-run 后发生变化")
    remote = list_org_repositories(client, organization)
    assert_frozen_legacy_repositories(client, organization, remote)
    execution = load_or_create_execution(execution_path, manifest_path, manifest)
    repository_index = {
        repository["repo_id"]: repository
        for repository in manifest["repositories"]
    }

    seed_ids = [REGISTRY_REPO_ID, MANAGEMENT_REPO_ID, template_id]
    seeded: list[dict[str, Any]] = []
    for repo_id in seed_ids:
        repository = repository_index[repo_id]
        files = (
            control_seed(repo_id, manifest_path)
            if repository["repo_type"] == "control"
            else template_seed(repo_id)
        )
        metadata, revision = validate_existing_seed(
            client=client,
            organization=organization,
            repository=repository,
            files=files,
            execution=execution,
        )
        seeded.append(
            {
                "repo_id": repo_id,
                "remote_url": metadata.get("html_url"),
                "initialized_commit": revision["commit"],
                "tree": revision["tree"],
                "is_template": metadata.get("is_template"),
            }
        )

    registry_readme = repository_content(
        client, organization, REGISTRY_REPO_ID, "README.md"
    )
    management_workflow = repository_content(
        client,
        organization,
        MANAGEMENT_REPO_ID,
        ".github/workflows/reusable-repository-sync.yml",
    )
    template_toml = repository_content(
        client, organization, template_id, "repository.toml"
    )
    template_workflow = repository_content(
        client, organization, template_id, ".github/workflows/sync.yml"
    )
    if "薪火课程注册表" not in registry_readme:
        raise RuntimeError("课程注册表 README 不是预期内容")
    if "workflow_call" not in management_workflow:
        raise RuntimeError("仓库管理 reusable workflow 不是预期内容")
    if REGISTRY_REPO_ID not in template_toml:
        raise RuntimeError("课程模板 repository.toml 不是预期内容")
    if "reusable-repository-sync.yml@main" not in template_workflow:
        raise RuntimeError("课程模板 sync workflow 不是预期内容")

    template_revision = get_head_revision(client, organization, template_id)
    template_commit = template_revision["commit"]
    template_tree = template_revision["tree"]
    try:
        target_metadata, action = generate_from_template(
            client,
            template_owner=organization,
            template_repo=template_id,
            target_owner=organization,
            repository=target,
        )
        wait_for_head_commit(client, organization, target_repo_id)
        target_revision = get_head_revision(client, organization, target_repo_id)
        target_metadata = get_repository(client, organization, target_repo_id)
        if not target_metadata:
            raise RuntimeError("REST 返回成功后无法读取正式 canary 仓库")
        if target_metadata.get("name") != target_repo_id:
            raise RuntimeError(
                f"正式 canary 仓库名称错误：{target_metadata.get('name')}"
            )
        if target_metadata.get("visibility") != "public" or target_metadata.get(
            "private"
        ):
            raise RuntimeError("正式 canary 仓库不是公开仓库")
        if target_metadata.get("archived"):
            raise RuntimeError("正式 canary 仓库意外处于归档状态")
        if target_metadata.get("default_branch") != "main":
            raise RuntimeError(
                f"正式 canary 默认分支错误：{target_metadata.get('default_branch')}"
            )
        if target_revision["tree"] != template_tree:
            raise RuntimeError(
                "正式 canary 内容树不等于模板树："
                f"{target_revision['tree']} != {template_tree}"
            )

        repository_toml = repository_content(
            client, organization, target_repo_id, "repository.toml"
        )
        workflow = repository_content(
            client, organization, target_repo_id, ".github/workflows/sync.yml"
        )
        if REGISTRY_REPO_ID not in repository_toml:
            raise RuntimeError("正式 canary repository.toml 不是预期模板内容")
        if "reusable-repository-sync.yml@main" not in workflow:
            raise RuntimeError("正式 canary workflow 不是预期模板内容")

        record_execution(
            execution_path,
            execution,
            target_repo_id,
            status="created" if action == "created" else "reused",
            action=action,
            repo_type=target["repo_type"],
            template_id=template_id,
            template_version=template_commit,
            template_tree=template_tree,
            remote_url=target_metadata.get("html_url"),
            initialized_commit=target_revision["commit"],
            initialized_tree=target_revision["tree"],
            error=None,
        )
    except Exception as error:
        existing = get_repository(client, organization, target_repo_id)
        revision = (
            get_head_revision(client, organization, target_repo_id)
            if existing and get_head_commit(client, organization, target_repo_id)
            else None
        )
        record_execution(
            execution_path,
            execution,
            target_repo_id,
            status="failed",
            action="canary",
            repo_type=target["repo_type"],
            template_id=template_id,
            template_version=template_commit,
            template_tree=template_tree,
            remote_url=existing.get("html_url") if existing else None,
            initialized_commit=revision.get("commit") if revision else None,
            initialized_tree=revision.get("tree") if revision else None,
            error=str(error),
        )
        raise

    report = {
        "schema_version": 2,
        "verified_at": now(),
        "valid": True,
        "organization": organization,
        "production_canary": True,
        "selection": dry_run["selection"],
        "seeded_repositories": seeded,
        "template_repo_id": template_id,
        "template_version": template_commit,
        "template_tree": template_tree,
        "canary_repo_id": target_repo_id,
        "canary_action": action,
        "remote_url": target_metadata.get("html_url"),
        "visibility": target_metadata.get("visibility"),
        "archived": target_metadata.get("archived"),
        "default_branch": target_metadata.get("default_branch"),
        "initialized_commit": target_revision["commit"],
        "initialized_tree": target_revision["tree"],
        "verified_files": ["repository.toml", ".github/workflows/sync.yml"],
    }
    atomic_json(canary_report_path, report)
    return report


def query_repository_heads(
    client: GitHubClient,
    organization: str,
    repo_ids: list[str],
    batch_size: int = 50,
) -> dict[str, dict[str, Any] | None]:
    result: dict[str, dict[str, Any] | None] = {}
    for batch in chunks(repo_ids, batch_size):
        definitions = ["$owner: String!"]
        variables: dict[str, Any] = {"owner": organization}
        fields: list[str] = []
        aliases: dict[str, str] = {}
        for index, repo_id in enumerate(batch):
            alias = f"r{index}"
            aliases[alias] = repo_id
            definitions.append(f"$n{index}: String!")
            variables[f"n{index}"] = repo_id
            fields.append(
                f"""{alias}: repository(owner: $owner, name: $n{index}) {{
                  nameWithOwner
                  url
                  defaultBranchRef {{
                    target {{ ... on Commit {{ oid tree {{ oid }} }} }}
                  }}
                }}"""
            )
        query = f"query({', '.join(definitions)}) {{\n" + "\n".join(fields) + "\n}"
        response = client.graphql(query, variables)
        data = response.get("data") or {}
        for alias, repo_id in aliases.items():
            result[repo_id] = data.get(alias)
    return result


def provision_templated_repositories(
    *,
    client: GitHubClient,
    organization: str,
    manifest: dict[str, Any],
    repositories: list[dict[str, Any]],
    execution_path: Path,
    execution: dict[str, Any],
    batch_size: int,
) -> None:
    if not repositories:
        return

    remote = list_org_repositories(client, organization)
    remote_by_name = {repo["name"]: repo for repo in remote}
    repository_index = {
        repository["repo_id"]: repository
        for repository in manifest["repositories"]
    }
    template_metadata: dict[str, dict[str, Any]] = {}
    for template_id in sorted(
        {repo["template_id"] for repo in repositories if repo.get("template_id")}
    ):
        template_repository = repository_index[template_id]
        metadata, revision = validate_existing_seed(
            client=client,
            organization=organization,
            repository=template_repository,
            files=template_seed(template_id),
            execution=execution,
        )
        template_metadata[template_id] = {
            "commit": revision["commit"],
            "tree": revision["tree"],
            "url": metadata.get("html_url"),
        }

    existing_ids = [
        repository["repo_id"]
        for repository in repositories
        if repository["repo_id"] in remote_by_name
    ]
    existing_heads = query_repository_heads(client, organization, existing_ids)

    total = len(repositories)
    completed = 0
    for batch in chunks(repositories, batch_size):
        for repository in batch:
            repo_id = repository["repo_id"]
            template_id = repository["template_id"]
            template = template_metadata[template_id]
            try:
                if repo_id in remote_by_name:
                    metadata = remote_by_name[repo_id]
                    if metadata.get("visibility") != "public" or metadata.get("archived"):
                        raise RuntimeError(f"已有仓库不是未归档公开仓库：{repo_id}")
                    if metadata.get("is_template"):
                        raise RuntimeError(f"已有普通仓库意外标记为 template：{repo_id}")
                    head = existing_heads.get(repo_id)
                    revision = (
                        head.get("defaultBranchRef", {}).get("target", {})
                        if head
                        else {}
                    )
                    commit = revision.get("oid")
                    tree = revision.get("tree", {}).get("oid")
                    if not commit or not tree:
                        raise RuntimeError(f"已有仓库没有 main commit/tree：{repo_id}")
                    prior = execution["repositories"].get(repo_id, {})
                    expected_commit = prior.get("initialized_commit")
                    expected_tree = prior.get("initialized_tree") or template["tree"]
                    if expected_commit and commit != expected_commit:
                        raise RuntimeError(
                            f"已有仓库 commit 已漂移：{repo_id}: "
                            f"{commit} != {expected_commit}"
                        )
                    if tree != expected_tree:
                        raise RuntimeError(
                            f"已有仓库 tree 不等于冻结初始化树：{repo_id}: "
                            f"{tree} != {expected_tree}"
                        )
                    action = "reused"
                    remote_url = metadata.get("html_url")
                else:
                    metadata, generated_action = generate_from_template(
                        client,
                        template_owner=organization,
                        template_repo=template_id,
                        target_owner=organization,
                        repository=repository,
                    )
                    wait_for_head_commit(client, organization, repo_id)
                    revision = get_head_revision(client, organization, repo_id)
                    commit = revision["commit"]
                    tree = revision["tree"]
                    if tree != template["tree"]:
                        raise RuntimeError(
                            f"新仓库 tree 不等于模板树：{repo_id}: "
                            f"{tree} != {template['tree']}"
                        )
                    action = "created" if generated_action == "created" else "reused"
                    remote_url = metadata.get("html_url")

                record_execution(
                    execution_path,
                    execution,
                    repo_id,
                    status=action,
                    action=action,
                    repo_type=repository["repo_type"],
                    template_id=template_id,
                    template_version=template["commit"],
                    template_tree=template["tree"],
                    remote_url=remote_url,
                    initialized_commit=commit,
                    initialized_tree=tree,
                    error=None,
                )
            except Exception as error:
                existing = get_repository(client, organization, repo_id)
                revision = (
                    get_head_revision(client, organization, repo_id)
                    if existing and get_head_commit(client, organization, repo_id)
                    else None
                )
                record_execution(
                    execution_path,
                    execution,
                    repo_id,
                    status="failed",
                    action="create",
                    repo_type=repository["repo_type"],
                    template_id=template_id,
                    template_version=template["commit"],
                    template_tree=template["tree"],
                    remote_url=existing.get("html_url") if existing else None,
                    initialized_commit=revision.get("commit") if revision else None,
                    initialized_tree=revision.get("tree") if revision else None,
                    error=str(error),
                )
                raise RuntimeError(f"模板仓库处理失败 {repo_id}: {error}") from error
            completed += 1
        print(f"本轮仓库已处理 {completed}/{total}", flush=True)


def verify_remote(
    *,
    client: GitHubClient,
    manifest: dict[str, Any],
    repositories: list[dict[str, Any]],
    selection: dict[str, Any],
    execution_path: Path,
    execution: dict[str, Any],
) -> dict[str, Any]:
    organization = manifest["organization"]
    remote = list_org_repositories(client, organization)
    remote_by_fold: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for repository in remote:
        remote_by_fold[repository["name"].casefold()].append(repository)

    frozen_legacy = inspect_frozen_legacy_repositories(client, organization, remote)
    errors: list[str] = []
    errors.extend(
        f"冻结旧世代仓库前置条件：{error}"
        for error in frozen_legacy["errors"]
    )
    exact_ids: list[str] = []
    for repository in repositories:
        repo_id = repository["repo_id"]
        matches = remote_by_fold.get(repo_id.casefold(), [])
        if len(matches) != 1:
            errors.append(f"{repo_id}: 远程匹配数 {len(matches)}")
            continue
        metadata = matches[0]
        if metadata["name"] != repo_id:
            errors.append(f"{repo_id}: 远程大小写为 {metadata['name']}")
            continue
        if metadata.get("visibility") != "public" or metadata.get("archived"):
            errors.append(f"{repo_id}: 不是未归档公开仓库")
            continue
        if repository["repo_type"] == "template" and not metadata.get("is_template"):
            errors.append(f"{repo_id}: 未标记为 template")
            continue
        if repository["repo_type"] != "template" and metadata.get("is_template"):
            errors.append(f"{repo_id}: 普通仓库意外标记为 template")
            continue
        exact_ids.append(repo_id)

    heads = query_repository_heads(client, organization, exact_ids)
    for repository in repositories:
        repo_id = repository["repo_id"]
        metadata = heads.get(repo_id)
        revision = (
            metadata.get("defaultBranchRef", {}).get("target", {})
            if metadata
            else {}
        )
        commit = revision.get("oid")
        tree = revision.get("tree", {}).get("oid")
        if not commit or not tree:
            errors.append(f"{repo_id}: 缺少 main commit/tree")
            continue

        prior = execution["repositories"].get(repo_id, {})
        expected_commit = prior.get("initialized_commit")
        expected_tree = prior.get("initialized_tree")
        frozen = None
        if not expected_commit and frozen:
            expected_commit = frozen["commit"]
        if not expected_tree and frozen:
            expected_tree = frozen["tree"]
        if expected_commit and expected_commit != commit:
            errors.append(
                f"{repo_id}: execution commit {expected_commit} != remote {commit}"
            )
        if expected_tree and expected_tree != tree:
            errors.append(f"{repo_id}: execution tree {expected_tree} != remote {tree}")

    full_manifest = len(repositories) == len(manifest["repositories"])
    verification = {
        "verified_at": now(),
        "valid": not errors,
        "scope": "full" if full_manifest else "partial",
        "selection": selection,
        "manifest_repository_count": len(manifest["repositories"]),
        "expected_repository_count": len(repositories),
        "matched_repository_count": len(exact_ids),
        "initialized_repository_count": sum(
            bool(
                metadata
                and metadata.get("defaultBranchRef", {}).get("target", {}).get("oid")
                and metadata.get("defaultBranchRef", {})
                .get("target", {})
                .get("tree", {})
                .get("oid")
            )
            for metadata in heads.values()
        ),
        "errors": errors,
        "frozen_legacy_repositories": frozen_legacy,
    }
    execution["verification"] = verification
    if not errors and full_manifest:
        execution["completed_at"] = now()
    save_execution(execution_path, execution)
    return verification


def apply(
    *,
    client: GitHubClient,
    manifest_path: Path,
    manifest: dict[str, Any],
    dry_run_path: Path,
    execution_path: Path,
    selection: dict[str, Any],
    batch_size: int,
) -> dict[str, Any]:
    dry_run = load_json(dry_run_path)
    assert_safe_dry_run(dry_run, manifest_path, selection)
    selected = repositories_from_dry_run(manifest, dry_run)

    identity = get_identity(client, manifest["organization"])
    if identity != dry_run["identity"]:
        raise RuntimeError("GitHub 身份或组织成员状态自 dry-run 后发生变化")

    current_remote = list_org_repositories(client, manifest["organization"])
    assert_frozen_legacy_repositories(client, manifest["organization"], current_remote)
    current_items, _ = classify(manifest, current_remote, selected)
    if [item["repo_id"] for item in current_items] != [
        repository["repo_id"] for repository in selected
    ]:
        raise RuntimeError("写入前分类结果与 dry-run 目标快照不一致")
    current_counts = Counter(item["action"] for item in current_items)
    if current_counts["conflict"] or current_counts["invalid"]:
        raise RuntimeError(
            "写入前重新检查发现冲突："
            f"conflict={current_counts['conflict']} invalid={current_counts['invalid']}"
        )

    execution = load_or_create_execution(execution_path, manifest_path, manifest)
    execution["last_selection"] = dry_run["selection"]
    save_execution(execution_path, execution)
    records_by_repo: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for record in manifest["curriculum_records"]:
        records_by_repo[record["repo_id"]].append(record)

    seed_order = {
        repo_id: index
        for index, repo_id in enumerate(SEED_ORDER)
    }
    selected_control_templates = sorted(
        (
            repository
            for repository in selected
            if repository["repo_type"] in {"control", "template"}
        ),
        key=lambda repository: seed_order.get(repository["repo_id"], len(seed_order)),
    )
    for repository in selected_control_templates:
        repo_id = repository["repo_id"]
        files = (
            control_seed(repo_id, manifest_path)
            if repository["repo_type"] == "control"
            else template_seed(repo_id)
        )
        seed_repository(
            client=client,
            organization=manifest["organization"],
            repository=repository,
            files=files,
            execution_path=execution_path,
            execution=execution,
        )
        print(f"种子仓库已处理：{repo_id}", flush=True)

    selected_requirements = [
        repository
        for repository in selected
        if repository["repo_type"] == "requirement-set"
    ]
    for index, repository in enumerate(selected_requirements, start=1):
        repo_id = repository["repo_id"]
        files = requirement_seed(repository, records_by_repo[repo_id])
        seed_repository(
            client=client,
            organization=manifest["organization"],
            repository=repository,
            files=files,
            execution_path=execution_path,
            execution=execution,
        )
        print(
            f"培养要求仓库已处理：{index}/{len(selected_requirements)} {repo_id}",
            flush=True,
        )

    templated = [
        repository
        for repository in selected
        if repository["repo_type"] not in SEED_REPOSITORY_TYPES
    ]
    provision_templated_repositories(
        client=client,
        organization=manifest["organization"],
        manifest=manifest,
        repositories=templated,
        execution_path=execution_path,
        execution=execution,
        batch_size=batch_size,
    )
    execution["last_batch_completed_at"] = now()
    save_execution(execution_path, execution)
    return verify_remote(
        client=client,
        manifest=manifest,
        repositories=selected,
        selection=dry_run["selection"],
        execution_path=execution_path,
        execution=execution,
    )


def main() -> int:
    args = parse_args()
    manifest = load_json(args.manifest)
    try:
        assert_v2_manifest_contract(manifest)
    except RuntimeError as error:
        raise SystemExit(str(error)) from error
    organization = args.organization or manifest.get("organization")
    if not organization:
        raise SystemExit("manifest 与参数均未提供 organization")
    if organization != manifest.get("organization"):
        raise SystemExit("参数 organization 与 manifest 不一致")
    if args.batch_size < 1 or args.batch_size > 20:
        raise SystemExit("--batch-size 必须在 1..20")
    if args.mutation_interval < 0:
        raise SystemExit("--mutation-interval 不能小于 0")

    try:
        selected, selection = selection_from_args(
            manifest, args.manifest, args.execution_report, args
        )
    except (RuntimeError, ValueError) as error:
        raise SystemExit(str(error)) from error

    client = GitHubClient(gh_token(), args.mutation_interval)
    if args.dry_run:
        identity = get_identity(client, organization)
        remote = list_org_repositories(client, organization)
        frozen_legacy = assert_frozen_legacy_repositories(
            client, organization, remote
        )
        report = build_dry_run(
            manifest_path=args.manifest,
            manifest=manifest,
            identity=identity,
            remote_repositories=remote,
            selected_repositories=selected,
            selection=selection,
            frozen_legacy_repositories=frozen_legacy,
        )
        atomic_json(args.dry_run_report, report)
        print(json.dumps(report["summary"], ensure_ascii=False, indent=2))
        print(f"目标快照：{args.dry_run_report}")
        assert_safe_dry_run(report, args.manifest, selection)
        return 0

    if args.canary_test:
        report = canary_test_template_generation(
            client=client,
            manifest_path=args.manifest,
            manifest=manifest,
            dry_run_path=args.dry_run_report,
            execution_path=args.execution_report,
            canary_report_path=args.canary_report,
            selection=selection,
        )
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0

    if args.verify:
        dry_run = load_json(args.dry_run_report)
        assert_safe_dry_run(dry_run, args.manifest, selection)
        repositories = repositories_from_dry_run(manifest, dry_run)
        execution = load_or_create_execution(
            args.execution_report, args.manifest, manifest
        )
        verification = verify_remote(
            client=client,
            manifest=manifest,
            repositories=repositories,
            selection=dry_run["selection"],
            execution_path=args.execution_report,
            execution=execution,
        )
        print(json.dumps(verification, ensure_ascii=False, indent=2))
        return 0 if verification["valid"] else 1

    verification = apply(
        client=client,
        manifest_path=args.manifest,
        manifest=manifest,
        dry_run_path=args.dry_run_report,
        execution_path=args.execution_report,
        selection=selection,
        batch_size=args.batch_size,
    )
    print(json.dumps(verification, ensure_ascii=False, indent=2))
    return 0 if verification["valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

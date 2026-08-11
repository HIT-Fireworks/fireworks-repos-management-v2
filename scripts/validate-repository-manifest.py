#!/usr/bin/env python3
"""独立校验全量仓库分配 manifest 的源数据覆盖与引用完整性。"""

from __future__ import annotations

import argparse
import json
import hashlib
import re
import subprocess
import tomllib
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

REQUIRED_FIELDS = (
    "repo_id",
    "repo_type",
    "merge_key",
    "merge_reason",
    "source_paths",
    "status",
)
REGISTRY_REPO_ID = "fireworks-course-registry-v2"
EXPECTED_CONTROL_TEMPLATE_REPO_IDS = {
    REGISTRY_REPO_ID,
    "fireworks-repos-management-v2",
    "fireworks-course-template-v2",
    "fireworks-requirement-template-v2",
    "fireworks-collection-template-v2",
}
FROZEN_LEGACY_REPO_IDS = {
    "fireworks-course-registry",
    "fireworks-repos-management",
    "fireworks-course-template",
    "22AD11001",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path("data/repository-manifest.json"),
    )
    parser.add_argument(
        "--plans",
        type=Path,
        default=Path(".workspaces/hoahrb-jwts/hit-data/plans"),
    )
    parser.add_argument(
        "--legacy-repo",
        type=Path,
        default=Path(".workspaces/fireworks-attachments"),
    )
    parser.add_argument(
        "--resource-groups",
        type=Path,
        default=Path("config/resource-repository-groups.v1.json"),
    )
    parser.add_argument(
        "--course-groups",
        type=Path,
        default=Path("config/course-groups.v1.json"),
    )
    parser.add_argument(
        "--course-families",
        type=Path,
        default=Path("config/course-resource-families.v1.json"),
    )
    parser.add_argument(
        "--course-family-migration",
        type=Path,
        default=Path("config/course-resource-family-migration.v1.json"),
    )
    parser.add_argument(
        "--physical-repository-policy",
        type=Path,
        default=Path("config/physical-repository-policy.v1.json"),
    )
    parser.add_argument("--require-created", action="store_true")
    return parser.parse_args()


def git_paths(repository: Path) -> tuple[str, list[str]]:
    def run(*args: str) -> str:
        return subprocess.run(
            ["git", *args],
            cwd=repository,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=120,
            check=True,
        ).stdout.strip()

    commit = run("rev-parse", "origin/master")
    excluded = {".gitattributes", "LICENSE", "README.md"}
    paths = [
        path
        for path in run("ls-tree", "-r", "--name-only", "origin/master").splitlines()
        if path and path not in excluded
    ]
    return commit, paths


def normalize(value: Any) -> str:
    return unicodedata.normalize("NFKC", str(value or "")).strip()


def source_curriculum(
    plan_dir: Path,
) -> tuple[
    set[tuple[str, int]],
    dict[str, str],
    dict[str, set[str]],
    list[dict[str, str]],
]:
    keys: set[tuple[str, int]] = set()
    code_names: dict[str, str] = {}
    record_ids_by_code: dict[str, set[str]] = {}
    coded_rows: list[dict[str, str]] = []
    for plan_file in sorted(plan_dir.glob("*.toml")):
        document = tomllib.loads(plan_file.read_text(encoding="utf-8"))
        info = document["info"]
        plan_id = normalize(info["plan_ID"])
        for ordinal, course in enumerate(document.get("courses", [])):
            key = (plan_id, ordinal)
            if key in keys:
                raise ValueError(f"源培养方案记录键重复：{key}")
            keys.add(key)
            code = normalize(course.get("course_code"))
            if not code:
                continue
            name = normalize(course.get("course_name"))
            prior = code_names.setdefault(code, name)
            if prior != name:
                raise ValueError(f"源课程代码 {code} 同时对应 {prior!r} 与 {name!r}")
            record_id = "REC-" + hashlib.sha256(
                f"{plan_id}\0{ordinal}".encode("utf-8")
            ).hexdigest()[:16].upper()
            record_ids_by_code.setdefault(code, set()).add(record_id)
            coded_rows.append(
                {
                    "record_id": record_id,
                    "source_plan": plan_id,
                    "school_name": normalize(info.get("school_name")),
                    "major_name": normalize(info.get("major_name")),
                    "plan_version": normalize(info.get("plan_version")),
                    "offering_college": normalize(course.get("offering_college")),
                    "course_category": normalize(course.get("course_category")),
                    "course_nature": normalize(course.get("course_nature")),
                    "course_code": code,
                    "course_name": name,
                }
            )
    return keys, code_names, record_ids_by_code, coded_rows


def _names_to_codes(code_names: dict[str, str]) -> dict[str, set[str]]:
    result: dict[str, set[str]] = {}
    for code, name in code_names.items():
        result.setdefault(name, set()).add(code)
    return result


def require_fields(section: str, items: list[dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    for index, item in enumerate(items):
        missing = [field for field in REQUIRED_FIELDS if field not in item]
        if missing:
            errors.append(f"{section}[{index}] 缺少字段 {missing}")
    return errors


def natural_key(value: str) -> tuple[Any, ...]:
    return tuple(
        int(part) if part.isdigit() else part.casefold()
        for part in re.split(r"(\d+)", value)
    )


def unique(values: list[str] | set[str]) -> list[str]:
    return sorted(set(values), key=natural_key)


def base_resource_groups(
    group_config: dict[str, Any], source_code_names: dict[str, str]
) -> tuple[dict[str, dict[str, Any]], list[str]]:
    errors: list[str] = []
    names_to_codes = _names_to_codes(source_code_names)
    groups: dict[str, dict[str, Any]] = {}
    configured_names: set[str] = set()
    configured_repo_ids: set[str] = set()
    for index, configured in enumerate(group_config.get("groups", [])):
        group_id = normalize(configured.get("group_id"))
        repo_id = normalize(configured.get("repo_id"))
        names = unique(
            {
                normalize(name)
                for name in configured.get("course_names", [])
                if normalize(name)
            }
        )
        codes = unique(
            {
                code
                for name in names
                for code in names_to_codes.get(name, set())
            }
        )
        configured_codes = unique(
            {
                normalize(code)
                for code in configured.get("expected_course_codes", [])
                if normalize(code)
            }
        )
        legacy_units = unique(
            {
                normalize(boundary)
                for boundary in configured.get("legacy_units", [])
                if normalize(boundary)
            }
        )
        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", group_id):
            errors.append(f"基础资源分组 ID 非法：groups[{index}]={group_id!r}")
        if group_id in groups:
            errors.append(f"基础资源分组 ID 重复：{group_id}")
        if repo_id.casefold() in configured_repo_ids:
            errors.append(f"基础资源分组 repo_id 重复：{repo_id}")
        unknown_names = sorted(set(names) - set(names_to_codes))
        if unknown_names:
            errors.append(f"基础资源分组 {group_id} 含未知课程名：{unknown_names}")
        if set(configured_codes) != set(codes):
            errors.append(f"基础资源分组 {group_id} 的课程代码已漂移")
        if repo_id not in codes:
            errors.append(f"基础资源分组 {group_id} 的 repo_id 不是组内代码")
        overlap = configured_names & set(names)
        if overlap:
            errors.append(f"课程名被多个基础资源分组使用：{sorted(overlap)}")
        configured_names.update(names)
        configured_repo_ids.add(repo_id.casefold())
        groups[group_id] = {
            "resource_group_id": group_id,
            "repo_id": repo_id,
            "display_name": normalize(configured.get("display_name")),
            "course_names": names,
            "course_codes": codes,
            "legacy_units": legacy_units,
            "grouping_rule": "explicit-reviewed",
        }

    for name, codes in sorted(names_to_codes.items(), key=lambda item: natural_key(item[0])):
        if name in configured_names:
            continue
        group_id = "exact-name-" + hashlib.sha256(name.encode("utf-8")).hexdigest()[:16]
        sorted_codes = unique(codes)
        groups[group_id] = {
            "resource_group_id": group_id,
            "repo_id": sorted_codes[0],
            "display_name": name,
            "course_names": [name],
            "course_codes": sorted_codes,
            "legacy_units": [],
            "grouping_rule": "exact-normalized-course-name",
        }
    return groups, errors


def resource_group_identity(groups: dict[str, dict[str, Any]]) -> str:
    rows = [
        {
            "resource_group_id": group["resource_group_id"],
            "repo_id": group["repo_id"],
            "course_names": sorted(group["course_names"]),
            "course_codes": sorted(group["course_codes"]),
        }
        for group in groups.values()
    ]
    rows.sort(key=lambda row: row["resource_group_id"])
    encoded = json.dumps(
        rows, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def course_group_source_identity(rows: list[dict[str, str]]) -> str:
    fields = (
        "record_id",
        "source_plan",
        "school_name",
        "major_name",
        "plan_version",
        "course_nature",
        "course_code",
        "course_name",
    )
    snapshot = [{field: row[field] for field in fields} for row in rows]
    snapshot.sort(key=lambda item: (item["source_plan"], item["record_id"]))
    encoded = json.dumps(
        snapshot, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def materialize_course_groups(
    config: dict[str, Any], source_rows: list[dict[str, str]]
) -> tuple[str, list[dict[str, Any]], list[dict[str, Any]]]:
    if config.get("schema_version") != 1:
        raise ValueError("课程组配置 schema_version 不是 1")
    if config.get("role") != "semantic-relatedness-only":
        raise ValueError("课程组配置只能承担语义相关性角色")
    if config.get("overlapping_membership") != "allowed-and-expected":
        raise ValueError("课程组配置必须允许成员重叠")
    if config.get("course_groups_do_not_imply_repository_merge") is not True:
        raise ValueError("课程组配置必须声明不触发资料仓库合并")

    identity = course_group_source_identity(source_rows)
    if config.get("source_curriculum_identity_sha256") != identity:
        raise ValueError("课程组配置的培养方案摘要与源数据不一致")
    definitions = config.get("definitions")
    if not isinstance(definitions, list) or not definitions:
        raise ValueError("课程组配置 definitions 必须是非空数组")

    allowed_fields = {
        "source_plan",
        "school_name",
        "major_name",
        "course_nature",
    }
    allowed_group_types = {"plan-scope", "major-scope", "school-scope"}
    definition_ids: set[str] = set()
    group_ids: set[str] = set()
    groups: list[dict[str, Any]] = []
    memberships: list[dict[str, Any]] = []

    for index, definition in enumerate(definitions):
        if not isinstance(definition, dict):
            raise ValueError(f"课程组 definitions[{index}] 必须是对象")
        definition_id = normalize(definition.get("definition_id"))
        group_type = normalize(definition.get("group_type"))
        scope_fields = definition.get("scope_fields")
        raw_conditions = definition.get("scope_conditions")
        display_name_template = normalize(definition.get("display_name_template"))
        membership_semantics = normalize(definition.get("membership_semantics"))
        evidence = normalize(definition.get("evidence"))
        minimum_codes = definition.get("minimum_distinct_course_codes")

        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", definition_id):
            raise ValueError(f"课程组定义 ID 非法：{definition_id!r}")
        if definition_id in definition_ids:
            raise ValueError(f"课程组定义 ID 重复：{definition_id}")
        if group_type not in allowed_group_types:
            raise ValueError(f"课程组 {definition_id} 的 group_type 非法")
        if (
            not isinstance(scope_fields, list)
            or not scope_fields
            or len(scope_fields) != len(set(scope_fields))
            or set(scope_fields) - allowed_fields
        ):
            raise ValueError(f"课程组 {definition_id} 的 scope_fields 非法")
        if not isinstance(raw_conditions, dict):
            raise ValueError(f"课程组 {definition_id} 的 scope_conditions 必须是对象")
        if set(raw_conditions) - allowed_fields or set(raw_conditions) & set(scope_fields):
            raise ValueError(f"课程组 {definition_id} 的 scope_conditions 字段非法")
        if not display_name_template or not membership_semantics or not evidence:
            raise ValueError(f"课程组 {definition_id} 缺少展示名、成员语义或证据")
        if not isinstance(minimum_codes, int) or minimum_codes < 2:
            raise ValueError(f"课程组 {definition_id} 的最小课程代码数非法")

        conditions: dict[str, list[str]] = {}
        for field, raw_values in raw_conditions.items():
            if not isinstance(raw_values, list) or not raw_values:
                raise ValueError(f"课程组 {definition_id} 的条件 {field} 必须是非空数组")
            values = unique(
                {normalize(value) for value in raw_values if normalize(value)}
            )
            if not values:
                raise ValueError(f"课程组 {definition_id} 的条件 {field} 不能为空")
            conditions[field] = values

        buckets: dict[tuple[str, ...], list[dict[str, str]]] = defaultdict(list)
        for row in source_rows:
            if any(row[field] not in values for field, values in conditions.items()):
                continue
            buckets[tuple(row[field] for field in scope_fields)].append(row)

        materialized_count = 0
        for scope_values, member_rows in sorted(buckets.items()):
            course_codes = unique({row["course_code"] for row in member_rows})
            if len(course_codes) < minimum_codes:
                continue
            scope = dict(zip(scope_fields, scope_values, strict=True))
            relation_key = json.dumps(
                {"definition_id": definition_id, "scope": scope},
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )
            group_id = "course-group-" + hashlib.sha256(
                relation_key.encode("utf-8")
            ).hexdigest()[:16]
            if group_id in group_ids:
                raise ValueError(f"课程组 ID 冲突：{group_id}")

            template_values = dict(member_rows[0])
            template_values.update(scope)
            try:
                display_name = normalize(display_name_template.format_map(template_values))
            except KeyError as error:
                raise ValueError(
                    f"课程组 {definition_id} 的展示名引用未知字段：{error.args[0]}"
                ) from error
            record_ids = unique({row["record_id"] for row in member_rows})
            membership_snapshot = sorted(
                (
                    {"record_id": row["record_id"], "course_code": row["course_code"]}
                    for row in member_rows
                ),
                key=lambda item: (item["course_code"], item["record_id"]),
            )
            membership_identity = hashlib.sha256(
                json.dumps(
                    membership_snapshot,
                    ensure_ascii=False,
                    separators=(",", ":"),
                    sort_keys=True,
                ).encode("utf-8")
            ).hexdigest()
            effective_conditions: dict[str, Any] = dict(scope)
            effective_conditions.update(conditions)
            groups.append(
                {
                    "course_group_id": group_id,
                    "definition_id": definition_id,
                    "group_type": group_type,
                    "display_name": display_name,
                    "scope_conditions": effective_conditions,
                    "relation_key": relation_key,
                    "membership_semantics": membership_semantics,
                    "evidence": evidence,
                    "course_codes": course_codes,
                    "record_ids": record_ids,
                    "membership_identity_sha256": membership_identity,
                }
            )
            rows_by_code: dict[str, list[dict[str, str]]] = defaultdict(list)
            for row in member_rows:
                rows_by_code[row["course_code"]].append(row)
            memberships.extend(
                {
                    "course_group_id": group_id,
                    "descriptor_id": f"course-code:{course_code}",
                    "course_code": course_code,
                    "record_ids": unique(
                        {row["record_id"] for row in rows_by_code[course_code]}
                    ),
                }
                for course_code in course_codes
            )
            group_ids.add(group_id)
            materialized_count += 1

        if materialized_count != definition.get("expected_group_count"):
            raise ValueError(
                f"课程组定义 {definition_id} 的组数已漂移："
                f"{materialized_count} != {definition.get('expected_group_count')}"
            )
        definition_ids.add(definition_id)

    if len(groups) != config.get("expected_course_group_count"):
        raise ValueError("课程组总数与配置不一致")
    covered_codes = {code for group in groups for code in group["course_codes"]}
    source_codes = {row["course_code"] for row in source_rows}
    if (
        len(covered_codes) != config.get("expected_distinct_course_code_count")
        or covered_codes != source_codes
    ):
        raise ValueError("课程组未完整覆盖源课程代码")

    groups.sort(key=lambda item: item["course_group_id"])
    memberships.sort(
        key=lambda item: (item["course_group_id"], natural_key(item["course_code"]))
    )
    return identity, groups, memberships


def validate_course_group_contract(
    *,
    manifest: dict[str, Any],
    config: dict[str, Any],
    source_rows: list[dict[str, str]],
) -> list[str]:
    errors: list[str] = []
    try:
        identity, expected_groups, expected_memberships = materialize_course_groups(
            config, source_rows
        )
    except (KeyError, TypeError, ValueError) as error:
        return [f"课程组配置无法物化：{error}"]

    actual_groups = manifest.get("course_groups")
    actual_memberships = manifest.get("course_group_memberships")
    if actual_groups != expected_groups:
        errors.append("manifest 课程组与独立重算结果不一致")
    if actual_memberships != expected_memberships:
        errors.append("manifest 课程组成员关系与独立重算结果不一致")
    if not isinstance(actual_groups, list) or not isinstance(actual_memberships, list):
        return errors
    if any(
        "repo_id" in item or "resource_group_id" in item
        for item in [*actual_groups, *actual_memberships]
        if isinstance(item, dict)
    ):
        errors.append("课程组不得携带资料仓库或资源分组归属")

    source = manifest.get("sources", {}).get("course_groups", {})
    expected_source = {
        "schema_version": 1,
        "source_curriculum_identity_sha256": identity,
        "definition_count": len(config["definitions"]),
        "course_group_count": len(expected_groups),
        "membership_count": len(expected_memberships),
        "overlapping_membership": "allowed-and-expected",
        "course_groups_do_not_imply_repository_merge": True,
    }
    for field, expected in expected_source.items():
        if source.get(field) != expected:
            errors.append(f"manifest 课程组来源字段错误：{field}")

    memberships_by_code: dict[str, set[str]] = defaultdict(set)
    for membership in expected_memberships:
        memberships_by_code[membership["course_code"]].add(
            membership["course_group_id"]
        )
    summary = manifest.get("summary", {})
    expected_summary = {
        "course_group_count": len(expected_groups),
        "course_group_membership_count": len(expected_memberships),
        "course_codes_in_multiple_course_groups": sum(
            len(group_ids) > 1 for group_ids in memberships_by_code.values()
        ),
    }
    for field, expected in expected_summary.items():
        if summary.get(field) != expected:
            errors.append(f"manifest summary 的课程组字段错误：{field}")
    return errors


def validate_course_family_contract(
    *,
    manifest: dict[str, Any],
    group_config: dict[str, Any],
    family_config: dict[str, Any],
    migration_config: dict[str, Any],
    candidate_report: dict[str, Any],
    source_code_names: dict[str, str],
    resource_groups: list[dict[str, Any]],
    legacy_units: list[dict[str, Any]],
) -> list[str]:
    errors: list[str] = []
    base_groups, base_errors = base_resource_groups(
        group_config, source_code_names
    )
    errors.extend(base_errors)
    identity = resource_group_identity(base_groups)
    family_source = manifest.get("sources", {}).get("course_families", {})
    stage_id = family_source.get("stage_id")
    if family_config.get("schema_version") != 1:
        errors.append("课程族配置 schema_version 不是 1")
    if migration_config.get("schema_version") != 1:
        errors.append("课程族迁移映射 schema_version 不是 1")
    if family_config.get("policy", {}).get("runtime_heuristics") != (
        "disabled-materialized-membership-only"
    ):
        errors.append("课程族配置未禁用运行期启发式")
    if family_config.get("role") != "exclusive-resource-repository-policy":
        errors.append("课程族配置未声明互斥的资料仓库策略角色")
    if family_config.get("course_groups_do_not_imply_repository_merge") is not True:
        errors.append("课程族配置未声明 CourseGroup 不触发资料仓库合并")
    if family_config.get("source_resource_group_count") != len(base_groups):
        errors.append("课程族配置的基础资源分组数量错误")
    for section, value in (
        ("配置", family_config),
        ("迁移映射", migration_config),
        ("候选报告", candidate_report),
    ):
        if value.get("source_resource_group_identity_sha256") != identity:
            errors.append(f"课程族{section}的基础资源分组摘要错误")
    if family_source.get("source_resource_group_identity_sha256") != identity:
        errors.append("manifest 记录的基础资源分组摘要错误")

    stages = {
        stage.get("stage_id"): stage
        for stage in family_config.get("stages", [])
    }
    migration_stages = {
        stage.get("stage_id"): stage
        for stage in migration_config.get("stages", [])
    }
    stage = stages.get(stage_id)
    migration_stage = migration_stages.get(stage_id)
    if not stage or not migration_stage:
        errors.append(f"manifest 引用了未知课程族阶段：{stage_id!r}")
        return errors

    member_to_family: dict[str, dict[str, Any]] = {}
    family_ids: set[str] = set()
    family_repo_ids: set[str] = set()
    for family in stage.get("families", []):
        family_id = family.get("family_id")
        repo_id = family.get("repo_id")
        member_ids = family.get("member_group_ids", [])
        expected_family_id = "course-family-" + hashlib.sha256(
            f"repo:{str(repo_id).casefold()}".encode("utf-8")
        ).hexdigest()[:16]
        if family_id != expected_family_id:
            errors.append(f"课程族 ID 与稳定目标仓库不一致：{family_id}")
        if family_id in family_ids:
            errors.append(f"课程族 ID 重复：{family_id}")
        if len(member_ids) < 2 or len(member_ids) != len(set(member_ids)):
            errors.append(f"课程族成员无效：{family_id}")
        unknown = sorted(set(member_ids) - set(base_groups))
        if unknown:
            errors.append(f"课程族 {family_id} 引用未知基础组：{unknown}")
            continue
        if any(member_id in member_to_family for member_id in member_ids):
            errors.append(f"课程族成员重叠：{family_id}")
        members = [base_groups[member_id] for member_id in member_ids]
        names = unique(
            {name for member in members for name in member["course_names"]}
        )
        codes = unique(
            {code for member in members for code in member["course_codes"]}
        )
        boundaries = unique(
            {unit for member in members for unit in member["legacy_units"]}
        )
        source_repo_ids = unique([member["repo_id"] for member in members])
        for field, actual in (
            ("expected_course_names", names),
            ("expected_course_codes", codes),
            ("expected_legacy_units", boundaries),
            ("expected_source_repo_ids", source_repo_ids),
        ):
            if set(family.get(field, [])) != set(actual):
                errors.append(f"课程族 {family_id} 的 {field} 已漂移")
        if repo_id not in codes or repo_id in FROZEN_LEGACY_REPO_IDS:
            errors.append(f"课程族目标仓库无效：{family_id} -> {repo_id}")
        if str(repo_id).casefold() in family_repo_ids:
            errors.append(f"课程族目标仓库重复：{repo_id}")
        family_ids.add(family_id)
        family_repo_ids.add(str(repo_id).casefold())
        for member_id in member_ids:
            member_to_family[member_id] = family

    expected_groups: dict[str, dict[str, Any]] = {}
    for base_group_id, base_group in base_groups.items():
        family = member_to_family.get(base_group_id)
        if not family:
            expected_groups[base_group_id] = base_group
            continue
        family_id = family["family_id"]
        if family_id in expected_groups:
            continue
        members = [
            base_groups[member_id]
            for member_id in family["member_group_ids"]
        ]
        expected_groups[family_id] = {
            "resource_group_id": family_id,
            "preferred_repo_id": family["repo_id"],
            "display_name": family["display_name"],
            "course_names": unique(
                {name for member in members for name in member["course_names"]}
            ),
            "course_codes": unique(
                {code for member in members for code in member["course_codes"]}
            ),
            "legacy_units": unique(
                {unit for member in members for unit in member["legacy_units"]}
            ),
            "grouping_rule": "materialized-course-family",
            "member_resource_group_ids": unique(family["member_group_ids"]),
            "source_repo_ids": unique(family["expected_source_repo_ids"]),
            "approval": family["approval"],
        }

    actual_groups = {
        group.get("resource_group_id"): group for group in resource_groups
    }
    if set(actual_groups) != set(expected_groups):
        errors.append(
            "manifest 课程族资源组集合不一致："
            f"missing={len(set(expected_groups) - set(actual_groups))}, "
            f"extra={len(set(actual_groups) - set(expected_groups))}"
        )
    for group_id, expected in expected_groups.items():
        actual = actual_groups.get(group_id)
        if not actual:
            continue
        expected_preferred_repo_id = expected.get(
            "preferred_repo_id", expected.get("repo_id")
        )
        if actual.get("preferred_repo_id") != expected_preferred_repo_id:
            errors.append(f"课程族资源组 {group_id} 的 preferred_repo_id 错误")
        for field in (
            "display_name",
            "grouping_rule",
            "approval",
        ):
            if field in expected and actual.get(field) != expected.get(field):
                errors.append(f"课程族资源组 {group_id} 的 {field} 错误")
        for field in (
            "course_names",
            "course_codes",
            "legacy_units",
            "member_resource_group_ids",
            "source_repo_ids",
        ):
            if field in expected and set(actual.get(field, [])) != set(expected[field]):
                errors.append(f"课程族资源组 {group_id} 的 {field} 错误")

    mappings = migration_stage.get("mappings", [])
    by_source_group = {
        mapping.get("source_resource_group_id"): mapping for mapping in mappings
    }
    source_repo_ids = [mapping.get("source_repo_id") for mapping in mappings]
    if (
        len(mappings) != len(base_groups)
        or len(by_source_group) != len(base_groups)
        or len(set(source_repo_ids)) != len(base_groups)
    ):
        errors.append("课程族迁移映射未将每个基础组和 repo_id 恰好覆盖一次")
    for base_group_id, base_group in base_groups.items():
        mapping = by_source_group.get(base_group_id)
        family = member_to_family.get(base_group_id)
        target_group_id = family["family_id"] if family else base_group_id
        target_repo_id = family["repo_id"] if family else base_group["repo_id"]
        expected_action = (
            "retain" if target_repo_id == base_group["repo_id"] else "merge"
        )
        if not mapping or (
            mapping.get("source_repo_id") != base_group["repo_id"]
            or mapping.get("target_resource_group_id") != target_group_id
            or mapping.get("target_repo_id") != target_repo_id
            or mapping.get("action") != expected_action
            or target_group_id not in expected_groups
            or target_repo_id in FROZEN_LEGACY_REPO_IDS
        ):
            errors.append(f"课程族迁移映射错误：{base_group_id}")

    expected_reduction = len(base_groups) - len(expected_groups)
    if stage.get("expected_output_resource_group_count") != len(expected_groups):
        errors.append("课程族配置的阶段输出数量错误")
    if stage.get("expected_course_repository_reduction") != expected_reduction:
        errors.append("课程族配置的仓库减量错误")
    if migration_stage.get("target_course_repository_count") != len(expected_groups):
        errors.append("课程族迁移映射的目标数量错误")
    if family_source.get("base_resource_group_count") != len(base_groups):
        errors.append("manifest 记录的基础资源组数量错误")
    if family_source.get("result_resource_group_count") != len(expected_groups):
        errors.append("manifest 记录的课程族结果数量错误")
    if family_source.get("course_repository_reduction") != expected_reduction:
        errors.append("manifest 记录的课程仓库减量错误")
    if manifest.get("summary", {}).get("course_repository_reduction") != expected_reduction:
        errors.append("manifest summary 的课程仓库减量错误")
    if manifest.get("summary", {}).get("course_family_group_count") != len(
        stage.get("families", [])
    ):
        errors.append("manifest summary 的课程族数量错误")

    legacy_by_boundary = {
        normalize(unit.get("legacy_unit")): unit for unit in legacy_units
    }
    for base_group_id, base_group in base_groups.items():
        family = member_to_family.get(base_group_id)
        target_group_id = family["family_id"] if family else base_group_id
        for boundary in base_group["legacy_units"]:
            unit = legacy_by_boundary.get(normalize(boundary))
            if not unit:
                errors.append(f"基础资源分组引用不存在历史单元：{boundary}")
            elif unit.get("resource_group_id") != target_group_id:
                errors.append(f"历史单元未绑定合并后的逻辑资源组：{boundary}")

    if candidate_report.get("runtime_recomputation") is not False:
        errors.append("课程族候选报告未声明禁用运行期重算")
    report_stages = {
        "approved-candidates": candidate_report.get("approved_candidate_stage", {}),
        "aggressive-policy": candidate_report.get("aggressive_stage", {}),
    }
    for configured_stage_id, configured_stage in stages.items():
        report_stage = report_stages.get(configured_stage_id, {})
        if report_stage.get("course_repository_reduction") != configured_stage.get(
            "expected_course_repository_reduction"
        ):
            errors.append(f"课程族候选报告阶段减量错误：{configured_stage_id}")
        if report_stage.get("result_resource_group_count") != configured_stage.get(
            "expected_output_resource_group_count"
        ):
            errors.append(f"课程族候选报告阶段数量错误：{configured_stage_id}")
        configured_ids = {
            family.get("family_id") for family in configured_stage.get("families", [])
        }
        report_ids = {
            family.get("family_id") for family in report_stage.get("families", [])
        }
        if configured_ids != report_ids:
            errors.append(f"课程族候选报告成员集合错误：{configured_stage_id}")
    return errors

def physical_resource_group_identity(groups: list[dict[str, Any]]) -> str:
    rows = [
        {
            "resource_group_id": group.get("resource_group_id"),
            "preferred_repo_id": group.get("preferred_repo_id"),
            "course_codes": sorted(group.get("course_codes", [])),
        }
        for group in groups
    ]
    rows.sort(key=lambda row: str(row["resource_group_id"]))
    encoded = json.dumps(
        rows, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def canonical_owner(
    rows: list[dict[str, str]], fields: tuple[str, ...], missing_value: str
) -> dict[str, Any]:
    paths = [
        tuple(normalize(row.get(field)) or missing_value for field in fields)
        for row in rows
    ]
    if not paths:
        raise ValueError("资源分组没有可用于 canonical owner 的培养方案记录")
    depth = 0
    while depth < len(fields) and len({path[depth] for path in paths}) == 1:
        depth += 1
    owner_path = paths[0][:depth]
    return {
        "depth": depth,
        "path": [
            {"field": field, "value": value}
            for field, value in zip(fields[:depth], owner_path, strict=True)
        ],
    }


def physical_assignment(
    *,
    group: dict[str, Any],
    owner: dict[str, Any],
    fields: tuple[str, ...],
    minimum_shared_depth: int,
    frontier_depth: int,
) -> dict[str, Any]:
    group_id = group["resource_group_id"]
    if owner["depth"] < minimum_shared_depth:
        kind = "dedicated-resource-group"
        owner_scope = owner
        relation_value = {"kind": kind, "resource_group_id": group_id}
        repo_id = group["preferred_repo_id"]
    else:
        kind = "hierarchy-node"
        owner_scope = {
            "depth": frontier_depth,
            "path": owner["path"][:frontier_depth],
        }
        relation_value = {"kind": kind, "canonical_owner": owner_scope}
        relation_preview = json.dumps(
            relation_value,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        )
        repo_id = "COURSES-" + hashlib.sha256(
            relation_preview.encode("utf-8")
        ).hexdigest()[:12].upper()
    relation = json.dumps(
        relation_value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    return {
        "resource_group_id": group_id,
        "physical_repository_id": "physical-course-"
        + hashlib.sha256(relation.encode("utf-8")).hexdigest()[:16],
        "repo_id": repo_id,
        "materialization_kind": kind,
        "canonical_owner": owner,
        "owner_scope": owner_scope,
    }


def validate_physical_repository_contract(
    *,
    manifest: dict[str, Any],
    config: dict[str, Any],
    source_rows: list[dict[str, str]],
) -> list[str]:
    errors: list[str] = []
    resource_groups = manifest.get("resource_groups", [])
    repositories = manifest.get("repositories", [])
    memberships = manifest.get("course_group_memberships", [])
    descriptors = manifest.get("course_descriptors", [])
    records = manifest.get("curriculum_records", [])
    legacy_units = manifest.get("legacy_units", [])

    if config.get("schema_version") != 1:
        return ["物理仓库策略 schema_version 不是 1"]
    if config.get("role") != "exclusive-physical-repository-policy":
        errors.append("物理仓库策略未声明互斥物理归属角色")
    if config.get("course_groups_are_navigation_evidence_only") is not True:
        errors.append("物理仓库策略未声明 CourseGroup 仅作导航证据")
    if config.get("automatic_remapping") is not False:
        errors.append("物理仓库策略未禁用运行期自动重映射")

    canonical = config.get("canonical_owner", {})
    raw_fields = canonical.get("hierarchy_fields")
    allowed_fields = {
        "offering_college",
        "school_name",
        "major_name",
        "course_category",
    }
    if (
        canonical.get("algorithm")
        != "longest-common-prefix-over-curriculum-records"
        or not isinstance(raw_fields, list)
        or not raw_fields
        or len(raw_fields) != len(set(raw_fields))
        or set(raw_fields) - allowed_fields
    ):
        return [*errors, "物理仓库 canonical owner 配置非法"]
    fields = tuple(raw_fields)
    missing_value = normalize(canonical.get("missing_value")) or "未标注"
    minimum_shared_depth = canonical.get("minimum_shared_depth")
    frontier_depth = config.get("materialization", {}).get(
        "shared_frontier_depth"
    )
    if (
        not isinstance(minimum_shared_depth, int)
        or not isinstance(frontier_depth, int)
        or minimum_shared_depth < 1
        or frontier_depth != minimum_shared_depth
        or frontier_depth > len(fields)
    ):
        return [*errors, "物理仓库共享前沿深度非法"]

    source = manifest.get("sources", {}).get("physical_repositories", {})
    family_stage_id = manifest.get("sources", {}).get("course_families", {}).get(
        "stage_id"
    )
    stage_id = source.get("stage_id")
    stages = {
        stage.get("stage_id"): stage for stage in config.get("stages", [])
    }
    stage = stages.get(stage_id)
    if not stage or stage_id != family_stage_id:
        return [*errors, f"manifest 引用了未知或错配的物理仓库阶段：{stage_id!r}"]

    identity = physical_resource_group_identity(resource_groups)
    if stage.get("source_resource_group_identity_sha256") != identity:
        errors.append("物理仓库策略属于另一组生效 ResourceGroup")
    if source.get("source_resource_group_identity_sha256") != identity:
        errors.append("manifest 记录的物理仓库源摘要错误")
    if stage.get("expected_resource_group_count") != len(resource_groups):
        errors.append("物理仓库策略的 ResourceGroup 数量已漂移")

    rows_by_code: dict[str, list[dict[str, str]]] = defaultdict(list)
    for row in source_rows:
        rows_by_code[row["course_code"]].append(row)
    navigation_by_code: dict[str, set[str]] = defaultdict(set)
    for membership in memberships:
        navigation_by_code[membership.get("course_code")].add(
            membership.get("course_group_id")
        )

    assignments: dict[str, dict[str, Any]] = {}
    buckets: dict[str, dict[str, Any]] = {}
    owner_depth_counts: Counter[int] = Counter()
    for group in resource_groups:
        group_id = group.get("resource_group_id")
        preferred_repo_id = group.get("preferred_repo_id")
        if not group_id or not preferred_repo_id:
            errors.append(f"ResourceGroup 缺少逻辑或首选仓库身份：{group_id!r}")
            continue
        member_rows = [
            row
            for code in group.get("course_codes", [])
            for row in rows_by_code.get(code, [])
        ]
        try:
            owner = canonical_owner(member_rows, fields, missing_value)
        except ValueError as error:
            errors.append(f"ResourceGroup {group_id} 无法重算 canonical owner：{error}")
            continue
        owner_depth_counts[owner["depth"]] += 1
        expected = physical_assignment(
            group=group,
            owner=owner,
            fields=fields,
            minimum_shared_depth=minimum_shared_depth,
            frontier_depth=frontier_depth,
        )
        expected_navigation = unique(
            {
                course_group_id
                for code in group.get("course_codes", [])
                for course_group_id in navigation_by_code.get(code, set())
            }
        )
        expected["navigation_course_group_ids"] = expected_navigation
        assignments[group_id] = expected
        if group.get("canonical_owner") != owner:
            errors.append(f"ResourceGroup canonical owner 错误：{group_id}")
        if group.get("physical_repository_id") != expected["physical_repository_id"]:
            errors.append(f"ResourceGroup 物理仓库 ID 错误：{group_id}")
        if group.get("repo_id") != expected["repo_id"]:
            errors.append(f"ResourceGroup 物理 repo_id 错误：{group_id}")
        expected_lineage = {
            "semantics": "navigation-evidence-only",
            "course_group_ids": expected_navigation,
        }
        if group.get("split_lineage") != expected_lineage:
            errors.append(f"ResourceGroup split lineage 错误：{group_id}")

        bucket = buckets.setdefault(
            expected["physical_repository_id"],
            {
                "physical_repository_id": expected["physical_repository_id"],
                "repo_id": expected["repo_id"],
                "materialization_kind": expected["materialization_kind"],
                "canonical_owner": expected["owner_scope"],
                "member_resource_group_ids": [],
                "course_codes": [],
                "preferred_source_repo_ids": [],
                "split_lineage_course_group_ids": [],
            },
        )
        bucket["member_resource_group_ids"].append(group_id)
        bucket["course_codes"].extend(group.get("course_codes", []))
        bucket["preferred_source_repo_ids"].append(preferred_repo_id)
        bucket["split_lineage_course_group_ids"].extend(expected_navigation)

    expected_depth_counts = {
        str(depth): count for depth, count in sorted(owner_depth_counts.items())
    }
    configured_depth_counts = {
        str(depth): count
        for depth, count in stage.get(
            "expected_canonical_owner_depth_counts", {}
        ).items()
    }
    if expected_depth_counts != configured_depth_counts:
        errors.append("物理仓库 canonical owner 深度分布已漂移")
    if manifest.get("summary", {}).get("canonical_owner_depth_counts") != (
        expected_depth_counts
    ):
        errors.append("manifest summary 的 canonical owner 深度分布错误")

    expected_by_id: dict[str, dict[str, Any]] = {}
    for physical_id, bucket in buckets.items():
        expected_by_id[physical_id] = {
            **bucket,
            "member_resource_group_ids": unique(
                bucket["member_resource_group_ids"]
            ),
            "course_codes": unique(bucket["course_codes"]),
            "preferred_source_repo_ids": unique(
                bucket["preferred_source_repo_ids"]
            ),
            "split_lineage_course_group_ids": unique(
                bucket["split_lineage_course_group_ids"]
            ),
        }
    physical_repositories = [
        repository
        for repository in repositories
        if repository.get("repo_type") == "course"
        and repository.get("physical_repository_id")
    ]
    actual_by_id = {
        repository.get("physical_repository_id"): repository
        for repository in physical_repositories
    }
    if len(actual_by_id) != len(physical_repositories):
        errors.append("physical_repository_id 不唯一")
    if set(actual_by_id) != set(expected_by_id):
        errors.append(
            "manifest 物理课程仓库集合与独立重算结果不一致："
            f"missing={len(set(expected_by_id) - set(actual_by_id))}, "
            f"extra={len(set(actual_by_id) - set(expected_by_id))}"
        )
    for physical_id, expected in expected_by_id.items():
        actual = actual_by_id.get(physical_id)
        if not actual:
            continue
        if "resource_group_id" in actual:
            errors.append(f"物理课程仓库携带单值 resource_group_id：{physical_id}")
        for field in (
            "repo_id",
            "materialization_kind",
            "canonical_owner",
        ):
            if actual.get(field) != expected[field]:
                errors.append(f"物理课程仓库 {physical_id} 的 {field} 错误")
        for field in (
            "member_resource_group_ids",
            "course_codes",
            "preferred_source_repo_ids",
            "split_lineage_course_group_ids",
        ):
            if actual.get(field) != expected[field]:
                errors.append(f"物理课程仓库 {physical_id} 的 {field} 错误")

    expected_count = stage.get("expected_physical_course_repository_count")
    if len(expected_by_id) != expected_count:
        errors.append("物理课程仓库数量与策略不一致")
    if source.get("physical_course_repository_count") != expected_count:
        errors.append("manifest 记录的物理课程仓库数量错误")
    summary = manifest.get("summary", {})
    if summary.get("physical_course_repository_count") != expected_count:
        errors.append("manifest summary 的物理课程仓库数量错误")
    if summary.get("physical_repository_reduction") != len(resource_groups) - len(
        expected_by_id
    ):
        errors.append("manifest summary 的物理仓库减量错误")
    if len(repositories) != stage.get("expected_total_repository_count"):
        errors.append("manifest 总仓库数量与物理仓库策略不一致")

    for descriptor in descriptors:
        assignment = assignments.get(descriptor.get("resource_group_id"))
        if assignment and (
            descriptor.get("physical_repository_id")
            != assignment["physical_repository_id"]
            or descriptor.get("repo_id") != assignment["repo_id"]
        ):
            errors.append(f"descriptor 物理仓库绑定错误：{descriptor.get('course_code')}")
    for record in records:
        group_id = record.get("resource_group_id")
        assignment = assignments.get(group_id)
        if assignment and (
            record.get("physical_repository_id")
            != assignment["physical_repository_id"]
            or record.get("repo_id") != assignment["repo_id"]
        ):
            errors.append(f"培养方案记录物理仓库绑定错误：{record.get('record_id')}")
        if not group_id and record.get("physical_repository_id") is not None:
            errors.append(f"无代码记录意外绑定物理仓库：{record.get('record_id')}")
    for unit in legacy_units:
        group_id = unit.get("resource_group_id")
        assignment = assignments.get(group_id)
        if assignment and (
            unit.get("physical_repository_id")
            != assignment["physical_repository_id"]
            or unit.get("repo_id") != assignment["repo_id"]
        ):
            errors.append(f"历史单元物理仓库绑定错误：{unit.get('legacy_unit')}")
        if not group_id and unit.get("physical_repository_id") is not None:
            errors.append(f"非课程历史单元意外绑定物理仓库：{unit.get('legacy_unit')}")

    expected_source = {
        "schema_version": 1,
        "role": "exclusive-physical-repository-policy",
        "stage_id": stage_id,
        "source_resource_group_identity_sha256": identity,
        "resource_group_count": len(resource_groups),
        "physical_course_repository_count": len(expected_by_id),
        "canonical_owner_algorithm": canonical["algorithm"],
        "hierarchy_fields": list(fields),
        "shared_frontier_depth": frontier_depth,
        "course_groups_are_navigation_evidence_only": True,
    }
    for field, expected in expected_source.items():
        if source.get(field) != expected:
            errors.append(f"manifest 物理仓库来源字段错误：{field}")
    if manifest.get("policy", {}).get(
        "course_groups_are_navigation_evidence_only"
    ) is not True:
        errors.append("manifest 未声明 CourseGroup 仅作导航证据")
    return errors


def main() -> int:
    args = parse_args()
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    group_config = json.loads(args.resource_groups.read_text(encoding="utf-8"))
    course_group_config = json.loads(args.course_groups.read_text(encoding="utf-8"))
    family_config = json.loads(args.course_families.read_text(encoding="utf-8"))
    migration_config = json.loads(
        args.course_family_migration.read_text(encoding="utf-8")
    )
    physical_config = json.loads(
        args.physical_repository_policy.read_text(encoding="utf-8")
    )
    candidate_report_path = Path("data/course-resource-family-candidates.v1.json")
    candidate_report = json.loads(candidate_report_path.read_text(encoding="utf-8"))
    errors: list[str] = []

    if manifest.get("schema_version") != 2:
        errors.append(f"manifest schema_version 不是 2：{manifest.get('schema_version')}")
    repositories = manifest.get("repositories", [])
    resource_groups = manifest.get("resource_groups", [])
    descriptors = manifest.get("course_descriptors", [])
    records = manifest.get("curriculum_records", [])
    legacy_roots = manifest.get("legacy_roots", [])
    legacy_units = manifest.get("legacy_units", [])

    for section, items in (
        ("repositories", repositories),
        ("curriculum_records", records),
        ("legacy_roots", legacy_roots),
        ("legacy_units", legacy_units),
    ):
        errors.extend(require_fields(section, items))

    repo_ids = [repo.get("repo_id", "") for repo in repositories]
    repo_id_counts = Counter(repo_id.casefold() for repo_id in repo_ids)
    duplicates = sorted(repo_id for repo_id, count in repo_id_counts.items() if count > 1)
    if duplicates:
        errors.append(f"repo_id 大小写冲突：{duplicates}")
    for repo_id in repo_ids:
        if len(repo_id) > 100 or not re.fullmatch(r"[A-Za-z0-9._-]+", repo_id):
            errors.append(f"非法 repo_id：{repo_id}")
    repo_set = set(repo_ids)
    repository_index = {repo.get("repo_id"): repo for repo in repositories}
    template_ids = {
        repo.get("template_id")
        for repo in repositories
        if repo.get("template_id") is not None
    }
    legacy_references = sorted((repo_set | template_ids) & FROZEN_LEGACY_REPO_IDS)
    if legacy_references:
        errors.append(f"manifest v2 引用了冻结旧世代仓库：{legacy_references}")
    actual_control_ids = {
        repo.get("repo_id")
        for repo in repositories
        if repo.get("repo_type") in {"control", "template"}
    }
    if actual_control_ids != EXPECTED_CONTROL_TEMPLATE_REPO_IDS:
        errors.append(
            "v2 控制面仓库不完整："
            f"missing={sorted(EXPECTED_CONTROL_TEMPLATE_REPO_IDS - actual_control_ids)}, "
            f"extra={sorted(actual_control_ids - EXPECTED_CONTROL_TEMPLATE_REPO_IDS)}"
        )
    authority = manifest.get("policy", {}).get("authority")
    if authority != REGISTRY_REPO_ID:
        errors.append(f"manifest v2 权威注册表错误：{authority!r}")

    source_keys, source_code_names, source_record_ids_by_code, source_rows = source_curriculum(
        args.plans
    )
    errors.extend(
        validate_course_group_contract(
            manifest=manifest,
            config=course_group_config,
            source_rows=source_rows,
        )
    )
    errors.extend(
        validate_course_family_contract(
            manifest=manifest,
            group_config=group_config,
            family_config=family_config,
            migration_config=migration_config,
            candidate_report=candidate_report,
            source_code_names=source_code_names,
            resource_groups=resource_groups,
            legacy_units=legacy_units,
        )
    )
    errors.extend(
        validate_physical_repository_contract(
            manifest=manifest,
            config=physical_config,
            source_rows=source_rows,
        )
    )
    manifest_keys = {
        (record.get("source_plan"), record.get("source_ordinal")) for record in records
    }
    if manifest_keys != source_keys:
        errors.append(
            "培养方案覆盖不一致："
            f"missing={len(source_keys - manifest_keys)}, "
            f"extra={len(manifest_keys - source_keys)}"
        )
    if len(records) != len(manifest_keys):
        errors.append("manifest 中存在重复培养方案记录")

    for section, items in (
        ("curriculum_records", records),
        ("legacy_roots", legacy_roots),
        ("legacy_units", legacy_units),
    ):
        for index, item in enumerate(items):
            if item.get("repo_id") not in repo_set:
                errors.append(
                    f"{section}[{index}] 指向不存在仓库 {item.get('repo_id')!r}"
                )
            if item.get("status") in {None, "", "unmapped", "unknown"}:
                errors.append(f"{section}[{index}] 未映射")

    descriptor_codes = [descriptor.get("course_code") for descriptor in descriptors]
    descriptor_ids = [descriptor.get("descriptor_id") for descriptor in descriptors]
    if len(descriptor_codes) != len(set(descriptor_codes)):
        errors.append("课程代码 descriptor 不唯一")
    if len(descriptor_ids) != len(set(descriptor_ids)):
        errors.append("descriptor_id 不唯一")
    if set(descriptor_codes) != set(source_code_names):
        errors.append(
            "descriptor 课程代码覆盖不一致："
            f"missing={len(set(source_code_names) - set(descriptor_codes))}, "
            f"extra={len(set(descriptor_codes) - set(source_code_names))}"
        )
    descriptor_index = {descriptor.get("course_code"): descriptor for descriptor in descriptors}
    for code, source_name in source_code_names.items():
        descriptor = descriptor_index.get(code)
        if not descriptor:
            continue
        if descriptor.get("descriptor_id") != f"course-code:{code}":
            errors.append(f"descriptor_id 错误：{code}")
        if descriptor.get("course_name") != source_name:
            errors.append(f"descriptor 课程名错误：{code}")
        if descriptor.get("repo_id") not in repo_set:
            errors.append(f"descriptor 指向不存在仓库：{code}")
        if set(descriptor.get("record_ids", [])) != source_record_ids_by_code[code]:
            errors.append(f"descriptor 记录覆盖错误：{code}")

    group_ids = [group.get("resource_group_id") for group in resource_groups]
    if len(group_ids) != len(set(group_ids)):
        errors.append("resource_group_id 不唯一")
    group_index = {
        group.get("resource_group_id"): group for group in resource_groups
    }
    grouped_codes: list[str] = []
    for group in resource_groups:
        group_id = group.get("resource_group_id")
        repo_id = group.get("repo_id")
        codes = group.get("course_codes", [])
        grouped_codes.extend(codes)
        if repo_id not in repo_set:
            errors.append(f"资源分组指向不存在仓库：{group_id}")
            continue
        repository = repository_index[repo_id]
        if repository.get("repo_type") != "course":
            errors.append(f"资源分组目标不是 course 仓库：{repo_id}")
    if len(grouped_codes) != len(set(grouped_codes)) or set(grouped_codes) != set(
        source_code_names
    ):
        errors.append("资源分组未将每个源课程代码恰好分配一次")
    for name, codes in _names_to_codes(source_code_names).items():
        assigned_groups = {
            descriptor_index[code].get("resource_group_id")
            for code in codes
            if code in descriptor_index
        }
        if len(assigned_groups) != 1:
            errors.append(f"精确同名课程未自动共仓：{name}")

    for descriptor in descriptors:
        code = descriptor.get("course_code")
        group = group_index.get(descriptor.get("resource_group_id"))
        if not group:
            errors.append(f"descriptor 指向不存在资源分组：{code}")
            continue
        if descriptor.get("repo_id") != group.get("repo_id"):
            errors.append(f"descriptor 与资源分组仓库不一致：{code}")
    for record in records:
        code = record.get("course_code")
        if code:
            descriptor = descriptor_index.get(code)
            if not descriptor:
                continue
            if record.get("descriptor_id") != descriptor.get("descriptor_id"):
                errors.append(f"培养方案记录 descriptor 绑定错误：{record.get('record_id')}")
            if record.get("repo_id") != descriptor.get("repo_id"):
                errors.append(f"培养方案记录仓库绑定错误：{record.get('record_id')}")
            if record.get("resource_group_id") != descriptor.get(
                "resource_group_id"
            ):
                errors.append(f"培养方案记录资源分组错误：{record.get('record_id')}")
        elif record.get("descriptor_id") is not None or record.get(
            "resource_group_id"
        ) is not None:
            errors.append(f"无代码记录意外绑定 descriptor：{record.get('record_id')}")

    identity_lines = [
        f"{code}\0{source_code_names[code]}" for code in sorted(source_code_names)
    ]
    source_identity_sha256 = hashlib.sha256(
        "\n".join(identity_lines).encode("utf-8")
    ).hexdigest()
    if group_config.get("curriculum_identity_sha256") != source_identity_sha256:
        errors.append("资源分组配置的课程身份摘要与源数据不一致")
    manifest_group_source = manifest.get("sources", {}).get("resource_groups", {})
    if manifest_group_source.get("curriculum_identity_sha256") != source_identity_sha256:
        errors.append("manifest 记录的课程身份摘要与源数据不一致")
    configured_groups = group_config.get("groups", [])
    if manifest_group_source.get("explicit_group_count") != len(configured_groups):
        errors.append("manifest 基础显式资源分组数量与配置不一致")

    source_commit, source_legacy_paths = git_paths(args.legacy_repo)
    expected_legacy_uris = {
        f"github://HIT-Fireworks/fireworks-attachments@{source_commit}/{path}"
        for path in source_legacy_paths
    }
    manifest_legacy_uris = [
        path for unit in legacy_units for path in unit.get("source_paths", [])
    ]
    manifest_legacy_set = set(manifest_legacy_uris)
    if manifest_legacy_set != expected_legacy_uris:
        errors.append(
            "legacy 文件覆盖不一致："
            f"missing={len(expected_legacy_uris - manifest_legacy_set)}, "
            f"extra={len(manifest_legacy_set - expected_legacy_uris)}"
        )
    if len(manifest_legacy_uris) != len(manifest_legacy_set):
        errors.append("legacy 文件被重复分配")

    expected_roots = {path.split("/", 1)[0] for path in source_legacy_paths}
    manifest_roots = {item.get("legacy_root") for item in legacy_roots}
    if expected_roots != manifest_roots:
        errors.append(
            "legacy 根目录覆盖不一致："
            f"missing={sorted(expected_roots - manifest_roots)}, "
            f"extra={sorted(manifest_roots - expected_roots)}"
        )

    if args.require_created:
        not_created = [
            repo["repo_id"] for repo in repositories if repo.get("status") != "created"
        ]
        if not_created:
            errors.append(f"仍有 {len(not_created)} 个仓库未标记 created")

    report = {
        "valid": not errors,
        "errors": errors,
        "repository_count": len(repositories),
        "resource_group_count": len(resource_groups),
        "course_descriptor_count": len(descriptors),
        "curriculum_source_records": len(source_keys),
        "curriculum_manifest_records": len(records),
        "legacy_source_files": len(source_legacy_paths),
        "legacy_manifest_files": len(manifest_legacy_uris),
        "legacy_source_roots": len(expected_roots),
        "legacy_manifest_roots": len(manifest_roots),
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())

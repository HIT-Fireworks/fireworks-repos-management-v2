#!/usr/bin/env python3
"""生成薪火笔记社全量仓库分配 manifest。"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import tomllib
import unicodedata
from collections import defaultdict
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 2
FROZEN_LEGACY_REPO_IDS = {
    "fireworks-course-registry",
    "fireworks-repos-management",
    "fireworks-course-template",
    "22AD11001",
}
CONTROL_REPOSITORIES = (
    {
        "repo_id": "fireworks-course-registry-v2",
        "repo_type": "control",
        "display_name": "薪火课程注册表 v2",
        "description": "HIT 全量课程、培养方案记录、仓库绑定与历史资料映射的 v2 权威注册表",
        "template_id": None,
    },
    {
        "repo_id": "fireworks-repos-management-v2",
        "repo_type": "control",
        "display_name": "薪火课程仓库管理 v2",
        "description": "HIT-Fireworks v2 课程仓库的创建、索引、校验与聚合工作流",
        "template_id": None,
    },
    {
        "repo_id": "fireworks-course-template-v2",
        "repo_type": "template",
        "display_name": "薪火课程仓库模板 v2",
        "description": "薪火笔记社多代码课程资料仓库 v2 模板",
        "template_id": None,
    },
    {
        "repo_id": "fireworks-requirement-template-v2",
        "repo_type": "template",
        "display_name": "薪火培养要求仓库模板 v2",
        "description": "薪火笔记社 requirement-set 仓库 v2 模板",
        "template_id": None,
    },
    {
        "repo_id": "fireworks-collection-template-v2",
        "repo_type": "template",
        "display_name": "薪火资料集合仓库模板 v2",
        "description": "薪火笔记社 collection、shared、competition 与 software 仓库 v2 模板",
        "template_id": None,
    },
)

ROOT_REPOSITORIES = {
    "【公共课】": ("CAT-public-courses", "collection"),
    "交通科学与工程学院": ("CAT-transportation", "collection"),
    "人文社科学部": ("CAT-humanities", "collection"),
    "仪器学院": ("CAT-instruments", "collection"),
    "化工与化学学院": ("CAT-chemistry", "collection"),
    "土木工程学院": ("CAT-civil-engineering", "collection"),
    "实验报告": ("CAT-experiment-reports", "collection"),
    "数学学院": ("CAT-mathematics", "collection"),
    "未来技术学院": ("CAT-future-technology", "collection"),
    "机电工程学院": ("CAT-mechatronics", "collection"),
    "校内资源": ("CAT-campus-resources", "shared"),
    "物理学院": ("CAT-physics", "collection"),
    "环境学院": ("CAT-environment", "collection"),
    "生命科学和医学学部": ("CAT-life-medicine", "collection"),
    "电信学院": ("CAT-electronics-information", "collection"),
    "经管学院": ("CAT-management", "collection"),
    "薪火笔记社-PPT及报告模板": ("CAT-document-templates", "shared"),
    "薪火笔记社-竞赛": ("CAT-competitions", "competition"),
    "询问中": ("CAT-pending-identification", "collection"),
}

LEGACY_NAME_ALIASES = {
    "习概": "习近平新时代中国特色社会主义思想概论",
    "毛概": "毛泽东思想和中国特色社会主义理论体系概论",
    "马克思主义原理": "马克思主义基本原理",
    "近现代史纲要": "中国近现代史纲要",
}

REQUIRED_ITEM_FIELDS = (
    "repo_id",
    "repo_type",
    "merge_key",
    "merge_reason",
    "source_paths",
    "status",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
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
    parser.add_argument(
        "--course-family-stage",
        help="覆盖课程族配置的 default_stage；用于生成批准候选中间 manifest",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("data/repository-manifest.json"),
    )
    parser.add_argument("--organization", default="HIT-Fireworks")
    return parser.parse_args()


def normalize(value: Any) -> str:
    return unicodedata.normalize("NFKC", str(value or "")).strip()


def digest(value: str, length: int = 12) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:length].upper()


def natural_key(value: str) -> tuple[Any, ...]:
    return tuple(int(part) if part.isdigit() else part.casefold() for part in re.split(r"(\d+)", value))


def unique(values: list[str] | set[str]) -> list[str]:
    return sorted(set(values), key=natural_key)


def source_uri(plan_file: Path, ordinal: int) -> str:
    return f"curriculum://hit-data/plans/{plan_file.name}#courses[{ordinal}]"


def legacy_uri(commit: str, path: str) -> str:
    return f"github://HIT-Fireworks/fireworks-attachments@{commit}/{path}"


def template_for(repo_type: str) -> str | None:
    if repo_type == "course":
        return "fireworks-course-template-v2"
    if repo_type == "requirement-set":
        return "fireworks-requirement-template-v2"
    if repo_type in {"collection", "shared", "competition", "software"}:
        return "fireworks-collection-template-v2"
    return None


def repo_record(
    *,
    repo_id: str,
    repo_type: str,
    display_name: str,
    description: str,
    merge_key: str,
    merge_reason: str,
    source_paths: list[str],
    status: str = "planned",
    aliases: list[str] | None = None,
    course_codes: list[str] | None = None,
    member_repo_ids: list[str] | None = None,
    resource_group_id: str | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "repo_id": repo_id,
        "repo_type": repo_type,
        "display_name": display_name,
        "description": description[:350],
        "merge_key": merge_key,
        "merge_reason": merge_reason,
        "source_paths": unique(source_paths),
        "status": status,
        "template_id": template_for(repo_type),
        "aliases": unique(aliases or []),
        "course_codes": unique(course_codes or []),
        "member_repo_ids": unique(member_repo_ids or []),
        "resource_group_id": resource_group_id,
    }
    return result


def load_curriculum(plan_dir: Path) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    plan_files = sorted(plan_dir.glob("*.toml"), key=lambda path: natural_key(path.name))
    if not plan_files:
        raise SystemExit(f"未找到培养方案 TOML：{plan_dir}")

    rows: list[dict[str, Any]] = []
    plan_metadata: dict[str, Any] = {}
    for plan_file in plan_files:
        document = tomllib.loads(plan_file.read_text(encoding="utf-8"))
        info = document["info"]
        plan_id = normalize(info["plan_ID"])
        if plan_id in plan_metadata:
            raise ValueError(f"重复 plan_ID：{plan_id}")
        plan_metadata[plan_id] = {
            "file": plan_file,
            "campus": normalize(info.get("campus", "hit")),
            "plan_version": normalize(info.get("plan_version")),
            "department_code": normalize(info.get("department_code")),
            "school_name": normalize(info.get("school_name")),
            "major_code": normalize(info.get("major_code")),
            "major_name": normalize(info.get("major_name")),
            "year": normalize(info.get("year")),
        }
        for ordinal, course in enumerate(document.get("courses", [])):
            rows.append(
                {
                    "record_id": f"REC-{digest(f'{plan_id}\0{ordinal}', 16)}",
                    "source_plan": plan_id,
                    "source_plan_file": plan_file.name,
                    "source_ordinal": ordinal,
                    "campus": normalize(info.get("campus", "hit")),
                    "plan_version": normalize(info.get("plan_version")),
                    "department_code": normalize(info.get("department_code")),
                    "school_name": normalize(info.get("school_name")),
                    "major_code": normalize(info.get("major_code")),
                    "major_name": normalize(info.get("major_name")),
                    "course_code": normalize(course.get("course_code")),
                    "course_name": normalize(course.get("course_name")),
                    "credit": course.get("credit"),
                    "total_hours": course.get("total_hours"),
                    "assessment_method": normalize(course.get("assessment_method")),
                    "course_nature": normalize(course.get("course_nature")),
                    "course_category": normalize(course.get("course_category")),
                    "offering_college": normalize(course.get("offering_college")),
                    "recommended_year_semester": normalize(
                        course.get("recommended_year_semester")
                    ),
                    "hours": course.get("hours", {}),
                    "_source_uri": source_uri(plan_file, ordinal),
                }
            )
    return rows, plan_metadata


def course_group_source_identity(rows: list[dict[str, Any]]) -> str:
    snapshot = [
        {
            field: row[field]
            for field in (
                "record_id",
                "source_plan",
                "school_name",
                "major_name",
                "plan_version",
                "course_nature",
                "course_code",
                "course_name",
            )
        }
        for row in rows
        if row["course_code"]
    ]
    snapshot.sort(key=lambda item: (item["source_plan"], item["record_id"]))
    encoded = json.dumps(
        snapshot, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def load_course_groups(path: Path, rows: list[dict[str, Any]]) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1:
        raise ValueError(f"不支持的课程组配置版本：{document.get('schema_version')}")
    if document.get("role") != "semantic-relatedness-only":
        raise ValueError("课程组配置只能承担语义相关性角色")
    if document.get("overlapping_membership") != "allowed-and-expected":
        raise ValueError("课程组配置必须允许成员重叠")
    if document.get("course_groups_do_not_imply_repository_merge") is not True:
        raise ValueError("课程组配置必须声明不触发资料仓库合并")

    identity_sha256 = course_group_source_identity(rows)
    if document.get("source_curriculum_identity_sha256") != identity_sha256:
        raise ValueError("课程组配置属于另一份培养方案数据快照")

    definitions = document.get("definitions")
    if not isinstance(definitions, list) or not definitions:
        raise ValueError("课程组配置 definitions 必须是非空数组")

    coded_rows = [row for row in rows if row["course_code"]]
    allowed_fields = {
        "source_plan",
        "school_name",
        "major_name",
        "course_nature",
    }
    allowed_group_types = {"plan-scope", "major-scope", "school-scope"}
    definition_ids: set[str] = set()
    course_group_ids: set[str] = set()
    course_groups: list[dict[str, Any]] = []
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
            values = unique([normalize(value) for value in raw_values if normalize(value)])
            if not values:
                raise ValueError(f"课程组 {definition_id} 的条件 {field} 不能为空")
            conditions[field] = values

        buckets: dict[tuple[str, ...], list[dict[str, Any]]] = defaultdict(list)
        for row in coded_rows:
            if any(row[field] not in values for field, values in conditions.items()):
                continue
            buckets[tuple(row[field] for field in scope_fields)].append(row)

        definition_group_count = 0
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
            course_group_id = "course-group-" + hashlib.sha256(
                relation_key.encode("utf-8")
            ).hexdigest()[:16]
            if course_group_id in course_group_ids:
                raise ValueError(f"课程组 ID 冲突：{course_group_id}")

            representative = dict(member_rows[0])
            representative.update(scope)
            try:
                display_name = normalize(display_name_template.format_map(representative))
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
            course_groups.append(
                {
                    "course_group_id": course_group_id,
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
            rows_by_code: dict[str, list[dict[str, Any]]] = defaultdict(list)
            for row in member_rows:
                rows_by_code[row["course_code"]].append(row)
            for course_code in course_codes:
                memberships.append(
                    {
                        "course_group_id": course_group_id,
                        "descriptor_id": f"course-code:{course_code}",
                        "course_code": course_code,
                        "record_ids": unique(
                            {row["record_id"] for row in rows_by_code[course_code]}
                        ),
                    }
                )
            course_group_ids.add(course_group_id)
            definition_group_count += 1

        if definition_group_count != definition.get("expected_group_count"):
            raise ValueError(
                f"课程组定义 {definition_id} 的组数已漂移："
                f"{definition_group_count} != {definition.get('expected_group_count')}"
            )
        definition_ids.add(definition_id)

    if len(course_groups) != document.get("expected_course_group_count"):
        raise ValueError("课程组总数与配置不一致")
    covered_codes = {code for group in course_groups for code in group["course_codes"]}
    if len(covered_codes) != document.get("expected_distinct_course_code_count"):
        raise ValueError("课程组覆盖的不同课程代码数与配置不一致")
    if covered_codes != {row["course_code"] for row in coded_rows}:
        raise ValueError("课程组未覆盖全部有代码课程")

    course_groups.sort(key=lambda item: item["course_group_id"])
    memberships.sort(key=lambda item: (item["course_group_id"], natural_key(item["course_code"])))
    return {
        "path": path.as_posix(),
        "identity_sha256": identity_sha256,
        "definitions": definitions,
        "course_groups": course_groups,
        "memberships": memberships,
    }


def load_resource_groups(path: Path, rows: list[dict[str, Any]]) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1:
        raise ValueError(f"不支持的资源分组配置版本：{document.get('schema_version')}")
    if document.get("automatic_grouping") != "exact-normalized-course-name":
        raise ValueError("资源分组配置必须使用精确规范化课程名自动分组")
    if document.get("future_changes_require_review") is not True:
        raise ValueError("资源分组配置必须要求后续差分经过审阅")

    code_name: dict[str, str] = {}
    names_to_codes: dict[str, set[str]] = defaultdict(set)
    for row in rows:
        code = row["course_code"]
        if not code:
            continue
        name = row["course_name"]
        prior = code_name.setdefault(code, name)
        if prior != name:
            raise ValueError(f"课程代码 {code} 同时对应 {prior!r} 与 {name!r}")
        names_to_codes[name].add(code)

    identity_lines = [f"{code}\0{code_name[code]}" for code in sorted(code_name)]
    identity_sha256 = hashlib.sha256("\n".join(identity_lines).encode("utf-8")).hexdigest()
    if document.get("curriculum_identity_sha256") != identity_sha256:
        raise ValueError(
            "课程代码—名称数据已变化；必须审阅资源分组差分后更新配置摘要"
        )

    raw_groups = document.get("groups")
    if not isinstance(raw_groups, list):
        raise ValueError("资源分组配置 groups 必须是数组")

    groups: list[dict[str, Any]] = []
    name_to_group: dict[str, dict[str, Any]] = {}
    legacy_unit_to_group: dict[str, dict[str, Any]] = {}
    used_group_ids: set[str] = set()
    used_repo_ids: set[str] = set()
    for index, raw_group in enumerate(raw_groups):
        if not isinstance(raw_group, dict):
            raise ValueError(f"资源分组配置 groups[{index}] 必须是对象")
        group_id = normalize(raw_group.get("group_id"))
        repo_id = normalize(raw_group.get("repo_id"))
        display_name = normalize(raw_group.get("display_name"))
        evidence = normalize(raw_group.get("evidence"))
        raw_names = raw_group.get("course_names")
        raw_codes = raw_group.get("expected_course_codes")
        raw_legacy_units = raw_group.get("legacy_units")

        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", group_id):
            raise ValueError(f"资源分组 groups[{index}] 的 group_id 非法：{group_id!r}")
        if group_id in used_group_ids:
            raise ValueError(f"资源分组 group_id 重复：{group_id}")
        if not repo_id or len(repo_id) > 100 or not re.fullmatch(
            r"[A-Za-z0-9._-]+", repo_id
        ):
            raise ValueError(f"资源分组 groups[{index}] 的 repo_id 非法：{repo_id!r}")
        if repo_id.casefold() in used_repo_ids:
            raise ValueError(f"资源分组 repo_id 大小写重复：{repo_id}")
        if not display_name or not evidence:
            raise ValueError(f"资源分组 {group_id} 缺少 display_name 或 evidence")
        if not isinstance(raw_names, list) or not raw_names:
            raise ValueError(f"资源分组 {group_id}.course_names 必须是非空数组")
        if not isinstance(raw_codes, list) or not raw_codes:
            raise ValueError(
                f"资源分组 {group_id}.expected_course_codes 必须是非空数组"
            )
        if not isinstance(raw_legacy_units, list):
            raise ValueError(f"资源分组 {group_id}.legacy_units 必须是数组")

        names = unique([normalize(name) for name in raw_names if normalize(name)])
        unknown_names = [name for name in names if name not in names_to_codes]
        if unknown_names:
            raise ValueError(f"资源分组 {group_id} 含未知课程名：{unknown_names}")
        expected_codes = unique(
            {code for name in names for code in names_to_codes[name]}
        )
        configured_codes = unique(
            [normalize(code) for code in raw_codes if normalize(code)]
        )
        if configured_codes != expected_codes:
            raise ValueError(
                f"资源分组 {group_id} 的课程代码已漂移："
                f"configured={configured_codes} actual={expected_codes}"
            )
        if repo_id not in expected_codes:
            raise ValueError(f"资源分组 {group_id} 的 repo_id 必须是组内课程代码")

        legacy_units = unique(
            [normalize(unit) for unit in raw_legacy_units if normalize(unit)]
        )
        group = {
            "group_id": group_id,
            "repo_id": repo_id,
            "display_name": display_name,
            "course_names": names,
            "course_codes": expected_codes,
            "legacy_units": legacy_units,
            "evidence": evidence,
            "grouping_rule": "explicit-reviewed",
        }
        for name in names:
            if name in name_to_group:
                raise ValueError(
                    f"课程名 {name!r} 同时属于 {name_to_group[name]['group_id']} 和 {group_id}"
                )
            name_to_group[name] = group
        for legacy_unit in legacy_units:
            if legacy_unit in legacy_unit_to_group:
                raise ValueError(
                    f"历史单元 {legacy_unit!r} 同时属于 "
                    f"{legacy_unit_to_group[legacy_unit]['group_id']} 和 {group_id}"
                )
            legacy_unit_to_group[legacy_unit] = group
        groups.append(group)
        used_group_ids.add(group_id)
        used_repo_ids.add(repo_id.casefold())

    return {
        "path": path.as_posix(),
        "identity_sha256": identity_sha256,
        "groups": groups,
        "name_to_group": name_to_group,
        "legacy_unit_to_group": legacy_unit_to_group,
    }

def resource_group_identity(groups: list[dict[str, Any]]) -> str:
    rows = [
        {
            "resource_group_id": group["group_id"],
            "repo_id": group["repo_id"],
            "course_names": sorted(group["course_names"]),
            "course_codes": sorted(group["course_codes"]),
        }
        for group in groups
    ]
    rows.sort(key=lambda row: row["resource_group_id"])
    encoded = json.dumps(
        rows, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def load_course_families(
    path: Path,
    migration_path: Path,
    base_groups: list[dict[str, Any]],
    requested_stage: str | None,
) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    migration = json.loads(migration_path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1 or migration.get("schema_version") != 1:
        raise ValueError("不支持的课程族配置或迁移映射版本")
    if document.get("policy", {}).get("runtime_heuristics") != (
        "disabled-materialized-membership-only"
    ):
        raise ValueError("课程族配置必须禁用运行期启发式重算")
    if document.get("role") != "exclusive-resource-repository-policy":
        raise ValueError("课程族配置必须承担互斥的资料仓库策略角色")
    if document.get("course_groups_do_not_imply_repository_merge") is not True:
        raise ValueError("课程族配置必须声明 CourseGroup 不触发资料仓库合并")

    identity = resource_group_identity(base_groups)
    for section, value in (("配置", document), ("迁移映射", migration)):
        if value.get("source_resource_group_identity_sha256") != identity:
            raise ValueError(f"课程族{section}属于另一组基础资源分组")
    if document.get("source_resource_group_count") != len(base_groups):
        raise ValueError("课程族配置的基础资源分组数量已漂移")

    stage_id = requested_stage or document.get("default_stage")
    stages = {
        stage.get("stage_id"): stage for stage in document.get("stages", [])
    }
    migration_stages = {
        stage.get("stage_id"): stage for stage in migration.get("stages", [])
    }
    stage = stages.get(stage_id)
    migration_stage = migration_stages.get(stage_id)
    if not stage or not migration_stage:
        raise ValueError(f"未知或缺少迁移映射的课程族阶段：{stage_id!r}")

    base_by_id = {group["group_id"]: group for group in base_groups}
    base_repo_ids = {group["repo_id"] for group in base_groups}
    member_to_family: dict[str, dict[str, Any]] = {}
    family_ids: set[str] = set()
    target_repo_ids: set[str] = set()
    for index, family in enumerate(stage.get("families", [])):
        family_id = normalize(family.get("family_id"))
        repo_id = normalize(family.get("repo_id"))
        member_ids = family.get("member_group_ids")
        if not re.fullmatch(r"course-family-[0-9a-f]{16}", family_id):
            raise ValueError(f"课程族 families[{index}] 的 family_id 非法")
        if family_id in family_ids:
            raise ValueError(f"课程族 family_id 重复：{family_id}")
        if not isinstance(member_ids, list) or len(member_ids) < 2:
            raise ValueError(f"课程族 {family_id} 必须至少包含两个基础资源组")
        if len(member_ids) != len(set(member_ids)):
            raise ValueError(f"课程族 {family_id} 重复引用基础资源组")
        unknown = sorted(set(member_ids) - set(base_by_id))
        if unknown:
            raise ValueError(f"课程族 {family_id} 引用未知基础资源组：{unknown}")
        if any(member_id in member_to_family for member_id in member_ids):
            raise ValueError(f"课程族 {family_id} 与其他课程族成员重叠")

        members = [base_by_id[member_id] for member_id in member_ids]
        names = unique(
            {name for member in members for name in member["course_names"]}
        )
        codes = unique(
            {code for member in members for code in member["course_codes"]}
        )
        legacy_units = unique(
            {unit for member in members for unit in member.get("legacy_units", [])}
        )
        source_repo_ids = unique([member["repo_id"] for member in members])
        expected = {
            "expected_course_names": names,
            "expected_course_codes": codes,
            "expected_legacy_units": legacy_units,
            "expected_source_repo_ids": source_repo_ids,
        }
        for field, actual in expected.items():
            if unique(family.get(field, [])) != actual:
                raise ValueError(f"课程族 {family_id} 的 {field} 已漂移")
        if repo_id not in codes:
            raise ValueError(f"课程族 {family_id} 的 repo_id 必须是族内课程代码")
        if repo_id in FROZEN_LEGACY_REPO_IDS:
            raise ValueError(f"课程族 {family_id} 命中冻结旧仓库：{repo_id}")
        if repo_id.casefold() in {value.casefold() for value in target_repo_ids}:
            raise ValueError(f"课程族目标 repo_id 大小写重复：{repo_id}")
        family_ids.add(family_id)
        target_repo_ids.add(repo_id)
        for member_id in member_ids:
            member_to_family[member_id] = family

    final_repo_ids = {
        member_to_family.get(group["group_id"], group)["repo_id"]
        for group in base_groups
    }
    if len(final_repo_ids) != stage.get("expected_output_resource_group_count"):
        raise ValueError("课程族阶段输出资源组数量与配置不一致")
    reduction = len(base_groups) - len(final_repo_ids)
    if reduction != stage.get("expected_course_repository_reduction"):
        raise ValueError("课程族阶段仓库减量与配置不一致")
    if final_repo_ids & FROZEN_LEGACY_REPO_IDS:
        raise ValueError("课程族阶段目标命中冻结旧仓库")

    mappings = migration_stage.get("mappings", [])
    source_mapping_ids = [mapping.get("source_repo_id") for mapping in mappings]
    if len(source_mapping_ids) != len(base_repo_ids) or set(source_mapping_ids) != base_repo_ids:
        raise ValueError("课程族迁移映射未将每个基础 repo_id 恰好覆盖一次")
    expected_mappings = {
        group["repo_id"]: (
            member_to_family.get(group["group_id"], group)["repo_id"],
            member_to_family.get(group["group_id"], group).get(
                "family_id", group["group_id"]
            ),
        )
        for group in base_groups
    }
    for mapping in mappings:
        source_repo_id = mapping.get("source_repo_id")
        target_repo_id, target_group_id = expected_mappings[source_repo_id]
        if (
            mapping.get("target_repo_id") != target_repo_id
            or mapping.get("target_resource_group_id") != target_group_id
            or mapping.get("target_repo_id") not in final_repo_ids
            or mapping.get("target_repo_id") in FROZEN_LEGACY_REPO_IDS
        ):
            raise ValueError(f"课程族迁移映射错误：{source_repo_id}")
    if migration_stage.get("target_course_repository_count") != len(final_repo_ids):
        raise ValueError("课程族迁移映射的目标仓库数量错误")

    return {
        "path": path.as_posix(),
        "migration_path": migration_path.as_posix(),
        "identity_sha256": identity,
        "stage_id": stage_id,
        "stage": stage,
        "member_to_family": member_to_family,
        "migration_stage": migration_stage,
    }


def apply_course_families(
    base_groups: list[dict[str, Any]], family_config: dict[str, Any]
) -> list[dict[str, Any]]:
    base_by_id = {group["group_id"]: group for group in base_groups}
    member_to_family = family_config["member_to_family"]
    result: list[dict[str, Any]] = []
    emitted_families: set[str] = set()
    for group in base_groups:
        family = member_to_family.get(group["group_id"])
        if not family:
            result.append(dict(group))
            continue
        family_id = family["family_id"]
        if family_id in emitted_families:
            continue
        members = [base_by_id[member_id] for member_id in family["member_group_ids"]]
        result.append(
            {
                "group_id": family_id,
                "repo_id": family["repo_id"],
                "display_name": family["display_name"],
                "course_names": unique(
                    {name for member in members for name in member["course_names"]}
                ),
                "course_codes": unique(
                    {code for member in members for code in member["course_codes"]}
                ),
                "legacy_units": unique(
                    {unit for member in members for unit in member.get("legacy_units", [])}
                ),
                "evidence": family["evidence"],
                "grouping_rule": "materialized-course-family",
                "member_resource_group_ids": unique(family["member_group_ids"]),
                "source_repo_ids": unique(family["expected_source_repo_ids"]),
                "approval": family["approval"],
            }
        )
        emitted_families.add(family_id)
    return result


def build_base_course_groups(
    rows: list[dict[str, Any]], group_config: dict[str, Any]
) -> list[dict[str, Any]]:
    code_name: dict[str, str] = {}
    names_to_codes: dict[str, set[str]] = defaultdict(set)
    for row in rows:
        code = row["course_code"]
        if not code:
            continue
        name = row["course_name"]
        prior = code_name.setdefault(code, name)
        if prior != name:
            raise ValueError(f"课程代码 {code} 同时对应 {prior!r} 与 {name!r}")
        names_to_codes[name].add(code)

    groups: list[dict[str, Any]] = [dict(group) for group in group_config["groups"]]
    explicitly_grouped_names = set(group_config["name_to_group"])
    for name, code_set in sorted(names_to_codes.items(), key=lambda item: natural_key(item[0])):
        if name in explicitly_grouped_names:
            continue
        codes = unique(code_set)
        groups.append(
            {
                "group_id": f"exact-name-{digest(name, 16).lower()}",
                "repo_id": codes[0],
                "display_name": name,
                "course_names": [name],
                "course_codes": codes,
                "legacy_units": [],
                "evidence": "规范化课程名称完全相同，自动共用资料仓库",
                "grouping_rule": "exact-normalized-course-name",
            }
        )
    return groups


def remap_legacy_unit_groups(
    legacy_unit_to_group: dict[str, dict[str, Any]],
    family_config: dict[str, Any],
    effective_groups: list[dict[str, Any]],
) -> dict[str, dict[str, Any]]:
    effective_by_id = {group["group_id"]: group for group in effective_groups}
    member_to_family = family_config["member_to_family"]
    result: dict[str, dict[str, Any]] = {}
    for boundary, base_group in legacy_unit_to_group.items():
        family = member_to_family.get(base_group["group_id"])
        target_group_id = family["family_id"] if family else base_group["group_id"]
        target_group = effective_by_id.get(target_group_id)
        if not target_group:
            raise ValueError(
                f"历史单元 {boundary!r} 的课程族目标不存在：{target_group_id}"
            )
        prior = result.get(boundary)
        if prior and prior["group_id"] != target_group_id:
            raise ValueError(
                f"历史单元 {boundary!r} 同时映射到课程族 "
                f"{prior['group_id']} 和 {target_group_id}"
            )
        result[boundary] = target_group
    return result


def physical_resource_group_identity(groups: list[dict[str, Any]]) -> str:
    rows = [
        {
            "resource_group_id": group["group_id"],
            "preferred_repo_id": group["repo_id"],
            "course_codes": sorted(group["course_codes"]),
        }
        for group in groups
    ]
    rows.sort(key=lambda row: row["resource_group_id"])
    encoded = json.dumps(
        rows, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def longest_common_owner_path(
    rows: list[dict[str, Any]], fields: tuple[str, ...], missing_value: str
) -> tuple[str, ...]:
    paths = [
        tuple(normalize(row.get(field)) or missing_value for field in fields)
        for row in rows
    ]
    if not paths:
        raise ValueError("资源分组没有可用于 canonical owner 的培养方案记录")
    depth = 0
    while depth < len(fields) and len({path[depth] for path in paths}) == 1:
        depth += 1
    return paths[0][:depth]


def load_physical_repository_policy(
    path: Path,
    groups: list[dict[str, Any]],
    stage_id: str,
) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1:
        raise ValueError("物理仓库策略 schema_version 不是 1")
    if document.get("role") != "exclusive-physical-repository-policy":
        raise ValueError("物理仓库策略必须声明互斥物理归属角色")
    if document.get("course_groups_are_navigation_evidence_only") is not True:
        raise ValueError("物理仓库策略必须声明 CourseGroup 仅作导航证据")
    if document.get("automatic_remapping") is not False:
        raise ValueError("物理仓库策略禁止运行期自动重映射")

    canonical = document.get("canonical_owner", {})
    fields = canonical.get("hierarchy_fields")
    allowed_fields = {
        "offering_college",
        "school_name",
        "major_name",
        "course_category",
    }
    if (
        canonical.get("algorithm")
        != "longest-common-prefix-over-curriculum-records"
        or not isinstance(fields, list)
        or not fields
        or len(fields) != len(set(fields))
        or set(fields) - allowed_fields
    ):
        raise ValueError("物理仓库 canonical owner 配置非法")
    minimum_shared_depth = canonical.get("minimum_shared_depth")
    frontier_depth = document.get("materialization", {}).get(
        "shared_frontier_depth"
    )
    if (
        not isinstance(minimum_shared_depth, int)
        or not isinstance(frontier_depth, int)
        or minimum_shared_depth < 1
        or frontier_depth != minimum_shared_depth
        or frontier_depth > len(fields)
    ):
        raise ValueError("物理仓库共享前沿深度非法")

    stages = {
        stage.get("stage_id"): stage for stage in document.get("stages", [])
    }
    stage = stages.get(stage_id)
    if not stage:
        raise ValueError(f"物理仓库策略缺少课程族阶段：{stage_id}")
    identity = physical_resource_group_identity(groups)
    if stage.get("source_resource_group_identity_sha256") != identity:
        raise ValueError("物理仓库策略属于另一组生效 ResourceGroup")
    if stage.get("expected_resource_group_count") != len(groups):
        raise ValueError("物理仓库策略的 ResourceGroup 数量已漂移")
    return {
        "path": path.as_posix(),
        "document": document,
        "stage": stage,
        "stage_id": stage_id,
        "identity_sha256": identity,
        "fields": tuple(fields),
        "missing_value": normalize(canonical.get("missing_value")) or "未标注",
        "minimum_shared_depth": minimum_shared_depth,
        "frontier_depth": frontier_depth,
    }


def allocate_course_repositories(
    rows: list[dict[str, Any]],
    groups: list[dict[str, Any]],
    physical_policy: dict[str, Any],
    course_group_memberships: list[dict[str, Any]],
) -> tuple[
    list[dict[str, Any]],
    dict[str, dict[str, str]],
    dict[str, dict[str, str]],
    list[dict[str, Any]],
    list[dict[str, Any]],
]:
    code_rows: dict[str, list[dict[str, Any]]] = defaultdict(list)
    code_name: dict[str, str] = {}
    for row in rows:
        code = row["course_code"]
        if not code:
            continue
        name = row["course_name"]
        prior = code_name.setdefault(code, name)
        if prior != name:
            raise ValueError(f"课程代码 {code} 同时对应 {prior!r} 与 {name!r}")
        code_rows[code].append(row)

    navigation_groups_by_code: dict[str, set[str]] = defaultdict(set)
    for membership in course_group_memberships:
        navigation_groups_by_code[membership["course_code"]].add(
            membership["course_group_id"]
        )

    fields = physical_policy["fields"]
    missing_value = physical_policy["missing_value"]
    minimum_shared_depth = physical_policy["minimum_shared_depth"]
    frontier_depth = physical_policy["frontier_depth"]
    assignments_by_group: dict[str, dict[str, Any]] = {}
    buckets: dict[str, dict[str, Any]] = {}

    for group in sorted(groups, key=lambda item: item["group_id"]):
        group_id = group["group_id"]
        codes = unique(group["course_codes"])
        owner_rows = [row for code in codes for row in code_rows[code]]
        owner_path = longest_common_owner_path(owner_rows, fields, missing_value)
        canonical_owner = {
            "depth": len(owner_path),
            "path": [
                {"field": field, "value": value}
                for field, value in zip(
                    fields[: len(owner_path)], owner_path, strict=True
                )
            ],
        }
        if len(owner_path) < minimum_shared_depth:
            kind = "dedicated-resource-group"
            relation = json.dumps(
                {"kind": kind, "resource_group_id": group_id},
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )
            repo_id = group["repo_id"]
            display_name = group["display_name"]
            owner_scope = canonical_owner
        else:
            kind = "hierarchy-node"
            frontier_path = owner_path[:frontier_depth]
            owner_scope = {
                "depth": frontier_depth,
                "path": [
                    {"field": field, "value": value}
                    for field, value in zip(
                        fields[:frontier_depth], frontier_path, strict=True
                    )
                ],
            }
            relation = json.dumps(
                {"kind": kind, "canonical_owner": owner_scope},
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )
            repo_id = f"COURSES-{digest(relation, 12)}"
            display_name = " / ".join(frontier_path)
        physical_repository_id = (
            "physical-course-" + hashlib.sha256(relation.encode("utf-8")).hexdigest()[:16]
        )
        navigation_ids = unique(
            {
                course_group_id
                for code in codes
                for course_group_id in navigation_groups_by_code[code]
            }
        )
        assignment = {
            "resource_group_id": group_id,
            "physical_repository_id": physical_repository_id,
            "repo_id": repo_id,
            "preferred_repo_id": group["repo_id"],
            "canonical_owner": canonical_owner,
            "navigation_course_group_ids": navigation_ids,
        }
        assignments_by_group[group_id] = assignment
        bucket = buckets.setdefault(
            physical_repository_id,
            {
                "physical_repository_id": physical_repository_id,
                "repo_id": repo_id,
                "display_name": display_name,
                "materialization_kind": kind,
                "canonical_owner": owner_scope,
                "groups": [],
            },
        )
        if bucket["repo_id"] != repo_id:
            raise ValueError(f"物理仓库 ID 冲突：{physical_repository_id}")
        bucket["groups"].append(group)

    expected_count = physical_policy["stage"].get(
        "expected_physical_course_repository_count"
    )
    if len(buckets) != expected_count:
        raise ValueError(
            f"物理课程仓库数量已漂移：{len(buckets)} != {expected_count}"
        )

    used_repo_ids: set[str] = set()
    repositories: list[dict[str, Any]] = []
    for bucket in sorted(buckets.values(), key=lambda item: natural_key(item["repo_id"])):
        repo_id = bucket["repo_id"]
        if repo_id.casefold() in used_repo_ids:
            raise ValueError(f"物理课程仓库 repo_id 冲突：{repo_id}")
        used_repo_ids.add(repo_id.casefold())
        member_groups = bucket["groups"]
        member_ids = unique([group["group_id"] for group in member_groups])
        codes = unique(
            {code for group in member_groups for code in group["course_codes"]}
        )
        names = unique(
            {name for group in member_groups for name in group["course_names"]}
        )
        source_paths = unique(
            [row["_source_uri"] for code in codes for row in code_rows[code]]
        )
        navigation_ids = unique(
            {
                group_id
                for member_id in member_ids
                for group_id in assignments_by_group[member_id][
                    "navigation_course_group_ids"
                ]
            }
        )
        repository = repo_record(
            repo_id=repo_id,
            repo_type="course",
            display_name=bucket["display_name"],
            description=(
                f"{bucket['display_name']}课程资料（{len(member_ids)} 个逻辑资源分组，"
                f"{len(codes)} 个课程代码）"
            ),
            merge_key=f"physical-repository:{bucket['physical_repository_id']}",
            merge_reason=(
                "版本化互斥物理仓库策略按 canonical owner 物化；"
                "CourseGroup 仅保留为导航和未来拆分证据"
            ),
            source_paths=source_paths,
            aliases=names,
            course_codes=codes,
        )
        repository.pop("resource_group_id", None)
        repository.update(
            {
                "physical_repository_id": bucket["physical_repository_id"],
                "member_resource_group_ids": member_ids,
                "preferred_source_repo_ids": unique(
                    [group["repo_id"] for group in member_groups]
                ),
                "materialization_kind": bucket["materialization_kind"],
                "canonical_owner": bucket["canonical_owner"],
                "split_lineage_course_group_ids": navigation_ids,
            }
        )
        repositories.append(repository)

    code_assignments: dict[str, dict[str, str]] = {}
    name_assignments: dict[str, dict[str, str]] = {}
    descriptors: list[dict[str, Any]] = []
    resource_groups: list[dict[str, Any]] = []
    for group in sorted(groups, key=lambda item: natural_key(item["repo_id"])):
        group_id = group["group_id"]
        assignment = assignments_by_group[group_id]
        codes = unique(group["course_codes"])
        names = unique(group["course_names"])
        resource_group = {
            "resource_group_id": group_id,
            "preferred_repo_id": group["repo_id"],
            "physical_repository_id": assignment["physical_repository_id"],
            "repo_id": assignment["repo_id"],
            "display_name": group["display_name"],
            "course_names": names,
            "course_codes": codes,
            "grouping_rule": group["grouping_rule"],
            "evidence": group["evidence"],
            "legacy_units": unique(group["legacy_units"]),
            "canonical_owner": assignment["canonical_owner"],
            "split_lineage": {
                "semantics": "navigation-evidence-only",
                "course_group_ids": assignment["navigation_course_group_ids"],
            },
            "status": "mapped",
        }
        if group["grouping_rule"] == "materialized-course-family":
            resource_group.update(
                {
                    "member_resource_group_ids": unique(
                        group["member_resource_group_ids"]
                    ),
                    "source_repo_ids": unique(group["source_repo_ids"]),
                    "approval": group["approval"],
                }
            )
        resource_groups.append(resource_group)

        public_assignment = {
            "resource_group_id": group_id,
            "physical_repository_id": assignment["physical_repository_id"],
            "repo_id": assignment["repo_id"],
        }
        for name in names:
            if name in name_assignments:
                raise ValueError(f"课程名 {name!r} 被分配到多个逻辑资源分组")
            name_assignments[name] = public_assignment
        for code in codes:
            if code in code_assignments:
                raise ValueError(f"课程代码 {code} 被分配到多个逻辑资源分组")
            code_assignments[code] = public_assignment
            descriptors.append(
                {
                    "descriptor_id": f"course-code:{code}",
                    "course_code": code,
                    "course_name": code_name[code],
                    "resource_group_id": group_id,
                    "physical_repository_id": assignment["physical_repository_id"],
                    "repo_id": assignment["repo_id"],
                    "record_ids": unique(
                        [row["record_id"] for row in code_rows[code]]
                    ),
                    "source_paths": unique(
                        [row["_source_uri"] for row in code_rows[code]]
                    ),
                    "status": "mapped",
                }
            )

    if set(code_assignments) != set(code_rows):
        raise ValueError("并非所有课程代码都已绑定物理资料仓库")
    descriptors.sort(key=lambda item: natural_key(item["course_code"]))
    resource_groups.sort(key=lambda item: natural_key(item["preferred_repo_id"]))
    return repositories, code_assignments, name_assignments, descriptors, resource_groups


def requirement_repo_id(plan: dict[str, Any]) -> str:
    version = "".join(re.findall(r"\d+", plan["plan_version"])) or "UNVERSIONED"
    department = re.sub(r"[^A-Za-z0-9]+", "-", plan["department_code"]).strip("-")
    major = re.sub(r"[^A-Za-z0-9]+", "-", plan["major_code"]).strip("-")
    return f"REQ-HIT-{version}-{department}-{major}"


def allocate_requirement_repositories(
    rows: list[dict[str, Any]],
    plan_metadata: dict[str, Any],
) -> tuple[list[dict[str, Any]], dict[str, str]]:
    pending_by_plan: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        if not row["course_code"]:
            pending_by_plan[row["source_plan"]].append(row)

    repositories: list[dict[str, Any]] = []
    plan_to_repo: dict[str, str] = {}
    used: set[str] = set()
    for plan_id in sorted(pending_by_plan):
        plan = plan_metadata[plan_id]
        repo_id = requirement_repo_id(plan)
        if repo_id.casefold() in used:
            repo_id = f"{repo_id}-{digest(plan_id, 8)}"
        used.add(repo_id.casefold())
        plan_to_repo[plan_id] = repo_id
        pending_rows = pending_by_plan[plan_id]
        repositories.append(
            repo_record(
                repo_id=repo_id,
                repo_type="requirement-set",
                display_name=f"{plan['major_name']}培养要求",
                description=(
                    f"{plan['plan_version']} {plan['major_name']}无课程代码记录的逐项维护仓库"
                ),
                merge_key=f"plan-requirements:{plan_id}",
                merge_reason=(
                    "记录缺少稳定课程代码；按来源培养方案集中进一个维护仓库，"
                    "仓库内每条记录拥有独立 record_id，同仓不表示课程身份相同"
                ),
                source_paths=[row["_source_uri"] for row in pending_rows],
            )
        )
    return repositories, plan_to_repo


def attach_curriculum_mapping(
    rows: list[dict[str, Any]],
    code_assignments: dict[str, dict[str, str]],
    plan_to_requirement_repo: dict[str, str],
    repository_index: dict[str, dict[str, Any]],
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for row in rows:
        clean_row = {key: value for key, value in row.items() if not key.startswith("_")}
        if row["course_code"]:
            assignment = code_assignments[row["course_code"]]
            repo_id = assignment["repo_id"]
            repository = repository_index[repo_id]
            status = "mapped"
            identity_status = "coded"
            descriptor_id = f"course-code:{row['course_code']}"
            resource_group_id = assignment["resource_group_id"]
            physical_repository_id = assignment["physical_repository_id"]
        else:
            repo_id = plan_to_requirement_repo[row["source_plan"]]
            repository = repository_index[repo_id]
            status = "mapped"
            identity_status = "pending-course-code"
            descriptor_id = None
            resource_group_id = None
            physical_repository_id = None
        clean_row.update(
            {
                "repo_id": repo_id,
                "repo_type": repository["repo_type"],
                "resource_group_id": resource_group_id,
                "physical_repository_id": physical_repository_id,
                "descriptor_id": descriptor_id,
                "merge_key": repository["merge_key"],
                "merge_reason": repository["merge_reason"],
                "source_paths": [row["_source_uri"]],
                "status": status,
                "identity_status": identity_status,
            }
        )
        result.append(clean_row)
    return result


def git_output(repository: Path, *args: str) -> str:
    process = subprocess.run(
        ["git", *args],
        cwd=repository,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=120,
        check=True,
    )
    return process.stdout.strip()


def load_legacy_paths(repository: Path) -> tuple[str, list[str]]:
    commit = git_output(repository, "rev-parse", "origin/master")
    paths = git_output(repository, "ls-tree", "-r", "--name-only", "origin/master").splitlines()
    excluded = {".gitattributes", "LICENSE", "README.md"}
    return commit, [path for path in paths if path and path not in excluded]


def strip_extension(value: str) -> str:
    path = Path(value)
    return path.stem if path.suffix else value


def clean_legacy_name(value: str) -> str:
    text = normalize(strip_extension(value))
    while True:
        updated = re.sub(r"^【[^】]+】\s*", "", text).strip()
        if updated == text:
            break
        text = updated
    return LEGACY_NAME_ALIASES.get(text, text)


def legacy_boundary(path: str) -> tuple[str, str, str]:
    parts = path.split("/")
    top = parts[0]
    second = parts[1] if len(parts) > 1 else top

    if top == "【公共课】":
        return "/".join(parts[:2]), "course-candidate", clean_legacy_name(second)
    if top == "实验报告":
        boundary = "/".join(parts[:3]) if len(parts) >= 3 else top
        name = parts[2] if len(parts) >= 3 else top
        return boundary, "collection", clean_legacy_name(name)
    if top == "薪火笔记社-PPT及报告模板":
        return top, "shared", top
    if top == "薪火笔记社-竞赛":
        if second == "【软件】交通仿真软件教程":
            return "/".join(parts[:2]), "software", clean_legacy_name(second)
        if second == "HIT外置" and len(parts) >= 3:
            return "/".join(parts[:3]), "course-candidate", clean_legacy_name(parts[2])
        return "/".join(parts[:2]), "competition", clean_legacy_name(second)
    if top == "校内资源":
        if len(parts) >= 4 and parts[1] == "材料科学与工程学院":
            if parts[2] == "专业基础课":
                return "/".join(parts[:4]), "course-candidate", clean_legacy_name(parts[3])
            if parts[2] == "细分专业课" and len(parts) >= 5:
                return "/".join(parts[:5]), "course-candidate", clean_legacy_name(parts[4])
        return top, "shared", top
    if top in {"数学学院", "物理学院"}:
        if len(parts) >= 4 and parts[1] == "细分专业课":
            return "/".join(parts[:4]), "course-candidate", clean_legacy_name(parts[3])
        if len(parts) >= 3 and parts[1] == "专业基础课":
            return "/".join(parts[:3]), "course-candidate", clean_legacy_name(parts[2])
    if top == "未来技术学院":
        return "/".join(parts[:2]), "course-candidate", clean_legacy_name(second)
    if top == "询问中":
        return "/".join(parts[:2]), "pending", clean_legacy_name(second)
    return "/".join(parts[:2]), "course-candidate", clean_legacy_name(second)


def new_legacy_repo_id(prefix: str, key: str) -> str:
    return f"{prefix}-{digest(key, 12)}"


def allocate_legacy(
    *,
    commit: str,
    paths: list[str],
    name_assignments: dict[str, dict[str, str]],
    legacy_unit_to_group: dict[str, dict[str, Any]],
    resource_group_assignments: dict[str, dict[str, str]],
    repositories: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    repository_index = {repo["repo_id"]: repo for repo in repositories}
    root_names = unique({path.split("/", 1)[0] for path in paths})
    legacy_roots: list[dict[str, Any]] = []

    for root_name in root_names:
        if root_name not in ROOT_REPOSITORIES:
            raise ValueError(f"缺少 legacy 根目录仓库映射：{root_name}")
        repo_id, repo_type = ROOT_REPOSITORIES[root_name]
        root_uri = legacy_uri(commit, f"{root_name}/")
        if repo_id not in repository_index:
            repository = repo_record(
                repo_id=repo_id,
                repo_type=repo_type,
                display_name=root_name,
                description=f"历史资料分类：{root_name}",
                merge_key=f"legacy-root:{digest(root_name, 16)}",
                merge_reason="保留原有顶层导航与分类边界；该仓库只维护目录索引和共享资料，不替代课程资料仓库",
                source_paths=[root_uri],
            )
            repositories.append(repository)
            repository_index[repo_id] = repository
        legacy_roots.append(
            {
                "legacy_root": root_name,
                "repo_id": repo_id,
                "repo_type": repo_type,
                "merge_key": repository_index[repo_id]["merge_key"],
                "merge_reason": repository_index[repo_id]["merge_reason"],
                "source_paths": [root_uri],
                "status": "mapped",
            }
        )

    grouped_paths: dict[tuple[str, str, str], list[str]] = defaultdict(list)
    for path in paths:
        grouped_paths[legacy_boundary(path)].append(path)

    configured_units_seen: set[str] = set()
    legacy_units: list[dict[str, Any]] = []
    for (boundary, kind, display_name), unit_paths in sorted(
        grouped_paths.items(), key=lambda item: natural_key(item[0][0])
    ):
        source_paths = [legacy_uri(commit, path) for path in unit_paths]
        aliases = [boundary, display_name]
        explicit_group = legacy_unit_to_group.get(normalize(boundary))
        assignment = (
            resource_group_assignments.get(explicit_group["group_id"])
            if explicit_group
            else name_assignments.get(display_name)
        )
        resource_group_id: str | None = None
        physical_repository_id: str | None = None
        member_repo_ids: list[str] = []

        if assignment:
            repo_id = assignment["repo_id"]
            repository = repository_index[repo_id]
            repository["source_paths"] = unique(repository["source_paths"] + source_paths)
            repository["aliases"] = unique(repository["aliases"] + aliases)
            repo_type = "course"
            merge_key = repository["merge_key"]
            merge_reason = (
                "历史资料单元先绑定唯一逻辑 ResourceGroup，再沿互斥物理仓库策略进入唯一目标"
            )
            status = "mapped"
            resource_group_id = assignment["resource_group_id"]
            physical_repository_id = assignment["physical_repository_id"]
            if explicit_group:
                configured_units_seen.add(normalize(boundary))
        else:
            if kind == "course-candidate":
                repo_type = "course"
                prefix = "LEGACY"
                merge_reason = (
                    "现有资料目录表现为稳定课程，但培养方案中没有可确认的名称绑定；"
                    "建立独立资源仓库并保留原路径，后续可通过显式分组合并"
                )
            elif kind == "collection":
                repo_type = "collection"
                prefix = "COLL"
                merge_reason = "原目录是跨实验项目的稳定集合，保留为集合仓库并维护成员关系"
            elif kind == "competition":
                repo_type = "competition"
                prefix = "COMP"
                merge_reason = "原目录属于竞赛专题，不与培养方案课程合并"
            elif kind == "software":
                repo_type = "software"
                prefix = "SOFTWARE"
                merge_reason = "原目录是完整软件与教程分发单元，保持独立仓库以保留内部文件结构"
            elif kind == "shared":
                repo_type = "shared"
                prefix = "SHARED"
                merge_reason = "原目录是跨课程共享资料，保持共享仓库，不复制到多个课程仓库"
            elif kind == "pending":
                repo_type = "collection"
                prefix = "PENDING"
                merge_reason = "原目录身份尚未确定；先建立可访问维护仓库并保留原路径，状态显式标记为待绑定"
            else:
                raise ValueError(f"未知 legacy kind：{kind}")

            repo_id = new_legacy_repo_id(prefix, boundary)
            merge_key = f"legacy-unit:{digest(boundary, 20)}"
            status = "mapped-pending-identity" if kind == "pending" else "mapped"
            if repo_id not in repository_index:
                repository = repo_record(
                    repo_id=repo_id,
                    repo_type=repo_type,
                    display_name=display_name,
                    description=f"历史资料：{display_name}",
                    merge_key=merge_key,
                    merge_reason=merge_reason,
                    source_paths=source_paths,
                    status="planned",
                    aliases=aliases,
                    member_repo_ids=member_repo_ids,
                )
                repositories.append(repository)
                repository_index[repo_id] = repository
            else:
                repository_index[repo_id]["source_paths"] = unique(
                    repository_index[repo_id]["source_paths"] + source_paths
                )

        legacy_units.append(
            {
                "legacy_unit": boundary,
                "display_name": display_name,
                "repo_id": repo_id,
                "repo_type": repo_type,
                "resource_group_id": resource_group_id,
                "physical_repository_id": physical_repository_id,
                "merge_key": merge_key,
                "merge_reason": merge_reason,
                "source_paths": unique(source_paths),
                "status": status,
                "member_repo_ids": member_repo_ids,
            }
        )

    missing_configured_units = set(legacy_unit_to_group) - configured_units_seen
    if missing_configured_units:
        raise ValueError(
            f"资源分组配置引用不存在的历史单元：{unique(missing_configured_units)}"
        )
    represented_paths = [path for unit in legacy_units for path in unit["source_paths"]]
    if len(represented_paths) != len(set(represented_paths)):
        raise ValueError("legacy 文件被重复分配到物理仓库")
    return legacy_roots, legacy_units




def add_control_repositories(repositories: list[dict[str, Any]]) -> None:
    for item in CONTROL_REPOSITORIES:
        repositories.append(
            repo_record(
                repo_id=item["repo_id"],
                repo_type=item["repo_type"],
                display_name=item["display_name"],
                description=item["description"],
                merge_key=f"control:{item['repo_id']}",
                merge_reason="组织级控制面或仓库模板，独立维护且不与课程内容仓库合并",
                source_paths=[],
            )
        )


def validate_manifest(manifest: dict[str, Any], expected_rows: int, expected_paths: int) -> None:
    repositories = manifest["repositories"]
    records = manifest["curriculum_records"]
    descriptors = manifest["course_descriptors"]
    resource_groups = manifest["resource_groups"]
    legacy_roots = manifest["legacy_roots"]
    legacy_units = manifest["legacy_units"]

    if manifest.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"manifest schema_version 错误：{manifest.get('schema_version')}")
    if len(records) != expected_rows:
        raise ValueError(f"培养方案记录覆盖错误：{len(records)} != {expected_rows}")
    record_ids = [record["record_id"] for record in records]
    if len(record_ids) != len(set(record_ids)):
        raise ValueError("record_id 不唯一")

    repo_ids = [repo["repo_id"] for repo in repositories]
    if len(repo_ids) != len({repo_id.casefold() for repo_id in repo_ids}):
        raise ValueError("repo_id 大小写不敏感冲突")
    repo_set = set(repo_ids)
    legacy_references = sorted(
        (
            repo_set
            | {
                repo.get("template_id")
                for repo in repositories
                if repo.get("template_id") is not None
            }
        )
        & FROZEN_LEGACY_REPO_IDS
    )
    if legacy_references:
        raise ValueError(f"manifest v2 禁止引用冻结旧世代仓库：{legacy_references}")
    expected_control_ids = {item["repo_id"] for item in CONTROL_REPOSITORIES}
    actual_control_ids = {
        repo["repo_id"]
        for repo in repositories
        if repo["repo_type"] in {"control", "template"}
    }
    if actual_control_ids != expected_control_ids:
        raise ValueError(
            "v2 控制面仓库不完整："
            f"missing={sorted(expected_control_ids - actual_control_ids)}, "
            f"extra={sorted(actual_control_ids - expected_control_ids)}"
        )
    if manifest.get("policy", {}).get("authority") != "fireworks-course-registry-v2":
        raise ValueError("manifest v2 权威注册表错误")
    repository_index = {repo["repo_id"]: repo for repo in repositories}
    for repo_id in repo_ids:
        if len(repo_id) > 100 or not re.fullmatch(r"[A-Za-z0-9._-]+", repo_id):
            raise ValueError(f"非法 GitHub repo_id：{repo_id}")

    for section_name, items in (
        ("repositories", repositories),
        ("curriculum_records", records),
        ("legacy_roots", legacy_roots),
        ("legacy_units", legacy_units),
    ):
        for index, item in enumerate(items):
            missing = [field for field in REQUIRED_ITEM_FIELDS if field not in item]
            if missing:
                raise ValueError(f"{section_name}[{index}] 缺字段：{missing}")
            if item["repo_id"] not in repo_set:
                raise ValueError(f"{section_name}[{index}] 指向不存在仓库：{item['repo_id']}")
            if item["status"] in {"unmapped", "unknown", ""}:
                raise ValueError(f"{section_name}[{index}] 未映射")

    descriptor_codes = [descriptor["course_code"] for descriptor in descriptors]
    descriptor_ids = [descriptor["descriptor_id"] for descriptor in descriptors]
    if len(descriptor_codes) != len(set(descriptor_codes)):
        raise ValueError("课程代码 descriptor 不唯一")
    if len(descriptor_ids) != len(set(descriptor_ids)):
        raise ValueError("descriptor_id 不唯一")
    coded_records = [record for record in records if record["course_code"]]
    coded_record_codes = {record["course_code"] for record in coded_records}
    if set(descriptor_codes) != coded_record_codes:
        raise ValueError("课程代码 descriptor 覆盖与培养方案不一致")
    course_groups = manifest.get("course_groups", [])
    course_group_memberships = manifest.get("course_group_memberships", [])
    course_group_ids = [group.get("course_group_id") for group in course_groups]
    if len(course_group_ids) != len(set(course_group_ids)):
        raise ValueError("course_group_id 不唯一")
    if any(
        "repo_id" in item or "resource_group_id" in item
        for item in [*course_groups, *course_group_memberships]
    ):
        raise ValueError("课程组不得携带资料仓库或资源分组归属")
    membership_pairs = [
        (membership.get("course_group_id"), membership.get("course_code"))
        for membership in course_group_memberships
    ]
    if len(membership_pairs) != len(set(membership_pairs)):
        raise ValueError("课程组成员关系重复")
    memberships_by_group: dict[str, set[str]] = defaultdict(set)
    memberships_by_code: dict[str, set[str]] = defaultdict(set)
    for membership in course_group_memberships:
        group_id = membership.get("course_group_id")
        course_code = membership.get("course_code")
        if group_id not in set(course_group_ids) or course_code not in coded_record_codes:
            raise ValueError("课程组成员引用不存在的课程组或课程代码")
        memberships_by_group[group_id].add(course_code)
        memberships_by_code[course_code].add(group_id)
    for group in course_groups:
        if memberships_by_group[group["course_group_id"]] != set(group["course_codes"]):
            raise ValueError(f"课程组成员快照不一致：{group['course_group_id']}")
    if set(memberships_by_code) != coded_record_codes:
        raise ValueError("课程组成员关系未覆盖全部课程代码")
    if not any(len(group_ids) > 1 for group_ids in memberships_by_code.values()):
        raise ValueError("课程组配置未产生预期的重叠成员关系")

    resource_group_ids = [group["resource_group_id"] for group in resource_groups]
    if len(resource_group_ids) != len(set(resource_group_ids)):
        raise ValueError("resource_group_id 不唯一")
    group_index = {group["resource_group_id"]: group for group in resource_groups}
    descriptor_index = {descriptor["course_code"]: descriptor for descriptor in descriptors}
    physical_repositories = [
        repository
        for repository in repositories
        if repository["repo_type"] == "course"
        and repository.get("physical_repository_id")
    ]
    physical_ids = [repository["physical_repository_id"] for repository in physical_repositories]
    if len(physical_ids) != len(set(physical_ids)):
        raise ValueError("physical_repository_id 不唯一")
    physical_by_id = {
        repository["physical_repository_id"]: repository
        for repository in physical_repositories
    }
    represented_group_ids: list[str] = []
    for repository in physical_repositories:
        if "resource_group_id" in repository:
            raise ValueError("物理课程仓库不得携带单值 resource_group_id")
        member_ids = repository.get("member_resource_group_ids", [])
        if not member_ids or len(member_ids) != len(set(member_ids)):
            raise ValueError(f"物理课程仓库成员无效：{repository['repo_id']}")
        represented_group_ids.extend(member_ids)
        expected_codes = unique(
            {
                code
                for group_id in member_ids
                for code in group_index[group_id]["course_codes"]
            }
        )
        if repository.get("course_codes") != expected_codes:
            raise ValueError(f"物理仓库课程代码并集错误：{repository['repo_id']}")
    if (
        len(represented_group_ids) != len(set(represented_group_ids))
        or set(represented_group_ids) != set(resource_group_ids)
    ):
        raise ValueError("物理课程仓库未将每个 ResourceGroup 恰好覆盖一次")

    grouped_codes: list[str] = []
    for group in resource_groups:
        repo_id = group["repo_id"]
        physical_id = group.get("physical_repository_id")
        repository = physical_by_id.get(physical_id)
        if not repository or repository["repo_id"] != repo_id:
            raise ValueError(f"资源分组指向不存在物理仓库：{group['resource_group_id']}")
        if group["resource_group_id"] not in repository["member_resource_group_ids"]:
            raise ValueError(f"资源分组未列入物理仓库成员：{group['resource_group_id']}")
        grouped_codes.extend(group["course_codes"])
    if len(grouped_codes) != len(set(grouped_codes)) or set(grouped_codes) != set(
        descriptor_codes
    ):
        raise ValueError("资源分组未将每个课程代码恰好分配一次")

    record_ids_by_code: dict[str, set[str]] = defaultdict(set)
    for record in coded_records:
        record_ids_by_code[record["course_code"]].add(record["record_id"])
        descriptor = descriptor_index[record["course_code"]]
        if record.get("descriptor_id") != descriptor["descriptor_id"]:
            raise ValueError(f"培养方案记录 descriptor 绑定错误：{record['record_id']}")
        if record["repo_id"] != descriptor["repo_id"]:
            raise ValueError(f"培养方案记录仓库绑定错误：{record['record_id']}")
        if record.get("resource_group_id") != descriptor["resource_group_id"]:
            raise ValueError(f"培养方案记录资源分组绑定错误：{record['record_id']}")
        if record.get("physical_repository_id") != descriptor["physical_repository_id"]:
            raise ValueError(f"培养方案记录物理仓库绑定错误：{record['record_id']}")
    for descriptor in descriptors:
        if descriptor["resource_group_id"] not in group_index:
            raise ValueError(f"descriptor 指向不存在资源分组：{descriptor['course_code']}")
        group = group_index[descriptor["resource_group_id"]]
        if descriptor["repo_id"] != group["repo_id"]:
            raise ValueError(f"descriptor 与资源分组仓库不一致：{descriptor['course_code']}")
        if descriptor.get("physical_repository_id") != group["physical_repository_id"]:
            raise ValueError(f"descriptor 与资源分组物理仓库不一致：{descriptor['course_code']}")
        if set(descriptor["record_ids"]) != record_ids_by_code[descriptor["course_code"]]:
            raise ValueError(f"descriptor 记录覆盖错误：{descriptor['course_code']}")

    for record in records:
        if not record["course_code"] and (
            record.get("descriptor_id") is not None
            or record.get("resource_group_id") is not None
            or record.get("physical_repository_id") is not None
        ):
            raise ValueError(f"无代码记录意外绑定课程 descriptor：{record['record_id']}")
    for item in [*repositories, *legacy_units]:
        for member_repo_id in item.get("member_repo_ids", []):
            if member_repo_id not in repo_set:
                raise ValueError(f"成员仓库引用不存在：{member_repo_id}")

    represented_paths = [path for unit in legacy_units for path in unit["source_paths"]]
    if len(represented_paths) != expected_paths:
        raise ValueError(
            f"legacy 文件覆盖错误：{len(represented_paths)} != {expected_paths}"
        )
    if len(represented_paths) != len(set(represented_paths)):
        raise ValueError("legacy 文件被重复分配")

    expected_roots = set(ROOT_REPOSITORIES)
    represented_roots = {root["legacy_root"] for root in legacy_roots}
    if represented_roots != expected_roots:
        raise ValueError(
            f"legacy 根目录覆盖错误：missing={expected_roots - represented_roots}, "
            f"extra={represented_roots - expected_roots}"
        )


def main() -> int:
    args = parse_args()
    rows, plan_metadata = load_curriculum(args.plans)
    course_group_config = load_course_groups(args.course_groups, rows)
    group_config = load_resource_groups(args.resource_groups, rows)
    base_groups = build_base_course_groups(rows, group_config)
    family_config = load_course_families(
        args.course_families,
        args.course_family_migration,
        base_groups,
        args.course_family_stage,
    )
    effective_groups = apply_course_families(base_groups, family_config)
    physical_policy = load_physical_repository_policy(
        args.physical_repository_policy,
        effective_groups,
        family_config["stage_id"],
    )
    effective_legacy_unit_to_group = remap_legacy_unit_groups(
        group_config["legacy_unit_to_group"], family_config, effective_groups
    )

    (
        repositories,
        code_assignments,
        name_assignments,
        course_descriptors,
        resource_groups,
    ) = allocate_course_repositories(
        rows,
        effective_groups,
        physical_policy,
        course_group_config["memberships"],
    )
    resource_group_assignments = {
        group["resource_group_id"]: {
            "resource_group_id": group["resource_group_id"],
            "physical_repository_id": group["physical_repository_id"],
            "repo_id": group["repo_id"],
        }
        for group in resource_groups
    }
    requirement_repositories, plan_to_requirement_repo = allocate_requirement_repositories(
        rows, plan_metadata
    )
    repositories.extend(requirement_repositories)
    add_control_repositories(repositories)

    repository_index = {repo["repo_id"]: repo for repo in repositories}
    curriculum_records = attach_curriculum_mapping(
        rows, code_assignments, plan_to_requirement_repo, repository_index
    )

    legacy_commit, legacy_paths = load_legacy_paths(args.legacy_repo)
    legacy_roots, legacy_units = allocate_legacy(
        commit=legacy_commit,
        paths=legacy_paths,
        name_assignments=name_assignments,
        legacy_unit_to_group=effective_legacy_unit_to_group,
        resource_group_assignments=resource_group_assignments,
        repositories=repositories,
    )

    repositories.sort(key=lambda repo: natural_key(repo["repo_id"]))
    curriculum_records.sort(
        key=lambda record: (natural_key(record["source_plan"]), record["source_ordinal"])
    )

    counts_by_type: dict[str, int] = defaultdict(int)
    for repository in repositories:
        counts_by_type[repository["repo_type"]] += 1
    physical_course_repositories = [
        repository
        for repository in repositories
        if repository["repo_type"] == "course"
        and repository.get("physical_repository_id")
    ]
    multi_code_groups = sum(
        len(group["course_codes"]) > 1 for group in resource_groups
    )
    explicit_groups = sum(
        group["grouping_rule"] == "explicit-reviewed" for group in resource_groups
    )
    family_groups = sum(
        group["grouping_rule"] == "materialized-course-family"
        for group in resource_groups
    )
    owner_depth_counts: dict[int, int] = defaultdict(int)
    for group in resource_groups:
        owner_depth_counts[group["canonical_owner"]["depth"]] += 1

    physical_stage = physical_policy["stage"]
    if len(repositories) != physical_stage["expected_total_repository_count"]:
        raise ValueError(
            "物理仓库策略的总仓库数量已漂移："
            f"{len(repositories)} != {physical_stage['expected_total_repository_count']}"
        )
    if {str(depth): count for depth, count in sorted(owner_depth_counts.items())} != {
        str(depth): count
        for depth, count in physical_stage["expected_canonical_owner_depth_counts"].items()
    }:
        raise ValueError("canonical owner 深度分布已漂移")

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "organization": args.organization,
        "policy": {
            "authority": "fireworks-course-registry-v2",
            "curriculum_identity": "record-and-exact-course-code",
            "logical_resource_group_cardinality": "one-course-code-to-one-resource-group",
            "physical_repository_cardinality": "many-resource-groups-to-one-repository",
            "automatic_resource_grouping": "exact-normalized-course-name-then-materialized-course-family",
            "cross_name_grouping": "versioned-user-approved-family-config-only",
            "physical_repository_grouping": "versioned-exclusive-canonical-owner-policy-only",
            "course_groups_are_navigation_evidence_only": True,
            "uncoded_maintenance": "one requirement-set repository per source plan",
            "legacy_path_policy": "preserve relative path beneath the selected legacy unit",
            "content_review": "reuse-current-cleaned-state-without-second-review",
        },
        "sources": {
            "curriculum": {
                "campus": "hit",
                "plan_version": "2022版",
                "plan_count": len(plan_metadata),
                "record_count": len(rows),
                "coded_record_count": sum(bool(row["course_code"]) for row in rows),
                "uncoded_record_count": sum(not row["course_code"] for row in rows),
                "distinct_course_code_count": len(course_descriptors),
            },
            "resource_groups": {
                "file": args.resource_groups.as_posix(),
                "schema_version": 1,
                "curriculum_identity_sha256": group_config["identity_sha256"],
                "explicit_group_count": len(group_config["groups"]),
            },
            "course_groups": {
                "file": args.course_groups.as_posix(),
                "schema_version": 1,
                "source_curriculum_identity_sha256": course_group_config[
                    "identity_sha256"
                ],
                "definition_count": len(course_group_config["definitions"]),
                "course_group_count": len(course_group_config["course_groups"]),
                "membership_count": len(course_group_config["memberships"]),
                "overlapping_membership": "allowed-and-expected",
                "course_groups_do_not_imply_repository_merge": True,
            },
            "course_families": {
                "file": args.course_families.as_posix(),
                "migration_file": args.course_family_migration.as_posix(),
                "schema_version": 1,
                "source_resource_group_identity_sha256": family_config[
                    "identity_sha256"
                ],
                "stage_id": family_config["stage_id"],
                "base_resource_group_count": len(base_groups),
                "family_group_count": family_groups,
                "result_resource_group_count": len(resource_groups),
                "course_repository_reduction": len(base_groups) - len(resource_groups),
            },
            "physical_repositories": {
                "file": args.physical_repository_policy.as_posix(),
                "schema_version": 1,
                "role": "exclusive-physical-repository-policy",
                "stage_id": physical_policy["stage_id"],
                "source_resource_group_identity_sha256": physical_policy[
                    "identity_sha256"
                ],
                "resource_group_count": len(resource_groups),
                "physical_course_repository_count": len(physical_course_repositories),
                "canonical_owner_algorithm": physical_policy["document"][
                    "canonical_owner"
                ]["algorithm"],
                "hierarchy_fields": list(physical_policy["fields"]),
                "shared_frontier_depth": physical_policy["frontier_depth"],
                "course_groups_are_navigation_evidence_only": True,
            },
            "legacy": {
                "repository": "HIT-Fireworks/fireworks-attachments",
                "commit": legacy_commit,
                "file_count": len(legacy_paths),
                "root_count": len({path.split("/", 1)[0] for path in legacy_paths}),
            },
        },
        "summary": {
            "repository_count": len(repositories),
            "repository_counts_by_type": dict(sorted(counts_by_type.items())),
            "resource_group_count": len(resource_groups),
            "physical_course_repository_count": len(physical_course_repositories),
            "physical_repository_reduction": len(resource_groups)
            - len(physical_course_repositories),
            "canonical_owner_depth_counts": {
                str(depth): count for depth, count in sorted(owner_depth_counts.items())
            },
            "multi_code_resource_group_count": multi_code_groups,
            "explicit_resource_group_count": explicit_groups,
            "course_family_group_count": family_groups,
            "base_resource_group_count": len(base_groups),
            "course_repository_reduction": len(base_groups) - len(resource_groups),
            "course_group_count": len(course_group_config["course_groups"]),
            "course_group_membership_count": len(course_group_config["memberships"]),
            "course_codes_in_multiple_course_groups": sum(
                count > 1
                for count in {
                    code: sum(
                        membership["course_code"] == code
                        for membership in course_group_config["memberships"]
                    )
                    for code in {row["course_code"] for row in rows if row["course_code"]}
                }.values()
            ),
            "course_descriptor_count": len(course_descriptors),
            "curriculum_record_count": len(curriculum_records),
            "legacy_root_count": len(legacy_roots),
            "legacy_unit_count": len(legacy_units),
            "legacy_file_count": len(legacy_paths),
            "unmapped_curriculum_records": 0,
            "unmapped_course_descriptors": 0,
            "unmapped_legacy_files": 0,
        },
        "repositories": repositories,
        "resource_groups": resource_groups,
        "course_groups": course_group_config["course_groups"],
        "course_group_memberships": course_group_config["memberships"],
        "course_descriptors": course_descriptors,
        "curriculum_records": curriculum_records,
        "legacy_roots": legacy_roots,
        "legacy_units": legacy_units,
    }

    validate_manifest(manifest, len(rows), len(legacy_paths))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(manifest["summary"], ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

use crate::jwts::{CandidatePlan, CandidateSnapshot};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeType {
    PlanMetadata,
    CourseOccurrence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurriculumChange {
    pub change_id: String,
    pub change_type: ChangeType,
    pub kind: ChangeKind,
    pub plan_id: String,
    pub occurrence_key: Option<String>,
    pub occurrence_index: Option<usize>,
    pub course_code: Option<String>,
    pub course_name: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub title: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub change_count: usize,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurriculumDiff {
    pub schema_version: u32,
    pub generated_at: String,
    pub source_identity: Value,
    pub diff_identity_sha256: String,
    pub summary: DiffSummary,
    pub changes: Vec<CurriculumChange>,
    pub current: CandidateSnapshot,
    pub candidate: CandidateSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Accept,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DecisionSet {
    pub diff_identity_sha256: String,
    pub decisions: BTreeMap<String, Decision>,
}

#[derive(Debug, Clone)]
struct Occurrence {
    course: Value,
    raw: Value,
    identity: String,
    sequence_index: usize,
}

#[derive(Debug, Clone)]
struct Slot {
    before: Option<Occurrence>,
    after: Option<Occurrence>,
    occurrence_key: String,
    occurrence_index: usize,
}

pub fn baseline_snapshot(manifest: &Value) -> Result<CandidateSnapshot> {
    let plans = manifest
        .get("curriculum_plans")
        .and_then(Value::as_array)
        .context("当前数据缺少培养方案")?;
    let records = manifest
        .get("curriculum_records")
        .and_then(Value::as_array)
        .context("当前数据缺少课程记录")?;
    let mut records_by_plan: HashMap<String, Vec<&Value>> = HashMap::new();
    for record in records {
        records_by_plan
            .entry(string_field(record, "source_plan").to_string())
            .or_default()
            .push(record);
    }
    let mut result = Vec::new();
    for plan in plans {
        let plan_id = string_field(plan, "plan_id").to_string();
        if plan_id.trim().is_empty() {
            bail!("培养方案编号缺失")
        }
        let mut courses = records_by_plan.remove(&plan_id).unwrap_or_default();
        courses.sort_by_key(|value| {
            value
                .get("source_ordinal")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX)
        });
        let mut info = plan.clone();
        if let Some(object) = info.as_object_mut() {
            object.insert("plan_id".to_string(), Value::String(plan_id.clone()));
        }
        result.push(CandidatePlan {
            plan_id,
            info,
            courses: courses.into_iter().map(strip_repository_fields).collect(),
        });
    }
    result.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
    let snapshot = CandidateSnapshot {
        generated_at: "baseline".to_string(),
        base_url: "registry-manifest".to_string(),
        plans: result,
    };
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn diff_snapshots(
    current: CandidateSnapshot,
    candidate: CandidateSnapshot,
) -> Result<CurriculumDiff> {
    build_diff(current, candidate)
}

fn build_diff(current: CandidateSnapshot, candidate: CandidateSnapshot) -> Result<CurriculumDiff> {
    let mut changes = Vec::new();
    visit_changes(&current, &candidate, |change| {
        changes.push(change);
        Ok(())
    })?;
    ensure_unique_change_ids(&changes)?;
    let source_identity = source_identity(&current, &candidate)?;
    let diff_identity_sha256 = diff_identity(&source_identity, &changes)?;
    let summary = summarize(&changes);
    Ok(CurriculumDiff {
        schema_version: 2,
        generated_at: Utc::now().to_rfc3339(),
        source_identity,
        diff_identity_sha256,
        summary,
        changes,
        current,
        candidate,
    })
}

fn visit_changes(
    current: &CandidateSnapshot,
    candidate: &CandidateSnapshot,
    mut visit: impl FnMut(CurriculumChange) -> Result<()>,
) -> Result<()> {
    validate_snapshot(current)?;
    validate_snapshot(candidate)?;
    validate_candidate_transition(current, candidate)?;
    let current_index = current.plans.iter()
        .map(|plan| (plan.plan_id.as_str(), plan)).collect::<BTreeMap<_, _>>();
    let candidate_index = candidate.plans.iter()
        .map(|plan| (plan.plan_id.as_str(), plan)).collect::<BTreeMap<_, _>>();
    let plan_ids = current_index.keys().chain(candidate_index.keys()).copied().collect::<BTreeSet<_>>();
    for plan_id in plan_ids {
        let before_plan = current_index.get(plan_id).copied();
        let after_plan = candidate_index.get(plan_id).copied();
        validate_same_plan_scope(before_plan, after_plan)?;
        let before_info = before_plan.map(|plan| canonical_metadata(&plan.info));
        let after_info = after_plan.map(|plan| canonical_metadata(&plan.info));
        if before_info != after_info {
            let identity = json!({"plan_id":plan_id,"before":before_info,"after":after_info});
            let kind = change_kind(before_info.as_ref(), after_info.as_ref());
            let explanation = metadata_explanation(&kind, before_info.as_ref(), after_info.as_ref());
            visit(CurriculumChange {
                change_id: format!("plan-metadata-change-{}", &sha256(&identity)[..20]),
                change_type: ChangeType::PlanMetadata,
                kind,
                plan_id: plan_id.to_string(),
                occurrence_key: None,
                occurrence_index: None,
                course_code: None,
                course_name: String::new(),
                before: before_info,
                after: after_info,
                title: format!("方案信息：{}", plan_title(after_plan.or(before_plan))),
                explanation,
            })?;
        }
        let before_courses = before_plan.map(|plan| plan.courses.as_slice()).unwrap_or(&[]);
        let after_courses = after_plan.map(|plan| plan.courses.as_slice()).unwrap_or(&[]);
        for slot in align_occurrences(plan_id, before_courses, after_courses) {
            let before = slot.before.map(|value| value.course);
            let after = slot.after.map(|value| value.course);
            if before == after { continue; }
            let representative = after.as_ref().or(before.as_ref()).context("课程差异为空")?;
            let kind = change_kind(before.as_ref(), after.as_ref());
            let code = normalized_string(representative.get("course_code"));
            let name = normalized_string(representative.get("course_name"));
            let identity = json!({"plan_id":plan_id,"occurrence_key":slot.occurrence_key,"before":before,"after":after});
            let label = course_label(&name, &code);
            let explanation = course_explanation(&kind, &label, before.as_ref(), after.as_ref());
            visit(CurriculumChange {
                change_id: format!("course-change-{}", &sha256(&identity)[..20]),
                change_type: ChangeType::CourseOccurrence,
                kind,
                plan_id: plan_id.to_string(),
                occurrence_key: Some(slot.occurrence_key),
                occurrence_index: Some(slot.occurrence_index),
                course_code: (!code.is_empty()).then_some(code),
                course_name: name,
                before,
                after,
                title: format!("{}：{}", plan_title(after_plan.or(before_plan)), label),
                explanation,
            })?;
        }
    }
    Ok(())
}

pub fn validate_diff(diff: &CurriculumDiff) -> Result<()> {
    if diff.schema_version != 2 {
        bail!("差异文件版本不受支持，请重新抓取并审阅")
    }
    let identity = source_identity(&diff.current, &diff.candidate)?;
    if diff.source_identity != identity {
        bail!("差异中的来源快照校验失败")
    }
    let mut expected = diff.changes.iter();
    let mut summary = DiffSummary::default();
    visit_changes(&diff.current, &diff.candidate, |change| {
        if expected.next() != Some(&change) {
            bail!("差异内容与冻结快照不一致，可能已被修改")
        }
        summary.change_count += 1;
        match change.kind {
            ChangeKind::Added => summary.added += 1,
            ChangeKind::Removed => summary.removed += 1,
            ChangeKind::Changed => summary.changed += 1,
        }
        Ok(())
    })?;
    if expected.next().is_some() || summary != diff.summary {
        bail!("差异内容与冻结快照不一致，可能已被修改")
    }
    ensure_unique_change_ids(&diff.changes)?;
    if diff.diff_identity_sha256 != diff_identity(&identity, &diff.changes)? {
        bail!("差异身份校验失败，旧裁决不能复用")
    }
    Ok(())
}

pub fn default_decisions(diff: &CurriculumDiff, decision: Decision) -> DecisionSet {
    DecisionSet {
        diff_identity_sha256: diff.diff_identity_sha256.clone(),
        decisions: diff
            .changes
            .iter()
            .map(|change| (change.change_id.clone(), decision.clone()))
            .collect(),
    }
}

pub fn materialize(diff: &CurriculumDiff, decisions: &DecisionSet) -> Result<CandidateSnapshot> {
    validate_diff(diff)?;
    if decisions.diff_identity_sha256 != diff.diff_identity_sha256 {
        bail!("这些选择属于另一批教务变化")
    }
    let expected = diff
        .changes
        .iter()
        .map(|change| change.change_id.as_str())
        .collect::<BTreeSet<_>>();
    let actual = decisions
        .decisions
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if let Some(unknown) = actual.difference(&expected).next() {
        bail!("选择中包含未知变化：{unknown}")
    }
    if let Some(missing) = expected.difference(&actual).next() {
        bail!("还有变化没有选择接受或保留现状：{missing}")
    }
    if decisions
        .decisions
        .values()
        .all(|value| *value == Decision::Reject)
        && !diff.changes.is_empty()
    {
        return Ok(diff.current.clone());
    }
    if decisions
        .decisions
        .values()
        .all(|value| *value == Decision::Accept)
    {
        return Ok(diff.candidate.clone());
    }

    let current_index: BTreeMap<_, _> = diff
        .current
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect();
    let candidate_index: BTreeMap<_, _> = diff
        .candidate
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect();
    let plan_ids = current_index
        .keys()
        .chain(candidate_index.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut plans = Vec::new();
    for plan_id in plan_ids {
        let before_plan = current_index.get(plan_id).copied();
        let after_plan = candidate_index.get(plan_id).copied();
        let metadata_change = diff.changes.iter().find(|change| {
            change.plan_id == plan_id && change.change_type == ChangeType::PlanMetadata
        });
        let info = if let Some(change) = metadata_change {
            match decisions
                .decisions
                .get(&change.change_id)
                .context("方案信息变化尚未选择")?
            {
                Decision::Accept => after_plan.map(|plan| plan.info.clone()),
                Decision::Reject => before_plan.map(|plan| plan.info.clone()),
            }
        } else {
            after_plan
                .map(|plan| plan.info.clone())
                .or_else(|| before_plan.map(|plan| plan.info.clone()))
        };
        let before_courses = before_plan
            .map(|plan| plan.courses.as_slice())
            .unwrap_or(&[]);
        let after_courses = after_plan
            .map(|plan| plan.courses.as_slice())
            .unwrap_or(&[]);
        let mut courses = Vec::new();
        for slot in align_occurrences(plan_id, before_courses, after_courses) {
            let matching_change = diff.changes.iter().find(|change| {
                change.change_type == ChangeType::CourseOccurrence
                    && change.plan_id == plan_id
                    && change.occurrence_key.as_deref() == Some(slot.occurrence_key.as_str())
            });
            let chosen = if let Some(change) = matching_change {
                match decisions
                    .decisions
                    .get(&change.change_id)
                    .context("课程变化尚未选择")?
                {
                    Decision::Accept => slot.after,
                    Decision::Reject => slot.before,
                }
            } else {
                slot.after.or(slot.before)
            };
            if let Some(value) = chosen {
                courses.push(value.raw);
            }
        }
        if let Some(info) = info {
            plans.push(CandidatePlan {
                plan_id: plan_id.to_string(),
                info,
                courses,
            });
        } else if !courses.is_empty() {
            bail!("方案信息已选择删除，但仍有课程被选择保留")
        }
    }
    plans.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
    let decision_identity = sha256(&serde_json::to_value(decisions)?);
    let snapshot = CandidateSnapshot {
        generated_at: format!("review:{}", &decision_identity[..20]),
        base_url: diff.candidate.base_url.clone(),
        plans,
    };
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn validate_snapshot(snapshot: &CandidateSnapshot) -> Result<()> {
    let mut plan_ids = BTreeSet::new();
    for plan in &snapshot.plans {
        if plan.plan_id.trim().is_empty() || !plan_ids.insert(plan.plan_id.as_str()) {
            bail!("方案编号缺失或重复")
        }
        validate_plan_identity(plan)?;
        for course in &plan.courses {
            if !course.is_object() {
                bail!("课程记录不是对象：{}", plan.plan_id)
            }
            let name = normalized_string(course.get("course_name"));
            if name.is_empty() && !declared_module_reference(plan, course) {
                bail!("课程缺少名称：{}", plan.plan_id)
            }
        }
    }
    Ok(())
}

fn declared_module_reference(plan: &CandidatePlan, course: &Value) -> bool {
    if string_field(&plan.info, "source_kind") != "execution"
        || string_field(course, "source_section") != "execution-module"
        || normalized_string(course.get("course_code")).is_empty()
    {
        return false;
    }
    let Some(relation) = course.get("relation").and_then(Value::as_object) else {
        return false;
    };
    let Some(module_id) = relation.get("module_id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
        return false;
    };
    let Some(direction_key) = relation.get("direction_key").and_then(Value::as_str).filter(|key| !key.is_empty()) else {
        return false;
    };
    let Some(modules) = plan.info.pointer("/academic_structure/module_details").and_then(Value::as_array) else {
        return false;
    };
    let canonical = canonical_course(course);
    modules.iter().any(|module| {
        string_field(module, "module_id") == module_id
            && string_field(module, "direction_key") == direction_key
            && module.get("courses").and_then(Value::as_array).is_some_and(|courses| {
                courses.iter().any(|member| canonical_course(member) == canonical)
            })
    })
}

fn validate_plan_identity(plan: &CandidatePlan) -> Result<()> {
    if !plan.info.is_object() {
        bail!("方案信息不是对象：{}", plan.plan_id)
    }
    let canonical = canonical_metadata(&plan.info);
    let embedded_id = normalized_string(canonical.get("plan_id"));
    if !embedded_id.is_empty() && embedded_id != plan.plan_id {
        bail!("方案编号与方案信息不一致：{}", plan.plan_id)
    }
    let source_kind = normalized_string(canonical.get("source_kind"));
    if source_kind.is_empty() {
        return Ok(());
    }
    if source_kind != "curriculum" && source_kind != "execution" {
        bail!("未知方案来源：{source_kind}")
    }
    // 旧管理器曾保存非 hit: 格式的方案编号。已有记录继续保留其身份，
    // 与新抓取的匹配只允许状态层通过来源、版本、院系、专业代码完成。
    if !plan.plan_id.starts_with("hit:") && source_kind == "curriculum" {
        if canonical
            .get("entry_cohort")
            .is_some_and(|value| !value.is_null())
        {
            bail!("培养方案不能带入学年级身份")
        }
        return Ok(());
    }
    let parts = plan.plan_id.split(':').collect::<Vec<_>>();
    let department = normalized_string(canonical.get("department_code"));
    let major = normalized_string(canonical.get("major_code"));
    if source_kind == "execution" {
        if parts.len() != 5 || parts[0] != "hit" || parts[1] != "execution" {
            bail!("执行教学计划编号不符合稳定格式：{}", plan.plan_id)
        }
        let cohort = normalized_string(canonical.get("entry_cohort"));
        if cohort.is_empty() || department.is_empty() || major.is_empty() {
            bail!("执行教学计划缺少年级、院系或专业代码：{}", plan.plan_id)
        }
        if parts[2] != identity_component(&cohort)
            || parts[3] != identity_component(&department)
            || parts[4] != identity_component(&major)
        {
            bail!("执行教学计划编号与来源范围不一致：{}", plan.plan_id)
        }
        if canonical
            .get("plan_version")
            .is_some_and(|value| !value.is_null())
        {
            bail!("执行教学计划不能把版本号当作入学年级：{}", plan.plan_id)
        }
    } else {
        if parts.len() != 4 || parts[0] != "hit" || parts[1].is_empty() {
            bail!("培养方案编号不符合稳定格式：{}", plan.plan_id)
        }
        let version = normalized_string(canonical.get("plan_version"));
        if version.is_empty() || department.is_empty() || major.is_empty() {
            bail!("培养方案缺少版本、院系或专业代码：{}", plan.plan_id)
        }
        if parts[1] != identity_component(&version)
            || parts[2] != identity_component(&department)
            || parts[3] != identity_component(&major)
        {
            bail!("培养方案编号与来源范围不一致：{}", plan.plan_id)
        }
        if canonical
            .get("entry_cohort")
            .is_some_and(|value| !value.is_null())
        {
            bail!("培养方案版本不能被解释为入学年级：{}", plan.plan_id)
        }
    }
    Ok(())
}

fn validate_candidate_transition(
    current: &CandidateSnapshot,
    candidate: &CandidateSnapshot,
) -> Result<()> {
    if !current.plans.is_empty() && candidate.plans.is_empty() {
        bail!("候选快照没有任何方案，不能据此删除全部计划")
    }
    let current_index = current
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect::<BTreeMap<_, _>>();
    let candidate_index = candidate
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect::<BTreeMap<_, _>>();
    for plan_id in current_index.keys() {
        if !candidate_index.contains_key(plan_id) {
            bail!("候选快照缺少方案 {plan_id}，且没有完整采集证据证明可删除")
        }
    }
    for plan in &candidate.plans {
        let capture = plan.info.get("source_capture");
        if capture.is_some()
            && capture
                .and_then(|value| value.get("complete"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            bail!("方案采集不完整，不能生成差异：{}", plan.plan_id)
        }
        let Some(before) = current_index.get(plan.plan_id.as_str()).copied() else {
            continue;
        };
        let before_info = canonical_metadata(&before.info);
        let after_info = canonical_metadata(&plan.info);
        let removes_metadata = semantic_removal(&before_info, &after_info);
        let removes_courses = align_occurrences(&plan.plan_id, &before.courses, &plan.courses)
            .iter()
            .any(|slot| slot.before.is_some() && slot.after.is_none());
        if (removes_metadata || removes_courses)
            && capture
                .and_then(|value| value.get("complete"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            bail!(
                "方案缺少完整采集证据，不能把未返回内容当作删除：{}",
                plan.plan_id
            )
        }
    }
    Ok(())
}

fn validate_same_plan_scope(
    before: Option<&CandidatePlan>,
    after: Option<&CandidatePlan>,
) -> Result<()> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(());
    };
    let before = canonical_metadata(&before.info);
    let after = canonical_metadata(&after.info);
    for key in [
        "source_kind",
        "entry_cohort",
        "plan_version",
        "department_code",
        "major_code",
    ] {
        let left = normalized_string(before.get(key));
        let right = normalized_string(after.get(key));
        if !left.is_empty() && !right.is_empty() && left != right {
            bail!("同一方案编号的来源范围发生冲突（{key}），请勿串用不同计划")
        }
    }
    Ok(())
}

fn align_occurrences(plan_id: &str, before: &[Value], after: &[Value]) -> Vec<Slot> {
    let before = occurrences(before);
    let after = occurrences(after);
    let mut used_before = vec![false; before.len()];
    let mut used_after = vec![false; after.len()];
    let mut pairs = Vec::new();

    // All exact matches are reserved before any approximate pairing. This prevents a changed
    // duplicate from consuming the only unchanged occurrence later in the source order.
    for before_index in 0..before.len() {
        let matched = after
            .iter()
            .enumerate()
            .filter(|(after_index, item)| {
                !used_after[*after_index]
                    && item.identity == before[before_index].identity
                    && item.course == before[before_index].course
            })
            .min_by_key(|(_, item)| {
                (
                    item.sequence_index
                        .abs_diff(before[before_index].sequence_index),
                    item.sequence_index,
                )
            })
            .map(|(index, _)| index);
        if let Some(after_index) = matched {
            used_before[before_index] = true;
            used_after[after_index] = true;
            pairs.push((
                Some(before[before_index].clone()),
                Some(after[after_index].clone()),
            ));
        }
    }

    let mut possible = Vec::new();
    for (before_index, before_item) in before.iter().enumerate() {
        if used_before[before_index] {
            continue;
        }
        for (after_index, after_item) in after.iter().enumerate() {
            if !used_after[after_index] && before_item.identity == after_item.identity {
                possible.push((
                    semantic_distance(&before_item.course, &after_item.course),
                    before_item
                        .sequence_index
                        .abs_diff(after_item.sequence_index),
                    before_item.sequence_index,
                    after_item.sequence_index,
                    before_index,
                    after_index,
                ));
            }
        }
    }
    possible.sort();
    for (_, _, _, _, before_index, after_index) in possible {
        if !used_before[before_index] && !used_after[after_index] {
            used_before[before_index] = true;
            used_after[after_index] = true;
            pairs.push((
                Some(before[before_index].clone()),
                Some(after[after_index].clone()),
            ));
        }
    }
    for (index, item) in before.iter().enumerate() {
        if !used_before[index] {
            pairs.push((Some(item.clone()), None));
        }
    }
    for (index, item) in after.iter().enumerate() {
        if !used_after[index] {
            pairs.push((None, Some(item.clone())));
        }
    }
    pairs.sort_by_key(|(before, after)| {
        let primary = after
            .as_ref()
            .map(|item| item.sequence_index)
            .or_else(|| before.as_ref().map(|item| item.sequence_index))
            .unwrap_or(usize::MAX);
        let removed = after.is_none();
        let old = before
            .as_ref()
            .map(|item| item.sequence_index)
            .unwrap_or(usize::MAX);
        let identity = after
            .as_ref()
            .or(before.as_ref())
            .map(|item| item.identity.clone())
            .unwrap_or_default();
        (primary, removed, old, identity)
    });
    let mut identity_counts: HashMap<String, usize> = HashMap::new();
    pairs
        .into_iter()
        .map(|(before, after)| {
            let representative = after.as_ref().or(before.as_ref()).expect("occurrence");
            let index = identity_counts
                .entry(representative.identity.clone())
                .or_default();
            let occurrence_index = *index;
            *index += 1;
            let occurrence_key = format!(
                "{}:{}:{}",
                plan_id, representative.identity, occurrence_index
            );
            Slot {
                before,
                after,
                occurrence_key,
                occurrence_index,
            }
        })
        .collect()
}

pub(crate) fn aligned_record_pairs(
    before: &[Value],
    after: &[Value],
) -> Vec<(Option<usize>, Option<usize>)> {
    align_occurrences("record-alignment", before, after)
        .into_iter()
        .map(|slot| {
            (
                slot.before.map(|item| item.sequence_index),
                slot.after.map(|item| item.sequence_index),
            )
        })
        .collect()
}

fn occurrences(courses: &[Value]) -> Vec<Occurrence> {
    courses
        .iter()
        .enumerate()
        .map(|(sequence_index, course)| Occurrence {
            raw: course.clone(),
            course: canonical_course(course),
            identity: course_identity(course),
            sequence_index,
        })
        .collect()
}
fn course_identity(course: &Value) -> String {
    let code = normalized_string(course.get("course_code"));
    if !code.is_empty() {
        format!("coded:{code}")
    } else {
        format!(
            "uncoded:{}",
            normalized_string(course.get("course_name")).nfkc().collect::<String>().to_lowercase()
        )
    }
}

pub(crate) fn canonical_course(course: &Value) -> Value {
    let mut value = course.clone();
    if let Some(object) = value.as_object_mut() {
        for key in [
            "record_id",
            "source_plan",
            "source_plan_file",
            "source_ordinal",
            "source_page",
            "source_row",
            "source_path",
            "repo_id",
            "repo_type",
            "resource_group_id",
            "physical_repository_id",
            "descriptor_id",
            "attachment_repo_id",
            "merge_key",
            "merge_reason",
            "source_paths",
            "status",
            "identity_status",
            "metadata_repo_id",
            "metadata_path",
            "fetched_at",
            "generated_at",
            "campus",
            "source_kind",
            "plan_version",
            "entry_cohort",
            "department_code",
            "school_name",
            "major_code",
            "major_name",
            "major_full_name",
            "program_type",
        ] {
            object.remove(key);
        }
    }
    strip_nested_provenance(&mut value);
    value
}

fn strip_repository_fields(record: &Value) -> Value {
    canonical_course(record)
}

fn canonical_metadata(info: &Value) -> Value {
    let mut value = info.clone();
    if let Some(object) = value.as_object_mut() {
        if !object.contains_key("plan_id") {
            if let Some(plan_id) = object.get("plan_ID").cloned() {
                object.insert("plan_id".to_string(), plan_id);
            }
        }
        object.remove("plan_ID");
        let execution = object.get("source_kind").and_then(Value::as_str) == Some("execution")
            || object
                .get("plan_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("hit:execution:"));
        if !execution && !object.contains_key("plan_version") {
            if let Some(year) = object.get("year").cloned() {
                object.insert("plan_version".to_string(), year);
            }
        }
        object.remove("year");
        for (target, alias) in [
            ("department_code", "college_code"),
            ("school_name", "college_name"),
        ] {
            if !object.contains_key(target) {
                if let Some(value) = object.get(alias).cloned() {
                    object.insert(target.to_string(), value);
                }
            }
            object.remove(alias);
        }
        let kind = if execution { "execution" } else { "curriculum" };
        if object
            .get("plan_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("hit:"))
        {
            object.entry("source_kind").or_insert(json!(kind));
        }
        object.remove("grade");
        for key in ["metadata_repo_id", "metadata_path", "source_plan_file"] {
            object.remove(key);
        }
    }
    strip_nested_provenance(&mut value);
    value
}

fn strip_nested_provenance(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in [
                "source_capture",
                "source_path",
                "source_url",
                "fetched_at",
                "generated_at",
                "captured_at",
            ] {
                object.remove(key);
            }
            for child in object.values_mut() {
                strip_nested_provenance(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_nested_provenance(child);
            }
        }
        _ => {}
    }
}

fn semantic_removal(before: &Value, after: &Value) -> bool {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => before.iter().any(|(key, value)| {
            !value.is_null()
                && match after.get(key) {
                    None | Some(Value::Null) => true,
                    Some(after) => semantic_removal(value, after),
                }
        }),
        (Value::Array(before), Value::Array(after)) => !before.is_empty() && after.is_empty(),
        _ => false,
    }
}

fn semantic_distance(left: &Value, right: &Value) -> usize {
    if left == right {
        return 0;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => left
            .keys()
            .chain(right.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|key| match (left.get(key), right.get(key)) {
                (Some(left), Some(right)) => semantic_distance(left, right),
                _ => 1,
            })
            .sum(),
        _ => 1,
    }
}

fn plan_title(plan: Option<&CandidatePlan>) -> String {
    let Some(plan) = plan else {
        return "未知方案".to_string();
    };
    let info = canonical_metadata(&plan.info);
    let kind = match normalized_string(info.get("source_kind")).as_str() {
        "execution" => "执行教学计划",
        _ => "培养方案",
    };
    let version = if kind == "执行教学计划" {
        let cohort = normalized_string(info.get("entry_cohort"));
        (!cohort.is_empty()).then(|| format!("{cohort}级"))
    } else {
        let version = normalized_string(info.get("plan_version"));
        (!version.is_empty()).then(|| format!("版本{version}"))
    };
    let school = normalized_string(info.get("school_name"));
    let full_name = normalized_string(info.get("major_full_name"));
    let major_name = normalized_string(info.get("major_name"));
    let major = if full_name.is_empty() {
        major_name
    } else {
        full_name
    };
    let major_code = normalized_string(info.get("major_code"));
    let mut parts = vec![kind.to_string()];
    if let Some(version) = version {
        parts.push(version);
    }
    if !school.is_empty() {
        parts.push(school);
    }
    if !major.is_empty() || !major_code.is_empty() {
        parts.push(course_label(&major, &major_code));
    }
    if parts.len() == 1 {
        parts.push(plan.plan_id.clone());
    }
    parts.join(" · ")
}

fn metadata_explanation(
    kind: &ChangeKind,
    before: Option<&Value>,
    after: Option<&Value>,
) -> String {
    match kind {
        ChangeKind::Added => "本次查询新增了该方案信息，可逐项审阅后入库".to_string(),
        ChangeKind::Removed => {
            "本次查询未返回该方案；这不等于学校永久删除方案，也不会删除已有资料".to_string()
        }
        ChangeKind::Changed => format!("方案字段变化：{}", change_summary(before, after)),
    }
}

fn course_explanation(
    kind: &ChangeKind,
    label: &str,
    before: Option<&Value>,
    after: Option<&Value>,
) -> String {
    match kind {
        ChangeKind::Added => format!("本次查询新增了{label}"),
        ChangeKind::Removed => format!(
            "本次查询未返回{label}，或已从当前计划移出；这不等于学校取消课程，也不会删除已有资料"
        ),
        ChangeKind::Changed => format!("{label}的教学字段变化：{}", change_summary(before, after)),
    }
}

fn change_summary(before: Option<&Value>, after: Option<&Value>) -> String {
    let mut changes = Vec::new();
    collect_changes("", before, after, &mut changes);
    let total = changes.len();
    let mut shown = changes.into_iter().take(8).collect::<Vec<_>>();
    if total > shown.len() {
        shown.push(format!("另有{}项变化", total - shown.len()));
    }
    if shown.is_empty() {
        "内容已调整（完整修改前后值已保留）".to_string()
    } else {
        shown.join("；")
    }
}

fn collect_changes(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    output: &mut Vec<String>,
) {
    if before == after {
        return;
    }
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            collect_changes(&child, before.get(key), after.get(key), output);
        }
        return;
    }
    let label = field_label(path);
    if path.starts_with("academic_structure")
        && matches!(before, Some(Value::Array(_) | Value::Object(_)))
    {
        output.push(format!("{label}已调整（完整前后结构已保留）"));
    } else {
        output.push(format!(
            "{label}：{}→{}",
            display_value(before),
            display_value(after)
        ));
    }
}

fn field_label(path: &str) -> String {
    let key = path.rsplit('.').next().unwrap_or(path);
    match key {
        "source_kind" => "方案来源".to_string(),
        "plan_version" => "方案版本".to_string(),
        "entry_cohort" => "入学年级".to_string(),
        "department_code" => "院系代码".to_string(),
        "school_name" => "院系".to_string(),
        "major_code" => "专业代码".to_string(),
        "major_name" => "专业名称".to_string(),
        "major_full_name" => "官方专业全称".to_string(),
        "program_type" => "专业类型".to_string(),
        "academic_structure" => "培养模块与毕业要求".to_string(),
        "course_name" => "课程名称".to_string(),
        "course_name_en" | "english_name" => "英文课程名称".to_string(),
        "credit" | "credits" => "学分".to_string(),
        "academic_year" | "school_year" => "开课学年".to_string(),
        "semester" => "开课学期".to_string(),
        "direction" | "major_direction" => "专业方向".to_string(),
        "course_nature" | "nature" => "课程性质".to_string(),
        "course_category" | "category" => "课程类别".to_string(),
        "total_hours" => "总学时".to_string(),
        "assessment" | "assessment_method" => "考核方式".to_string(),
        "graduation_requirement" | "requirement" => "毕业要求".to_string(),
        "notes" | "remark" => "备注".to_string(),
        _ => key.to_string(),
    }
}

fn display_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "未填写".to_string(),
        Some(Value::String(value)) if value.trim().is_empty() => "空".to_string(),
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Bool(value)) => if *value { "是" } else { "否" }.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(values)) => format!("{}项", values.len()),
        Some(Value::Object(values)) => format!("{}个字段", values.len()),
    }
}

fn course_label(name: &str, code: &str) -> String {
    match (name.is_empty(), code.is_empty()) {
        (false, false) => format!("{name}（{code}）"),
        (false, true) => name.to_string(),
        (true, false) => format!("课程（{code}）"),
        (true, true) => "未命名课程".to_string(),
    }
}

fn identity_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn change_kind(before: Option<&Value>, after: Option<&Value>) -> ChangeKind {
    if before.is_none() {
        ChangeKind::Added
    } else if after.is_none() {
        ChangeKind::Removed
    } else {
        ChangeKind::Changed
    }
}

fn source_identity(current: &CandidateSnapshot, candidate: &CandidateSnapshot) -> Result<Value> {
    Ok(json!({
        "current": snapshot_sha256(current)?,
        "candidate": snapshot_sha256(candidate)?,
    }))
}

fn snapshot_sha256(snapshot: &CandidateSnapshot) -> Result<String> {
    let mut writer = HashWriter(Sha256::new());
    writer.0.update(b"{\"base_url\":");
    serde_json::to_writer(&mut writer, &snapshot.base_url)?;
    writer.0.update(b",\"generated_at\":");
    serde_json::to_writer(&mut writer, &snapshot.generated_at)?;
    writer.0.update(b",\"plans\":[");
    for (index, plan) in snapshot.plans.iter().enumerate() {
        if index != 0 { writer.0.update(b","); }
        writer.0.update(b"{\"courses\":");
        serde_json::to_writer(&mut writer, &plan.courses)?;
        writer.0.update(b",\"info\":");
        serde_json::to_writer(&mut writer, &plan.info)?;
        writer.0.update(b",\"plan_id\":");
        serde_json::to_writer(&mut writer, &plan.plan_id)?;
        writer.0.update(b"}");
    }
    writer.0.update(b"]}");
    Ok(format!("{:x}", writer.0.finalize()))
}

fn diff_identity(source_identity: &Value, changes: &[CurriculumChange]) -> Result<String> {
    // 字段顺序与原 serde_json::to_value 的排序对象完全一致，旧裁决身份不变。
    #[derive(Serialize)]
    struct OrderedChange<'a> {
        after: &'a Option<Value>,
        before: &'a Option<Value>,
        change_id: &'a str,
        change_type: &'a ChangeType,
        course_code: &'a Option<String>,
        course_name: &'a str,
        explanation: &'a str,
        kind: &'a ChangeKind,
        occurrence_index: &'a Option<usize>,
        occurrence_key: &'a Option<String>,
        plan_id: &'a str,
        title: &'a str,
    }
    let mut writer = HashWriter(Sha256::new());
    writer.0.update(b"{\"changes\":[");
    for (index, change) in changes.iter().enumerate() {
        if index != 0 { writer.0.update(b","); }
        serde_json::to_writer(&mut writer, &OrderedChange {
            after: &change.after,
            before: &change.before,
            change_id: &change.change_id,
            change_type: &change.change_type,
            course_code: &change.course_code,
            course_name: &change.course_name,
            explanation: &change.explanation,
            kind: &change.kind,
            occurrence_index: &change.occurrence_index,
            occurrence_key: &change.occurrence_key,
            plan_id: &change.plan_id,
            title: &change.title,
        })?;
    }
    writer.0.update(b"],\"schema_version\":2,\"source_identity\":");
    serde_json::to_writer(&mut writer, source_identity)?;
    writer.0.update(b"}");
    Ok(format!("{:x}", writer.0.finalize()))
}

fn summarize(changes: &[CurriculumChange]) -> DiffSummary {
    DiffSummary {
        change_count: changes.len(),
        added: changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Added)
            .count(),
        removed: changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Removed)
            .count(),
        changed: changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Changed)
            .count(),
    }
}

fn ensure_unique_change_ids(changes: &[CurriculumChange]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for change in changes {
        if !ids.insert(change.change_id.as_str()) {
            bail!("差异中出现重复变化编号")
        }
    }
    Ok(())
}

fn normalized_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn sha256(value: &Value) -> String {
    value_sha256(value)
}

pub(crate) fn value_sha256(value: &Value) -> String {
    serialized_sha256(value).expect("canonical JSON")
}

pub(crate) fn serialized_sha256<T: serde::Serialize + ?Sized>(value: &T) -> Result<String> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

struct HashWriter(Sha256);

impl std::io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}


fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_hashes_preserve_existing_snapshot_and_decision_identities() {
        let old = json!({"course_code":"A1","course_name":"课程\"甲\\乙","credit":2.5,"unknown":{"z":[null,true,"中文"],"a":1}});
        let mut new = old.clone();
        new["credit"] = json!(3.5);
        let current = snapshot(legacy_plan(vec![old]), "before");
        let candidate = snapshot(legacy_plan(vec![new]), "after");
        for value in [&current, &candidate] {
            assert_eq!(snapshot_sha256(value).unwrap(), sha256(&serde_json::to_value(value).unwrap()));
        }
        let diff = diff_snapshots(current, candidate).unwrap();
        let legacy_identity = sha256(&json!({"schema_version":2,"source_identity":diff.source_identity,"changes":diff.changes}));
        assert_eq!(diff.diff_identity_sha256, legacy_identity);
        validate_diff(&diff).unwrap();
    }

    fn legacy_plan(courses: Vec<Value>) -> CandidatePlan {
        CandidatePlan {
            plan_id: "plan-a".into(),
            info: json!({"plan_ID":"plan-a","year":"2024","major_name":"计算机科学与技术"}),
            courses,
        }
    }

    fn execution_plan(cohort: &str, courses: Vec<Value>, complete: Option<bool>) -> CandidatePlan {
        let plan_id = format!("hit:execution:{cohort}:001:0809");
        let mut info = json!({
            "plan_id": plan_id,
            "campus": "hit",
            "source_kind": "execution",
            "entry_cohort": cohort,
            "department_code": "001",
            "school_name": "计算学部",
            "major_code": "0809",
            "major_name": "计算机科学与技术",
            "major_full_name": "计算机科学与技术（卓越班）",
            "program_type": "本科"
        });
        if let Some(complete) = complete {
            info["source_capture"] =
                json!({"complete":complete,"fetched_at":"old","pages":1,"rows":courses.len()});
        }
        CandidatePlan {
            plan_id,
            info,
            courses,
        }
    }

    fn curriculum_plan(courses: Vec<Value>) -> CandidatePlan {
        CandidatePlan {
            plan_id: "hit:2024%E7%89%88:001:0809".into(),
            info: json!({
                "plan_id":"hit:2024%E7%89%88:001:0809","source_kind":"curriculum",
                "plan_version":"2024版","department_code":"001","school_name":"计算学部",
                "major_code":"0809","major_name":"计算机科学与技术",
                "major_full_name":"计算机科学与技术","program_type":"本科"
            }),
            courses,
        }
    }

    fn snapshot(plan: CandidatePlan, generated_at: &str) -> CandidateSnapshot {
        CandidateSnapshot {
            generated_at: generated_at.into(),
            base_url: "test".into(),
            plans: vec![plan],
        }
    }

    fn course(code: Option<&str>, name: &str, credit: u64, semester: &str) -> Value {
        json!({"course_code":code,"course_name":name,"credit":credit,"semester":semester})
    }

    fn course_changes(diff: &CurriculumDiff) -> Vec<&CurriculumChange> {
        diff.changes
            .iter()
            .filter(|change| change.change_type == ChangeType::CourseOccurrence)
            .collect()
    }

    #[test]
    fn same_code_name_credit_and_semester_are_one_readable_change() {
        let current = snapshot(
            legacy_plan(vec![course(Some("A1"), "课程甲", 3, "大二秋")]),
            "before",
        );
        let candidate = snapshot(
            legacy_plan(vec![course(Some("A1"), "课程甲（荣誉）", 4, "大二春")]),
            "after",
        );
        let diff = diff_snapshots(current, candidate).unwrap();
        let changes = course_changes(&diff);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].explanation.contains("课程名称"));
        assert!(changes[0].explanation.contains("学分：3→4"));
        assert!(changes[0].explanation.contains("开课学期：大二秋→大二春"));
    }

    #[test]
    fn exact_duplicate_is_reserved_before_changed_pairing() {
        let current = snapshot(
            legacy_plan(vec![
                json!({"course_code":"A1","course_name":"课程甲","direction":"方向甲","credit":2}),
                json!({"course_code":"A1","course_name":"课程甲","direction":"方向乙","credit":2}),
            ]),
            "before",
        );
        let candidate = snapshot(
            legacy_plan(vec![
                json!({"course_code":"A1","course_name":"课程甲","direction":"方向乙","credit":2}),
                json!({"course_code":"A1","course_name":"课程甲","direction":"方向甲","credit":3}),
            ]),
            "after",
        );
        let diff = diff_snapshots(current, candidate).unwrap();
        let changes = course_changes(&diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before.as_ref().unwrap()["direction"], "方向甲");
    }

    #[test]
    fn duplicate_and_course_reordering_is_semantically_empty() {
        let first =
            json!({"course_code":"A1","course_name":"课程甲","direction":"方向甲","credit":2});
        let second =
            json!({"course_code":"A1","course_name":"课程甲","direction":"方向乙","credit":2});
        let other = course(Some("B1"), "课程乙", 1, "秋");
        let current = snapshot(
            legacy_plan(vec![first.clone(), second.clone(), other.clone()]),
            "before",
        );
        let candidate = snapshot(legacy_plan(vec![other, second, first]), "after");
        assert!(diff_snapshots(current, candidate)
            .unwrap()
            .changes
            .is_empty());
    }

    #[test]
    fn uncoded_same_name_occurrences_remain_a_multiset() {
        let current = snapshot(
            legacy_plan(vec![
                course(None, "创新实践", 1, "秋"),
                course(None, "创新实践", 2, "春"),
            ]),
            "before",
        );
        let candidate = snapshot(
            legacy_plan(vec![
                course(None, "创新实践", 2, "春"),
                course(None, "创新实践", 3, "秋"),
            ]),
            "after",
        );
        let diff = diff_snapshots(current, candidate).unwrap();
        let changes = course_changes(&diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].course_code, None);
        assert_eq!(changes[0].course_name, "创新实践");
    }

    #[test]
    fn code_change_is_removal_and_addition_not_rename() {
        let current = snapshot(
            execution_plan(
                "2024",
                vec![course(Some("A1"), "同名课程", 2, "秋")],
                Some(true),
            ),
            "before",
        );
        let candidate = snapshot(
            execution_plan(
                "2024",
                vec![course(Some("B1"), "同名课程", 2, "秋")],
                Some(true),
            ),
            "after",
        );
        let diff = diff_snapshots(current, candidate).unwrap();
        let changes = course_changes(&diff);
        assert_eq!(changes.len(), 2);
        assert!(changes
            .iter()
            .any(|change| change.kind == ChangeKind::Removed));
        assert!(changes
            .iter()
            .any(|change| change.kind == ChangeKind::Added));
    }

    #[test]
    fn official_metadata_and_academic_structure_are_reviewable() {
        let current = snapshot(curriculum_plan(vec![]), "before");
        let mut plan = curriculum_plan(vec![]);
        plan.info["major_full_name"] = json!("计算机科学与技术（卓越班）");
        plan.info["program_type"] = json!("拔尖人才培养");
        plan.info["academic_structure"] = json!({"modules":[{"name":"专业核心","credits":24}],"graduation_requirement":"至少24学分"});
        let diff = diff_snapshots(current, snapshot(plan, "after")).unwrap();
        let metadata = diff
            .changes
            .iter()
            .find(|change| change.change_type == ChangeType::PlanMetadata)
            .unwrap();
        assert!(metadata.explanation.contains("官方专业全称"));
        assert!(metadata.explanation.contains("专业类型"));
        assert_eq!(
            metadata.after.as_ref().unwrap()["academic_structure"]["modules"][0]["credits"],
            24
        );
    }

    #[test]
    fn curriculum_execution_and_cohorts_never_cross_identity() {
        let curriculum = curriculum_plan(vec![]);
        let execution = execution_plan("2024", vec![], Some(true));
        let diff = diff_snapshots(
            CandidateSnapshot {
                generated_at: "before".into(),
                base_url: "test".into(),
                plans: vec![curriculum.clone(), execution.clone()],
            },
            CandidateSnapshot {
                generated_at: "after".into(),
                base_url: "test".into(),
                plans: vec![execution, curriculum],
            },
        )
        .unwrap();
        assert!(diff.changes.is_empty());
        let current = snapshot(execution_plan("2024", vec![], Some(true)), "before");
        let mut crossed = execution_plan("2024", vec![], Some(true));
        crossed.info["entry_cohort"] = json!("2025");
        assert!(diff_snapshots(current, snapshot(crossed, "after")).is_err());
    }

    #[test]
    fn title_distinguishes_source_cohort_school_full_name_and_code() {
        let plan = execution_plan(
            "2024",
            vec![course(Some("A1"), "课程甲", 2, "秋")],
            Some(true),
        );
        let title = plan_title(Some(&plan));
        assert!(title.contains("执行教学计划"));
        assert!(title.contains("2024级"));
        assert!(title.contains("计算学部"));
        assert!(title.contains("计算机科学与技术（卓越班）"));
        assert!(title.contains("0809"));
    }

    #[test]
    fn capture_provenance_does_not_create_semantic_diff_but_changes_source_hash() {
        let current = snapshot(
            execution_plan(
                "2024",
                vec![
                    json!({"course_code":"A1","course_name":"课程甲","source_path":"page-1","fetched_at":"old"}),
                ],
                Some(true),
            ),
            "before",
        );
        let mut after = execution_plan(
            "2024",
            vec![
                json!({"course_code":"A1","course_name":"课程甲","source_path":"page-9","fetched_at":"new"}),
            ],
            Some(true),
        );
        after.info["source_capture"]["fetched_at"] = json!("later");
        after.info["source_capture"]["pages"] = json!(9);
        let diff = diff_snapshots(current, snapshot(after, "after")).unwrap();
        assert!(diff.changes.is_empty());
        assert_ne!(
            diff.source_identity["current"],
            diff.source_identity["candidate"]
        );
    }

    #[test]
    fn incomplete_capture_cannot_turn_absence_into_removal_but_complete_empty_can() {
        let current = snapshot(
            execution_plan(
                "2024",
                vec![course(Some("A1"), "课程甲", 2, "秋")],
                Some(true),
            ),
            "before",
        );
        assert!(diff_snapshots(
            current.clone(),
            snapshot(execution_plan("2024", vec![], Some(false)), "after")
        )
        .is_err());
        assert!(diff_snapshots(
            current.clone(),
            snapshot(execution_plan("2024", vec![], None), "after")
        )
        .is_err());
        let diff = diff_snapshots(
            current,
            snapshot(execution_plan("2024", vec![], Some(true)), "after"),
        )
        .unwrap();
        let removed = course_changes(&diff)[0];
        assert!(removed.explanation.contains("本次查询未返回"));
        assert!(removed.explanation.contains("不等于学校取消课程"));
    }

    #[test]
    fn mixed_decisions_reject_all_and_validation_are_safe() {
        let current = snapshot(
            legacy_plan(vec![
                course(Some("A1"), "课程甲", 1, "秋"),
                course(Some("B1"), "课程乙", 1, "秋"),
            ]),
            "before",
        );
        let candidate = snapshot(
            legacy_plan(vec![
                course(Some("A1"), "课程甲", 2, "秋"),
                course(Some("B1"), "课程乙", 3, "秋"),
            ]),
            "after",
        );
        let diff = diff_snapshots(current.clone(), candidate).unwrap();
        let mut mixed = default_decisions(&diff, Decision::Reject);
        mixed
            .decisions
            .insert(course_changes(&diff)[0].change_id.clone(), Decision::Accept);
        let result = materialize(&diff, &mixed).unwrap();
        assert_eq!(result.plans[0].courses[0]["credit"], 2);
        assert_eq!(result.plans[0].courses[1]["credit"], 1);
        assert_eq!(
            serde_json::to_value(
                materialize(&diff, &default_decisions(&diff, Decision::Reject)).unwrap()
            )
            .unwrap(),
            serde_json::to_value(current).unwrap()
        );
    }

    #[test]
    fn serialized_tampering_stale_missing_and_injected_decisions_are_rejected() {
        let current = snapshot(
            legacy_plan(vec![course(Some("A1"), "课程甲", 1, "秋")]),
            "before",
        );
        let candidate = snapshot(
            legacy_plan(vec![course(Some("A1"), "课程甲", 2, "秋")]),
            "after",
        );
        let diff = diff_snapshots(current, candidate).unwrap();
        let mut restored: CurriculumDiff =
            serde_json::from_str(&serde_json::to_string(&diff).unwrap()).unwrap();
        validate_diff(&restored).unwrap();
        restored.changes[0].after.as_mut().unwrap()["credit"] = json!(99);
        assert!(validate_diff(&restored).is_err());
        let mut stale = default_decisions(&diff, Decision::Accept);
        stale.diff_identity_sha256 = "old".into();
        assert!(materialize(&diff, &stale).is_err());
        let mut injected = default_decisions(&diff, Decision::Accept);
        injected
            .decisions
            .insert("injected".into(), Decision::Accept);
        assert!(materialize(&diff, &injected).is_err());
        let mut missing = default_decisions(&diff, Decision::Accept);
        missing.decisions.clear();
        assert!(materialize(&diff, &missing).is_err());
    }

    #[test]
    fn baseline_canonicalization_is_idempotent_and_keeps_unknown_teaching_fields() {
        let manifest = json!({
            "curriculum_plans":[{"plan_id":"legacy-1","year":"2024","major_name":"专业甲","unknown_rule":{"minimum":10},"metadata_repo_id":"registry"}],
            "curriculum_records":[{"source_plan":"legacy-1","source_ordinal":1,"course_code":null,"course_name":"课程甲","unknown_field":"保留","repo_id":"course-a"}]
        });
        let baseline = baseline_snapshot(&manifest).unwrap();
        assert_eq!(baseline.plans[0].info["year"], "2024");
        assert_eq!(
            canonical_metadata(&baseline.plans[0].info)["plan_version"],
            "2024"
        );
        assert_eq!(baseline.plans[0].info["unknown_rule"]["minimum"], 10);
        assert_eq!(baseline.plans[0].courses[0]["unknown_field"], "保留");
        assert!(baseline.plans[0].courses[0].get("repo_id").is_none());
        assert_eq!(
            canonical_metadata(&baseline.plans[0].info),
            canonical_metadata(&canonical_metadata(&baseline.plans[0].info))
        );
    }

    #[test]
    fn uncoded_width_only_name_change_preserves_occurrence_identity() {
        let old = json!({"record_id":"REC-KEPT","course_code":"","course_name":"创新实践(训练)","credit":2,"semester":"春季"});
        let new = json!({"course_code":"","course_name":"创新实践（训练）","credit":3,"semester":"春季"});
        let mut candidate_plan = legacy_plan(vec![new.clone()]);
        candidate_plan.info["source_capture"] = json!({"complete":true});
        let diff = diff_snapshots(snapshot(legacy_plan(vec![old.clone()]), "old"), snapshot(candidate_plan, "new")).unwrap();
        let changes = course_changes(&diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Changed);
        assert_eq!(changes[0].before.as_ref().unwrap()["course_name"], "创新实践(训练)");
        assert_eq!(changes[0].after.as_ref().unwrap()["course_name"], "创新实践（训练）");
        assert_eq!(aligned_record_pairs(&[old], &[new]), vec![(Some(0), Some(0))]);
    }

    #[test]
    fn code_only_module_references_preserve_official_missing_names() {
        let reference = json!({"course_code":"AS33128","source_section":"execution-module","relation":{"module_id":"M1","direction_key":"0"}});
        let mut plan = execution_plan("2023", vec![reference.clone()], Some(true));
        plan.info["academic_structure"] = json!({"module_details":[{"module_id":"M1","direction_key":"0","courses":[reference]}]});
        let captured = snapshot(plan.clone(), "captured");
        assert!(validate_snapshot(&captured).is_ok());
        assert!(captured.plans[0].courses[0].get("course_name").is_none());
        plan.courses[0]["relation"]["module_id"] = json!("unknown");
        assert!(validate_snapshot(&snapshot(plan.clone(), "captured")).is_err());
        plan.courses[0]["relation"]["module_id"] = json!("M1");
        plan.courses[0]["source_section"] = json!("execution-main");
        assert!(validate_snapshot(&snapshot(plan.clone(), "captured")).is_err());
        plan.courses[0]["source_section"] = json!("execution-module");
        plan.courses[0]["course_code"] = json!("");
        assert!(validate_snapshot(&snapshot(plan, "captured")).is_err());
    }
}

use crate::jwts::{CandidatePlan, CandidateSnapshot};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
        let plan_id = string_field(record, "source_plan");
        records_by_plan
            .entry(plan_id.to_string())
            .or_default()
            .push(record);
    }
    let mut result = Vec::new();
    for plan in plans {
        let plan_id = string_field(plan, "plan_id").to_string();
        let mut courses = records_by_plan.remove(&plan_id).unwrap_or_default();
        courses.sort_by_key(|value| {
            value
                .get("source_ordinal")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX)
        });
        let course_values = courses
            .into_iter()
            .map(strip_repository_fields)
            .collect::<Vec<_>>();
        result.push(CandidatePlan {
            plan_id,
            info: plan.clone(),
            courses: course_values,
        });
    }
    result.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
    Ok(CandidateSnapshot {
        generated_at: "baseline".to_string(),
        base_url: "registry-manifest".to_string(),
        plans: result,
    })
}

pub fn diff_snapshots(
    current: CandidateSnapshot,
    candidate: CandidateSnapshot,
) -> Result<CurriculumDiff> {
    validate_snapshot(&current)?;
    validate_snapshot(&candidate)?;
    let current_index: BTreeMap<_, _> = current
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect();
    let candidate_index: BTreeMap<_, _> = candidate
        .plans
        .iter()
        .map(|plan| (plan.plan_id.as_str(), plan))
        .collect();
    let plan_ids = current_index
        .keys()
        .chain(candidate_index.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for plan_id in plan_ids {
        let before_plan = current_index.get(plan_id).copied();
        let after_plan = candidate_index.get(plan_id).copied();
        let before_info = before_plan.map(|plan| canonical_metadata(&plan.info));
        let after_info = after_plan.map(|plan| canonical_metadata(&plan.info));
        if before_info != after_info {
            let identity = json!({
                "plan_id":plan_id,
                "before":before_info,
                "after":after_info
            });
            let kind = change_kind(before_info.as_ref(), after_info.as_ref());
            changes.push(CurriculumChange {
                change_id: format!("plan-metadata-change-{}", &sha256(&identity)[..20]),
                change_type: ChangeType::PlanMetadata,
                kind: kind.clone(),
                plan_id: plan_id.to_string(),
                occurrence_key: None,
                occurrence_index: None,
                course_code: None,
                course_name: String::new(),
                before: before_info,
                after: after_info,
                title: format!("培养方案信息：{}", plan_title(after_plan.or(before_plan))),
                explanation: kind_explanation(&kind, "培养方案信息"),
            });
        }
        let before_courses = before_plan
            .map(|plan| plan.courses.as_slice())
            .unwrap_or(&[]);
        let after_courses = after_plan
            .map(|plan| plan.courses.as_slice())
            .unwrap_or(&[]);
        for slot in align_occurrences(plan_id, before_courses, after_courses) {
            let before = slot.before.as_ref().map(|value| value.course.clone());
            let after = slot.after.as_ref().map(|value| value.course.clone());
            if before == after {
                continue;
            }
            let representative = after.as_ref().or(before.as_ref()).context("课程差异为空")?;
            let kind = change_kind(before.as_ref(), after.as_ref());
            let code = normalized_string(representative.get("course_code"));
            let name = normalized_string(representative.get("course_name"));
            let identity = json!({
                "occurrence_key":slot.occurrence_key,
                "before":before,
                "after":after
            });
            let label = if code.is_empty() {
                name.clone()
            } else {
                format!("{name}（{code}）")
            };
            changes.push(CurriculumChange {
                change_id: format!("course-change-{}", &sha256(&identity)[..20]),
                change_type: ChangeType::CourseOccurrence,
                kind: kind.clone(),
                plan_id: plan_id.to_string(),
                occurrence_key: Some(slot.occurrence_key),
                occurrence_index: Some(slot.occurrence_index),
                course_code: (!code.is_empty()).then_some(code),
                course_name: name,
                before,
                after,
                title: format!("{}：{}", plan_title(after_plan.or(before_plan)), label),
                explanation: kind_explanation(&kind, &label),
            });
        }
    }
    let source_identity = json!({
        "current":sha256(&serde_json::to_value(&current)?),
        "candidate":sha256(&serde_json::to_value(&candidate)?)
    });
    let identity_payload = json!({
        "source_identity":source_identity,
        "changes":changes
    });
    let summary = DiffSummary {
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
    };
    Ok(CurriculumDiff {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        source_identity,
        diff_identity_sha256: sha256(&identity_payload),
        summary,
        changes,
        current,
        candidate,
    })
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
    if decisions.diff_identity_sha256 != diff.diff_identity_sha256 {
        bail!("这些选择属于另一批教务变化")
    }
    if decisions.decisions.len() != diff.changes.len() {
        bail!("还有变化没有选择接受或保留现状")
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
    let change_index: HashMap<_, _> = diff
        .changes
        .iter()
        .map(|change| (change.change_id.as_str(), change))
        .collect();
    for change_id in decisions.decisions.keys() {
        if !change_index.contains_key(change_id.as_str()) {
            bail!("选择中包含未知变化")
        }
    }
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
            let decision = decisions
                .decisions
                .get(&change.change_id)
                .context("方案信息变化尚未选择")?;
            match decision {
                Decision::Accept => change.after.clone(),
                Decision::Reject => change.before.clone(),
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
                courses.push(value.course);
            }
        }
        if let Some(info) = info {
            plans.push(CandidatePlan {
                plan_id: plan_id.to_string(),
                info,
                courses,
            });
        } else if !courses.is_empty() {
            bail!("方案已删除但仍保留课程")
        }
    }
    plans.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
    Ok(CandidateSnapshot {
        generated_at: Utc::now().to_rfc3339(),
        base_url: diff.candidate.base_url.clone(),
        plans,
    })
}

pub fn validate_snapshot(snapshot: &CandidateSnapshot) -> Result<()> {
    if snapshot.plans.is_empty() {
        bail!("候选数据没有培养方案")
    }
    let mut plan_ids = BTreeSet::new();
    for plan in &snapshot.plans {
        if plan.plan_id.trim().is_empty() || !plan_ids.insert(plan.plan_id.as_str()) {
            bail!("培养方案编号缺失或重复")
        }
        if plan.courses.is_empty() {
            bail!("培养方案没有课程：{}", plan.plan_id)
        }
        for course in &plan.courses {
            let name = normalized_string(course.get("course_name"));
            if name.is_empty() {
                bail!("课程缺少名称：{}", plan.plan_id)
            }
        }
    }
    Ok(())
}

fn align_occurrences(plan_id: &str, before: &[Value], after: &[Value]) -> Vec<Slot> {
    let before = occurrences(before);
    let after = occurrences(after);
    let mut used_after = vec![false; after.len()];
    let mut pairs = Vec::new();
    for before_item in before {
        let exact = after.iter().enumerate().find(|(index, item)| {
            !used_after[*index]
                && item.identity == before_item.identity
                && item.course == before_item.course
        });
        let matched = exact.or_else(|| {
            after
                .iter()
                .enumerate()
                .filter(|(index, item)| {
                    !used_after[*index] && item.identity == before_item.identity
                })
                .min_by_key(|(_, item)| item.sequence_index.abs_diff(before_item.sequence_index))
        });
        if let Some((index, item)) = matched {
            used_after[index] = true;
            pairs.push((Some(before_item), Some(item.clone())));
        } else {
            pairs.push((Some(before_item), None));
        }
    }
    for (index, item) in after.into_iter().enumerate() {
        if !used_after[index] {
            pairs.push((None, Some(item)));
        }
    }
    pairs.sort_by_key(|(before, after)| {
        after
            .as_ref()
            .map(|item| item.sequence_index)
            .or_else(|| before.as_ref().map(|item| item.sequence_index))
            .unwrap_or(usize::MAX)
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

fn occurrences(courses: &[Value]) -> Vec<Occurrence> {
    courses
        .iter()
        .enumerate()
        .map(|(sequence_index, course)| Occurrence {
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
            normalized_string(course.get("course_name")).to_lowercase()
        )
    }
}

fn canonical_course(course: &Value) -> Value {
    let mut value = course.clone();
    if let Some(object) = value.as_object_mut() {
        for key in [
            "record_id",
            "source_plan",
            "source_plan_file",
            "source_ordinal",
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
        ] {
            object.remove(key);
        }
    }
    value
}

fn strip_repository_fields(record: &Value) -> Value {
    canonical_course(record)
}

fn canonical_metadata(info: &Value) -> Value {
    let mut value = info.clone();
    if let Some(object) = value.as_object_mut() {
        for key in ["metadata_repo_id", "metadata_path", "source_plan_file"] {
            object.remove(key);
        }
    }
    value
}

fn plan_title(plan: Option<&CandidatePlan>) -> String {
    plan.and_then(|plan| {
        let major = normalized_string(plan.info.get("major_name"));
        (!major.is_empty()).then_some(major)
    })
    .unwrap_or_else(|| "未知专业".to_string())
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

fn kind_explanation(kind: &ChangeKind, label: &str) -> String {
    match kind {
        ChangeKind::Added => format!("教务系统新增了{label}"),
        ChangeKind::Removed => format!("教务系统不再返回{label}"),
        ChangeKind::Changed => format!("教务系统修改了{label}"),
    }
}

fn normalized_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn sha256(value: &Value) -> String {
    let canonical = canonical_json(value);
    let encoded = serde_json::to_vec(&canonical).expect("canonical json");
    format!("{:x}", Sha256::digest(encoded))
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        _ => value.clone(),
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(courses: Vec<Value>) -> CandidatePlan {
        CandidatePlan {
            plan_id: "plan-a".to_string(),
            info: json!({"plan_ID":"plan-a","major_name":"计算机科学与技术"}),
            courses,
        }
    }

    #[test]
    fn duplicate_courses_are_aligned_without_collapsing() {
        let current = CandidateSnapshot {
            generated_at: "before".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![
                json!({"course_code":"A1","course_name":"课程甲","credit":1}),
                json!({"course_code":"A1","course_name":"课程甲","credit":2}),
            ])],
        };
        let candidate = CandidateSnapshot {
            generated_at: "after".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![
                json!({"course_code":"A1","course_name":"课程甲","credit":1}),
                json!({"course_code":"A1","course_name":"课程甲","credit":3}),
            ])],
        };
        let diff = diff_snapshots(current, candidate).unwrap();
        let course_changes = diff
            .changes
            .iter()
            .filter(|change| change.change_type == ChangeType::CourseOccurrence)
            .collect::<Vec<_>>();
        assert_eq!(course_changes.len(), 1);
        assert_eq!(course_changes[0].occurrence_index, Some(1));
    }

    #[test]
    fn uncoded_courses_keep_separate_occurrences() {
        let current = CandidateSnapshot {
            generated_at: "before".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![
                json!({"course_code":null,"course_name":"创新实践","credit":1}),
                json!({"course_code":null,"course_name":"创新实践","credit":2}),
            ])],
        };
        let candidate = CandidateSnapshot {
            generated_at: "after".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![
                json!({"course_code":null,"course_name":"创新实践","credit":1}),
                json!({"course_code":null,"course_name":"创新实践","credit":3}),
            ])],
        };
        let diff = diff_snapshots(current, candidate).unwrap();
        assert_eq!(
            diff.changes
                .iter()
                .filter(|change| change.change_type == ChangeType::CourseOccurrence)
                .count(),
            1
        );
    }

    #[test]
    fn accept_and_reject_materialize_expected_records() {
        let current = CandidateSnapshot {
            generated_at: "before".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![json!({
                "course_code":"A1","course_name":"课程甲","credit":1
            })])],
        };
        let candidate = CandidateSnapshot {
            generated_at: "after".into(),
            base_url: "test".into(),
            plans: vec![plan(vec![json!({
                "course_code":"A1","course_name":"课程甲","credit":2
            })])],
        };
        let diff = diff_snapshots(current, candidate).unwrap();
        let accepted = materialize(&diff, &default_decisions(&diff, Decision::Accept)).unwrap();
        let rejected = materialize(&diff, &default_decisions(&diff, Decision::Reject)).unwrap();
        assert_eq!(accepted.plans[0].courses[0]["credit"], json!(2));
        assert_eq!(rejected.plans[0].courses[0]["credit"], json!(1));
    }
}

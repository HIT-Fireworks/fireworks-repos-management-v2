use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde::ser::{SerializeMap, SerializeSeq};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;
use unicode_normalization::UnicodeNormalization;
use base64::Engine;

const DEFAULT_MANIFEST: &str = "data/repository-manifest.no-collection.v4.json";
const DEFAULT_TOPOLOGY: &str = "config/repository-topology.v4.json";
const DEFAULT_ROUTES: &str = "config/repository-file-routes.v4.json";
const DEFAULT_OPERATIONS: &str = "data/repository-management-operations";
const DEFAULT_REMOTE_TEMPLATE: &str = "https://github.com/{organization}/{repo_id}.git";
const CURRICULUM_REVIEW_DIRECTORY: &str = "curriculum-reviews";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CourseAssignment {
    Existing { repo_id: String },
    New { title: String },
    NewGroup { repo_id: String, title: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CourseRepositoryChoice {
    pub course_code: String,
    pub course_name: String,
    pub offering_colleges: Vec<String>,
    pub suggested_repo_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurriculumReviewSummary {
    pub path: String,
    pub title: String,
    pub updated_at: String,
    pub pending_changes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Health {
    pub organization: String,
    pub health: String,
    pub health_message: Option<String>,
    pub repository_count: usize,
    pub course_route_count: usize,
    pub file_route_count: usize,
    pub inventory_complete_repository_count: usize,
    pub virtual_collection_count: usize,
    pub special_topic_route_count: usize,
    pub identity: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepositorySummary {
    pub repo_id: String,
    pub repo_type: String,
    pub display_name: String,
    pub description: String,
    pub course_codes: Vec<String>,
    pub course_names: Vec<String>,
    #[serde(default)]
    pub unowned_paths: Vec<String>,
    pub file_count: usize,
    pub bytes: u64,
    pub head: Option<String>,
    pub inventory_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepositoryDetail {
    #[serde(flatten)]
    pub summary: RepositorySummary,
    pub physical_repository_id: Option<String>,
    pub file_routes: Vec<Value>,
    pub course_routes: Vec<Value>,
    pub topology: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutesSnapshot {
    pub file_total: usize,
    pub course_code_total: usize,
    pub file_routes: Vec<Value>,
    pub course_code_routes: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanSummary {
    pub path: String,
    pub operation_id: Option<String>,
    pub kind: Option<String>,
    pub created_at: Option<String>,
    pub plan_identity_sha256: Option<String>,
    pub confirmation_phrase: Option<String>,
    pub state: Option<String>,
    pub valid: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JournalSummary {
    pub path: String,
    pub operation_id: Option<String>,
    pub kind: Option<String>,
    pub status: String,
    pub recovery_state: String,
    pub plan_identity_sha256: Option<String>,
    pub confirmation_phrase: Option<String>,
    pub error: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlannedOperation {
    pub path: String,
    pub plan: Value,
    pub risk: Value,
}

impl PlannedOperation {
    pub fn operation_id(&self) -> &str {
        self.plan
            .get("operation_id")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    pub fn identity(&self) -> &str {
        self.plan
            .pointer("/core/plan_identity_sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    pub fn confirmation_phrase(&self) -> &str {
        self.plan
            .pointer("/core/confirmation_phrase")
            .and_then(Value::as_str)
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitTarget {
    pub repo_id: String,
    pub display_name: String,
    pub course_codes: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SplitCourse {
    pub course_code: String,
    pub title: String,
    pub shared_course_codes: Vec<String>,
    pub file_count: usize,
    pub bytes: u64,
    pub sample_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LooseFile {
    pub internal_path: String,
    pub title: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SplitOptions {
    pub source_repo_id: String,
    pub source_title: String,
    pub courses: Vec<SplitCourse>,
    pub loose_files: Vec<LooseFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemStatus {
    pub offline_ready: bool,
    pub git_available: bool,
    pub github_logged_in: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CurriculumUpdateStatus {
    pub stage: String,
    pub message: String,
    pub base_url: String,
    pub candidate_plan_count: usize,
    pub candidate_record_count: usize,
    pub change_count: usize,
    pub pending_decision_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepositorySyncPreview {
    pub manifest: Value,
    pub topology: Value,
    pub routes: Value,
    #[serde(default)]
    pub baseline: Value,
    #[serde(default)]
    pub identity_sha256: String,
    pub create_repositories: Vec<String>,
    pub archive_repositories: Vec<String>,
    pub metadata_repositories: Vec<String>,
    pub plan_count: usize,
    pub record_count: usize,
    pub descriptor_count: usize,
    pub new_course_code_count: usize,
    pub removed_course_code_count: usize,
    pub summary_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistrySyncPlan {
    pub remote_url: String,
    pub baseline: Value,
    pub files: BTreeMap<String, Value>,
    pub identity_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryLifecycleKind {
    Create,
    Update,
    Archive,
    Unarchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryLifecycleAction {
    pub repo_id: String,
    pub title: String,
    pub kind: RepositoryLifecycleKind,
    pub description: String,
    pub private: bool,
    pub archived: bool,
    pub template: bool,
    pub default_branch: String,
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub template_repository: Option<String>,
    pub baseline: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepositoryLifecyclePreview {
    pub organization: String,
    pub registry: RegistrySyncPlan,
    pub actions: Vec<RepositoryLifecycleAction>,
    pub summary_lines: Vec<String>,
    pub identity_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateExecutionJournal {
    pub schema_version: u32,
    pub operation_id: String,
    pub preview_identity_sha256: String,
    pub preview_path: String,
    pub status: String,
    pub stage: String,
    pub registry_commit: Option<String>,
    pub repository_results: BTreeMap<String, String>,
    pub completed_stages: Vec<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct UpdateSession {
    pub client: Option<crate::jwts::JwtsClient>,
    pub kind: crate::jwts::PlanKind,
    pub catalog: crate::jwts::CurriculumCatalog,
    pub selections: Vec<crate::jwts::CrawlSelection>,
    pub candidate: Option<crate::jwts::CandidateSnapshot>,
    pub diff: Option<crate::curriculum::CurriculumDiff>,
    pub decisions: crate::curriculum::DecisionSet,
    pub assignments: BTreeMap<String, CourseAssignment>,
    pub status: CurriculumUpdateStatus,
}

impl UpdateSession {
    pub fn connect(base_url: &str, cookie: &str, kind: crate::jwts::PlanKind) -> Result<Self> {
        Self::from_client(crate::jwts::JwtsClient::new(base_url, cookie)?, kind)
    }

    pub fn from_client(
        client: crate::jwts::JwtsClient,
        kind: crate::jwts::PlanKind,
    ) -> Result<Self> {
        let catalog = client.catalog(kind)?;
        let base_url = client.base_url().to_string();
        Ok(Self {
            client: Some(client),
            kind,
            catalog,
            selections: Vec::new(),
            candidate: None,
            diff: None,
            decisions: crate::curriculum::DecisionSet::default(),
            assignments: BTreeMap::new(),
            status: CurriculumUpdateStatus {
                stage: "connected".to_string(),
                message: format!("已连接教务系统，来源：{}", kind.label()),
                base_url,
                updated_at: now(),
                ..CurriculumUpdateStatus::default()
            },
        })
    }

    pub fn change(&self, index: usize) -> Option<&crate::curriculum::CurriculumChange> {
        self.diff.as_ref()?.changes.get(index)
    }

    pub fn set_decision(
        &mut self,
        index: usize,
        decision: crate::curriculum::Decision,
    ) -> Result<()> {
        let diff = self.diff.as_ref().context("尚未生成教务差异")?;
        let change = diff.changes.get(index).context("变化序号无效")?;
        self.decisions
            .decisions
            .insert(change.change_id.clone(), decision);
        self.assignments.clear();
        self.status.pending_decision_count = diff
            .changes
            .len()
            .saturating_sub(self.decisions.decisions.len());
        self.status.updated_at = now();
        Ok(())
    }

    pub fn accept_all(&mut self) -> Result<()> {
        let diff = self.diff.as_ref().context("尚未生成教务差异")?;
        self.decisions =
            crate::curriculum::default_decisions(diff, crate::curriculum::Decision::Accept);
        self.assignments.clear();
        self.status.pending_decision_count = 0;
        self.status.updated_at = now();
        Ok(())
    }

    pub fn reject_all(&mut self) -> Result<()> {
        let diff = self.diff.as_ref().context("尚未生成教务差异")?;
        self.decisions =
            crate::curriculum::default_decisions(diff, crate::curriculum::Decision::Reject);
        self.assignments.clear();
        self.status.pending_decision_count = 0;
        self.status.updated_at = now();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Manager {
    workspace: PathBuf,
    manifest_path: PathBuf,
    topology_path: PathBuf,
    routes_path: PathBuf,
    operations_path: PathBuf,
    remote_template: String,
    registry_remote: Option<String>,
    manifest: Value,
    topology: Value,
    routes: Value,
}

impl Manager {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        let manifest_path = workspace.join(DEFAULT_MANIFEST);
        let topology_path = workspace.join(DEFAULT_TOPOLOGY);
        let routes_path = workspace.join(DEFAULT_ROUTES);
        let operations_path = workspace.join(DEFAULT_OPERATIONS);
        let remote_template = std::env::var("FIREWORKS_REMOTE_URL_TEMPLATE")
            .unwrap_or_else(|_| DEFAULT_REMOTE_TEMPLATE.to_string());
        Self {
            workspace,
            manifest_path,
            topology_path,
            routes_path,
            operations_path,
            remote_template,
            registry_remote: std::env::var("FIREWORKS_REGISTRY_REMOTE").ok(),
            manifest: Value::Null,
            topology: Value::Null,
            routes: Value::Null,
        }
    }

    pub fn with_remote_template(mut self, template: impl Into<String>) -> Self {
        self.remote_template = template.into();
        self
    }

    pub fn with_registry_remote(mut self, remote: impl Into<String>) -> Self {
        self.registry_remote = Some(remote.into());
        self
    }

    pub fn discover() -> Result<Self> {
        if let Some(path) = std::env::var_os("FIREWORKS_WORKSPACE") {
            let mut manager = Self::new(PathBuf::from(path));
            manager.reload()?;
            return Ok(manager);
        }
        let mut candidates = vec![std::env::current_dir().unwrap_or_default()];
        if let Ok(executable) = std::env::current_exe() {
            candidates.extend(executable.ancestors().map(Path::to_path_buf));
        }
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        let mut seen = HashSet::new();
        for candidate in candidates {
            let candidate = candidate.canonicalize().unwrap_or(candidate);
            if !seen.insert(candidate.clone()) {
                continue;
            }
            if candidate.join(DEFAULT_MANIFEST).is_file()
                && candidate.join(DEFAULT_TOPOLOGY).is_file()
                && candidate.join(DEFAULT_ROUTES).is_file()
            {
                let mut manager = Self::new(candidate);
                manager.reload()?;
                return Ok(manager);
            }
        }
        bail!("没有找到管理数据。请重新解压完整安装包后双击启动。")
    }

    pub fn reload(&mut self) -> Result<()> {
        self.manifest = read_json(&self.manifest_path)?;
        self.topology = read_json(&self.topology_path)?;
        self.routes = read_json(&self.routes_path)?;
        validate_state(&self.topology, &self.routes, false)?;
        validate_direct_manifest(&self.manifest)?;
        validate_resource_layout(&self.manifest, &self.routes)?;
        Ok(())
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn begin_curriculum_update(
        &self,
        cookie: &str,
        kind: crate::jwts::PlanKind,
    ) -> Result<UpdateSession> {
        UpdateSession::connect(crate::jwts::DEFAULT_HIT_BASE_URL, cookie, kind)
    }

    pub fn begin_curriculum_update_at(
        &self,
        base_url: &str,
        cookie: &str,
        kind: crate::jwts::PlanKind,
    ) -> Result<UpdateSession> {
        let mut session = UpdateSession::connect(base_url, cookie, kind)?;
        session.status.base_url = base_url.to_string();
        Ok(session)
    }

    pub fn select_source_kind(
        &self,
        session: &mut UpdateSession,
        kind: crate::jwts::PlanKind,
    ) -> Result<()> {
        let client = session
            .client
            .as_ref()
            .context("恢复的审阅任务不能重新抓取，请新建更新任务")?;
        session.catalog = client.catalog(kind)?;
        session.kind = kind;
        session.selections.clear();
        session.candidate = None;
        session.diff = None;
        session.decisions = crate::curriculum::DecisionSet::default();
        session.assignments.clear();
        session.status.stage = "connected".to_string();
        session.status.message = format!("已切换来源：{}", kind.label());
        session.status.updated_at = now();
        Ok(())
    }

    pub fn update_majors(
        &self,
        session: &UpdateSession,
        grade: &str,
        college_code: &str,
    ) -> Result<Vec<crate::jwts::CatalogOption>> {
        session
            .client
            .as_ref()
            .context("恢复的审阅任务不能重新抓取，请新建更新任务")?
            .majors(session.kind, college_code, grade)
    }

    pub fn stage_curriculum_update(
        &self,
        session: &mut UpdateSession,
        selections: Vec<crate::jwts::CrawlSelection>,
    ) -> Result<()> {
        self.require_current_workspace()?;
        validate_crawl_selections(session, &selections)?;
        let client = session
            .client
            .as_ref()
            .context("恢复的审阅任务不能重新抓取，请新建更新任务")?;
        let mut plans = Vec::with_capacity(selections.len());
        for selection in &selections {
            let plan = client.fetch_plan(selection)?;
            validate_captured_plan(&plan, selection)?;
            plans.push(plan);
        }
        let captured = crate::jwts::CandidateSnapshot {
            generated_at: now(),
            base_url: client.base_url().to_string(),
            plans,
        };
        self.stage_curriculum_snapshot(session, selections, captured)
    }

    pub fn stage_curriculum_snapshot(
        &self,
        session: &mut UpdateSession,
        selections: Vec<crate::jwts::CrawlSelection>,
        captured: crate::jwts::CandidateSnapshot,
    ) -> Result<()> {
        self.require_current_workspace()?;
        validate_crawl_selections(session, &selections)?;
        validate_snapshot_source(&captured.base_url, &session.status.base_url)?;
        if captured.generated_at.trim().is_empty() {
            bail!("候选快照缺少真实采集时间")
        }

        let mut selected = selections
            .iter()
            .map(|selection| (selection.plan_id(), selection))
            .collect::<BTreeMap<_, _>>();
        let mut captured_ids = BTreeSet::new();
        for plan in &captured.plans {
            if !captured_ids.insert(plan.plan_id.clone()) {
                bail!("候选快照包含重复计划：{}", plan.plan_id)
            }
            let Some(selection) = selected.remove(&plan.plan_id) else {
                bail!("候选快照包含未选择的额外范围：{}", plan.plan_id)
            };
            validate_captured_plan(plan, selection)?;
        }
        if !selected.is_empty() {
            bail!(
                "候选快照缺少所选计划：{}",
                selected.keys().cloned().collect::<Vec<_>>().join("、")
            )
        }

        let baseline = crate::curriculum::baseline_snapshot(&self.manifest)?;
        let selection_by_id = selections
            .iter()
            .map(|selection| (selection.plan_id(), selection))
            .collect::<BTreeMap<_, _>>();
        let crate::jwts::CandidateSnapshot {
            generated_at,
            base_url,
            plans: captured_plans,
        } = captured;
        let mut fetched = captured_plans
            .into_iter()
            .map(|plan| {
                let selection = selection_by_id
                    .get(&plan.plan_id)
                    .copied()
                    .expect("候选计划集合已完成一一对应校验");
                (selection, plan)
            })
            .collect::<Vec<_>>();
        let mut replaced = BTreeSet::new();
        for (selection, plan) in &mut fetched {
            let mut matches = baseline
                .plans
                .iter()
                .filter(|existing| {
                    existing.plan_id == plan.plan_id
                        || plan_matches_selection(&existing.info, selection)
                })
                .map(|existing| existing.plan_id.clone())
                .collect::<Vec<_>>();
            matches.sort();
            matches.dedup();
            if matches.len() > 1 {
                bail!(
                    "当前数据中有多个同范围旧方案，无法安全替换：{}",
                    plan.plan_id
                )
            }
            if let Some(existing_id) = matches.first() {
                plan.plan_id = existing_id.clone();
                plan.info["plan_id"] = json!(existing_id);
                if let Some(info) = plan.info.as_object_mut() {
                    info.remove("plan_ID");
                }
            }
            replaced.extend(matches);
        }
        let mut plans = baseline
            .plans
            .iter()
            .filter(|plan| !replaced.contains(&plan.plan_id))
            .cloned()
            .collect::<Vec<_>>();
        plans.extend(fetched.into_iter().map(|(_, plan)| plan));
        plans.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
        let candidate = crate::jwts::CandidateSnapshot {
            generated_at,
            base_url,
            plans,
        };
        self.require_current_workspace()?;
        crate::curriculum::validate_snapshot(&candidate)?;
        let diff = crate::curriculum::diff_snapshots(baseline, candidate.clone())?;
        session.selections = selections;
        session.candidate = Some(candidate);
        session.decisions = crate::curriculum::DecisionSet {
            diff_identity_sha256: diff.diff_identity_sha256.clone(),
            decisions: BTreeMap::new(),
        };
        session.assignments.clear();
        session.status = CurriculumUpdateStatus {
            stage: "review".to_string(),
            message: "已抓取完整候选数据，请逐条审阅教学差异".to_string(),
            base_url: session.status.base_url.clone(),
            candidate_plan_count: diff.candidate.plans.len(),
            candidate_record_count: diff
                .candidate
                .plans
                .iter()
                .map(|plan| plan.courses.len())
                .sum(),
            change_count: diff.changes.len(),
            pending_decision_count: diff.changes.len(),
            updated_at: now(),
        };
        session.diff = Some(diff);
        self.save_curriculum_review(session)?;
        Ok(())
    }

    pub fn course_assignment_targets(&self) -> Result<Vec<RepositorySummary>> {
        let mut rows = self
            .repository_rows()?
            .into_iter()
            .filter(|row| self.valid_assignment_repository(&row.repo_id))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            normalize(&left.display_name)
                .cmp(&normalize(&right.display_name))
                .then(left.repo_id.cmp(&right.repo_id))
        });
        Ok(rows)
    }

    pub fn pending_course_assignments(
        &self,
        session: &UpdateSession,
    ) -> Result<Vec<CourseRepositoryChoice>> {
        let snapshot = reviewed_snapshot(session)?;
        let known = array_at(&self.manifest, "course_descriptors")?
            .iter()
            .map(|value| normalize(string_field(value, "course_code")))
            .collect::<BTreeSet<_>>();
        let targets = self.course_assignment_targets()?;
        let mut choices = BTreeMap::<String, (String, BTreeSet<String>)>::new();
        for plan in &snapshot.plans {
            for course in &plan.courses {
                let code = normalized_value(course.get("course_code"));
                if code.is_empty()
                    || known.contains(&code)
                    || session.assignments.contains_key(&code)
                {
                    continue;
                }
                let name = normalized_value(course.get("course_name"));
                let college = [
                    course.get("offering_college"),
                    course.get("school_name"),
                    plan.info.get("school_name"),
                ]
                .into_iter()
                .flatten()
                .map(|value| normalized_value(Some(value)))
                .find(|value| !value.is_empty())
                .unwrap_or_default();
                let entry = choices.entry(code).or_default();
                if entry.0.is_empty() && !name.is_empty() {
                    entry.0 = name;
                }
                if !college.is_empty() {
                    entry.1.insert(college);
                }
            }
        }
        Ok(choices
            .into_iter()
            .map(|(course_code, (course_name, offering_colleges))| {
                let mut suggested_repo_ids = targets
                    .iter()
                    .filter(|target| {
                        (!course_name.is_empty()
                            && normalize(&target.display_name)
                                .to_lowercase()
                                .contains(&normalize(&course_name).to_lowercase()))
                            || offering_colleges
                                .iter()
                                .any(|college| target.display_name.contains(college))
                    })
                    .map(|target| target.repo_id.clone())
                    .collect::<Vec<_>>();
                suggested_repo_ids.sort();
                suggested_repo_ids.dedup();
                CourseRepositoryChoice {
                    course_code,
                    course_name,
                    offering_colleges: offering_colleges.into_iter().collect(),
                    suggested_repo_ids,
                }
            })
            .collect())
    }

    pub fn assign_course(
        &self,
        session: &mut UpdateSession,
        course_code: &str,
        assignment: CourseAssignment,
    ) -> Result<()> {
        self.assign_courses(session, BTreeMap::from([(course_code.to_string(), assignment)]))
    }

    pub fn assign_courses(
        &self,
        session: &mut UpdateSession,
        assignments: BTreeMap<String, CourseAssignment>,
    ) -> Result<()> {
        self.require_current_workspace()?;
        if assignments.is_empty() {
            bail!("请至少选择一个新增课程的资料归属")
        }
        let pending = self.pending_course_assignments(session)?
            .into_iter()
            .map(|choice| choice.course_code)
            .collect::<BTreeSet<_>>();
        let mut normalized = BTreeMap::new();
        for (input, assignment) in assignments {
            let code = normalize(&input);
            if code.is_empty() || session.assignments.contains_key(&code) || normalized.contains_key(&code) {
                bail!("课程代码无效、重复或已经完成归属")
            }
            if !pending.contains(&code) {
                bail!("该课程代码不是当前已接受变化中的新增代码：{code}")
            }
            match &assignment {
                CourseAssignment::Existing { repo_id } => {
                    safe_repo_id(repo_id)?;
                    if !self.valid_assignment_repository(repo_id) {
                        bail!("所选仓库不是可用的现有课程资料仓")
                    }
                }
                CourseAssignment::New { title } => {
                    if normalize(title).is_empty() {
                        bail!("新课程资料仓标题不能为空")
                    }
                }
                CourseAssignment::NewGroup { repo_id, title } => {
                    safe_repo_id(repo_id)?;
                    if normalize(title).is_empty()
                        || repositories(&self.topology)?.contains_key(repo_id)
                        || self.manifest_repository(repo_id).is_some()
                    {
                        bail!("新资料库标题为空或身份已存在")
                    }
                    if normalized.values().chain(session.assignments.values()).any(|existing| {
                        matches!(existing, CourseAssignment::NewGroup { repo_id: other_id, title: other_title }
                            if other_id == repo_id && normalize(other_title) != normalize(title))
                    }) {
                        bail!("同一新资料库不能指定不同标题")
                    }
                }
            }
            normalized.insert(code, assignment);
        }
        let added = normalized.keys().cloned().collect::<Vec<_>>();
        session.assignments.extend(normalized);
        let previous_updated_at = std::mem::replace(&mut session.status.updated_at, now());
        if let Err(error) = self.save_curriculum_review(session) {
            for code in added {
                session.assignments.remove(&code);
            }
            session.status.updated_at = previous_updated_at;
            return Err(error);
        }
        Ok(())
    }

    pub fn materialize_curriculum_update(
        &mut self,
        session: &mut UpdateSession,
    ) -> Result<RepositorySyncPreview> {
        self.materialize_curriculum_updates(std::slice::from_mut(session))
    }

    pub fn materialize_curriculum_updates(
        &mut self,
        sessions: &mut [UpdateSession],
    ) -> Result<RepositorySyncPreview> {
        self.require_current_workspace()?;
        if sessions.is_empty() {
            bail!("联合更新至少需要一项审阅")
        }
        let baseline = crate::curriculum::baseline_snapshot(&self.manifest)?;
        let baseline_identity = canonical_sha256(&serde_json::to_value(&baseline)?);
        let mut plans = baseline.plans.into_iter()
            .map(|plan| (plan.plan_id.clone(), plan)).collect::<BTreeMap<_, _>>();
        let mut touched = BTreeSet::new();
        let mut assignments = BTreeMap::new();
        let mut reviewed = Vec::with_capacity(sessions.len());
        for session in sessions.iter() {
            let diff = session.diff.as_ref().context("审阅缺少差异")?;
            if canonical_sha256(&serde_json::to_value(&diff.current)?) != baseline_identity {
                bail!("联合审阅不属于同一正式数据基线")
            }
            let snapshot = reviewed_snapshot(session)?;
            let changed = diff.changes.iter().map(|change| change.plan_id.clone()).collect::<BTreeSet<_>>();
            for plan_id in &changed {
                if !touched.insert(plan_id.clone()) {
                    bail!("联合审阅重复修改同一计划：{plan_id}")
                }
            }
            for (code, assignment) in &session.assignments {
                if assignments.get(code).is_some_and(|previous| previous != assignment) {
                    bail!("联合审阅的新课程归属冲突：{code}")
                }
                assignments.insert(code.clone(), assignment.clone());
            }
            for plan in &snapshot.plans {
                if changed.contains(&plan.plan_id) {
                    plans.insert(plan.plan_id.clone(), plan.clone());
                }
            }
            reviewed.push(snapshot);
        }
        let snapshot = crate::jwts::CandidateSnapshot {
            generated_at: now(),
            base_url: reviewed[0].base_url.clone(),
            plans: plans.into_values().collect(),
        };
        let mut preview = self.rebuild_from_snapshot(&snapshot, &assignments)?;
        for (session, result) in sessions.iter().zip(&reviewed) {
            append_curriculum_history(&mut preview.manifest, session.diff.as_ref().unwrap(),
                &session.decisions, &session.assignments, result)?;
        }
        finalize_repository_preview(&mut preview, &self.workspace_identity())?;
        for session in sessions {
            session.status.stage = "preview".to_string();
            session.status.message = "已生成完整联合预览，确认后才会连接远端".to_string();
            session.status.pending_decision_count = 0;
            session.status.updated_at = now();
            self.save_curriculum_review(session)?;
        }
        Ok(preview)
    }
    pub fn save_curriculum_review(&self, session: &UpdateSession) -> Result<PathBuf> {
        self.require_current_workspace()?;
        let diff = session.diff.as_ref().context("尚未生成教务差异")?;
        validate_review_session(diff, &session.decisions, &session.assignments)?;
        let baseline = crate::curriculum::baseline_snapshot(&self.manifest)?;
        if canonical_sha256(&serde_json::to_value(&baseline)?)
            != canonical_sha256(&serde_json::to_value(&diff.current)?)
        {
            bail!("审阅的课程基线已变化，不能把旧裁决保存到新基线")
        }
        let review_dir = self.operations_path.join(CURRICULUM_REVIEW_DIRECTORY);
        fs::create_dir_all(&review_dir)?;
        let review_id = &diff.diff_identity_sha256[..20];
        let path = review_dir.join(format!("curriculum-review-{review_id}.json"));
        if path.exists() {
            let existing = read_json(&path)?;
            validate_review_payload(&existing)?;
            if existing.get("completed").and_then(Value::as_bool) == Some(true) {
                bail!("这项审阅已完成，请重新抓取建立新任务")
            }
            if existing.get("workspace_baseline") != Some(&self.workspace_identity()) {
                bail!("已有审阅基线已变化，不能覆盖")
            }
        }
        let completed = session.status.stage == "completed";
        let mut payload = json!({
            "schema_version":1,
            "review_id":review_id,
            "kind":session.kind,
            "title":review_title(diff, session.kind),
            "updated_at":session.status.updated_at,
            "completed":completed,
            "workspace_baseline":self.workspace_identity(),
            "diff":diff,
            "decisions":session.decisions,
            "assignments":session.assignments,
            "status":session.status,
            "selections":session.selections,
        });
        let content_sha256 = canonical_sha256(&payload);
        payload["content_sha256"] = json!(content_sha256);
        atomic_json(&path, &payload)?;
        Ok(path)
    }

    pub fn curriculum_reviews(&self) -> Result<Vec<CurriculumReviewSummary>> {
        let directory = self.operations_path.join(CURRICULUM_REVIEW_DIRECTORY);
        if !directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for path in sorted_json_files(&directory)? {
            let payload = read_json(&path)?;
            validate_review_payload(&payload)?;
            if payload.get("completed").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let diff: crate::curriculum::CurriculumDiff =
                serde_json::from_value(payload.get("diff").cloned().context("审阅文件缺少差异")?)?;
            crate::curriculum::validate_diff(&diff)?;
            let decisions: crate::curriculum::DecisionSet = serde_json::from_value(
                payload
                    .get("decisions")
                    .cloned()
                    .context("审阅文件缺少裁决")?,
            )?;
            result.push(CurriculumReviewSummary {
                path: path.to_string_lossy().to_string(),
                title: string_field(&payload, "title").to_string(),
                updated_at: string_field(&payload, "updated_at").to_string(),
                pending_changes: diff.changes.len().saturating_sub(decisions.decisions.len()),
            });
        }
        result.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(result)
    }

    pub fn resume_curriculum_review(&self, path: &Path) -> Result<UpdateSession> {
        self.require_current_workspace()?;
        let controlled = self.controlled_review_path(path)?;
        let payload = read_json(&controlled)?;
        validate_review_payload(&payload)?;
        if payload.get("completed").and_then(Value::as_bool) == Some(true) {
            bail!("这项教学计划审阅已经完成")
        }
        if payload.get("workspace_baseline") != Some(&self.workspace_identity()) {
            bail!("当前生产数据已变化，旧审阅不能继续")
        }
        let diff: crate::curriculum::CurriculumDiff =
            serde_json::from_value(payload.get("diff").cloned().context("审阅文件缺少差异")?)?;
        crate::curriculum::validate_diff(&diff)?;
        let decisions: crate::curriculum::DecisionSet = serde_json::from_value(
            payload
                .get("decisions")
                .cloned()
                .context("审阅文件缺少裁决")?,
        )?;
        let assignments: BTreeMap<String, CourseAssignment> = serde_json::from_value(
            payload
                .get("assignments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?;
        validate_review_session(&diff, &decisions, &assignments)?;
        let baseline = crate::curriculum::baseline_snapshot(&self.manifest)?;
        if canonical_sha256(&serde_json::to_value(&baseline)?)
            != canonical_sha256(&serde_json::to_value(&diff.current)?)
        {
            bail!("当前课程基线已变化，旧裁决不能继续")
        }
        let kind: crate::jwts::PlanKind = serde_json::from_value(
            payload
                .get("kind")
                .cloned()
                .context("审阅文件缺少来源类型")?,
        )?;
        let status: CurriculumUpdateStatus =
            serde_json::from_value(payload.get("status").cloned().context("审阅文件缺少状态")?)?;
        Ok(UpdateSession {
            client: None,
            kind,
            catalog: crate::jwts::CurriculumCatalog::default(),
            selections: serde_json::from_value(
                payload
                    .get("selections")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )?,
            candidate: Some(diff.candidate.clone()),
            diff: Some(diff),
            decisions,
            assignments,
            status,
        })
    }

    fn controlled_review_path(&self, path: &Path) -> Result<PathBuf> {
        let directory = self.operations_path.join(CURRICULUM_REVIEW_DIRECTORY);
        let canonical_directory = directory.canonicalize().context("审阅任务目录不存在")?;
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        let canonical = candidate.canonicalize().context("审阅任务文件不存在")?;
        if canonical.parent() != Some(canonical_directory.as_path())
            || !canonical
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with("curriculum-review-") && name.ends_with(".json")
                })
        {
            bail!("只能恢复管理工具审阅目录中的任务")
        }
        Ok(canonical)
    }

    pub fn plan_remote_sync(
        &self,
        preview: &RepositorySyncPreview,
    ) -> Result<RepositoryLifecyclePreview> {
        self.require_current_workspace()?;
        validate_repository_preview(preview)?;
        if preview.baseline != self.workspace_identity() {
            bail!("本地三份管理数据已变化，请重新生成预览")
        }
        validate_state(&preview.topology, &preview.routes, false)?;
        let organization = string_field(&preview.topology, "organization").to_string();
        let registry_remote = self.registry_remote.clone().unwrap_or_else(|| {
            remote_url(
                &self.remote_template,
                &organization,
                "fireworks-course-registry-v2",
            )
        });
        let baseline = remote_revision(&registry_remote)?;
        let files = registry_dynamic_tree(preview)?;
        let identity_sha256 = canonical_sha256(&json!({
            "remote_url":registry_remote,
            "baseline":baseline,
            "files":files
        }));
        let registry = RegistrySyncPlan {
            remote_url: registry_remote,
            baseline,
            files,
            identity_sha256,
        };
        let manifest_repositories: HashMap<_, _> = array_at(&preview.manifest, "repositories")?
            .iter()
            .filter_map(|value| {
                value
                    .get("repo_id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_string(), value))
            })
            .collect();
        let create: HashSet<_> = preview.create_repositories.iter().cloned().collect();
        let archive: HashSet<_> = preview.archive_repositories.iter().cloned().collect();
        let mut ids = preview.metadata_repositories.clone();
        ids.extend(preview.create_repositories.clone());
        ids.extend(preview.archive_repositories.clone());
        ids.sort();
        ids.dedup();
        let mut actions = Vec::new();
        for repo_id in ids {
            let manifest_repo = manifest_repositories.get(&repo_id).copied();
            let description = manifest_repo
                .map(|value| string_field(value, "description"))
                .unwrap_or("").to_string();
            if description.chars().count() > 350 {
                bail!("仓库描述超过完整条目投影上限：{repo_id}")
            }
            let title = manifest_repo
                .map(|value| string_field(value, "display_name"))
                .filter(|value| !value.is_empty())
                .unwrap_or(&repo_id)
                .to_string();
            let template = manifest_repo
                .and_then(|value| value.get("repo_type"))
                .and_then(Value::as_str)
                == Some("template");
            let remote = remote_url(&self.remote_template, &organization, &repo_id);
            let mut baseline = remote_repository_metadata(&organization, &repo_id, &remote)?;
            let exists = baseline
                .get("exists")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let currently_archived = baseline
                .get("archived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let kind = if create.contains(&repo_id) || !exists {
                RepositoryLifecycleKind::Create
            } else if archive.contains(&repo_id) {
                RepositoryLifecycleKind::Archive
            } else if currently_archived {
                RepositoryLifecycleKind::Unarchive
            } else {
                RepositoryLifecycleKind::Update
            };
            let is_course = manifest_repo.is_some_and(|value| string_field(value, "repo_type") == "course");
            let template_repository = (is_course && !exists && github_repository_path(&remote).is_some())
                .then(|| format!("{organization}/fireworks-course-template-v2"));
            let readme = if is_course {
                let mapping = repository_course_mapping(&preview.manifest, &repo_id)?;
                let generated = crate::repository_metadata::readme(&mapping)?;
                if github_repository_path(&remote).is_some() {
                    let source_repository = template_repository.clone().unwrap_or_else(|| format!("{organization}/{repo_id}"));
                    let original = github_readme(&source_repository)?;
                    let merged = merge_managed_course_readme(original.as_ref().map(|(_, text)| text.as_str()), &generated)?;
                    baseline["readme_sha"] = original.map(|(sha, _)| json!(sha)).unwrap_or(Value::Null);
                    Some(merged)
                } else {
                    Some(generated)
                }
            } else {
                None
            };
            actions.push(RepositoryLifecycleAction {
                repo_id: repo_id.clone(),
                title,
                kind,
                description,
                private: false,
                archived: archive.contains(&repo_id),
                template,
                default_branch: "main".to_string(),
                readme,
                template_repository,
                baseline,
            });
        }
        let summary_lines = vec![
            format!("同步课程注册表：{} 个动态文件", registry.files.len()),
            format!(
                "创建仓库：{} 个",
                actions
                    .iter()
                    .filter(|item| item.kind == RepositoryLifecycleKind::Create)
                    .count()
            ),
            format!(
                "更新仓库设置：{} 个",
                actions
                    .iter()
                    .filter(|item| item.kind == RepositoryLifecycleKind::Update)
                    .count()
            ),
            format!(
                "归档仓库：{} 个",
                actions
                    .iter()
                    .filter(|item| item.kind == RepositoryLifecycleKind::Archive)
                    .count()
            ),
        ];
        let mut result = RepositoryLifecyclePreview {
            organization,
            registry,
            actions,
            summary_lines,
            identity_sha256: String::new(),
        };
        result.identity_sha256 = lifecycle_identity(&result)?;
        Ok(result)
    }

    pub fn refresh_curriculum_preview(&self, preview: &mut RepositorySyncPreview) -> Result<()> {
        self.require_current_workspace()?;
        validate_repository_preview(preview)?;
        if preview.baseline != self.workspace_identity() {
            bail!("不能重整其他数据基线的预览")
        }
        refresh_curriculum_metadata(&mut preview.manifest)?;
        finalize_repository_preview(preview, &self.workspace_identity())
    }

    pub fn execute_remote_sync(
        &mut self,
        state_preview: &RepositorySyncPreview,
        remote_preview: &RepositoryLifecyclePreview,
    ) -> Result<UpdateExecutionJournal> {
        self.execute_remote_sync_with(state_preview, remote_preview, &mut GhLifecycleApi)
    }

    fn execute_remote_sync_with(
        &mut self,
        state_preview: &RepositorySyncPreview,
        remote_preview: &RepositoryLifecyclePreview,
        github: &mut impl GithubLifecycleApi,
    ) -> Result<UpdateExecutionJournal> {
        self.require_current_workspace()?;
        validate_repository_preview(state_preview)?;
        let current_identity = self.workspace_identity();
        let target_identity = json!({
            "manifest_sha256":canonical_sha256(&state_preview.manifest),
            "topology_sha256":canonical_sha256(&state_preview.topology),
            "routes_sha256":canonical_sha256(&state_preview.routes)
        });
        let already_applied = current_identity == target_identity;
        if state_preview.baseline != current_identity && !already_applied {
            bail!("本地三份管理数据已变化，请重新生成预览")
        }
        if lifecycle_identity(remote_preview)? != remote_preview.identity_sha256 {
            bail!("仓库同步预览已被修改")
        }
        if !registry_files_match(state_preview, &remote_preview.registry.files)? {
            bail!("远端预览不属于这份本地状态预览")
        }
        let operation_identity = canonical_sha256(
            &json!({"state":state_preview.identity_sha256,"remote":remote_preview.identity_sha256}),
        );
        let operation_id = format!("curriculum-update-{}", &operation_identity[..20]);
        let preview_path = self
            .operations_path
            .join(format!("{operation_id}.update-preview.json"));
        if preview_path.exists() {
            if crate::json_store::canonical_sha256(&preview_path)? != preview_bundle_sha256(state_preview, remote_preview, &operation_identity)? {
                bail!("已保存的更新预览与当前内容不一致")
            }
        } else {
            let bundle = json!({"state":state_preview,"remote":remote_preview,"identity_sha256":operation_identity});
            atomic_json(&preview_path, &bundle)?;
        }
        let journal_path = self
            .operations_path
            .join(format!("{operation_id}.update.json"));
        let mut journal = if journal_path.exists() {
            serde_json::from_value::<UpdateExecutionJournal>(read_json(&journal_path)?)?
        } else {
            UpdateExecutionJournal {
                schema_version: 2,
                operation_id: operation_id.clone(),
                preview_identity_sha256: operation_identity.clone(),
                preview_path: preview_path.to_string_lossy().to_string(),
                status: "applying".to_string(),
                stage: "registry".to_string(),
                registry_commit: None,
                repository_results: BTreeMap::new(),
                completed_stages: Vec::new(),
                error: None,
                created_at: now(),
                updated_at: now(),
            }
        };
        if journal.preview_identity_sha256 != operation_identity {
            bail!("任务记录属于另一批更新")
        }
        if journal.status == "completed" {
            self.verify_remote_sync(remote_preview, &journal)?;
            return Ok(journal);
        }
        recover_legacy_creation(github, &remote_preview.organization, &remote_preview.actions, &mut journal, &journal_path)?;
        journal.status = "applying".to_string();
        journal.error = None;
        journal.updated_at = now();
        save_update_journal(&journal_path, &journal)?;
        let result = (|| -> Result<()> {
            if !journal
                .completed_stages
                .iter()
                .any(|value| value == "registry")
            {
                journal.stage = "registry".to_string();
                save_update_journal(&journal_path, &journal)?;
                journal.registry_commit = Some(sync_registry(
                    &remote_preview.registry,
                    &journal.operation_id,
                )?);
                journal.completed_stages.push("registry".to_string());
                journal.updated_at = now();
                save_update_journal(&journal_path, &journal)?;
            }
            journal.stage = "repositories".to_string();
            for action in &remote_preview.actions {
                if journal
                    .repository_results
                    .get(&action.repo_id)
                    .is_some_and(|value| value == "completed")
                {
                    continue;
                }
                apply_repository_lifecycle(github, &remote_preview.organization, action, &mut journal, &journal_path)?;
                verify_repository_lifecycle_with(github, &remote_preview.organization, action)?;
                journal
                    .repository_results
                    .insert(action.repo_id.clone(), "completed".to_string());
                journal.updated_at = now();
                save_update_journal(&journal_path, &journal)?;
            }
            if !journal
                .completed_stages
                .iter()
                .any(|value| value == "repositories")
            {
                journal.completed_stages.push("repositories".to_string());
            }
            journal.stage = "remote-verify".to_string();
            verify_registry(&remote_preview.registry, journal.registry_commit.as_deref())?;
            for action in &remote_preview.actions {
                verify_repository_lifecycle_with(github, &remote_preview.organization, action)?;
            }
            if !journal
                .completed_stages
                .iter()
                .any(|value| value == "remote-verified")
            {
                journal.completed_stages.push("remote-verified".to_string());
            }
            journal.stage = "local-state".to_string();
            if !already_applied {
                self.apply_repository_sync_preview(state_preview)?;
            }
            if !journal
                .completed_stages
                .iter()
                .any(|value| value == "local-state")
            {
                journal.completed_stages.push("local-state".to_string());
            }
            journal.status = "completed".to_string();
            journal.stage = "completed".to_string();
            journal.updated_at = now();
            journal.error = None;
            save_update_journal(&journal_path, &journal)?;
            Ok(())
        })();
        if let Err(error) = result {
            journal.status = "failed".to_string();
            journal.error = Some(format!("{error:#}"));
            journal.updated_at = now();
            let _ = save_update_journal(&journal_path, &journal);
            return Err(error);
        }
        Ok(journal)
    }

    pub fn resume_remote_sync(
        &mut self,
        journal: &UpdateExecutionJournal,
    ) -> Result<UpdateExecutionJournal> {
        let mut bundle = read_json(Path::new(&journal.preview_path))?;
        let state: RepositorySyncPreview = serde_json::from_value(
            bundle
                .as_object_mut().context("更新预览不是对象")?
                .remove("state")
                .context("更新任务缺少本地预览")?,
        )?;
        let remote: RepositoryLifecyclePreview = serde_json::from_value(
            bundle
                .as_object_mut().context("更新预览不是对象")?
                .remove("remote")
                .context("更新任务缺少远端预览")?,
        )?;
        let expected = canonical_sha256(
            &json!({"state":state.identity_sha256,"remote":remote.identity_sha256}),
        );
        if bundle.get("identity_sha256").and_then(Value::as_str) != Some(expected.as_str())
            || journal.preview_identity_sha256 != expected
        {
            bail!("更新预览与任务记录不一致")
        }
        if journal.status == "completed" {
            self.verify_remote_sync(&remote, journal)?;
            return Ok(journal.clone());
        }
        self.execute_remote_sync(&state, &remote)
    }

    pub fn verify_update_journal(&self, journal: &UpdateExecutionJournal) -> Result<()> {
        let mut bundle = read_json(Path::new(&journal.preview_path))?;
        let state: RepositorySyncPreview = serde_json::from_value(
            bundle
                .as_object_mut().context("更新预览不是对象")?
                .remove("state")
                .context("更新任务缺少本地预览")?,
        )?;
        validate_repository_preview(&state)?;
        let remote: RepositoryLifecyclePreview = serde_json::from_value(
            bundle
                .as_object_mut().context("更新预览不是对象")?
                .remove("remote")
                .context("更新任务缺少远端预览")?,
        )?;
        if !registry_files_match(&state, &remote.registry.files)? {
            bail!("远端预览与本地状态不一致")
        }
        self.verify_remote_sync(&remote, journal)
    }

    pub fn current_repository_sync_preview(&self) -> Result<RepositorySyncPreview> {
        let manifest_repositories = array_at(&self.manifest, "repositories")?;
        let metadata_repositories = manifest_repositories
            .iter()
            .filter_map(|value| value.get("repo_id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let mut preview = RepositorySyncPreview {
            manifest: self.manifest.clone(),
            topology: self.topology.clone(),
            routes: self.routes.clone(),
            baseline: self.workspace_identity(),
            identity_sha256: String::new(),
            create_repositories: Vec::new(),
            archive_repositories: Vec::new(),
            metadata_repositories,
            plan_count: array_at(&self.manifest, "curriculum_plans")?.len(),
            record_count: array_at(&self.manifest, "curriculum_records")?.len(),
            descriptor_count: array_at(&self.manifest, "course_descriptors")?.len(),
            new_course_code_count: 0,
            removed_course_code_count: 0,
            summary_lines: vec!["检查课程注册表和全部仓库设置".to_string()],
        };
        finalize_repository_preview(&mut preview, &self.workspace_identity())?;
        Ok(preview)
    }

    pub fn update_journals(&self) -> Result<Vec<UpdateExecutionJournal>> {
        if !self.operations_path.is_dir() {
            return Ok(Vec::new());
        }
        let mut result: Vec<UpdateExecutionJournal> = Vec::new();
        for path in sorted_json_files(&self.operations_path)? {
            if !path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.ends_with(".update.json"))
            {
                continue;
            }
            let value = read_json(&path)?;
            result.push(serde_json::from_value(value)?);
        }
        result.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(result)
    }

    pub fn verify_remote_sync(
        &self,
        remote_preview: &RepositoryLifecyclePreview,
        journal: &UpdateExecutionJournal,
    ) -> Result<()> {
        if lifecycle_identity(remote_preview)? != remote_preview.identity_sha256 {
            bail!("仓库同步预览已被修改")
        }
        if journal.status != "completed" {
            bail!("更新尚未完成")
        }
        verify_registry(&remote_preview.registry, journal.registry_commit.as_deref())?;
        for action in &remote_preview.actions {
            verify_repository_lifecycle(&remote_preview.organization, action)?;
        }
        Ok(())
    }

    pub fn inspect(&self) -> Result<Health> {
        validate_state(&self.topology, &self.routes, false)?;
        let repositories = repositories(&self.topology)?;
        let files = array_at(&self.routes, "files")?;
        let course_routes = array_at(&self.routes, "course_code_routes").unwrap_or(&[]);
        let complete = array_at(&self.routes, "inventory_complete_repositories")?;
        let virtual_count = array_at(&self.manifest, "virtual_collections")
            .map(|value| value.len())
            .unwrap_or(0);
        let mut special = BTreeSet::new();
        for file in files {
            if string_field(file, "route_kind") == "special-topic" {
                if let Some(keys) = file.get("route_keys").and_then(Value::as_array) {
                    for key in keys.iter().filter_map(Value::as_str) {
                        special.insert(key.to_string());
                    }
                }
            }
        }
        Ok(Health {
            organization: string_field(&self.topology, "organization").to_string(),
            health: "healthy".to_string(),
            health_message: None,
            repository_count: repositories.len(),
            course_route_count: course_routes.len(),
            file_route_count: files.len(),
            inventory_complete_repository_count: complete.len(),
            virtual_collection_count: virtual_count,
            special_topic_route_count: special.len(),
            identity: self.workspace_identity(),
        })
    }

    pub fn search(&self, term: &str) -> Result<Vec<RepositorySummary>> {
        let term = normalize(term).to_lowercase();
        Ok(self
            .repository_rows()?
            .into_iter()
            .filter(|row| {
                if term.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {} {} {} {}",
                    row.repo_id,
                    row.display_name,
                    row.description,
                    row.course_codes.join(" "),
                    row.course_names.join(" ")
                )
                .to_lowercase();
                haystack.contains(&term)
            })
            .collect())
    }

    pub fn repository(&self, repo_id: &str) -> Result<RepositoryDetail> {
        let summary = self
            .repository_rows()?
            .into_iter()
            .find(|row| row.repo_id == repo_id)
            .with_context(|| "没有找到这份资料")?;
        let file_routes = array_at(&self.routes, "files")?
            .iter()
            .filter(|row| string_field(row, "repo_id") == repo_id)
            .cloned()
            .collect();
        let course_routes = array_at(&self.routes, "course_code_routes")
            .unwrap_or(&[])
            .iter()
            .filter(|row| string_field(row, "repo_id") == repo_id)
            .cloned()
            .collect();
        let topology = repositories(&self.topology)?
            .get(repo_id)
            .cloned()
            .unwrap_or(Value::Null);
        Ok(RepositoryDetail {
            physical_repository_id: topology
                .get("physical_repository_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            summary,
            file_routes,
            course_routes,
            topology,
        })
    }

    pub fn routes(&self, repo_id: Option<&str>) -> Result<RoutesSnapshot> {
        let filter = repo_id.unwrap_or("");
        let file_routes: Vec<_> = array_at(&self.routes, "files")?
            .iter()
            .filter(|row| filter.is_empty() || string_field(row, "repo_id") == filter)
            .cloned()
            .collect();
        let course_code_routes: Vec<_> = array_at(&self.routes, "course_code_routes")
            .unwrap_or(&[])
            .iter()
            .filter(|row| filter.is_empty() || string_field(row, "repo_id") == filter)
            .cloned()
            .collect();
        Ok(RoutesSnapshot {
            file_total: file_routes.len(),
            course_code_total: course_code_routes.len(),
            file_routes,
            course_code_routes,
        })
    }

    pub fn plans(&self) -> Result<Vec<PlanSummary>> {
        let mut result = Vec::new();
        if !self.operations_path.is_dir() {
            return Ok(result);
        }
        for path in sorted_json_files(&self.operations_path)? {
            if !path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.ends_with(".plan.json"))
            {
                continue;
            }
            match read_json(&path).and_then(|plan| {
                validate_plan_identity(&plan)?;
                Ok(plan)
            }) {
                Ok(plan) => result.push(PlanSummary {
                    path: path.to_string_lossy().to_string(),
                    operation_id: plan
                        .get("operation_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    kind: plan
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    created_at: plan
                        .get("created_at")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    plan_identity_sha256: plan
                        .pointer("/core/plan_identity_sha256")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    confirmation_phrase: plan
                        .pointer("/core/confirmation_phrase")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    state: Some(self.plan_phase(&plan)),
                    valid: true,
                    error: None,
                }),
                Err(error) => result.push(PlanSummary {
                    path: path.to_string_lossy().to_string(),
                    valid: false,
                    error: Some(human_error(&error)),
                    ..PlanSummary::default()
                }),
            }
        }
        Ok(result)
    }

    pub fn journals(&self) -> Result<Vec<JournalSummary>> {
        let mut result = Vec::new();
        if !self.operations_path.is_dir() {
            return Ok(result);
        }
        for path in sorted_json_files(&self.operations_path)? {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if name.ends_with(".plan.json") {
                continue;
            }
            match read_json(&path) {
                Ok(journal) => {
                    let plan = journal.get("plan").cloned().unwrap_or(Value::Null);
                    let valid = validate_plan_identity(&plan).is_ok();
                    let status = string_field(&journal, "status").to_string();
                    let phase = self.plan_phase_with_journal(&plan, Some(&journal));
                    let recovery_state = if !valid {
                        "invalid"
                    } else if phase == "drifted" {
                        "drifted"
                    } else if status == "completed" && phase == "after" {
                        "completed"
                    } else if matches!(status.as_str(), "planned" | "applying" | "failed") {
                        "resumable"
                    } else {
                        "invalid"
                    };
                    result.push(JournalSummary {
                        path: path.to_string_lossy().to_string(),
                        operation_id: journal
                            .get("operation_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        kind: journal
                            .get("kind")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        status,
                        recovery_state: recovery_state.to_string(),
                        plan_identity_sha256: plan
                            .pointer("/core/plan_identity_sha256")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        confirmation_phrase: journal
                            .get("operation_id")
                            .and_then(Value::as_str)
                            .map(|value| format!("RESUME {value}")),
                        error: journal
                            .get("error")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        updated_at: journal
                            .get("updated_at")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    });
                }
                Err(error) => result.push(JournalSummary {
                    path: path.to_string_lossy().to_string(),
                    status: "invalid".to_string(),
                    recovery_state: "invalid".to_string(),
                    error: Some(human_error(&error)),
                    ..JournalSummary::default()
                }),
            }
        }
        Ok(result)
    }

    pub fn plan_detail(&self, operation_id: &str) -> Result<PlannedOperation> {
        let path = self
            .operations_path
            .join(format!("{operation_id}.plan.json"));
        let plan = read_json(&path)?;
        validate_plan_identity(&plan)?;
        Ok(plan_result(path, plan))
    }

    pub fn split_options(&self, repo_id: &str) -> Result<SplitOptions> {
        let row = self.repository_rows()?.into_iter().find(|row| row.repo_id == repo_id)
            .context("没有找到这份资料")?;
        if !row.inventory_complete { bail!("这份资料还没有完成文件清点，暂时不能拆分。") }
        let files: Vec<_> = array_at(&self.routes, "files")?.iter()
            .filter(|file| string_field(file, "repo_id") == repo_id).collect();
        let names: BTreeMap<_, _> = array_at(&self.manifest, "course_descriptors")?.iter()
            .map(|descriptor| (string_field(descriptor, "course_code"), string_field(descriptor, "course_name"))).collect();
        let courses = row.course_codes.iter().map(|code| {
            let assigned: Vec<_> = files.iter().filter(|file| string_array(file, "course_codes").contains(code)).collect();
            SplitCourse {
                course_code: code.clone(),
                title: names.get(code.as_str()).copied().unwrap_or(code).to_string(),
                shared_course_codes: assigned.iter().flat_map(|file| string_array(file, "course_codes"))
                    .filter(|other| other != code).collect::<BTreeSet<_>>().into_iter().collect(),
                file_count: assigned.len(),
                bytes: assigned.iter().map(|file| file.get("size").and_then(Value::as_u64).unwrap_or(0)).sum(),
                sample_paths: assigned.iter().take(5).map(|file| string_field(file, "path").to_string()).collect(),
            }
        }).collect();
        let mut loose_files: Vec<_> = files.iter().filter(|file| string_array(file, "course_codes").is_empty())
            .map(|file| LooseFile { internal_path: string_field(file, "path").to_string(),
                title: friendly_path(string_field(file, "path")), size: file.get("size").and_then(Value::as_u64).unwrap_or(0) }).collect();
        loose_files.extend(self.source_loose_files(repo_id)?);
        Ok(SplitOptions { source_repo_id: row.repo_id, source_title: row.display_name, courses, loose_files })
    }

    pub fn automatic_repo_id(&self, title: &str, semantic_keys: &[String]) -> String {
        let mut keys = semantic_keys.to_vec();
        keys.sort();
        let value = json!({"title": normalize(title), "members": keys});
        format!("MANAGED-{}", &canonical_sha256(&value)[..12].to_uppercase())
    }

    pub fn plan_split(
        &self,
        source_repo_id: &str,
        targets: &[SplitTarget],
    ) -> Result<PlannedOperation> {
        let plan = self.build_split_plan(source_repo_id, targets)?;
        self.prepare_plan(plan)
    }

    pub fn plan_merge(
        &self,
        source_repo_ids: &[String],
        target_repo_id: &str,
        display_name: &str,
    ) -> Result<PlannedOperation> {
        let plan = self.build_merge_plan(source_repo_ids, target_repo_id, display_name)?;
        self.prepare_plan(plan)
    }

    pub fn discard_plan(&self, _plan: &PlannedOperation) -> Result<()> {
        Ok(())
    }
    pub fn apply(&mut self, plan: &PlannedOperation) -> Result<Value> {
        self.require_current_workspace()?;
        validate_plan_identity(&plan.plan)?;
        if self.plan_phase(&plan.plan) != "before" {
            bail!("资料状态已经变化，请重新开始这次操作。")
        }
        self.validate_remote_baseline(&plan.plan, None)?;
        let journal_path = self
            .operations_path
            .join(format!("{}.json", plan.operation_id()));
        if journal_path.exists() {
            bail!("这次操作已经有任务记录，请从“任务记录”继续。")
        }
        let mut journal = json!({
            "schema_version": 1,
            "operation_id": plan.operation_id(),
            "kind": string_field(&plan.plan, "kind"),
            "status": "planned",
            "created_at": now(),
            "updated_at": now(),
            "plan": plan.plan,
            "completed_stages": [],
            "error": null
        });
        atomic_json(&journal_path, &journal)?;
        match self.resume_journal(&journal_path, &mut journal) {
            Ok(()) => {
                self.reload()?;
                Ok(journal)
            }
            Err(error) => {
                journal["status"] = json!("failed");
                journal["error"] = json!(human_error(&error));
                journal["updated_at"] = json!(now());
                atomic_json(&journal_path, &journal)?;
                Err(error)
            }
        }
    }

    pub fn resume(&mut self, journal: &JournalSummary) -> Result<Value> {
        self.require_current_workspace()?;
        if journal.recovery_state != "resumable" {
            bail!("这项任务不能自动继续，请先查看系统检查。")
        }
        let path = PathBuf::from(&journal.path);
        let mut value = read_json(&path)?;
        let plan = self.validate_journal(&value)?;
        self.validate_remote_baseline(&plan, Some(&value))?;
        self.resume_journal(&path, &mut value)?;
        self.reload()?;
        Ok(value)
    }

    pub fn verify(&self, journal: &JournalSummary) -> Result<Value> {
        let value = read_json(Path::new(&journal.path))?;
        if string_field(&value, "status") != "completed" {
            bail!("这项任务尚未完成，暂时不能检查结果。")
        }
        let plan = self.validate_journal(&value)?;
        self.validate_remote_baseline(&plan, Some(&value))?;
        if self.plan_phase_with_journal(&plan, Some(&value)) != "after" {
            bail!("最终资料状态与任务记录不一致。")
        }
        validate_state(&self.topology, &self.routes, false)?;
        if let Some(targets) = value.pointer("/git/targets").and_then(Value::as_object) {
            let resolved = value
                .get("resolved_after_routes")
                .context("任务记录缺少最终路由")?;
            for (repo_id, record) in targets {
                let expected = resolved
                    .pointer(&format!("/repository_heads/{}", escape_pointer(repo_id)))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let remote = string_field(record, "remote_url");
                if remote_head(remote)? != Some(expected.to_string()) {
                    bail!("远端资料与本地记录不一致，请停止操作并联系维护人员。")
                }
            }
        }
        Ok(json!({"valid": true}))
    }

    pub fn system_status(&self) -> SystemStatus {
        let git_available = command_available("git", &["--version"]);
        let github_logged_in = command_available("gh", &["auth", "status"]);
        let summary = if git_available && github_logged_in {
            "浏览、合并、拆分都可以使用".to_string()
        } else if git_available {
            "浏览可用；修改资料前需要登录 GitHub".to_string()
        } else {
            "浏览可用；修改资料前需要安装 Git".to_string()
        };
        SystemStatus {
            offline_ready: true,
            git_available,
            github_logged_in,
            summary,
        }
    }

    fn workspace_identity(&self) -> Value {
        json!({
            "manifest_sha256": canonical_sha256(&self.manifest),
            "topology_sha256": canonical_sha256(&self.topology),
            "routes_sha256": canonical_sha256(&self.routes)
        })
    }

    fn disk_workspace_identity(&self) -> Result<Value> {
        Ok(json!({
            "manifest_sha256":canonical_sha256(&read_json(&self.manifest_path)?),
            "topology_sha256":canonical_sha256(&read_json(&self.topology_path)?),
            "routes_sha256":canonical_sha256(&read_json(&self.routes_path)?)
        }))
    }

    fn require_current_workspace(&self) -> Result<()> {
        if self.disk_workspace_identity()? != self.workspace_identity() {
            bail!("磁盘上的管理数据已变化，请刷新后重新审阅")
        }
        Ok(())
    }

    fn repository_rows(&self) -> Result<Vec<RepositorySummary>> {
        let manifest_repos: HashMap<_, _> = array_at(&self.manifest, "repositories")?
            .iter()
            .filter_map(|value| {
                value
                    .get("repo_id")
                    .and_then(Value::as_str)
                    .map(|repo_id| (repo_id.to_string(), value))
            })
            .collect();
        let descriptors: HashMap<_, _> = array_at(&self.manifest, "course_descriptors")
            .unwrap_or(&[])
            .iter()
            .filter_map(|value| {
                value
                    .get("course_code")
                    .and_then(Value::as_str)
                    .map(|code| {
                        (
                            code.to_string(),
                            string_field(value, "course_name").to_string(),
                        )
                    })
            })
            .collect();
        let files = array_at(&self.routes, "files")?;
        let complete: HashSet<_> = array_at(&self.routes, "inventory_complete_repositories")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let heads = object_at(&self.routes, "repository_heads")?;
        let mut result = Vec::new();
        for (repo_id, topology) in repositories(&self.topology)? {
            let manifest = manifest_repos.get(repo_id);
            let codes = string_array(topology, "course_codes");
            let mut names = BTreeSet::new();
            for code in &codes {
                if let Some(name) = descriptors.get(code) {
                    names.insert(name.clone());
                }
            }
            let repo_files: Vec<_> = files
                .iter()
                .filter(|value| string_field(value, "repo_id") == repo_id)
                .collect();
            result.push(RepositorySummary {
                repo_id: repo_id.clone(),
                repo_type: string_field(topology, "repo_type").to_string(),
                display_name: if string_field(topology, "display_name").is_empty() {
                    manifest
                        .map(|value| string_field(value, "display_name"))
                        .unwrap_or(repo_id)
                        .to_string()
                } else {
                    string_field(topology, "display_name").to_string()
                },
                description: manifest
                    .map(|value| string_field(value, "description"))
                    .unwrap_or("")
                    .to_string(),
                course_codes: codes,
                course_names: names.into_iter().collect(),
                unowned_paths: repo_files
                    .iter()
                    .filter(|value| string_array(value, "course_codes").is_empty())
                    .map(|value| string_field(value, "path").to_string())
                    .collect(),
                file_count: repo_files.len(),
                bytes: repo_files
                    .iter()
                    .map(|value| value.get("size").and_then(Value::as_u64).unwrap_or(0))
                    .sum(),
                head: heads
                    .get(repo_id)
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                inventory_complete: complete.contains(repo_id.as_str()),
            });
        }
        result.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        Ok(result)
    }

    fn manifest_repository(&self, repo_id: &str) -> Option<&Value> {
        array_at(&self.manifest, "repositories")
            .ok()?
            .iter()
            .find(|value| string_field(value, "repo_id") == repo_id)
    }


    fn prepare_plan(&self, mut plan: Value) -> Result<PlannedOperation> {
        self.require_current_workspace()?;
        let manifest = manifest_for_routes(&self.manifest, &plan["after"]["topology"], &plan["after"]["routes"])?;
        plan["after"]["manifest_sha256"] = json!(canonical_sha256(&manifest));
        validate_resource_layout(&manifest, &plan["after"]["routes"])?;
        plan["after"]["manifest"] = manifest;
        validate_direct_manifest(&self.manifest)?;
        let operation_id = string_field(&plan, "operation_id").to_string();
        let organization = string_field(&self.topology, "organization").to_string();
        let actor = current_actor(&self.remote_template)?;
        let sources = plan
            .pointer("/details/source_repository_heads")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let targets = plan
            .pointer("/after/routes/unresolved_repository_heads")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut source_baseline = Map::new();
        let mut target_baseline = Map::new();
        for (repo_id, expected_head) in sources {
            let url = remote_url(&self.remote_template, &organization, &repo_id);
            let revision = remote_revision(&url)?;
            if revision.get("head") != Some(&expected_head) {
                bail!("远端资料已经变化，请刷新后重新开始。")
            }
            source_baseline.insert(repo_id, revision);
        }
        for repo_id in targets.iter().filter_map(Value::as_str) {
            let url = remote_url(&self.remote_template, &organization, repo_id);
            target_baseline.insert(repo_id.to_string(), remote_revision(&url)?);
        }
        let registry = if self.remote_template == DEFAULT_REMOTE_TEMPLATE {
            let repo_id = self
                .manifest
                .pointer("/sources/curriculum/metadata_repo_id")
                .and_then(Value::as_str)
                .unwrap_or("fireworks-course-registry-v2");
            let url = remote_url(&self.remote_template, &organization, repo_id);
            json!({"repo_id":repo_id,"revision":remote_revision(&url)?})
        } else {
            json!({
                "repo_id":"local-test-registry",
                "revision":null,
                "manifest_sha256":canonical_sha256(&self.manifest)
            })
        };
        plan["core"] = json!({
            "organization": organization,
            "github_actor": actor,
            "workspace_identity": self.workspace_identity(),
            "confirmation_phrase": format!("APPLY {operation_id}"),
            "remote_baseline": {
                "remote_url_template": self.remote_template,
                "registry":registry,
                "sources":source_baseline,
                "targets":target_baseline
            }
        });
        self.preserve_source_tree(&mut plan)?;
        let identity = plan_identity_sha256(&plan);
        plan["core"]["plan_identity_sha256"] = json!(identity);
        Ok(plan_result(PathBuf::new(), plan))
    }

    fn preserve_source_tree(&self, plan: &mut Value) -> Result<()> {
        let temp = TempDir::new().context("无法创建预览 Git 工作区")?;
        run_git(temp.path(), &["init", "--bare"], None, &[])?;
        let sources = object_at(&plan["details"], "source_repository_heads")?.clone();
        let targets: BTreeSet<_> = string_array(&plan["after"]["routes"], "unresolved_repository_heads").into_iter().collect();
        let mut moves = array_at(&plan["details"], "file_moves")?.to_vec();
        let mut occupied = HashMap::<String, HashSet<String>>::new();
        for item in &moves {
            let target = string_field(item, "target_repo_id");
            if !targets.contains(target) { bail!("文件迁移指向操作范围外的目标仓库") }
            reserve_target_path(string_field(item, "target_path"), occupied.entry(target.to_string()).or_default())?;
        }
        for (repo, expected_head) in sources {
            let (head, tree) = self.frozen_source_tree(&repo, temp.path())?;
            if expected_head.as_str() != Some(head.as_str()) { bail!("源仓库冻结版本不一致") }
            let mut owners = BTreeMap::<String, String>::new();
            for item in moves.iter().filter(|item| string_field(item, "source_repo_id") == repo) {
                let path = string_field(item, "source_path");
                if !tree.contains_key(path) { bail!("仓库 {repo} 的清单包含冻结源树中不存在的文件：{path}") }
                if owners.insert(path.to_string(), string_field(item, "target_repo_id").to_string()).is_some() {
                    bail!("同一源文件被重复迁移")
                }
            }
            let boundaries = source_dependency_boundaries(temp.path(), &head, &tree, &self.manifest)?;
            // Only evidenced dependency scopes propagate ownership, never categories or years.
            loop {
                let mut changed = false;
                for boundary in &boundaries {
                    let destinations: BTreeSet<_> = boundary.iter().filter_map(|path| owners.get(path)).cloned().collect();
                    if destinations.len() > 1 { bail!("软件包或相对引用文档被分到了多个目标仓库，请保持完整结构。") }
                    if let Some(owner) = destinations.into_iter().next() {
                        for path in boundary {
                            if !owners.contains_key(path) { owners.insert(path.clone(), owner.clone()); changed = true; }
                        }
                    }
                }
                if !changed { break; }
            }
            for path in tree.keys() {
                if moves.iter().any(|item| string_field(item, "source_repo_id") == repo && string_field(item, "source_path") == path) { continue; }
                let target = owners.get(path).cloned()
                    .or_else(|| (repository_infrastructure(path) && targets.contains(&repo)).then(|| repo.clone()))
                    .or_else(|| (targets.len() == 1).then(|| targets.iter().next().cloned()).flatten())
                    .or_else(|| repository_infrastructure(path).then(|| targets.iter().next().cloned()).flatten())
                    .with_context(|| format!("源仓库 {repo} 有未清点文件 {path}，请在拆分路径选择中明确去向后重新预览。"))?;
                reserve_target_path(path, occupied.entry(target.clone()).or_default())?;
                moves.push(json!({"source_repo_id":repo,"source_path":path,"target_repo_id":target,"target_path":path,"preserved_unmanaged":true}));
            }
            // An all-new split must not produce repositories without the source explanation/license.
            // Copy original blobs, and reject collisions instead of overwriting another source's metadata.
            if targets.len() > 1 && !targets.contains(&repo) {
                for required in ["readme", "license"] {
                    let paths: Vec<_> = tree.keys().filter(|path| root_metadata_kind(path) == Some(required)).collect();
                    if paths.is_empty() { bail!("源仓库 {repo} 缺少 {required}，请补齐说明和许可后再拆为全新仓库。") }
                    for target in &targets {
                        for path in &paths {
                            if moves.iter().any(|item| string_field(item, "target_repo_id") == target && string_field(item, "target_path") == path.as_str()) { continue; }
                            reserve_target_path(path, occupied.entry(target.clone()).or_default())?;
                            moves.push(json!({"source_repo_id":repo,"source_path":path,"target_repo_id":target,"target_path":path,"preserved_unmanaged":true}));
                        }
                    }
                }
            }
        }
        plan["details"]["file_moves"] = json!(moves);
        Ok(())
    }

    fn source_loose_files(&self, repo: &str) -> Result<Vec<LooseFile>> {
        let temp = TempDir::new().context("无法创建预览 Git 工作区")?;
        run_git(temp.path(), &["init", "--bare"], None, &[])?;
        let (_, tree) = self.frozen_source_tree(repo, temp.path())?;
        let files = array_at(&self.routes, "files")?;
        Ok(tree.keys().filter(|path| !repository_infrastructure(path))
            .filter(|path| !files.iter().any(|file| string_field(file, "repo_id") == repo && string_field(file, "path") == path.as_str()))
            .map(|path| LooseFile { internal_path: path.clone(), title: friendly_path(path), size: 0 }).collect())
    }

    fn frozen_source_tree(&self, repo: &str, object_repo: &Path) -> Result<(String, BTreeMap<String, (String, String)>)> {
        let head = object_at(&self.routes, "repository_heads")?.get(repo).and_then(Value::as_str).context("源仓库缺少冻结版本")?.to_string();
        let remote = remote_url(&self.remote_template, string_field(&self.topology, "organization"), repo);
        fetch_commit(object_repo, &remote, &head, &format!("refs/source/{repo}"))?;
        let output = run_git(object_repo, &["ls-tree", "-r", "-z", &head], None, &[])?;
        let mut tree = BTreeMap::new();
        for entry in output.split('\0').filter(|entry| !entry.is_empty()) {
            let (metadata, path) = entry.split_once('\t').context("源树格式无效")?;
            let fields: Vec<_> = metadata.split_whitespace().collect();
            if fields.len() != 3 || fields[1] != "blob" { bail!("源仓库 {repo} 含非普通文件 {path}，请先显式处理。") }
            if safe_path(path)? != path { bail!("源树路径不是规范路径：{path}") }
            tree.insert(path.to_string(), (fields[0].to_string(), fields[2].to_string()));
        }
        Ok((head, tree))
    }

    fn build_split_plan(&self, source_repo_id: &str, targets: &[SplitTarget]) -> Result<Value> {
        validate_state(&self.topology, &self.routes, false)?;
        let source = repositories(&self.topology)?
            .get(source_repo_id)
            .cloned()
            .context("没有找到要拆分的资料")?;
        let complete: HashSet<_> = array_at(&self.routes, "inventory_complete_repositories")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if !complete.contains(source_repo_id) {
            bail!("这份资料还没有完成文件清点，暂时不能拆分。")
        }
        if targets.len() < 2 {
            bail!("至少需要分成两份资料。")
        }
        let source_codes: BTreeSet<_> = string_array(&source, "course_codes").into_iter().collect();
        let mut code_target = HashMap::new();
        let mut path_target = HashMap::new();
        let mut seen_targets = HashSet::new();
        for target in targets {
            safe_repo_id(&target.repo_id)?;
            if !seen_targets.insert(target.repo_id.clone()) {
                bail!("有两个目标资料名称相同，请修改名称。")
            }
            if repositories(&self.topology)?.contains_key(&target.repo_id)
                && target.repo_id != source_repo_id
            {
                bail!("目标资料已经存在，请换一个名称。")
            }
            if target.course_codes.is_empty() && target.paths.is_empty() {
                bail!("每份目标资料至少要包含一个课程代码或一个文件。")
            }
            for code in &target.course_codes {
                if !source_codes.contains(code) { bail!("课程代码 {code} 不属于源仓库。") }
                if code_target.insert(code.clone(), target.repo_id.clone()).is_some() {
                    bail!("课程代码 {code} 被重复分配。")
                }
            }
            for path in &target.paths {
                safe_path(path)?;
                if path_target
                    .insert(path.clone(), target.repo_id.clone())
                    .is_some()
                {
                    bail!("同一文件被分到了两份资料。")
                }
            }
        }
        if code_target.keys().cloned().collect::<BTreeSet<_>>() != source_codes {
            bail!("还有课程代码没有分配，请完成全部选择。")
        }
        let mut after_routes = self.routes.clone();
        let route_files = after_routes
            .get_mut("files")
            .and_then(Value::as_array_mut)
            .context("路由文件损坏")?;
        let mut moves = Vec::new();
        let mut source_paths = HashSet::new();
        let mut routed_counts = HashMap::<String, usize>::new();
        let mut occupied = HashMap::<String, HashSet<String>>::new();
        route_files.sort_by_key(|file| (string_field(file, "repo_id").to_string(), string_field(file, "path").to_string()));
        for file in route_files.iter_mut() {
            if string_field(file, "repo_id") != source_repo_id {
                continue;
            }
            let path = string_field(file, "path").to_string();
            source_paths.insert(path.clone());
            let codes = string_array(file, "course_codes");
            let mut owners = BTreeSet::new();
            for code in &codes {
                owners.insert(code_target.get(code).with_context(|| format!("文件 {path} 的课程代码 {code} 不属于源仓库"))?.clone());
            }
            if owners.len() > 1 {
                bail!("文件“{}”由课程 {} 共享，这些课程必须分到同一个目标仓库。", friendly_path(&path), codes.join("、"))
            }
            let semantic_target = owners.into_iter().next();
            let explicit_target = path_target.get(&path).cloned();
            if let (Some(left), Some(right)) = (&semantic_target, &explicit_target) {
                if left != right {
                    bail!("文件“{}”的课程归属与手动选择冲突。", friendly_path(&path))
                }
            }
            let target = explicit_target
                .or(semantic_target)
                .or_else(|| (repository_infrastructure(&path) && seen_targets.contains(source_repo_id)).then(|| source_repo_id.to_string()))
                .or_else(|| repository_infrastructure(&path).then(|| targets.first().map(|target| target.repo_id.clone())).flatten())
                .with_context(|| format!("文件“{}”还没有选择去向。", friendly_path(&path)))?;
            let target_path = reserve_target_path(&path, occupied.entry(target.clone()).or_default())?;
            file["path"] = json!(target_path);
            file["repo_id"] = json!(target);
            moves.push(json!({
                "source_repo_id": source_repo_id,
                "source_path": path,
                "target_repo_id": target,
                "target_path": target_path
            }));
            *routed_counts.entry(target).or_default() += 1;
        }
        if let Some(course_routes) = after_routes
            .get_mut("course_code_routes")
            .and_then(Value::as_array_mut)
        {
            for route in course_routes.iter_mut() {
                if string_field(route, "repo_id") != source_repo_id {
                    continue;
                }
                let code = string_field(route, "course_code");
                let target = code_target.get(code).cloned().context("课程代码没有分配到目标资料")?;
                let target_topology = targets
                    .iter()
                    .find(|item| item.repo_id == target)
                    .context("课程代码目标不存在")?;
                route["repo_id"] = json!(target);
                route["physical_repository_id"] = json!(if target == source_repo_id {
                    string_field(&source, "physical_repository_id").to_string()
                } else {
                    format!(
                        "physical-managed-{}",
                        &canonical_sha256(
                            &json!({"repo_id":target_topology.repo_id,"operation":"split"})
                        )[..16]
                    )
                });
            }
        }
        for (path, target) in &path_target {
            if !source_paths.contains(path) {
                let target_path = reserve_target_path(path, occupied.entry(target.clone()).or_default())?;
                moves.push(json!({"source_repo_id":source_repo_id,"source_path":path,"target_repo_id":target,"target_path":target_path,"preserved_unmanaged":true}));
            }
        }
        let mut after_topology = self.topology.clone();
        let repos = after_topology
            .get_mut("repositories")
            .and_then(Value::as_object_mut)
            .context("资料索引损坏")?;
        repos.remove(source_repo_id);
        for target in targets {
            let physical_id = if target.repo_id == source_repo_id {
                string_field(&source, "physical_repository_id").to_string()
            } else {
                format!(
                    "physical-managed-{}",
                    &canonical_sha256(&json!({"repo_id":target.repo_id,"operation":"split"}))[..16]
                )
            };
            repos.insert(
                target.repo_id.clone(),
                json!({
                    "repo_id": target.repo_id,
                    "repo_type": string_field(&source,"repo_type"),
                    "display_name": target.display_name,
                    "physical_repository_id": physical_id,
                    "course_codes": target.course_codes,
                    "lineage": {"kind":"split","source_repo_ids":[source_repo_id]}
                }),
            );
        }
        let generation = self.topology["generation"].as_i64().unwrap_or(0) + 1;
        after_topology["generation"] = json!(generation);
        after_routes["generation"] = json!(generation);
        let mut complete_values: BTreeSet<String> =
            array_at(&after_routes, "inventory_complete_repositories")?
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
        complete_values.remove(source_repo_id);
        complete_values.extend(targets.iter().map(|target| target.repo_id.clone()));
        after_routes["inventory_complete_repositories"] = json!(complete_values);
        if let Some(heads) = after_routes
            .get_mut("repository_heads")
            .and_then(Value::as_object_mut)
        {
            heads.remove(source_repo_id);
        }
        after_routes["unresolved_repository_heads"] = json!(targets
            .iter()
            .map(|target| target.repo_id.clone())
            .collect::<Vec<_>>());
        validate_state(&after_topology, &after_routes, true)?;
        Ok(operation_plan(
            "split",
            &self.topology,
            &self.routes,
            after_topology,
            after_routes,
            json!({
                "source_repo_id":source_repo_id,
                "targets":targets,
                "source_repository_heads":{
                    source_repo_id:self.routes.pointer(&format!("/repository_heads/{}",escape_pointer(source_repo_id))).cloned().unwrap_or(Value::Null)
                },
                "file_moves":moves,
                "routed_file_counts":routed_counts
            }),
        ))
    }

    fn build_merge_plan(
        &self,
        source_repo_ids: &[String],
        target_repo_id: &str,
        display_name: &str,
    ) -> Result<Value> {
        validate_state(&self.topology, &self.routes, false)?;
        let sources: BTreeSet<_> = source_repo_ids.iter().cloned().collect();
        if sources.len() < 2 {
            bail!("请至少选择两份资料。")
        }
        safe_repo_id(target_repo_id)?;
        let repos = repositories(&self.topology)?;
        for id in &sources {
            if !repos.contains_key(id) {
                bail!("有一份已选资料已经不存在，请重新选择。")
            }
        }
        if repos.contains_key(target_repo_id) && !sources.contains(target_repo_id) {
            bail!("目标资料已经存在，请换一个名称。")
        }
        let complete: HashSet<_> = array_at(&self.routes, "inventory_complete_repositories")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if sources.iter().any(|id| !complete.contains(id.as_str())) {
            bail!("所选资料中有尚未完成文件清点的项目，暂时不能合并。")
        }
        let mut after_routes = self.routes.clone();
        let route_files = after_routes
            .get_mut("files")
            .and_then(Value::as_array_mut)
            .context("路由文件损坏")?;
        let mut by_path = BTreeMap::<String, Vec<usize>>::new();
        for (index, file) in route_files.iter().enumerate() {
            if sources.contains(string_field(file, "repo_id")) {
                by_path
                    .entry(string_field(file, "path").to_lowercase())
                    .or_default()
                    .push(index);
            }
        }
        let preferred = if sources.contains(target_repo_id) {
            target_repo_id.to_string()
        } else {
            sources.iter().next().cloned().unwrap_or_default()
        };
        let mut occupied = HashSet::new();
        let mut moves = Vec::new();
        let relocations: Vec<Value> = Vec::new();
        for indices in by_path.values() {
            let mut sorted = indices.clone();
            sorted.sort_by_key(|index| string_field(&route_files[*index], "repo_id") != preferred);
            for index in sorted {
                let original_repo = string_field(&route_files[index], "repo_id").to_string();
                let original_path = string_field(&route_files[index], "path").to_string();
                let target_path = reserve_target_path(&original_path, &mut occupied)?;
                safe_path(&target_path)?;
                route_files[index]["repo_id"] = json!(target_repo_id);
                route_files[index]["path"] = json!(target_path);
                moves.push(json!({
                    "source_repo_id":original_repo,
                    "source_path":original_path,
                    "target_repo_id":target_repo_id,
                    "target_path":target_path
                }));
            }
        }
        let mut after_topology = self.topology.clone();
        let after_repos = after_topology
            .get_mut("repositories")
            .and_then(Value::as_object_mut)
            .context("资料索引损坏")?;
        let source_records: Vec<_> = sources
            .iter()
            .filter_map(|id| repos.get(id))
            .cloned()
            .collect();
        let mut course_codes = BTreeSet::new();
        for id in &sources {
            course_codes.extend(string_array(&repos[id], "course_codes"));
            after_repos.remove(id);
        }
        let preserved = repos.get(target_repo_id);
        let physical_id = preserved
            .map(|value| string_field(value, "physical_repository_id").to_string())
            .unwrap_or_else(|| {
                format!(
                    "physical-managed-{}",
                    &canonical_sha256(&json!({"repo_id":target_repo_id,"operation":"merge"}))[..16]
                )
            });
        let repo_types: BTreeSet<_> = source_records
            .iter()
            .map(|value| string_field(value, "repo_type").to_string())
            .collect();
        let repo_type = if repo_types.len() == 1 {
            repo_types
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "course".to_string())
        } else {
            "collection".to_string()
        };
        after_repos.insert(
            target_repo_id.to_string(),
            json!({
                "repo_id":target_repo_id,
                "repo_type":repo_type,
                "display_name":display_name,
                "physical_repository_id":physical_id,
                "course_codes":course_codes,
                "lineage":{"kind":"merge","source_repo_ids":sources}
            }),
        );
        let generation = self.topology["generation"].as_i64().unwrap_or(0) + 1;
        after_topology["generation"] = json!(generation);
        if let Some(course_routes) = after_routes
            .get_mut("course_code_routes")
            .and_then(Value::as_array_mut)
        {
            for route in course_routes.iter_mut() {
                if sources.contains(string_field(route, "repo_id")) {
                    route["repo_id"] = json!(target_repo_id);
                    route["physical_repository_id"] = json!(physical_id);
                }
            }
        }
        after_routes["generation"] = json!(generation);
        let mut complete_values: BTreeSet<String> =
            array_at(&after_routes, "inventory_complete_repositories")?
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
        for id in &sources {
            complete_values.remove(id);
        }
        complete_values.insert(target_repo_id.to_string());
        after_routes["inventory_complete_repositories"] = json!(complete_values);
        if let Some(heads) = after_routes
            .get_mut("repository_heads")
            .and_then(Value::as_object_mut)
        {
            for id in &sources {
                heads.remove(id);
            }
        }
        after_routes["unresolved_repository_heads"] = json!([target_repo_id]);
        validate_state(&after_topology, &after_routes, true)?;
        let source_heads = sources
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    self.routes
                        .pointer(&format!("/repository_heads/{}", escape_pointer(id)))
                        .cloned()
                        .unwrap_or(Value::Null),
                )
            })
            .collect::<Map<_, _>>();
        Ok(operation_plan(
            "merge",
            &self.topology,
            &self.routes,
            after_topology,
            after_routes,
            json!({
                "source_repo_ids":sources,
                "target_repo_id":target_repo_id,
                "source_repository_heads":source_heads,
                "file_moves":moves,
                "relocations":relocations
            }),
        ))
    }

    fn plan_phase(&self, plan: &Value) -> String {
        self.plan_phase_with_journal(plan, None)
    }

    fn plan_phase_with_journal(&self, plan: &Value, journal: Option<&Value>) -> String {
        let manifest = canonical_sha256(&self.manifest);
        let before_manifest = plan.pointer("/core/workspace_identity/manifest_sha256").and_then(Value::as_str);
        let after_manifest = plan.pointer("/after/manifest_sha256").and_then(Value::as_str);
        if Some(manifest.as_str()) != before_manifest && Some(manifest.as_str()) != after_manifest {
            return "drifted".to_string();
        }
        let topology = canonical_sha256(&self.topology);
        let routes = canonical_sha256(&self.routes);
        let before_topology = plan
            .pointer("/before/topology_sha256")
            .and_then(Value::as_str);
        let before_routes = plan
            .pointer("/before/routes_sha256")
            .and_then(Value::as_str);
        let after_topology = plan
            .pointer("/after/topology_sha256")
            .and_then(Value::as_str);
        let after_routes = journal
            .and_then(|value| value.get("resolved_after_routes_sha256"))
            .and_then(Value::as_str)
            .or_else(|| plan.pointer("/after/routes_sha256").and_then(Value::as_str));
        if Some(manifest.as_str()) == before_manifest && Some(topology.as_str()) == before_topology && Some(routes.as_str()) == before_routes {
            "before".to_string()
        } else if Some(topology.as_str()) == after_topology
            && Some(routes.as_str()) == before_routes
        {
            "topology-applied".to_string()
        } else if Some(manifest.as_str()) == after_manifest && Some(topology.as_str()) == after_topology && Some(routes.as_str()) == after_routes
        {
            "after".to_string()
        } else {
            "drifted".to_string()
        }
    }

    fn validate_remote_baseline(&self, plan: &Value, journal: Option<&Value>) -> Result<()> {
        let baseline = plan
            .pointer("/core/remote_baseline")
            .and_then(Value::as_object)
            .context("计划缺少远端检查信息")?;
        let expected_actor = plan
            .pointer("/core/github_actor")
            .and_then(Value::as_str)
            .context("计划缺少 GitHub 身份")?;
        if current_actor(&self.remote_template)? != expected_actor {
            bail!("当前 GitHub 登录账号与操作预览不一致。")
        }
        if self.remote_template == DEFAULT_REMOTE_TEMPLATE {
            let registry = baseline.get("registry").context("计划缺少 Registry 基线")?;
            let expected = registry.get("revision").context("计划缺少 Registry 基线")?;
            let repo_id = registry
                .get("repo_id")
                .and_then(Value::as_str)
                .context("计划缺少 Registry 名称")?;
            let organization = plan
                .pointer("/core/organization")
                .and_then(Value::as_str)
                .unwrap_or("");
            let current =
                remote_revision(&remote_url(&self.remote_template, organization, repo_id))?;
            if &current != expected {
                bail!("课程注册表已经变化，请重新开始这次操作。")
            }
        }
        let sources = baseline
            .get("sources")
            .and_then(Value::as_object)
            .context("计划缺少源资料检查信息")?;
        let target_records = journal
            .and_then(|value| value.pointer("/git/targets"))
            .and_then(Value::as_object);
        let known_commits: HashSet<_> = target_records
            .into_iter()
            .flat_map(|records| records.values())
            .filter_map(|record| record.get("commit").and_then(Value::as_str))
            .collect();
        for record in sources.values() {
            let remote = string_field(record, "remote_url");
            let current = remote_revision(remote)?;
            if current == *record {
                continue;
            }
            if current
                .get("head")
                .and_then(Value::as_str)
                .is_some_and(|head| known_commits.contains(head))
            {
                continue;
            }
            bail!("远端资料已经变化，请重新开始这次操作。")
        }
        let targets = baseline
            .get("targets")
            .and_then(Value::as_object)
            .context("计划缺少目标资料检查信息")?;
        for (repo_id, expected) in targets {
            let remote = string_field(expected, "remote_url");
            let current = remote_revision(remote)?;
            if current == *expected {
                continue;
            }
            let journal_commit = target_records
                .and_then(|records| records.get(repo_id))
                .and_then(|target| target.get("commit"))
                .and_then(Value::as_str);
            if current.get("head").and_then(Value::as_str) == journal_commit {
                continue;
            }
            let created_empty = !expected
                .get("exists")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && current.get("exists").and_then(Value::as_bool) == Some(true)
                && current.get("head").is_none_or(Value::is_null);
            if created_empty {
                continue;
            }
            bail!("目标资料已经变化，请重新开始这次操作。")
        }
        Ok(())
    }

    fn validate_journal(&self, journal: &Value) -> Result<Value> {
        let plan = journal.get("plan").cloned().context("任务记录缺少计划")?;
        validate_plan_identity(&plan)?;
        if journal.get("operation_id") != plan.get("operation_id")
            || journal.get("kind") != plan.get("kind")
        {
            bail!("任务记录与原计划不一致")
        }
        if !matches!(
            string_field(journal, "status"),
            "planned" | "applying" | "failed" | "completed"
        ) {
            bail!("任务记录状态无效")
        }
        if let Some(git) = journal.get("git") {
            let baseline = plan
                .pointer("/core/remote_baseline")
                .context("计划缺少远端基线")?;
            let sources = git
                .get("sources")
                .and_then(Value::as_object)
                .context("任务记录缺少源资料")?;
            let targets = git
                .get("targets")
                .and_then(Value::as_object)
                .context("任务记录缺少目标资料")?;
            let expected_sources = baseline
                .get("sources")
                .and_then(Value::as_object)
                .context("计划缺少源资料")?;
            let expected_targets = baseline
                .get("targets")
                .and_then(Value::as_object)
                .context("计划缺少目标资料")?;
            if sources.keys().collect::<BTreeSet<_>>() != expected_sources.keys().collect()
                || targets.keys().collect::<BTreeSet<_>>() != expected_targets.keys().collect()
            {
                bail!("任务记录中的资料集合已被修改")
            }
            for (repo_id, record) in sources {
                let expected = &expected_sources[repo_id];
                if string_field(record, "remote_url") != string_field(expected, "remote_url")
                    || string_field(record, "expected_head") != string_field(expected, "head")
                {
                    bail!("任务记录中的远端地址已被修改")
                }
            }
            for (repo_id, record) in targets {
                let expected = &expected_targets[repo_id];
                if string_field(record, "remote_url") != string_field(expected, "remote_url")
                    || record.get("expected_head") != expected.get("head")
                {
                    bail!("任务记录中的目标地址已被修改")
                }
            }
            if !matches!(string_field(git, "status"), "pending" | "completed") {
                bail!("任务记录中的 Git 状态无效")
            }
            for record in targets.values() {
                let status = string_field(record, "status");
                if !matches!(status, "pending" | "prepared" | "completed") {
                    bail!("任务记录中的目标状态无效")
                }
                let commit = record.get("commit").and_then(Value::as_str);
                if commit.is_some_and(|value| !is_hex(value, 40)) {
                    bail!("任务记录中的目标版本无效")
                }
                if matches!(status, "prepared" | "completed") && commit.is_none() {
                    bail!("任务记录中的目标版本缺失")
                }
            }
        }
        if let Some(resolved) = journal.get("resolved_after_routes") {
            let expected = journal
                .get("resolved_after_routes_sha256")
                .and_then(Value::as_str)
                .context("任务记录缺少最终路由校验")?;
            if canonical_sha256(resolved) != expected {
                bail!("任务记录中的最终路由已被修改")
            }
            validate_state(
                plan.pointer("/after/topology")
                    .context("计划缺少目标索引")?,
                resolved,
                false,
            )?;
        }
        Ok(plan)
    }

    fn ensure_target_repository(&self, repo_id: &str, remote: &str) -> Result<()> {
        let revision = remote_revision(remote)?;
        if revision.get("exists").and_then(Value::as_bool) == Some(true) {
            if revision.get("head").is_none_or(Value::is_null) {
                return Ok(());
            }
            bail!("目标资料名称已被占用，请重新选择名称。")
        }
        if self.remote_template == DEFAULT_REMOTE_TEMPLATE {
            let organization = string_field(&self.topology, "organization");
            let status = Command::new("gh")
                .args([
                    "repo",
                    "create",
                    &format!("{organization}/{repo_id}"),
                    "--public",
                    "--disable-wiki",
                    "--description",
                    "由薪火仓库管理工具创建",
                ])
                .status()
                .context("无法启动 GitHub 工具")?;
            if !status.success() {
                bail!("无法创建目标资料，请检查 GitHub 登录和权限。")
            }
        } else {
            let path = PathBuf::from(remote.strip_prefix("file://").unwrap_or(remote));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let parent = path.parent().unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .context("目标路径无效")?;
            run_git(parent, &["init", "--bare", name], None, &[])?;
        }
        let created = remote_revision(remote)?;
        if created.get("exists").and_then(Value::as_bool) != Some(true)
            || !created.get("head").is_none_or(Value::is_null)
        {
            bail!("新建目标资料不是空仓库")
        }
        Ok(())
    }

    fn resume_journal(&self, journal_path: &Path, journal: &mut Value) -> Result<()> {
        let plan = self.validate_journal(journal)?;
        self.validate_remote_baseline(&plan, Some(journal))?;
        let phase = self.plan_phase_with_journal(&plan, Some(journal));
        if phase == "drifted" {
            bail!("资料状态已变化，不能自动继续。")
        }
        journal["status"] = json!("applying");
        journal["error"] = Value::Null;
        journal["updated_at"] = json!(now());
        atomic_json(journal_path, journal)?;
        self.execute_git(journal_path, journal)?;
        let after_topology = plan
            .pointer("/after/topology")
            .cloned()
            .context("计划缺少目标索引")?;
        let effective_routes = journal
            .get("resolved_after_routes")
            .cloned()
            .unwrap_or_else(|| {
                plan.pointer("/after/routes")
                    .cloned()
                    .unwrap_or(Value::Null)
            });
        self.require_current_workspace()?;
        validate_state(&after_topology, &effective_routes, false)?;
        let after_manifest = plan.pointer("/after/manifest").context("计划缺少目标 manifest")?;
        atomic_json_many(&[(&self.manifest_path, after_manifest), (&self.topology_path, &after_topology), (&self.routes_path, &effective_routes)])?;
        add_stage(journal, "local-state");
        journal["status"] = json!("completed");
        journal["completed_at"] = json!(now());
        journal["updated_at"] = journal["completed_at"].clone();
        journal["error"] = Value::Null;
        atomic_json(journal_path, journal)?;
        Ok(())
    }

    fn execute_git(&self, journal_path: &Path, journal: &mut Value) -> Result<()> {
        let plan = journal.get("plan").cloned().context("任务记录缺少计划")?;
        let template = plan
            .pointer("/core/remote_baseline/remote_url_template")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_REMOTE_TEMPLATE);
        let organization = plan
            .pointer("/core/organization")
            .and_then(Value::as_str)
            .unwrap_or("");
        let source_heads = plan
            .pointer("/details/source_repository_heads")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let target_ids = plan
            .pointer("/after/routes/unresolved_repository_heads")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if journal.get("git").is_none() {
            let mut sources = Map::new();
            let mut targets = Map::new();
            for (repo_id, head) in &source_heads {
                let url = remote_url(template, organization, repo_id);
                let actual = remote_head(&url)?;
                if actual.as_deref() != head.as_str() {
                    bail!("远端资料已经变化，请重新开始这次操作。")
                }
                sources.insert(
                    repo_id.clone(),
                    json!({"remote_url":url,"expected_head":head}),
                );
            }
            for repo_id in target_ids.iter().filter_map(Value::as_str) {
                let url = remote_url(template, organization, repo_id);
                let actual = remote_head_allow_missing(&url)?;
                let expected = source_heads.get(repo_id).and_then(Value::as_str);
                if expected.is_some() && actual.as_deref() != expected {
                    bail!("目标资料已经变化，请重新开始这次操作。")
                }
                if expected.is_none() {
                    if actual.is_some() {
                        bail!("目标资料名称已被占用，请重新选择名称。")
                    }
                    self.ensure_target_repository(repo_id, &url)?;
                }
                targets.insert(
                    repo_id.to_string(),
                    json!({
                        "remote_url":url,
                        "expected_head":expected,
                        "status":"pending",
                        "commit":null
                    }),
                );
            }
            journal["git"] = json!({"status":"pending","sources":sources,"targets":targets});
            atomic_json(journal_path, journal)?;
        }
        if journal.pointer("/git/status").and_then(Value::as_str) == Some("completed") {
            self.resolve_routes(journal)?;
            return Ok(());
        }
        let temp = TempDir::new().context("无法创建临时 Git 工作区")?;
        let object_repo = temp.path();
        run_git(object_repo, &["init", "--bare"], None, &[])?;
        let source_records = journal
            .pointer("/git/sources")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut source_map = HashMap::new();
        for (repo_id, record) in &source_records {
            let remote = string_field(record, "remote_url");
            let expected = string_field(record, "expected_head");
            fetch_commit(
                object_repo,
                remote,
                expected,
                &format!("refs/source/{repo_id}"),
            )?;
            source_map.insert(repo_id.clone(), expected.to_string());
        }
        let moves = plan
            .pointer("/details/file_moves")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let target_keys = journal
            .pointer("/git/targets")
            .and_then(Value::as_object)
            .map(|value| value.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for target_id in target_keys {
            let record = journal
                .pointer(&format!("/git/targets/{}", escape_pointer(&target_id)))
                .cloned()
                .context("任务记录损坏")?;
            let remote = string_field(&record, "remote_url").to_string();
            let status = string_field(&record, "status").to_string();
            if status == "completed" {
                let expected = string_field(&record, "commit");
                if remote_head(&remote)?.as_deref() != Some(expected) {
                    bail!("已完成的远端资料发生变化，请停止操作。")
                }
                continue;
            }
            let expected_parent = record
                .get("expected_head")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(parent) = &expected_parent {
                fetch_commit(
                    object_repo,
                    &remote,
                    parent,
                    &format!("refs/target/{target_id}"),
                )?;
            }
            let target_moves: Vec<_> = moves
                .iter()
                .filter(|value| string_field(value, "target_repo_id") == target_id)
                .cloned()
                .collect();
            let commit = build_target_commit(
                object_repo,
                string_field(&plan, "operation_id"),
                string_field(&plan, "created_at"),
                &target_id,
                expected_parent.as_deref(),
                &target_moves,
                &source_map,
            )?;
            journal["git"]["targets"][&target_id]["commit"] = json!(commit);
            journal["git"]["targets"][&target_id]["status"] = json!("prepared");
            atomic_json(journal_path, journal)?;
            run_git(
                object_repo,
                &[
                    "push",
                    "--porcelain",
                    &remote,
                    &format!("{commit}:refs/heads/main"),
                ],
                None,
                &[],
            )?;
            if remote_head(&remote)?.as_deref() != Some(commit.as_str()) {
                bail!("远端资料写入后校验失败。")
            }
            journal["git"]["targets"][&target_id]["status"] = json!("completed");
            journal["git"]["targets"][&target_id]["completed_at"] = json!(now());
            atomic_json(journal_path, journal)?;
        }
        journal["git"]["status"] = json!("completed");
        journal["git"]["completed_at"] = json!(now());
        self.resolve_routes(journal)?;
        atomic_json(journal_path, journal)?;
        add_stage(journal, "git");
        Ok(())
    }

    fn resolve_routes(&self, journal: &mut Value) -> Result<()> {
        let mut routes = journal
            .pointer("/plan/after/routes")
            .cloned()
            .context("计划缺少目标路由")?;
        let targets = journal
            .pointer("/git/targets")
            .and_then(Value::as_object)
            .context("任务记录缺少 Git 目标")?;
        let unresolved: BTreeSet<_> = string_array(&routes, "unresolved_repository_heads")
            .into_iter()
            .collect();
        if unresolved != targets.keys().cloned().collect() {
            bail!("目标资料集合不一致。")
        }
        let heads = routes
            .get_mut("repository_heads")
            .and_then(Value::as_object_mut)
            .context("目标路由缺少 HEAD")?;
        for (repo_id, record) in targets {
            if string_field(record, "status") != "completed" {
                bail!("还有远端资料没有完成。")
            }
            heads.insert(repo_id.clone(), json!(string_field(record, "commit")));
        }
        routes
            .as_object_mut()
            .unwrap()
            .remove("unresolved_repository_heads");
        let topology = journal
            .pointer("/plan/after/topology")
            .context("计划缺少目标索引")?;
        validate_state(topology, &routes, false)?;
        journal["resolved_after_routes_sha256"] = json!(canonical_sha256(&routes));
        journal["resolved_after_routes"] = routes;
        Ok(())
    }

    fn rebuild_from_snapshot(
        &self,
        snapshot: &crate::jwts::CandidateSnapshot,
        assignments: &BTreeMap<String, CourseAssignment>,
    ) -> Result<RepositorySyncPreview> {
        crate::curriculum::validate_snapshot(snapshot)?;
        let old_descriptors = array_at(&self.manifest, "course_descriptors")?;
        let old_descriptor_by_code = old_descriptors
            .iter()
            .filter_map(|value| {
                value
                    .get("course_code")
                    .and_then(Value::as_str)
                    .map(|code| (normalize(code), value.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let old_records = array_at(&self.manifest, "curriculum_records")?.to_vec();
        let mut old_records_by_plan = BTreeMap::<String, Vec<Value>>::new();
        for record in old_records {
            old_records_by_plan
                .entry(string_field(&record, "source_plan").to_string())
                .or_default()
                .push(record);
        }
        let mut manifest = self.manifest.clone();
        let mut topology = self.topology.clone();
        let mut routes = self.routes.clone();
        let mut bindings = BTreeMap::<String, Value>::new();
        let mut names = BTreeMap::<String, String>::new();
        let mut current_codes = BTreeSet::new();
        for plan in &snapshot.plans {
            for course in &plan.courses {
                let code = normalized_value(course.get("course_code"));
                if code.is_empty() {
                    continue;
                }
                current_codes.insert(code.clone());
                let name = names.entry(code.clone()).or_default();
                if name.is_empty() {
                    *name = normalized_value(course.get("course_name"));
                }
                if bindings.contains_key(&code) {
                    continue;
                }
                let binding = if let Some(old) = old_descriptor_by_code.get(&code) {
                    json!({
                        "physical_repository_id":string_field(old,"physical_repository_id"),
                        "repo_id":string_field(old,"repo_id")
                    })
                } else {
                    let assignment = assignments
                        .get(&code)
                        .context("新增课程代码缺少明确仓库归属")?;
                    self.binding_from_assignment(&code, &names[&code], assignment)?
                };
                bindings.insert(code, binding);
            }
        }
        let unknown_assignments = assignments
            .keys()
            .filter(|code| {
                !current_codes.contains(*code) || old_descriptor_by_code.contains_key(*code)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !unknown_assignments.is_empty() {
            bail!(
                "课程归属只能指定当前已接受的新增代码：{}",
                unknown_assignments.join("、")
            )
        }

        let mut plans = Vec::new();
        let mut records = Vec::new();
        let mut records_by_plan = BTreeMap::<String, Vec<String>>::new();
        let mut descriptor_records = BTreeMap::<String, Vec<String>>::new();
        let mut pending_uncoded = Vec::new();
        for plan in &snapshot.plans {
            let info = normalize_plan_info(&plan.plan_id, &plan.info);
            plans.push(info.clone());
            let old_plan_records = old_records_by_plan
                .get(&plan.plan_id)
                .cloned()
                .unwrap_or_default();
            let aligned_ids =
                crate::curriculum::aligned_record_pairs(&old_plan_records, &plan.courses)
                    .into_iter()
                    .filter_map(|(before, after)| {
                        Some((
                            after?,
                            string_field(&old_plan_records[before?], "record_id").to_string(),
                        ))
                    })
                    .filter(|(_, id)| !id.is_empty())
                    .collect::<BTreeMap<_, _>>();
            let mut used_old_ids = BTreeSet::new();
            let mut semantic_counts = BTreeMap::<String, usize>::new();
            let mut plan_record_ids = Vec::new();
            for (ordinal, course) in plan.courses.iter().enumerate() {
                let semantic = course_semantic_identity(course);
                let occurrence = semantic_counts.entry(semantic.clone()).or_default();
                let record_id = aligned_ids
                    .get(&ordinal)
                    .cloned()
                    .unwrap_or_else(|| stable_record_id(&plan.plan_id, &semantic, *occurrence));
                *occurrence += 1;
                if !used_old_ids.insert(record_id.clone()) {
                    bail!("课程记录无法一一匹配，生成了重复身份：{}", record_id)
                }
                let code = normalized_value(course.get("course_code"));
                let name = normalized_value(course.get("course_name"));
                let mut record = course.clone();
                if !record.is_object() {
                    record = json!({"course_name":name});
                }
                let object = record.as_object_mut().context("课程记录不是对象")?;
                object.insert("record_id".to_string(), json!(record_id));
                object.insert("source_plan".to_string(), json!(plan.plan_id));
                object.insert(
                    "source_plan_file".to_string(),
                    info.get("source_plan_file").cloned().unwrap_or(Value::Null),
                );
                object.insert("source_ordinal".to_string(), json!(ordinal));
                object.insert(
                    "metadata_repo_id".to_string(),
                    json!("fireworks-course-registry-v2"),
                );
                object.insert(
                    "metadata_path".to_string(),
                    json!(format!("curriculum/records/{record_id}.json")),
                );
                for key in [
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
                    if let Some(value) = info.get(key) {
                        object.insert(key.to_string(), value.clone());
                    }
                }
                if code.is_empty() {
                    object.insert("status".to_string(), json!("pending-course-code"));
                    object.insert("identity_status".to_string(), json!("uncoded"));
                    pending_uncoded.push(record_id.clone());
                } else {
                    let binding = bindings.get(&code).context("课程缺少仓库归属")?;
                    object.insert("repo_id".to_string(), binding["repo_id"].clone());
                    object.insert("repo_type".to_string(), json!("course"));
                    object.insert(
                        "physical_repository_id".to_string(),
                        binding["physical_repository_id"].clone(),
                    );
                    object.insert(
                        "descriptor_id".to_string(),
                        json!(format!("course-code:{code}")),
                    );
                    object.insert("attachment_repo_id".to_string(), binding["repo_id"].clone());
                    object.insert("status".to_string(), json!("mapped"));
                    object.insert("identity_status".to_string(), json!("coded"));
                    descriptor_records
                        .entry(code)
                        .or_default()
                        .push(record_id.clone());
                }
                plan_record_ids.push(record_id);
                records.push(record);
            }
            records_by_plan.insert(plan.plan_id.clone(), plan_record_ids);
        }
        plans.sort_by_key(|value| string_field(value, "plan_id").to_string());
        records.sort_by_key(|value| {
            (
                string_field(value, "source_plan").to_string(),
                value
                    .get("source_ordinal")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
            )
        });

        let mut descriptors = Vec::new();
        let all_codes = old_descriptor_by_code
            .keys()
            .chain(current_codes.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        for code in &all_codes {
            let old = old_descriptor_by_code.get(code);
            let binding = bindings.get(code).or_else(|| old);
            let mut descriptor = old.cloned().unwrap_or_else(|| json!({}));
            let object = descriptor.as_object_mut().context("课程描述不是对象")?;
            object.insert(
                "descriptor_id".to_string(),
                json!(format!("course-code:{code}")),
            );
            object.insert("course_code".to_string(), json!(code));
            if let Some(name) = names.get(code).filter(|name| !name.is_empty()) {
                object.insert("course_name".to_string(), json!(name));
            }
            if let Some(binding) = binding {
                for key in ["physical_repository_id", "repo_id"] {
                    object.insert(
                        key.to_string(),
                        binding.get(key).cloned().unwrap_or(Value::Null),
                    );
                }
                object.insert(
                    "attachment_repo_id".to_string(),
                    binding.get("repo_id").cloned().unwrap_or(Value::Null),
                );
            }
            if current_codes.contains(code) {
                object.insert(
                    "record_ids".to_string(),
                    json!(descriptor_records.get(code).cloned().unwrap_or_default()),
                );
                object.insert("status".to_string(), json!("mapped-current"));
            } else {
                let historical_ids = object
                    .get("record_ids")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                object.insert("historical_record_ids".to_string(), historical_ids);
                object.insert("record_ids".to_string(), json!([]));
                object.insert("status".to_string(), json!("historical-not-current"));
            }
            object.insert(
                "metadata_repo_id".to_string(),
                json!("fireworks-course-registry-v2"),
            );
            object.insert(
                "metadata_path".to_string(),
                json!(format!(
                    "curriculum/descriptors/{}.json",
                    encode_path_component(code)
                )),
            );
            descriptors.push(descriptor);
        }

        let object = manifest.as_object_mut().context("manifest 不是对象")?;
        object.insert("curriculum_plans".to_string(), json!(plans));
        object.insert("curriculum_records".to_string(), json!(records));
        object.insert("course_descriptors".to_string(), json!(descriptors));
        let indexes = object
            .entry("curriculum_metadata_indexes")
            .or_insert_with(|| json!({}));
        indexes["by_plan"] = json!(records_by_plan);
        indexes["pending_course_code"] = json!(pending_uncoded);
        self.update_course_membership(
            &mut manifest,
            &mut topology,
            &bindings,
            &names,
            assignments,
        )?;

        let mut route_index = array_at(&routes, "course_code_routes")
            .unwrap_or(&[])
            .iter()
            .filter_map(|route| {
                let code = normalized_value(route.get("course_code"));
                (!code.is_empty()).then_some((code, route.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for code in &all_codes {
            let binding = bindings
                .get(code)
                .or_else(|| old_descriptor_by_code.get(code))
                .context("课程路由缺少历史绑定")?;
            let mut route = route_index.remove(code).unwrap_or_else(|| {
                json!({
                    "course_code":code,
                    "has_material":false
                })
            });
            route["repo_id"] = binding.get("repo_id").cloned().unwrap_or(Value::Null);
            route["physical_repository_id"] = binding
                .get("physical_repository_id")
                .cloned()
                .unwrap_or(Value::Null);
            route["status"] = json!(if current_codes.contains(code) {
                "current"
            } else {
                "historical-not-current"
            });
            route_index.insert(code.clone(), route);
        }
        routes["course_code_routes"] = json!(route_index.into_values().collect::<Vec<_>>());
        refresh_curriculum_metadata(&mut manifest)?;
        validate_state(&topology, &routes, false)?;
        let old_codes = old_descriptor_by_code
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let added = current_codes
            .difference(&old_codes)
            .cloned()
            .collect::<Vec<_>>();
        let removed = old_codes
            .difference(&current_codes)
            .cloned()
            .collect::<Vec<_>>();
        let create_repositories = assignments
            .iter()
            .filter_map(|(code, assignment)| {
                matches!(assignment, CourseAssignment::New { .. } | CourseAssignment::NewGroup { .. })
                    .then(|| bindings[code]["repo_id"].as_str().unwrap_or("").to_string())
            })
            .collect::<BTreeSet<_>>();
        let metadata_repositories = bindings
            .values()
            .filter_map(|binding| binding.get("repo_id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        Ok(RepositorySyncPreview {
            manifest,
            topology,
            routes,
            baseline: self.workspace_identity(),
            identity_sha256: String::new(),
            create_repositories: create_repositories.iter().cloned().collect(),
            archive_repositories: Vec::new(),
            metadata_repositories: metadata_repositories.into_iter().collect(),
            plan_count: snapshot.plans.len(),
            record_count: records_by_plan.values().map(Vec::len).sum(),
            descriptor_count: all_codes.len(),
            new_course_code_count: added.len(),
            removed_course_code_count: removed.len(),
            summary_lines: vec![
                format!("培养计划：{} 个", snapshot.plans.len()),
                format!(
                    "课程记录：{} 条",
                    records_by_plan.values().map(Vec::len).sum::<usize>()
                ),
                format!("新增课程代码：{} 个", added.len()),
                format!("转为历史课程代码：{} 个", removed.len()),
                format!("需要创建仓库：{} 个", create_repositories.len()),
            ],
        })
    }

    pub fn apply_repository_sync_preview(&mut self, preview: &RepositorySyncPreview) -> Result<()> {
        self.require_current_workspace()?;
        validate_repository_preview(preview)?;
        let current = self.workspace_identity();
        let target = json!({
            "manifest_sha256":canonical_sha256(&preview.manifest),
            "topology_sha256":canonical_sha256(&preview.topology),
            "routes_sha256":canonical_sha256(&preview.routes)
        });
        if current == target {
            self.reload()?;
            self.complete_matching_curriculum_reviews()?;
            return Ok(());
        }
        if preview.baseline != current {
            bail!("本地三份管理数据已变化，请重新生成预览")
        }
        atomic_json_many(&[
            (&self.manifest_path, &preview.manifest),
            (&self.topology_path, &preview.topology),
            (&self.routes_path, &preview.routes),
        ])?;
        self.reload()?;
        self.complete_matching_curriculum_reviews()?;
        Ok(())
    }

    fn complete_matching_curriculum_reviews(&self) -> Result<()> {
        let directory = self.operations_path.join(CURRICULUM_REVIEW_DIRECTORY);
        if !directory.is_dir() {
            return Ok(());
        }
        let history = self
            .manifest
            .get("curriculum_history")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for path in sorted_json_files(&directory)? {
            let mut payload = read_json(&path)?;
            validate_review_payload(&payload)?;
            if payload.get("completed").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let diff = payload.get("diff").context("审阅文件缺少差异")?;
            let decisions = payload.get("decisions").context("审阅文件缺少裁决")?;
            let assignments = payload
                .get("assignments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if history.iter().any(|entry| {
                entry.get("diff_identity_sha256") == diff.get("diff_identity_sha256")
                    && entry.get("decisions") == Some(decisions)
                    && entry.get("assignments") == Some(&assignments)
            }) {
                payload["completed"] = json!(true);
                payload["completed_at"] = json!(now());
                payload.as_object_mut().unwrap().remove("content_sha256");
                payload["content_sha256"] = json!(canonical_sha256(&payload));
                atomic_json(&path, &payload)?;
            }
        }
        Ok(())
    }

    fn binding_from_assignment(
        &self,
        code: &str,
        _name: &str,
        assignment: &CourseAssignment,
    ) -> Result<Value> {
        match assignment {
            CourseAssignment::Existing { repo_id } => {
                if !self.valid_assignment_repository(repo_id) {
                    bail!("所选仓库不是可承载资料的活跃课程仓")
                }
                let repo = repositories(&self.topology)?.get(repo_id).context("所选仓库不存在")?;
                Ok(json!({"physical_repository_id":string_field(repo,"physical_repository_id"),"repo_id":repo_id}))
            }
            CourseAssignment::New { title } | CourseAssignment::NewGroup { title, .. } => {
                let title = normalize(title);
                if title.is_empty() {
                    bail!("新课程资料仓标题不能为空")
                }
                let repo_id = match assignment {
                    CourseAssignment::NewGroup { repo_id, .. } => {
                        safe_repo_id(repo_id)?;
                        repo_id.clone()
                    }
                    _ => stable_course_repo_id(code, &title),
                };
                if repositories(&self.topology)?.contains_key(&repo_id) || self.manifest_repository(&repo_id).is_some() {
                    bail!("新资料库身份已存在，请选择现有资料库")
                }
                Ok(json!({"physical_repository_id":format!("physical-managed-{}",&canonical_sha256(&json!({"repo_id":repo_id}))[..16]),"repo_id":repo_id,"display_name":title}))
            }
        }
    }

    fn valid_assignment_repository(&self, repo_id: &str) -> bool {
        let Some(topology) = repositories(&self.topology)
            .ok()
            .and_then(|repos| repos.get(repo_id))
        else {
            return false;
        };
        let kind = string_field(topology, "repo_type");
        if !matches!(kind, "course" | "shared") {
            return false;
        }
        let manifest = self.manifest_repository(repo_id);
        let archived = manifest
            .and_then(|value| value.get("archived"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || topology
                .get("archived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let template = manifest
            .and_then(|value| value.get("template"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || topology
                .get("template")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let lifecycle = manifest
            .map(|value| string_field(value, "lifecycle_status"))
            .unwrap_or("");
        !archived && !template && !matches!(lifecycle, "archived" | "retired" | "disabled")
    }

    fn update_course_membership(
        &self,
        manifest: &mut Value,
        topology: &mut Value,
        bindings: &BTreeMap<String, Value>,
        _names: &BTreeMap<String, String>,
        assignments: &BTreeMap<String, CourseAssignment>,
    ) -> Result<()> {
        let object = manifest.as_object_mut().context("manifest 格式无效")?;
        let manifest_repositories = object
            .get_mut("repositories")
            .and_then(Value::as_array_mut)
            .context("manifest 缺少仓库")?;
        let topology_repositories = topology
            .get_mut("repositories")
            .and_then(Value::as_object_mut)
            .context("topology 缺少 repositories")?;
        for (code, assignment) in assignments {
            let binding = &bindings[code];
            let repo_id = string_field(binding, "repo_id").to_string();
            let title = match assignment {
                CourseAssignment::New { title } | CourseAssignment::NewGroup { title, .. } => normalize(title),
                CourseAssignment::Existing { .. } => String::new(),
            };
            if let Some(repo) = manifest_repositories
                .iter_mut()
                .find(|repo| string_field(repo, "repo_id") == repo_id)
            {
                insert_unique_string(repo, "course_codes", code);
            } else {
                manifest_repositories.push(json!({"repo_id":repo_id,"repo_type":"course","display_name":title,"physical_repository_id":binding["physical_repository_id"],"course_codes":[code],"lineage":{"kind":"curriculum-explicit-assignment","source_repo_ids":[]}}));
            }
            if let Some(repo) = topology_repositories.get_mut(&repo_id) {
                insert_unique_string(repo, "course_codes", code);
            } else {
                topology_repositories.insert(repo_id.clone(), json!({"repo_id":repo_id,"repo_type":"course","display_name":title,"physical_repository_id":binding["physical_repository_id"],"course_codes":[code],"lineage":{"kind":"curriculum-explicit-assignment","source_repo_ids":[]}}));
            }
        }
        manifest_repositories.sort_by_key(|value| string_field(value, "repo_id").to_string());
        Ok(())
    }
}

fn reviewed_snapshot(session: &UpdateSession) -> Result<crate::jwts::CandidateSnapshot> {
    let diff = session.diff.as_ref().context("尚未生成教务差异")?;
    let snapshot = crate::curriculum::materialize(diff, &session.decisions)?;
    crate::curriculum::validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn validate_crawl_selections(
    session: &UpdateSession,
    selections: &[crate::jwts::CrawlSelection],
) -> Result<()> {
    if selections.is_empty() {
        bail!("请至少选择一个专业")
    }
    let selected_ids = selections
        .iter()
        .map(crate::jwts::CrawlSelection::plan_id)
        .collect::<BTreeSet<_>>();
    if selected_ids.len() != selections.len() {
        bail!("同一计划被重复选择")
    }
    if selections
        .iter()
        .any(|selection| selection.kind != session.kind)
    {
        bail!("选择的培养来源与当前任务不一致")
    }
    Ok(())
}

fn validate_snapshot_source(captured: &str, expected: &str) -> Result<()> {
    fn normalized_service_url(value: &str) -> Result<String> {
        let mut url = url::Url::parse(value).context("教务来源地址无效")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("教务来源地址必须是不含认证信息、查询参数或片段的 HTTP(S) 地址")
        }
        if url.path().is_empty() || url.path() == "/" {
            url.set_path("/");
        }
        Ok(url.to_string())
    }

    if normalized_service_url(captured)? != normalized_service_url(expected)? {
        bail!("候选快照来源与当前审阅任务不匹配")
    }
    Ok(())
}

fn plan_matches_selection(info: &Value, selection: &crate::jwts::CrawlSelection) -> bool {
    let source = string_field(info, "source_kind");
    let expected_source = match selection.kind {
        crate::jwts::PlanKind::Curriculum => "curriculum",
        crate::jwts::PlanKind::Execution => "execution",
    };
    let source_matches = if source.is_empty() {
        selection.kind == crate::jwts::PlanKind::Curriculum
            && !string_field(info, "plan_id").starts_with("hit:execution:")
    } else {
        source == expected_source
    };
    if !source_matches
        || normalize(string_field(info, "department_code")) != normalize(&selection.college_code)
        || normalize(string_field(info, "major_code")) != normalize(&selection.major_code)
    {
        return false;
    }
    match selection.kind {
        crate::jwts::PlanKind::Curriculum => {
            normalize(string_field(info, "plan_version")) == normalize(&selection.grade)
        }
        crate::jwts::PlanKind::Execution => {
            normalize(string_field(info, "entry_cohort")) == normalize(&selection.grade)
        }
    }
}

fn validate_captured_plan(
    plan: &crate::jwts::CandidatePlan,
    selection: &crate::jwts::CrawlSelection,
) -> Result<()> {
    if plan.plan_id != selection.plan_id() {
        bail!("教务返回的计划身份与所选范围不一致")
    }
    if !plan_matches_selection(&plan.info, selection) {
        bail!("教务返回的计划来源或专业范围不一致")
    }
    let capture = plan
        .info
        .get("source_capture")
        .and_then(Value::as_object)
        .context("教务结果缺少完整性证明")?;
    if capture.get("complete").and_then(Value::as_bool) != Some(true) {
        bail!("教务查询不完整，未写入审阅任务")
    }
    let expected_endpoint = match selection.kind {
        crate::jwts::PlanKind::Curriculum => "/pyfa/queryPykc",
        crate::jwts::PlanKind::Execution => "/zxjh/queryZxkc",
    };
    if capture.get("endpoint").and_then(Value::as_str) != Some(expected_endpoint) {
        bail!("教务采集证据的查询入口与计划来源不一致")
    }
    let scope = capture
        .get("scope")
        .and_then(Value::as_object)
        .context("教务采集证据缺少查询范围")?;
    let grade_key = match selection.kind {
        crate::jwts::PlanKind::Curriculum => "pageBbh",
        crate::jwts::PlanKind::Execution => "pageNj",
    };
    for (key, expected) in [
        (grade_key, selection.grade.as_str()),
        ("pageYxdm", selection.college_code.as_str()),
        ("pageZydm", selection.major_code.as_str()),
    ] {
        if scope.get(key).and_then(Value::as_str) != Some(expected) {
            bail!("教务采集证据的查询范围与所选专业不一致：{key}")
        }
    }
    let checks = capture
        .get("checks")
        .and_then(Value::as_object)
        .context("教务采集证据缺少完整性检查")?;
    for key in ["authenticated", "expected_headers", "pagination_consistent"] {
        if checks.get(key).and_then(Value::as_bool) != Some(true) {
            bail!("教务采集证据不足：{key}")
        }
    }
    let pages = capture
        .get("pages")
        .and_then(Value::as_array)
        .filter(|pages| !pages.is_empty())
        .context("教务采集证据缺少分页记录")?;
    let paged_rows = pages.iter().try_fold(0_u64, |total, value| {
        value
            .as_u64()
            .and_then(|rows| total.checked_add(rows))
            .context("教务采集证据的分页记录无效")
    })?;
    let rows = capture
        .get("rows")
        .and_then(Value::as_u64)
        .context("教务采集证据缺少课程行数")?;
    let unpaged_rows = capture
        .get("unpaged_rows")
        .and_then(Value::as_u64)
        .context("教务采集证据缺少非分页课程行数")?;
    if paged_rows.checked_add(unpaged_rows) != Some(rows) {
        bail!("教务采集证据的分页数量不一致")
    }
    crate::curriculum::validate_snapshot(&crate::jwts::CandidateSnapshot {
        generated_at: "capture-validation".to_string(),
        base_url: String::new(),
        plans: vec![plan.clone()],
    })?;
    Ok(())
}

fn course_semantic_identity(course: &Value) -> String {
    canonical_sha256(&crate::curriculum::canonical_course(course))
}

fn stable_record_id(plan_id: &str, semantic: &str, occurrence: usize) -> String {
    let hash =
        canonical_sha256(&json!({"plan_id":plan_id,"semantic":semantic,"occurrence":occurrence}));
    format!("REC-{}", &hash[..20].to_uppercase())
}

fn stable_course_repo_id(code: &str, title: &str) -> String {
    let hash = canonical_sha256(&json!({"course_code":normalize(code),"title":normalize(title)}));
    format!("COURSE-{}", &hash[..16].to_uppercase())
}

fn insert_unique_string(value: &mut Value, key: &str, item: &str) {
    let object = value.as_object_mut().expect("repository object");
    let array = object
        .entry(key.to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("repository member list");
    if !array.iter().any(|value| value.as_str() == Some(item)) {
        array.push(json!(item));
    }
    array.sort_by_key(|value| value.as_str().unwrap_or("").to_string());
}

fn append_curriculum_history(
    manifest: &mut Value,
    diff: &crate::curriculum::CurriculumDiff,
    decisions: &crate::curriculum::DecisionSet,
    assignments: &BTreeMap<String, CourseAssignment>,
    result: &crate::jwts::CandidateSnapshot,
) -> Result<()> {
    crate::curriculum::validate_diff(diff)?;
    let baseline = serde_json::to_value(&diff.current)?;
    let candidate = serde_json::to_value(&diff.candidate)?;
    let result_value = serde_json::to_value(result)?;
    let decision_value = serde_json::to_value(decisions)?;
    let assignment_value = serde_json::to_value(assignments)?;
    let history_id = canonical_sha256(
        &json!({"diff":diff.diff_identity_sha256,"decisions":decision_value,"assignments":assignment_value,"result":result_value}),
    );
    let entry = json!({
        "history_id":history_id,
        "created_at":now(),
        "baseline_sha256":canonical_sha256(&baseline),
        "candidate_sha256":canonical_sha256(&candidate),
        "decision_sha256":canonical_sha256(&decision_value),
        "result_sha256":canonical_sha256(&result_value),
        "assignment_sha256":canonical_sha256(&assignment_value),
        "diff_identity_sha256":diff.diff_identity_sha256,
        "baseline":baseline,
        "candidate":candidate,
        "decisions":decision_value,
        "assignments":assignment_value,
        "result":result_value
    });
    let object = manifest.as_object_mut().context("manifest 不是对象")?;
    let history = object
        .entry("curriculum_history")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("curriculum_history 不是数组")?;
    if history
        .iter()
        .all(|old| old.get("history_id") != entry.get("history_id"))
    {
        history.push(entry);
    }
    Ok(())
}

// 字段按原 serde_json::Value 的字典序输出，保留冻结身份但不复制大数据树。
struct RepositoryPreviewJson<'a> {
    preview: &'a RepositorySyncPreview,
    include_identity: bool,
}

impl Serialize for RepositoryPreviewJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let p = self.preview;
        let mut map = serializer.serialize_map(Some(if self.include_identity { 14 } else { 13 }))?;
        map.serialize_entry("archive_repositories", &p.archive_repositories)?;
        map.serialize_entry("baseline", &p.baseline)?;
        map.serialize_entry("create_repositories", &p.create_repositories)?;
        map.serialize_entry("descriptor_count", &p.descriptor_count)?;
        if self.include_identity { map.serialize_entry("identity_sha256", &p.identity_sha256)?; }
        map.serialize_entry("manifest", &p.manifest)?;
        map.serialize_entry("metadata_repositories", &p.metadata_repositories)?;
        map.serialize_entry("new_course_code_count", &p.new_course_code_count)?;
        map.serialize_entry("plan_count", &p.plan_count)?;
        map.serialize_entry("record_count", &p.record_count)?;
        map.serialize_entry("removed_course_code_count", &p.removed_course_code_count)?;
        map.serialize_entry("routes", &p.routes)?;
        map.serialize_entry("summary_lines", &p.summary_lines)?;
        map.serialize_entry("topology", &p.topology)?;
        map.end()
    }
}

struct LifecycleActionsJson<'a>(&'a [RepositoryLifecycleAction]);

impl Serialize for LifecycleActionsJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Action<'a> {
            archived: bool,
            baseline: &'a Value,
            default_branch: &'a str,
            description: &'a str,
            kind: &'a RepositoryLifecycleKind,
            private: bool,
            readme: &'a Option<String>,
            repo_id: &'a str,
            template: bool,
            template_repository: &'a Option<String>,
            title: &'a str,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for action in self.0 {
            sequence.serialize_element(&Action {
                archived: action.archived, baseline: &action.baseline,
                default_branch: &action.default_branch, description: &action.description,
                kind: &action.kind, private: action.private, readme: &action.readme,
                repo_id: &action.repo_id, template: action.template,
                template_repository: &action.template_repository, title: &action.title,
            })?;
        }
        sequence.end()
    }
}

struct LifecyclePreviewJson<'a> {
    preview: &'a RepositoryLifecyclePreview,
    include_identity: bool,
}

impl Serialize for LifecyclePreviewJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Registry<'a> {
            baseline: &'a Value,
            files: &'a BTreeMap<String, Value>,
            identity_sha256: &'a str,
            remote_url: &'a str,
        }
        let p = self.preview;
        let mut map = serializer.serialize_map(Some(if self.include_identity { 5 } else { 4 }))?;
        map.serialize_entry("actions", &LifecycleActionsJson(&p.actions))?;
        if self.include_identity { map.serialize_entry("identity_sha256", &p.identity_sha256)?; }
        map.serialize_entry("organization", &p.organization)?;
        map.serialize_entry("registry", &Registry {
            baseline: &p.registry.baseline, files: &p.registry.files,
            identity_sha256: &p.registry.identity_sha256, remote_url: &p.registry.remote_url,
        })?;
        map.serialize_entry("summary_lines", &p.summary_lines)?;
        map.end()
    }
}

fn preview_bundle_sha256(state: &RepositorySyncPreview, remote: &RepositoryLifecyclePreview, identity: &str) -> Result<String> {
    #[derive(Serialize)]
    struct Bundle<'a> {
        identity_sha256: &'a str,
        remote: LifecyclePreviewJson<'a>,
        state: RepositoryPreviewJson<'a>,
    }
    crate::curriculum::serialized_sha256(&Bundle {
        identity_sha256: identity,
        remote: LifecyclePreviewJson { preview: remote, include_identity: true },
        state: RepositoryPreviewJson { preview: state, include_identity: true },
    })
}

fn repository_preview_identity(preview: &RepositorySyncPreview) -> Result<String> {
    crate::curriculum::serialized_sha256(&RepositoryPreviewJson { preview, include_identity: false })
}

fn finalize_repository_preview(
    preview: &mut RepositorySyncPreview,
    baseline: &Value,
) -> Result<()> {
    preview.baseline = baseline.clone();
    preview.identity_sha256.clear();
    preview.identity_sha256 = repository_preview_identity(preview)?;
    Ok(())
}

fn validate_repository_preview(preview: &RepositorySyncPreview) -> Result<()> {
    if preview.identity_sha256.is_empty()
        || repository_preview_identity(preview)? != preview.identity_sha256
    {
        bail!("本地状态预览已被修改")
    }
    validate_state(&preview.topology, &preview.routes, false)?;
    let manifest_repositories = array_at(&preview.manifest, "repositories")?
        .iter()
        .filter_map(|value| value.get("repo_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let topology_repositories = repositories(&preview.topology)?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if manifest_repositories != topology_repositories {
        bail!("manifest 与 topology 的仓库成员不一致")
    }
    for descriptor in array_at(&preview.manifest, "course_descriptors")? {
        let repo_id = string_field(descriptor, "repo_id");
        if repo_id.is_empty() || !topology_repositories.contains(repo_id) {
            bail!("课程描述引用不存在的仓库")
        }
    }
    Ok(())
}

fn validate_review_payload(payload: &Value) -> Result<()> {
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        bail!("审阅文件版本不受支持")
    }
    let expected = payload
        .get("content_sha256")
        .and_then(Value::as_str)
        .context("审阅文件缺少内容校验")?;
    let mut unsigned = payload.clone();
    unsigned
        .as_object_mut()
        .context("审阅文件格式无效")?
        .remove("content_sha256");
    if canonical_sha256(&unsigned) != expected {
        bail!("审阅文件内容已被修改")
    }
    Ok(())
}

fn validate_review_session(
    diff: &crate::curriculum::CurriculumDiff,
    decisions: &crate::curriculum::DecisionSet,
    assignments: &BTreeMap<String, CourseAssignment>,
) -> Result<()> {
    crate::curriculum::validate_diff(diff)?;
    if decisions.diff_identity_sha256 != diff.diff_identity_sha256 {
        bail!("裁决属于另一批教学差异")
    }
    let known_changes = diff
        .changes
        .iter()
        .map(|change| change.change_id.as_str())
        .collect::<BTreeSet<_>>();
    if decisions
        .decisions
        .keys()
        .any(|id| !known_changes.contains(id.as_str()))
    {
        bail!("裁决包含未知教学差异")
    }
    if assignments.keys().any(|code| normalize(code).is_empty()) {
        bail!("课程归属包含无效代码")
    }
    Ok(())
}

fn review_title(diff: &crate::curriculum::CurriculumDiff, kind: crate::jwts::PlanKind) -> String {
    let scopes = diff
        .candidate
        .plans
        .iter()
        .filter(|plan| {
            diff.current.plans.iter().all(|old| {
                old.plan_id != plan.plan_id || old.info != plan.info || old.courses != plan.courses
            })
        })
        .map(|plan| {
            let name = normalized_value(plan.info.get("major_full_name"));
            if name.is_empty() {
                normalized_value(plan.info.get("major_name"))
            } else {
                name
            }
        })
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    if scopes.is_empty() {
        format!("{}审阅", kind.label())
    } else {
        format!(
            "{}：{}",
            kind.label(),
            scopes.into_iter().collect::<Vec<_>>().join("、")
        )
    }
}

fn normalize_plan_info(plan_id: &str, info: &Value) -> Value {
    let mut result = info.clone();
    if !result.is_object() {
        result = json!({});
    }
    let object = result.as_object_mut().expect("plan object");
    object.insert("plan_id".to_string(), json!(plan_id));
    object.insert(
        "metadata_repo_id".to_string(),
        json!("fireworks-course-registry-v2"),
    );
    object.insert(
        "metadata_path".to_string(),
        json!(format!(
            "curriculum/plans/{}.json",
            encode_path_component(plan_id)
        )),
    );
    let execution = object.get("source_kind").and_then(Value::as_str) == Some("execution")
        || plan_id.starts_with("hit:execution:");
    object
        .entry("source_kind".to_string())
        .or_insert_with(|| json!(if execution { "execution" } else { "curriculum" }));
    for (target, source) in [
        ("department_code", "college_code"),
        ("school_name", "college_name"),
        ("major_code", "major_code"),
        ("major_name", "major_name"),
    ] {
        if object.get(target).is_none_or(Value::is_null) {
            object.insert(
                target.to_string(),
                info.get(source).cloned().unwrap_or(Value::Null),
            );
        }
    }
    if execution {
        if object.get("entry_cohort").is_none_or(Value::is_null) {
            object.insert(
                "entry_cohort".to_string(),
                info.get("grade").cloned().unwrap_or(Value::Null),
            );
        }
    } else if object.get("plan_version").is_none_or(Value::is_null) {
        object.insert(
            "plan_version".to_string(),
            info.get("grade").cloned().unwrap_or(Value::Null),
        );
    }
    object.entry("campus".to_string()).or_insert(json!("hit"));
    let full_name = object.get("major_name").cloned().unwrap_or(Value::Null);
    object
        .entry("major_full_name".to_string())
        .or_insert(full_name);
    object
        .entry("program_type".to_string())
        .or_insert(Value::Null);
    object
        .entry("source_plan_file".to_string())
        .or_insert_with(|| {
            json!(format!(
                "{}_{}.json",
                encode_path_component(plan_id),
                normalized_value(info.get("major_name"))
            ))
        });
    result
}

fn normalized_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => normalize(value),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn encode_path_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn atomic_json_many(replacements: &[(&PathBuf, &Value)]) -> Result<()> {
    let mut staged = Vec::new();
    let mut backups = Vec::new();
    for (path, value) in replacements {
        let temporary = crate::json_store::stage(path, value)?;
        staged.push(((*path).clone(), temporary));
    }
    let mut backup_paths = Vec::new();
    for (path, _) in &staged {
        let backup = path.with_extension(format!(
            "{}.update-backup",
            path.extension().and_then(|v| v.to_str()).unwrap_or("json")
        ));
        if backup.exists() {
            bail!("检测到上次更新留下的备份，请先恢复该任务")
        }
        if !path.is_file() {
            bail!("原始管理数据缺失，不能切换状态")
        }
        backup_paths.push((path.clone(), backup));
    }
    for (path, backup) in backup_paths {
        if let Err(error) = fs::rename(&path, &backup) {
            for (original, saved) in backups.iter().rev() {
                fs::rename(saved, original).context("切换失败且无法恢复原文件")?;
            }
            return Err(error.into());
        }
        backups.push((path, backup));
    }
    let mut installed = Vec::new();
    for (path, temporary) in staged {
        match temporary.persist(&path) {
            Ok(_) => installed.push(path),
            Err(error) => {
                for path in installed.iter().rev() {
                    let _ = fs::remove_file(path);
                }
                for (path, backup) in backups.iter().rev() {
                    let _ = fs::rename(backup, path);
                }
                return Err(error.error.into());
            }
        }
    }
    for (_, backup) in backups {
        fs::remove_file(backup)?;
    }
    Ok(())
}

fn registry_dynamic_tree(preview: &RepositorySyncPreview) -> Result<BTreeMap<String, Value>> {
    Ok(registry_dynamic_tree_refs(preview)?.into_iter()
        .map(|(key, value)| (key, value.into_owned())).collect())
}

fn registry_dynamic_tree_refs(preview: &RepositorySyncPreview) -> Result<BTreeMap<String, std::borrow::Cow<'_, Value>>> {
    use std::borrow::Cow;
    let mut files = BTreeMap::new();
    files.insert("repository-manifest.json".to_string(), Cow::Borrowed(&preview.manifest));
    files.insert("repository-topology.v4.json".to_string(), Cow::Borrowed(&preview.topology));
    files.insert("repository-file-routes.v4.json".to_string(), Cow::Borrowed(&preview.routes));
    for (field, label) in [
        ("curriculum_plans", "培养方案"),
        ("curriculum_records", "课程记录"),
        ("course_descriptors", "课程描述"),
    ] {
        for value in array_at(&preview.manifest, field)? {
            let path = string_field(value, "metadata_path");
            if path.is_empty() { bail!("{label}缺少 Registry 路径"); }
            safe_path(path)?;
            files.insert(path.to_owned(), Cow::Borrowed(value));
        }
    }
    let indexes = preview.manifest.get("curriculum_metadata_indexes").context("manifest 缺少课程索引")?;
    files.insert("indexes/by-plan.json".to_string(), indexes.get("by_plan")
        .map(Cow::Borrowed).unwrap_or_else(|| Cow::Owned(json!({}))));
    files.insert("indexes/pending-course-code.json".to_string(), indexes.get("pending_course_code")
        .map(Cow::Borrowed).unwrap_or_else(|| Cow::Owned(json!([]))));
    if let Some(history) = preview.manifest.get("curriculum_history").and_then(Value::as_array) {
        for entry in history {
            let history_id = string_field(entry, "history_id");
            if history_id.is_empty() { bail!("课程历史缺少稳定身份"); }
            files.insert(format!("curriculum/history/{history_id}.json"), Cow::Borrowed(entry));
        }
    }
    Ok(files)
}

fn registry_files_match(preview: &RepositorySyncPreview, actual: &BTreeMap<String, Value>) -> Result<bool> {
    let expected = registry_dynamic_tree_refs(preview)?;
    Ok(expected.len() == actual.len() && expected.iter()
        .all(|(path, value)| actual.get(path) == Some(value.as_ref())))
}

fn lifecycle_identity(preview: &RepositoryLifecyclePreview) -> Result<String> {
    crate::curriculum::serialized_sha256(&LifecyclePreviewJson { preview, include_identity: false })
}

fn save_update_journal(path: &Path, journal: &UpdateExecutionJournal) -> Result<()> {
    atomic_json(path, &serde_json::to_value(journal)?)
}

fn github_read_request(endpoint: &str, query: Option<&str>) -> Result<std::process::Output> {
    for attempt in 0..4 {
        let mut command = Command::new("gh");
        command.args(["api", "--method", "GET", endpoint]);
        if let Some(query) = query { command.args(["--jq", query]); }
        let output = command.output().context("无法启动 GitHub 只读查询")?;
        let error = String::from_utf8_lossy(&output.stderr);
        let transient = error.contains(": EOF") || error.contains("unexpected EOF")
            || error.contains("connection reset") || error.contains("TLS handshake timeout");
        if output.status.success() || !transient || attempt == 3 {
            return Ok(output);
        }
        eprintln!("GitHub 只读连接中断，正在重试当前请求（{}/3）", attempt + 1);
        std::thread::sleep(std::time::Duration::from_millis(500 * (attempt + 1)));
    }
    unreachable!()
}

fn remote_repository_metadata(organization: &str, repo_id: &str, remote: &str) -> Result<Value> {
    if !remote.contains("github.com") {
        let revision = remote_revision(remote)?;
        return Ok(json!({
            "exists":revision["exists"],
            "head":revision["head"],
            "tree":revision["tree"],
            "remote_url":remote,
            "description":"",
            "private":false,
            "archived":false,
            "template":false,
            "default_branch":"main"
        }));
    }
    let output = github_read_request(&format!("repos/{organization}/{repo_id}"), None)?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        if message.contains("404") || message.contains("Not Found") {
            return Ok(json!({"exists":false,"remote_url":remote}));
        }
        bail!("无法读取 GitHub 仓库信息")
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let revision = remote_revision(remote)?;
    Ok(json!({
        "exists":true,
        "head":revision["head"],
        "tree":revision["tree"],
        "remote_url":remote,
        "description":value.get("description").cloned().unwrap_or(Value::Null),
        "private":value.get("private").cloned().unwrap_or(json!(false)),
        "archived":value.get("archived").cloned().unwrap_or(json!(false)),
        "template":value.get("is_template").cloned().unwrap_or(json!(false)),
        "default_branch":value.get("default_branch").cloned().unwrap_or(json!("main"))
    }))
}

fn sync_registry(plan: &RegistrySyncPlan, operation_id: &str) -> Result<String> {
    if remote_revision(&plan.remote_url)? != plan.baseline {
        bail!("课程注册表已经变化，请重新生成预览")
    }
    let temporary = TempDir::new().context("无法创建 Registry 临时目录")?;
    let work = temporary.path().join("registry");
    let work_text = work.to_string_lossy().to_string();
    run_git(
        temporary.path(),
        &[
            "clone",
            "--depth",
            "1",
            "--branch",
            "main",
            &plan.remote_url,
            &work_text,
        ],
        None,
        &[],
    )?;
    for relative in [
        "curriculum/plans",
        "curriculum/records",
        "curriculum/descriptors",
        "indexes",
    ] {
        let path = work.join(relative);
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
    }
    for root_file in [
        "repository-manifest.json",
        "repository-topology.v4.json",
        "repository-file-routes.v4.json",
    ] {
        let path = work.join(root_file);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    for (relative, value) in &plan.files {
        let path = work.join(relative);
        atomic_json(&path, value)?;
    }
    run_git(&work, &["add", "-A"], None, &[])?;
    let status = run_git(&work, &["status", "--porcelain"], None, &[])?;
    if status.trim().is_empty() {
        return Ok(remote_head(&plan.remote_url)?.unwrap_or_default());
    }
    run_git(
        &work,
        &["config", "user.name", "HIT Fireworks Repository Manager"],
        None,
        &[],
    )?;
    run_git(
        &work,
        &[
            "config",
            "user.email",
            "repository-manager@hit-fireworks.invalid",
        ],
        None,
        &[],
    )?;
    run_git(
        &work,
        &[
            "commit",
            "-m",
            &format!("chore(registry): apply {operation_id}"),
        ],
        None,
        &[],
    )?;
    let commit = run_git(&work, &["rev-parse", "HEAD"], None, &[])?
        .trim()
        .to_string();
    run_git(&work, &["push", "origin", "HEAD:main"], None, &[])?;
    if remote_head(&plan.remote_url)?.as_deref() != Some(commit.as_str()) {
        bail!("课程注册表推送后校验失败")
    }
    Ok(commit)
}

fn verify_registry(plan: &RegistrySyncPlan, expected_commit: Option<&str>) -> Result<()> {
    let head = remote_head(&plan.remote_url)?;
    if let Some(expected) = expected_commit {
        if head.as_deref() != Some(expected) {
            bail!("课程注册表远端版本不一致")
        }
    }
    let temporary = TempDir::new().context("无法创建 Registry 验证目录")?;
    let work = temporary.path().join("registry");
    let work_text = work.to_string_lossy().to_string();
    run_git(
        temporary.path(),
        &[
            "clone",
            "--depth",
            "1",
            "--branch",
            "main",
            &plan.remote_url,
            &work_text,
        ],
        None,
        &[],
    )?;
    for (relative, expected) in &plan.files {
        if crate::json_store::canonical_sha256(&work.join(relative))? != canonical_sha256(expected) {
            bail!("课程注册表文件校验失败：{relative}")
        }
    }
    Ok(())
}

fn repository_course_mapping(manifest: &Value, repo_id: &str) -> Result<BTreeMap<String, String>> {
    Ok(array_at(manifest, "course_descriptors")?.iter()
        .filter(|row| string_field(row, "repo_id") == repo_id)
        .map(|row| (string_field(row, "course_code").to_string(), string_field(row, "course_name").to_string()))
        .collect())
}

fn refresh_curriculum_metadata(manifest: &mut Value) -> Result<()> {
    let mut mappings = BTreeMap::<String, BTreeMap<String, String>>::new();
    for descriptor in array_at(manifest, "course_descriptors")? {
        mappings.entry(string_field(descriptor, "repo_id").to_string()).or_default()
            .insert(string_field(descriptor, "course_code").to_string(), string_field(descriptor, "course_name").to_string());
    }
    let plan_count = array_at(manifest, "curriculum_plans")?.len();
    let record_count = array_at(manifest, "curriculum_records")?.len();
    let descriptor_count = array_at(manifest, "course_descriptors")?.len();
    let uncoded_count = array_at(manifest, "curriculum_records")?.iter().filter(|record| string_field(record, "course_code").is_empty()).count();
    let repositories = manifest.get_mut("repositories").and_then(Value::as_array_mut).context("manifest 缺少仓库")?;
    let mut repository_counts = BTreeMap::<String, usize>::new();
    for repository in repositories.iter_mut() {
        let kind = string_field(repository, "repo_type").to_string();
        *repository_counts.entry(kind.clone()).or_default() += 1;
        if kind != "course" { continue; }
        let repo_id = string_field(repository, "repo_id").to_string();
        let mapping = mappings.get(&repo_id).cloned().unwrap_or_default();
        repository["description"] = json!(crate::repository_metadata::description(string_field(repository, "display_name"), &mapping)?);
        repository["course_codes"] = json!(mapping.keys().collect::<Vec<_>>());
        repository["course_names"] = json!(mapping.values().filter(|name| !name.is_empty()).collect::<BTreeSet<_>>());
    }
    let repository_count = repositories.len();
    let object = manifest.as_object_mut().context("manifest 不是对象")?;
    let summary = object.entry("summary").or_insert_with(|| json!({})).as_object_mut().context("summary 不是对象")?;
    for (key, count) in [("repository_count",repository_count),
        ("course_descriptor_count",descriptor_count),("curriculum_record_count",record_count),
        ("curriculum_metadata_plan_count",plan_count),("curriculum_metadata_record_count",record_count),
        ("curriculum_metadata_pending_course_code_record_count",uncoded_count)] {
        summary.insert(key.to_string(), json!(count));
    }
    summary.insert("repository_counts_by_type".to_string(), json!(repository_counts));
    if let Some(source) = object.get_mut("sources").and_then(|sources| sources.get_mut("curriculum")).and_then(Value::as_object_mut) {
        source.remove("plan_version");
        for (key, count) in [("plan_count",plan_count),("record_count",record_count),("coded_record_count",record_count-uncoded_count),
            ("uncoded_record_count",uncoded_count),("distinct_course_code_count",descriptor_count)] {
            source.insert(key.to_string(), json!(count));
        }
    }
    Ok(())
}

fn github_readme(repository: &str) -> Result<Option<(String, String)>> {
    let output = github_read_request(&format!("repos/{repository}/contents/README.md?ref=main"), None)?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("404") { return Ok(None); }
        bail!("无法读取仓库课程映射：{repository}")
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if string_field(&value, "encoding") != "base64" || string_field(&value, "type") != "file" {
        bail!("仓库 README 格式不支持：{repository}")
    }
    let encoded = string_field(&value, "content").split_whitespace().collect::<String>();
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).context("仓库 README 编码无效")?;
    Ok(Some((string_field(&value, "sha").to_string(), String::from_utf8(bytes).context("仓库 README 不是 UTF-8")?)))
}

fn merge_managed_course_readme(existing: Option<&str>, generated: &str) -> Result<String> {
    const HEADING: &str = "## 课程代码与原始课程名";
    let Some(existing) = existing else { return Ok(generated.to_string()); };
    let newline = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let replacement = generated.split_once(HEADING)
        .map(|(_, table)| table.trim().replace('\n', newline))
        .unwrap_or_default();
    let mut after_heading = None;
    let mut offset = 0;
    for line in existing.split_inclusive('\n') {
        if line.trim() == HEADING {
            if after_heading.replace(offset + line.len()).is_some() {
                bail!("README 存在重复课程映射标题，不能自动替换")
            }
        }
        offset += line.len();
    }
    let Some(after_heading) = after_heading else {
        if replacement.is_empty() { return Ok(existing.to_string()); }
        let separator = if existing.ends_with(&format!("{newline}{newline}")) {
            String::new()
        } else if existing.ends_with('\n') {
            newline.to_string()
        } else {
            format!("{newline}{newline}")
        };
        return Ok(format!("{existing}{separator}{HEADING}{newline}{newline}{replacement}{newline}"));
    };
    let mut table_start = after_heading;
    let mut table_end = after_heading;
    let mut table_rows = 0;
    for line in existing[after_heading..].split_inclusive('\n') {
        if table_rows == 0 && line.trim().is_empty() {
            table_start += line.len();
            table_end = table_start;
            continue;
        }
        if !line.trim_start().starts_with('|') { break; }
        if table_rows == 0 {
            let columns = line.trim().trim_matches('|').split('|').map(str::trim).collect::<Vec<_>>();
            if columns != ["课程代码", "原始课程名"] {
                bail!("README 课程映射表头无法识别，拒绝覆盖维护者内容")
            }
        }
        table_rows += 1;
        table_end += line.len();
    }
    if table_rows == 1 {
        bail!("README 课程映射表结构不完整，拒绝自动替换")
    }
    let prefix = &existing[..table_start];
    let suffix = &existing[table_end..];
    let separator = if table_start != after_heading {
        String::new()
    } else if prefix.ends_with('\n') {
        newline.to_string()
    } else {
        format!("{newline}{newline}")
    };
    let suffix_separator = if table_rows == 0 && !suffix.is_empty() { newline } else { "" };
    Ok(format!("{prefix}{separator}{replacement}{newline}{suffix_separator}{suffix}"))
}

struct GithubLifecycleResponse {
    success: bool,
    status: String,
    stdout: String,
    stderr: String,
}

impl GithubLifecycleResponse {
    fn is_status(&self, status: u16) -> bool {
        !self.success && self.stderr.contains(&format!("(HTTP {status})"))
    }

    fn require_success(&self, context: &str) -> Result<Value> {
        if !self.success {
            bail!("{context}（{}）\n{}\n{}", self.status, self.stderr.trim(), self.stdout.trim())
        }
        if self.stdout.trim().is_empty() { return Ok(Value::Null); }
        serde_json::from_str(&self.stdout).with_context(|| format!("{context}：GitHub 响应不是 JSON"))
    }
}

// Only the transport and polling clock are replaceable; tests run the production ordering.
trait GithubLifecycleApi {
    fn request(&mut self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<GithubLifecycleResponse>;
    fn wait_for_initialization(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

struct GhLifecycleApi;

impl GithubLifecycleApi for GhLifecycleApi {
    fn request(&mut self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<GithubLifecycleResponse> {
        let output = if method == "GET" {
            github_read_request(endpoint, None)?
        } else {
            let mut child = Command::new("gh").args(["api", "--method", method, endpoint, "--input", "-"])
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
                .spawn().context("无法启动 GitHub 工具")?;
            serde_json::to_writer(child.stdin.take().context("无法写入 GitHub 请求")?, body.unwrap_or(&Value::Null))?;
            child.wait_with_output()?
        };
        Ok(GithubLifecycleResponse {
            success: output.status.success(), status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn github_lifecycle_readme(github: &mut impl GithubLifecycleApi, repository: &str, branch: &str) -> Result<Option<(String, String)>> {
    let branch: String = url::form_urlencoded::byte_serialize(branch.as_bytes()).collect();
    let response = github.request("GET", &format!("repos/{repository}/contents/README.md?ref={branch}"), None)?;
    if response.is_status(404) { return Ok(None); }
    let value = response.require_success(&format!("无法读取仓库课程映射：{repository}"))?;
    if value["encoding"] != "base64" || value["type"] != "file" {
        bail!("仓库 README 格式不支持：{repository}")
    }
    let encoded = string_field(&value, "content").split_whitespace().collect::<String>();
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).context("仓库 README 编码无效")?;
    Ok(Some((string_field(&value, "sha").to_string(), String::from_utf8(bytes).context("仓库 README 不是 UTF-8")?)))
}

fn validate_course_readme(action: &RepositoryLifecycleAction, current: Option<&(String, String)>) -> Result<()> {
    let Some(expected) = &action.readme else { return Ok(()); };
    if current.is_some_and(|(_, text)| text == expected) { return Ok(()); }
    if current.map(|(sha, _)| sha.as_str()) != action.baseline.get("readme_sha").and_then(Value::as_str) {
        bail!("仓库 README 已变化，拒绝覆盖：{}", action.repo_id)
    }
    Ok(())
}

fn validate_created_repository(repository: &str, action: &RepositoryLifecycleAction, metadata: &Value) -> Result<u64> {
    if metadata["full_name"].as_str() != Some(repository)
        || metadata["description"].as_str() != Some(action.description.as_str())
        || metadata["private"].as_bool() != Some(action.private)
    {
        bail!("已存在仓库身份与创建预览不一致，拒绝续接：{repository}")
    }
    if let Some(source) = metadata.get("template_repository").filter(|value| !value.is_null()) {
        if source["full_name"].as_str() != action.template_repository.as_deref() {
            bail!("已存在仓库模板来源不一致：{repository}")
        }
    }
    metadata["id"].as_u64().filter(|id| *id != 0).context("GitHub 仓库缺少不可变身份")
}

fn save_creation_checkpoint(journal: &mut UpdateExecutionJournal, path: &Path, repo_id: &str, id: u64) -> Result<()> {
    journal.repository_results.insert(repo_id.to_string(), format!("created:{id}"));
    journal.updated_at = now();
    save_update_journal(path, journal)
}

fn recover_legacy_creation(
    github: &mut impl GithubLifecycleApi, organization: &str, actions: &[RepositoryLifecycleAction],
    journal: &mut UpdateExecutionJournal, path: &Path,
) -> Result<()> {
    // The old executor has no creation receipt. Only its exact empty-repository PATCH failure
    // plus a pristine, time-bounded template root can authorize adopting an existing repository.
    let Some(error) = journal.error.as_deref() else { return Ok(()); };
    if journal.status != "failed" || journal.stage != "repositories"
        || !error.contains("(HTTP 422)") || !error.contains("default_branch")
        || !error.contains("Cannot update default branch for an empty repository")
    { return Ok(()); }
    let candidates: Vec<_> = actions.iter().filter(|action| {
        action.kind == RepositoryLifecycleKind::Create && action.baseline["exists"] == false
            && !journal.repository_results.contains_key(&action.repo_id)
            && error.starts_with(&format!("无法更新仓库设置：{}（{}，", action.title, action.repo_id))
    }).collect();
    if candidates.len() != 1 { return Ok(()); }
    let action = candidates[0];
    let repository = format!("{organization}/{}", action.repo_id);
    if github_repository_path(string_field(&action.baseline, "remote_url")).as_deref() != Some(repository.as_str())
        || action.template_repository.is_none() || action.baseline["readme_sha"].as_str().is_none()
    { bail!("旧创建任务缺少安全恢复证据：{repository}"); }
    let metadata = github.request("GET", &format!("repos/{repository}"), None)?.require_success("无法验证旧创建仓库")?;
    let id = validate_created_repository(&repository, action, &metadata)?;
    let created = chrono::DateTime::parse_from_rfc3339(string_field(&metadata, "created_at"))?;
    if created < chrono::DateTime::parse_from_rfc3339(&journal.created_at)?
        || created > chrono::DateTime::parse_from_rfc3339(&journal.updated_at)?
        || metadata["archived"] != action.archived || metadata["is_template"] != action.template
        || metadata["default_branch"] != action.default_branch
    { bail!("旧创建仓库不满足事务时间或设置证据，拒绝续接：{repository}"); }
    let commit = github.request("GET", &format!("repos/{repository}/commits/{}", action.default_branch), None)?.require_success("无法验证模板初始提交")?;
    if !commit["parents"].as_array().is_some_and(Vec::is_empty) || !is_hex(string_field(&commit, "sha"), 40) {
        bail!("旧创建仓库不是模板初始提交，拒绝续接：{repository}")
    }
    let readme = github_lifecycle_readme(github, &repository, &action.default_branch)?;
    if readme.as_ref().map(|(sha, _)| sha.as_str()) != action.baseline["readme_sha"].as_str() {
        bail!("旧创建仓库 README 不等于冻结模板，拒绝续接：{repository}")
    }
    save_creation_checkpoint(journal, path, &action.repo_id, id)
}

fn prepare_github_creation(
    github: &mut impl GithubLifecycleApi, repository: &str, action: &RepositoryLifecycleAction,
    journal: &mut UpdateExecutionJournal, journal_path: &Path,
) -> Result<()> {
    if action.readme.is_none() && action.template_repository.is_none() {
        bail!("新建非模板仓库缺少真实初始内容：{repository}")
    }
    let response = github.request("GET", &format!("repos/{repository}"), None)?;
    if response.is_status(404) {
        if journal.repository_results.contains_key(&action.repo_id) {
            bail!("已记录创建的仓库不可见，拒绝重复创建：{repository}")
        }
        let (owner, name) = repository.split_once('/').context("仓库地址无效")?;
        let (endpoint, body) = if let Some(template) = &action.template_repository {
            (format!("repos/{template}/generate"), json!({"owner":owner,"name":name,"description":action.description,"private":action.private,"include_all_branches":false}))
        } else {
            (format!("orgs/{owner}/repos"), json!({"name":name,"description":action.description,"private":action.private,"auto_init":false,"has_wiki":false}))
        };
        let created = github.request("POST", &endpoint, Some(&body))?.require_success(&format!("无法创建仓库：{}（{}）", action.title, action.repo_id))?;
        let id = validate_created_repository(repository, action, &created)?;
        // Persist the server-issued immutable ID before any readiness read or settings write.
        save_creation_checkpoint(journal, journal_path, &action.repo_id, id)?;
    } else {
        let metadata = response.require_success(&format!("无法读取创建目标：{repository}"))?;
        let id = validate_created_repository(repository, action, &metadata)?;
        if journal.repository_results.get(&action.repo_id) != Some(&format!("created:{id}")) {
            bail!("同名仓库缺少本事务创建凭据，拒绝续接：{repository}")
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for attempt in 0..31 {
        let metadata = github.request("GET", &format!("repos/{repository}"), None)?.require_success("无法检查初始化仓库身份")?;
        let id = validate_created_repository(repository, action, &metadata)?;
        if journal.repository_results.get(&action.repo_id) != Some(&format!("created:{id}")) {
            bail!("初始化期间仓库身份变化：{repository}")
        }
        let response = github.request("GET", &format!("repos/{repository}/commits/{}", action.default_branch), None)?;
        if response.success {
            let commit = response.require_success("无法读取初始化提交")?;
            if !is_hex(string_field(&commit, "sha"), 40) { bail!("初始化提交缺少 SHA：{repository}"); }
            let current = github_lifecycle_readme(github, repository, &action.default_branch)?;
            if current.is_some() || action.template_repository.is_none() || action.baseline["readme_sha"].is_null() {
                validate_course_readme(action, current.as_ref())?;
                return Ok(());
            }
        } else if response.is_status(404) || response.is_status(409) {
            if action.template_repository.is_none() {
                // The real README creates the requested initial branch; never PATCH an empty repository.
                let current = github_lifecycle_readme(github, repository, &action.default_branch)?;
                validate_course_readme(action, current.as_ref())?;
                if current.is_none() {
                    let expected = action.readme.as_ref().context("新仓库缺少真实初始内容")?;
                    let body = json!({"message":"chore(curriculum): 初始化完整课程代码映射","branch":action.default_branch,
                        "content":base64::engine::general_purpose::STANDARD.encode(expected.as_bytes())});
                    github.request("PUT", &format!("repos/{repository}/contents/README.md"), Some(&body))?.require_success("无法初始化仓库课程映射")?;
                }
            }
        } else {
            response.require_success(&format!("无法检查仓库初始分支：{repository}"))?;
        }
        if attempt == 30 || std::time::Instant::now() >= deadline {
            bail!("等待仓库初始分支与模板 README 就绪超时（60 秒）：{repository}")
        }
        github.wait_for_initialization();
    }
    unreachable!()
}

fn synchronize_course_readme(github: &mut impl GithubLifecycleApi, repository: &str, action: &RepositoryLifecycleAction) -> Result<()> {
    let Some(expected) = &action.readme else { return Ok(()); };
    let current = github_lifecycle_readme(github, repository, &action.default_branch)?;
    validate_course_readme(action, current.as_ref())?;
    if current.as_ref().is_some_and(|(_, text)| text == expected) { return Ok(()); }
    let mut body = json!({"message":"chore(curriculum): 同步完整课程代码映射","branch":action.default_branch,
        "content":base64::engine::general_purpose::STANDARD.encode(expected.as_bytes())});
    if let Some((sha, _)) = current { body["sha"] = json!(sha); }
    github.request("PUT", &format!("repos/{repository}/contents/README.md"), Some(&body))?
        .require_success(&format!("无法同步课程映射：{}", action.repo_id))?;
    Ok(())
}

fn apply_repository_lifecycle(
    github: &mut impl GithubLifecycleApi,
    organization: &str,
    action: &RepositoryLifecycleAction,
    journal: &mut UpdateExecutionJournal,
    journal_path: &Path,
) -> Result<()> {
    let remote = string_field(&action.baseline, "remote_url");
    if !remote.contains("github.com") {
        match action.kind {
            RepositoryLifecycleKind::Create => {
                if !action
                    .baseline
                    .get("exists")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let path = PathBuf::from(remote);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if !path.exists() {
                        let parent = path.parent().unwrap_or(Path::new("."));
                        let name = path
                            .file_name()
                            .and_then(|value| value.to_str())
                            .context("仓库路径无效")?;
                        run_git(parent, &["init", "--bare", name], None, &[])?;
                    }
                }
            }
            _ => {}
        }
        return Ok(());
    }
    apply_github_repository_lifecycle(github, organization, action, journal, journal_path)
}

fn apply_github_repository_lifecycle(
    github: &mut impl GithubLifecycleApi,
    organization: &str,
    action: &RepositoryLifecycleAction,
    journal: &mut UpdateExecutionJournal,
    journal_path: &Path,
) -> Result<()> {
    let repository = format!("{organization}/{}", action.repo_id);
    if github_repository_path(string_field(&action.baseline, "remote_url")).as_deref() != Some(repository.as_str()) {
        bail!("仓库地址与冻结预览不一致：{}", action.repo_id)
    }
    if action.kind == RepositoryLifecycleKind::Create && action.baseline["exists"] == false {
        prepare_github_creation(github, &repository, action, journal, journal_path)?;
    }
    // Detect conflicts before any settings write; re-read again before PUT for optimistic concurrency.
    validate_course_readme(action, github_lifecycle_readme(github, &repository, &action.default_branch)?.as_ref())?;
    let body = json!({
        "description":action.description,
        "private":action.private,
        "archived":action.archived,
        "is_template":action.template,
        "default_branch":action.default_branch,
        "has_issues":true,
        "has_projects":false,
        "has_wiki":false
    });
    github.request("PATCH", &format!("repos/{repository}"), Some(&body))?.require_success(&format!("无法更新仓库设置：{}（{}）", action.title, action.repo_id))?;
    synchronize_course_readme(github, &repository, action)?;
    Ok(())
}

fn verify_repository_lifecycle(
    organization: &str,
    action: &RepositoryLifecycleAction,
) -> Result<()> {
    verify_repository_lifecycle_with(&mut GhLifecycleApi, organization, action)
}

fn verify_repository_lifecycle_with(
    github: &mut impl GithubLifecycleApi,
    organization: &str,
    action: &RepositoryLifecycleAction,
) -> Result<()> {
    let remote = string_field(&action.baseline, "remote_url");
    let actual = if remote.contains("github.com") {
        github.request("GET", &format!("repos/{organization}/{}", action.repo_id), None)?.require_success("无法验证 GitHub 仓库")?
    } else {
        remote_repository_metadata(organization, &action.repo_id, remote)?
    };
    if !remote.contains("github.com") && actual.get("exists").and_then(Value::as_bool) != Some(true) {
        bail!("仓库不存在：{}", action.title)
    }
    if remote.contains("github.com") {
        if actual.get("private").and_then(Value::as_bool) != Some(action.private)
            || actual.get("archived").and_then(Value::as_bool) != Some(action.archived)
            || actual.get("is_template").and_then(Value::as_bool) != Some(action.template)
            || actual.get("default_branch").and_then(Value::as_str)
                != Some(action.default_branch.as_str())
            || actual.get("description").and_then(Value::as_str)
                != Some(action.description.as_str())
        {
            bail!("仓库设置校验失败：{}", action.title)
        }
        if let Some(expected) = &action.readme {
            if !github_lifecycle_readme(github, &format!("{organization}/{}", action.repo_id), &action.default_branch)?.is_some_and(|(_, text)| text == *expected) {
                bail!("仓库完整课程映射校验失败：{}", action.repo_id)
            }
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    #[serde(skip)]
    pub client: Manager,
    pub health: Health,
    pub repositories: Vec<RepositorySummary>,
    pub routes: RoutesSnapshot,
    pub plans: Vec<PlanSummary>,
    pub journals: Vec<JournalSummary>,
    pub query: String,
    pub logs: Vec<String>,
}

impl Dashboard {
    pub fn load(workspace: impl Into<PathBuf>) -> Self {
        let mut manager = Manager::new(workspace);
        let mut dashboard = Self::load_with_client(manager.clone());
        if dashboard.health.health.is_empty() {
            if manager.reload().is_ok() {
                dashboard.client = manager;
                let _ = dashboard.refresh();
            }
        }
        dashboard
    }

    pub fn discover() -> Result<Self> {
        let manager = Manager::discover()?;
        let mut dashboard = Self::load_with_client(manager);
        dashboard.refresh()?;
        Ok(dashboard)
    }

    pub fn load_with_client(client: Manager) -> Self {
        let mut dashboard = Self {
            client,
            health: Health::default(),
            repositories: Vec::new(),
            routes: RoutesSnapshot::default(),
            plans: Vec::new(),
            journals: Vec::new(),
            query: String::new(),
            logs: Vec::new(),
        };
        if let Err(error) = dashboard.refresh() {
            dashboard.logs.push(human_error(&error));
        }
        dashboard
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.client.reload()?;
        self.health = self.client.inspect()?;
        self.repositories = self.client.search(&self.query)?;
        self.routes = self.client.routes(None)?;
        self.plans = self.client.plans()?;
        self.journals = self.client.journals()?;
        self.logs.push("资料已刷新".to_string());
        Ok(())
    }

    pub fn search(&mut self, query: String) -> Result<()> {
        self.query = query;
        self.repositories = self.client.search(&self.query)?;
        Ok(())
    }

    pub fn detail(&self, index: usize) -> Result<RepositoryDetail> {
        let repository = self.repositories.get(index).context("没有选择资料")?;
        self.client.repository(&repository.repo_id)
    }

    pub fn validate(&self) -> Result<()> {
        if self.health.health != "healthy" {
            bail!("资料索引需要检查")
        }
        Ok(())
    }
}

fn operation_plan(
    kind: &str,
    before_topology: &Value,
    before_routes: &Value,
    after_topology: Value,
    after_routes: Value,
    details: Value,
) -> Value {
    let body = json!({
        "kind":kind,
        "before":{
            "topology_sha256":canonical_sha256(before_topology),
            "routes_sha256":canonical_sha256(before_routes)
        },
        "after":{
            "topology":after_topology,
            "routes":after_routes,
            "topology_sha256":canonical_sha256(&after_topology),
            "routes_sha256":canonical_sha256(&after_routes)
        },
        "details":details
    });
    let id = format!("operation-{}", &canonical_sha256(&body)[..20]);
    let mut result = body;
    result["schema_version"] = json!(1);
    result["operation_id"] = json!(id);
    result["created_at"] = json!(now());
    result
}

fn plan_result(path: PathBuf, plan: Value) -> PlannedOperation {
    let sources = plan
        .pointer("/details/source_repository_heads")
        .and_then(Value::as_object)
        .map(|value| value.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let targets = plan
        .pointer("/after/routes/unresolved_repository_heads")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let moves = plan
        .pointer("/details/file_moves")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    PlannedOperation {
        path: path.to_string_lossy().to_string(),
        plan,
        risk: json!({
            "remote_mutation":true,
            "source_repo_ids":sources,
            "target_repo_ids":targets,
            "file_move_count":moves
        }),
    }
}

fn validate_plan_identity(plan: &Value) -> Result<()> {
    let expected = plan
        .pointer("/core/plan_identity_sha256")
        .and_then(Value::as_str)
        .context("计划缺少身份校验")?;
    if plan_identity_sha256(plan) != expected {
        bail!("计划文件已被修改，不能继续。")
    }
    Ok(())
}

fn plan_identity_sha256(plan: &Value) -> String {
    let mut payload = plan.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("created_at");
        if let Some(core) = object.get_mut("core").and_then(Value::as_object_mut) {
            core.remove("plan_identity_sha256");
        }
    }
    canonical_sha256(&payload)
}

fn reserve_target_path(path: &str, occupied: &mut HashSet<String>) -> Result<String> {
    safe_path(path)?;
    let key = path.to_lowercase();
    if occupied.iter().any(|other| other == &key || other.starts_with(&format!("{key}/")) || key.starts_with(&format!("{other}/"))) {
        bail!("目标路径“{path}”冲突；请先明确重命名源文件并重新清点，再预览操作。")
    }
    occupied.insert(key);
    Ok(path.to_string())
}
fn manifest_for_routes(before: &Value, topology: &Value, routes: &Value) -> Result<Value> {
    let mut manifest = before.clone();
    let bindings: BTreeMap<_, _> = array_at(routes, "course_code_routes")?.iter()
        .map(|route| (string_field(route, "course_code").to_string(), route)).collect();
    for field in ["course_descriptors", "curriculum_records"] {
        if let Some(records) = manifest.get_mut(field).and_then(Value::as_array_mut) {
            for record in records {
                let code = string_field(record, "course_code");
                if code.is_empty() { continue; }
                let route = bindings.get(code).with_context(|| format!("课程 {code} 缺少直接路由"))?;
                record["repo_id"] = route["repo_id"].clone();
                record["physical_repository_id"] = route["physical_repository_id"].clone();
                if record.get("attachment_repo_id").is_some() { record["attachment_repo_id"] = route["repo_id"].clone(); }
            }
        }
    }
    let old: BTreeMap<_, _> = array_at(before, "repositories")?.iter()
        .map(|repo| (string_field(repo, "repo_id"), repo)).collect();
    let mut result = Vec::new();
    for (id, topology_repo) in repositories(topology)? {
        let mut repo = old.get(id.as_str()).map(|value| Value::clone(value)).unwrap_or_else(|| json!({}));
        for (key, value) in topology_repo.as_object().context("仓库索引无效")? { repo[key] = value.clone(); }
        let mapping: BTreeMap<_, _> = array_at(&manifest, "course_descriptors")?.iter()
            .filter(|descriptor| string_field(descriptor, "repo_id") == id)
            .map(|descriptor| (string_field(descriptor, "course_code").to_string(), string_field(descriptor, "course_name").to_string())).collect();
        repo["course_names"] = json!(mapping.values().collect::<BTreeSet<_>>());
        match string_field(&repo, "repo_type") {
            "course" => repo["description"] = json!(crate::repository_metadata::description(string_field(&repo, "display_name"), &mapping)?),
            "shared" | "competition" => repo["description"] = repo["display_name"].clone(),
            _ => {}
        }
        result.push(repo);
    }
    manifest["repositories"] = json!(result);
    if manifest.get("files").is_some() { manifest["files"] = routes["files"].clone(); }
    if manifest.get("course_code_routes").is_some() { manifest["course_code_routes"] = routes["course_code_routes"].clone(); }
    if let Some(summary) = manifest.get_mut("summary").and_then(Value::as_object_mut) {
        summary.insert("repository_count".into(), json!(repositories(topology)?.len()));
    }
    Ok(manifest)
}

fn validate_direct_manifest(manifest: &Value) -> Result<()> {
    if manifest.get("resource_groups").is_some() { bail!("旧资源组 manifest 已停止支持，请先完成正式数据切换。") }
    for field in ["repositories", "course_descriptors", "curriculum_records", "course_code_routes", "files"] {
        if let Some(rows) = manifest.get(field).and_then(Value::as_array) {
            for row in rows {
                reject_resource_partition_fields(row)?;
            }
        }
    }
    Ok(())
}

fn reject_resource_partition_fields(value: &Value) -> Result<()> {
    for key in ["resource_group_id", "member_resource_group_ids", "component_id"] {
        if value.get(key).is_some() { bail!("当前直接仓库模型不接受旧字段 {key}，请完成正式数据切换。") }
    }
    Ok(())
}


fn validate_state(topology: &Value, routes: &Value, allow_unresolved: bool) -> Result<()> {
    let topology_version = topology
        .get("schema_version")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if !(1..=4).contains(&topology_version) {
        bail!("资料索引版本不受支持")
    }
    if routes.get("schema_version").and_then(Value::as_i64) != Some(topology_version) {
        bail!("资料索引和文件清单版本不一致")
    }
    if topology.get("generation") != routes.get("generation") {
        bail!("资料索引和文件清单代次不一致")
    }
    let repos = repositories(topology)?;
    let mut physical = HashSet::new();
    let mut course_owner = HashMap::new();
    for (key, repo) in repos {
        reject_resource_partition_fields(repo)?;
        safe_repo_id(key)?;
        if string_field(repo, "repo_id") != key {
            bail!("资料索引键不一致")
        }
        let physical_id = string_field(repo, "physical_repository_id");
        if !matches!(string_field(repo, "repo_type"), "control" | "template")
            && (physical_id.is_empty() || !physical.insert(physical_id.to_string())) {
            bail!("资料物理身份缺失或重复")
        }
        for code in string_array(repo, "course_codes") {
            if course_owner.insert(code, key.as_str()).is_some() {
                bail!("完整课程代码重复归属仓库")
            }
        }
    }
    let mut routed_codes = HashSet::new();
    for route in array_at(routes, "course_code_routes")? {
        reject_resource_partition_fields(route)?;
        let code = string_field(route, "course_code");
        let repo_id = string_field(route, "repo_id");
        if code.is_empty() || !routed_codes.insert(code) || course_owner.get(code).copied() != Some(repo_id) {
            bail!("课程代码路由必须与唯一仓库归属一致：{code}")
        }
        if string_field(route, "physical_repository_id") != string_field(&repos[repo_id], "physical_repository_id") {
            bail!("课程代码 {code} 的物理仓库身份不一致")
        }
    }
    if routed_codes.len() != course_owner.len() { bail!("仓库课程代码缺少直接路由") }
    let mut paths = HashSet::new();
    for file in array_at(routes, "files")? {
        reject_resource_partition_fields(file)?;
        let repo_id = string_field(file, "repo_id");
        let path = string_field(file, "path");
        safe_repo_id(repo_id)?;
        safe_path(path)?;
        if !repos.contains_key(repo_id) {
            bail!("文件指向不存在的资料")
        }
        if !paths.insert((repo_id.to_lowercase(), path.to_lowercase())) {
            bail!("同一份资料中存在重复文件路径")
        }
        for code in string_array(file, "course_codes") {
            if course_owner.get(&code).copied() != Some(repo_id) {
                bail!("文件 {path} 的课程代码 {code} 属于其他仓库")
            }
        }
    }
    let complete: BTreeSet<_> = string_array(routes, "inventory_complete_repositories")
        .into_iter()
        .collect();
    if complete.iter().any(|repo_id| !repos.contains_key(repo_id)) {
        bail!("完整文件清单引用不存在的资料")
    }
    let heads = object_at(routes, "repository_heads")?;
    let unresolved: BTreeSet<_> = string_array(routes, "unresolved_repository_heads")
        .into_iter()
        .collect();
    if !allow_unresolved && !unresolved.is_empty() {
        bail!("文件清单含未完成的远端状态")
    }
    if heads.keys().any(|key| unresolved.contains(key)) {
        bail!("同一份资料的远端状态冲突")
    }
    let head_keys: BTreeSet<_> = heads.keys().cloned().collect();
    if head_keys
        .union(&unresolved)
        .cloned()
        .collect::<BTreeSet<_>>()
        != complete
    {
        bail!("完整文件清单缺少远端版本")
    }
    for (repo_id, head) in heads {
        if !repos.contains_key(repo_id) || !head.as_str().is_some_and(|value| is_hex(value, 40)) {
            bail!("远端版本号无效")
        }
    }
    Ok(())
}

fn build_target_commit(
    object_repo: &Path,
    operation_id: &str,
    created_at: &str,
    target_repo_id: &str,
    expected_parent: Option<&str>,
    moves: &[Value],
    source_heads: &HashMap<String, String>,
) -> Result<String> {
    let index = object_repo.join(format!("index-{target_repo_id}"));
    let index_value = index.to_string_lossy().to_string();
    let env = [("GIT_INDEX_FILE", index_value.as_str())];
    run_git(object_repo, &["read-tree", "--empty"], None, &env)?;
    let mut seen = HashSet::new();
    let mut sorted = moves.to_vec();
    sorted.sort_by_key(|value| string_field(value, "target_path").to_string());
    for file_move in sorted {
        let target_path = string_field(&file_move, "target_path");
        safe_path(target_path)?;
        if !seen.insert(target_path.to_lowercase()) {
            bail!("目标资料中存在同名文件")
        }
        let source_repo = string_field(&file_move, "source_repo_id");
        let source_path = string_field(&file_move, "source_path");
        let head = source_heads.get(source_repo).context("缺少源资料版本")?;
        let output = run_git(
            object_repo,
            &["ls-tree", "-z", head, "--", source_path],
            None,
            &[],
        )?;
        let record = output.trim_end_matches('\0');
        let (meta, actual_path) = record.split_once('\t').context("源文件不存在")?;
        let fields = meta.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" || actual_path != source_path {
            bail!("源路径不是普通文件")
        }
        run_git(
            object_repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                fields[0],
                fields[2],
                target_path,
            ],
            None,
            &env,
        )?;
    }
    let tree = run_git(object_repo, &["write-tree"], None, &env)?;
    let mut arguments = vec!["commit-tree".to_string(), tree.trim().to_string()];
    if let Some(parent) = expected_parent {
        arguments.extend(["-p".to_string(), parent.to_string()]);
    }
    let message =
        format!("chore(repository-management): apply {operation_id} to {target_repo_id}\n");
    let author = [
        ("GIT_AUTHOR_NAME", "HIT Fireworks Repository Manager"),
        (
            "GIT_AUTHOR_EMAIL",
            "repository-manager@hit-fireworks.invalid",
        ),
        ("GIT_COMMITTER_NAME", "HIT Fireworks Repository Manager"),
        (
            "GIT_COMMITTER_EMAIL",
            "repository-manager@hit-fireworks.invalid",
        ),
        ("GIT_AUTHOR_DATE", created_at),
        ("GIT_COMMITTER_DATE", created_at),
    ];
    run_git_owned(object_repo, &arguments, Some(message.as_bytes()), &author)
        .map(|value| value.trim().to_string())
}

fn run_git(
    cwd: &Path,
    arguments: &[&str],
    input: Option<&[u8]>,
    environment: &[(&str, &str)],
) -> Result<String> {
    let arguments = arguments
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    run_git_owned(cwd, &arguments, input, environment)
}

fn run_git_owned(
    cwd: &Path,
    arguments: &[String],
    input: Option<&[u8]>,
    environment: &[(&str, &str)],
) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().context("无法启动 Git")?;
    if let Some(bytes) = input {
        child
            .stdin
            .as_mut()
            .context("无法写入 Git")?
            .write_all(bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Git 操作失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn remote_head(remote: &str) -> Result<Option<String>> {
    let revision = remote_revision(remote)?;
    if !revision
        .get("exists")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("远端资料不存在")
    }
    Ok(revision
        .get("head")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

fn fetch_commit(cwd: &Path, remote: &str, head: &str, reference: &str) -> Result<()> {
    run_git(
        cwd,
        &[
            "fetch",
            "--no-tags",
            "--force",
            remote,
            &format!("{head}:{reference}"),
        ],
        None,
        &[],
    )?;
    let fetched = run_git(cwd, &["rev-parse", reference], None, &[])?;
    if fetched.trim() != head {
        bail!("读取远端期间资料发生变化")
    }
    Ok(())
}

fn remote_head_allow_missing(remote: &str) -> Result<Option<String>> {
    Ok(remote_revision(remote)?
        .get("head")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

fn github_repository_path(remote: &str) -> Option<String> {
    let url = url::Url::parse(remote).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("github.com")
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return None;
    }
    let path = url.path().trim_matches('/');
    let (owner, repository) = path.split_once('/')?;
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    if safe_repo_id(owner).is_err() || safe_repo_id(repository).is_err() {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

fn remote_revision(remote: &str) -> Result<Value> {
    if !remote.contains("://") && !Path::new(remote).exists() {
        return Ok(json!({"exists":false,"head":null,"tree":null,"remote_url":remote}));
    }
    if let Some(repository) = github_repository_path(remote) {
        let output = github_read_request(&format!("repos/{repository}/commits/main"), Some("{head:.sha,tree:.commit.tree.sha}"))?;
        if output.status.success() {
            let revision: Value = serde_json::from_slice(&output.stdout)?;
            let head = string_field(&revision, "head");
            let tree = string_field(&revision, "tree");
            if !is_hex(head, 40) || !is_hex(tree, 40) {
                bail!("GitHub 远端版本响应无效")
            }
            return Ok(json!({"exists":true,"head":head,"tree":tree,"remote_url":remote}));
        }
        let error = String::from_utf8_lossy(&output.stderr);
        if !error.contains("404") && !error.contains("409") {
            bail!("无法读取 GitHub 远端版本：{}", error.trim())
        }
        let metadata = github_read_request(&format!("repos/{repository}"), Some(".id"))?;
        if metadata.status.success() {
            return Ok(json!({"exists":true,"head":null,"tree":null,"remote_url":remote}));
        }
        if String::from_utf8_lossy(&metadata.stderr).contains("404") {
            return Ok(json!({"exists":false,"head":null,"tree":null,"remote_url":remote}));
        }
        bail!("无法核对 GitHub 仓库是否存在")
    }
    let output = Command::new("git")
        .args(["ls-remote", remote, "refs/heads/main"])
        .output()
        .context("无法检查远端资料")?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if message.contains("not found")
            || message.contains("does not exist")
            || message.contains("repository not found")
            || message.contains("no such file")
        {
            return Ok(json!({"exists":false,"head":null,"tree":null,"remote_url":remote}));
        }
        bail!("无法连接远端资料")
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let head = value.split_whitespace().next().map(ToOwned::to_owned);
    let Some(head_value) = head else {
        return Ok(json!({"exists":true,"head":null,"tree":null,"remote_url":remote}));
    };
    let temp = TempDir::new().context("无法创建远端检查目录")?;
    run_git(temp.path(), &["init", "--bare"], None, &[])?;
    fetch_commit(temp.path(), remote, &head_value, "refs/revision/main")?;
    let tree = run_git(
        temp.path(),
        &["rev-parse", "refs/revision/main^{tree}"],
        None,
        &[],
    )?;
    Ok(json!({
        "exists":true,
        "head":head_value,
        "tree":tree.trim(),
        "remote_url":remote
    }))
}

fn current_actor(remote_template: &str) -> Result<String> {
    if remote_template != DEFAULT_REMOTE_TEMPLATE {
        return Ok("local-test".to_string());
    }
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("需要先安装并登录 GitHub 工具")?;
    if !output.status.success() {
        bail!("需要先登录 GitHub")
    }
    let actor = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if actor.is_empty() {
        bail!("需要先登录 GitHub")
    }
    Ok(actor)
}

fn remote_url(template: &str, organization: &str, repo_id: &str) -> String {
    template
        .replace("{organization}", organization)
        .replace("{repo_id}", repo_id)
}

fn read_json(path: &Path) -> Result<Value> {
    crate::json_store::read(path).with_context(|| format!("无法读取管理数据：{}", path.display()))
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    crate::json_store::write(path, value)
}

fn canonical_sha256(value: &Value) -> String {
    crate::curriculum::value_sha256(value)
}


fn repositories(topology: &Value) -> Result<&Map<String, Value>> {
    object_at(topology, "repositories")
}

fn object_at<'a>(value: &'a Value, key: &str) -> Result<&'a Map<String, Value>> {
    value
        .get(key)
        .and_then(Value::as_object)
        .with_context(|| format!("数据缺少 {key}"))
}

fn array_at<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("数据缺少 {key}"))
}

fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize(value: &str) -> String {
    value.nfkc().collect::<String>().trim().to_string()
}

fn safe_repo_id(value: &str) -> Result<String> {
    let value = normalize(value);
    if value.is_empty()
        || value.len() > 100
        || matches!(value.as_str(), "." | "..")
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        bail!("内部资料名称无效")
    }
    Ok(value)
}

fn safe_path(value: &str) -> Result<String> {
    let value = normalize(&value.replace('\\', "/"));
    let parts = value.split('/').collect::<Vec<_>>();
    if value.is_empty()
        || value.starts_with('/')
        || parts
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
        || parts.first() == Some(&".git")
    {
        bail!("文件路径无效")
    }
    Ok(value)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn sorted_json_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut result = fs::read_dir(path)?
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    result.sort();
    Ok(result)
}

fn add_stage(journal: &mut Value, stage: &str) {
    let stages = journal
        .get_mut("completed_stages")
        .and_then(Value::as_array_mut)
        .expect("completed stages");
    if !stages.iter().any(|value| value.as_str() == Some(stage)) {
        stages.push(json!(stage));
    }
    journal["updated_at"] = json!(now());
}

fn friendly_path(value: &str) -> String {
    value
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(value)
        .to_string()
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn command_available(program: &str, arguments: &[&str]) -> bool {
    Command::new(program)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn human_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    if text.contains("No such file") || text.contains("缺少数据文件") {
        "管理数据不完整，请重新解压安装包。".to_string()
    } else if text.contains("Git") && text.contains("无法启动") {
        "浏览功能可用；如需修改资料，请先安装 Git。".to_string()
    } else if text.contains("GitHub") || text.contains("远端") {
        "无法连接或验证 GitHub。请检查网络和登录状态后重试。".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_repo_id_is_stable_and_hidden() {
        let manager = Manager::new(".");
        let first = manager.automatic_repo_id("高等数学资料", &["微积分".into(), "习题".into()]);
        let second = manager.automatic_repo_id("高等数学资料", &["习题".into(), "微积分".into()]);
        assert_eq!(first, second);
        assert!(first.starts_with("MANAGED-"));
        assert_eq!(first.len(), 20);
    }

    #[test]
    fn plan_identity_detects_mutation() {
        let mut plan = json!({
            "operation_id":"operation-a",
            "created_at":"now",
            "core":{"confirmation_phrase":"APPLY operation-a"}
        });
        let identity = plan_identity_sha256(&plan);
        plan["core"]["plan_identity_sha256"] = json!(identity);
        assert!(validate_plan_identity(&plan).is_ok());
        plan["operation_id"] = json!("operation-b");
        assert!(validate_plan_identity(&plan).is_err());
    }

    #[test]
    fn safe_paths_reject_git_and_parent_segments() {
        assert!(safe_path("notes/a.pdf").is_ok());
        assert!(safe_path("../secret").is_err());
        assert!(safe_path(".git/config").is_err());
    }
}

#[cfg(test)]
#[path = "native_manager_tests.rs"]
mod native_manager_tests;


#[cfg(test)]
mod curriculum_rebuild_tests {
    use super::*;
    use crate::jwts::{CandidatePlan, CandidateSnapshot};

    pub(super) fn fixture() -> (TempDir, Manager) {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        let manifest = json!({
            "schema_version":1,
            "organization":"LOCAL",
            "repositories":[{
                "repo_id":"COURSE-A","repo_type":"course","display_name":"计算机学院 / 无资料课程",
                "physical_repository_id":"physical-a","course_codes":["A1"],
                "materialization_kind":"empty-course-code-college-bucket"
            }],
            "curriculum_plans":[{"plan_id":"plan-a","source_kind":"curriculum","plan_version":"2022版","department_code":"01","major_code":"CS","major_name":"计算机科学与技术","school_name":"计算机学院"}],
            "curriculum_records":[{"record_id":"REC-OLD","source_plan":"plan-a","source_ordinal":0,"course_code":"A1","course_name":"程序设计","credit":3}],
            "course_descriptors":[{
                "descriptor_id":"course-code:A1","course_code":"A1","course_name":"程序设计",
                "physical_repository_id":"physical-a","repo_id":"COURSE-A",
                "record_ids":["REC-OLD"]
            }],
            "curriculum_metadata_indexes":{"by_plan":{},"pending_course_code":[]},
            "virtual_collections":[]
        });
        let topology = json!({
            "schema_version":1,"generation":1,"organization":"LOCAL",
            "repositories":{"COURSE-A":{"repo_id":"COURSE-A","repo_type":"course","display_name":"计算机学院 / 无资料课程","physical_repository_id":"physical-a","course_codes":["A1"],"lineage":{"kind":"fixture","source_repo_ids":[]}}}
        });
        let routes = json!({
            "schema_version":1,"generation":1,"inventory_complete_repositories":[],"repository_heads":{},"files":[],
            "course_code_routes":[{"course_code":"A1","has_material":false,"physical_repository_id":"physical-a","repo_id":"COURSE-A"}]
        });
        write_value(&workspace.join(DEFAULT_MANIFEST), &manifest);
        write_value(&workspace.join(DEFAULT_TOPOLOGY), &topology);
        write_value(&workspace.join(DEFAULT_ROUTES), &routes);
        let remote_root = temp.path().join("remotes");
        fs::create_dir_all(&remote_root).unwrap();
        run_git(&remote_root, &["init", "--bare", "COURSE-A.git"], None, &[]).unwrap();
        let mut manager = Manager::new(workspace).with_remote_template(
            remote_root
                .join("{repo_id}.git")
                .to_string_lossy()
                .to_string(),
        );
        manager.reload().unwrap();
        (temp, manager)
    }

    fn write_value(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    #[test]
    fn new_same_name_course_requires_explicit_assignment_and_uses_own_group() {
        let (_temp, manager) = fixture();
        let snapshot = CandidateSnapshot {
            generated_at: "now".into(),
            base_url: "test".into(),
            plans: vec![CandidatePlan {
                plan_id: "plan-a".into(),
                info: json!({"source_kind":"curriculum","plan_version":"2022版","department_code":"01","major_code":"CS","major_name":"计算机科学与技术","school_name":"计算机学院"}),
                courses: vec![
                    json!({"course_code":"A1","course_name":"程序设计","credit":3}),
                    json!({"course_code":"A2","course_name":"程序设计","credit":2}),
                    json!({"course_code":null,"course_name":"创新实践"}),
                ],
            }],
        };
        assert!(manager
            .rebuild_from_snapshot(&snapshot, &BTreeMap::new())
            .is_err());
        let mut assignments = BTreeMap::new();
        assignments.insert(
            "A2".to_string(),
            CourseAssignment::Existing {
                repo_id: "COURSE-A".to_string(),
            },
        );
        let mut preview = manager
            .rebuild_from_snapshot(&snapshot, &assignments)
            .unwrap();
        finalize_repository_preview(&mut preview, &manager.workspace_identity()).unwrap();
        let a2 = preview.manifest["course_descriptors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["course_code"] == "A2")
            .unwrap();
        assert_eq!(a2["repo_id"], "COURSE-A");
        assert_eq!(preview.routes["files"], json!([]));
    }

    #[test]
    fn apply_preview_atomically_reloads_manager() {
        let (_temp, mut manager) = fixture();
        let snapshot = CandidateSnapshot {
            generated_at: "now".into(),
            base_url: "test".into(),
            plans: vec![CandidatePlan {
                plan_id: "plan-a".into(),
                info: json!({"major_name":"计算机科学与技术","school_name":"计算机学院"}),
                courses: vec![json!({"course_code":"A1","course_name":"程序设计"})],
            }],
        };
        let mut preview = manager
            .rebuild_from_snapshot(&snapshot, &BTreeMap::new())
            .unwrap();
        finalize_repository_preview(&mut preview, &manager.workspace_identity()).unwrap();
        manager.apply_repository_sync_preview(&preview).unwrap();
        assert_eq!(
            manager.manifest["curriculum_records"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            manager.routes["course_code_routes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn selected_plan_update_preserves_unselected_plans() {
        let (_temp, manager) = fixture();
        let mut current = crate::curriculum::baseline_snapshot(&manager.manifest).unwrap();
        current.plans[0].courses = vec![json!({"course_code":"A1","course_name":"程序设计"})];
        let mut candidate = current.clone();
        candidate.plans.push(CandidatePlan {
            plan_id: "plan-b".into(),
            info: json!({"major_name":"软件工程","school_name":"计算机学院"}),
            courses: vec![json!({"course_code":"B1","course_name":"软件工程导论"})],
        });
        let diff = crate::curriculum::diff_snapshots(current, candidate).unwrap();
        let accepted = crate::curriculum::materialize(
            &diff,
            &crate::curriculum::default_decisions(&diff, crate::curriculum::Decision::Accept),
        )
        .unwrap();
        assert!(accepted.plans.iter().any(|plan| plan.plan_id == "plan-a"));
        assert!(accepted.plans.iter().any(|plan| plan.plan_id == "plan-b"));
    }
}

#[cfg(test)]
mod registry_lifecycle_tests {
    use super::*;
    use crate::jwts::{CandidatePlan, CandidateSnapshot};

    struct GithubStep {
        method: &'static str,
        endpoint: String,
        body: Option<Value>,
        response: GithubLifecycleResponse,
    }

    struct ScriptedGithub {
        steps: std::collections::VecDeque<GithubStep>,
        journal_path: PathBuf,
    }

    impl GithubLifecycleApi for ScriptedGithub {
        fn request(&mut self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<GithubLifecycleResponse> {
            let step = self.steps.pop_front().expect("unexpected GitHub request");
            assert_eq!((method, endpoint), (step.method, step.endpoint.as_str()));
            if let Some(expected) = step.body { assert_eq!(body, Some(&expected)); }
            if method != "GET" {
                let journal = read_json(&self.journal_path).unwrap();
                assert_eq!(journal["status"], "applying");
                assert!(journal["error"].is_null());
            }
            Ok(step.response)
        }

        fn wait_for_initialization(&mut self) {
            let step = self.steps.pop_front().expect("unexpected initialization wait");
            assert_eq!(step.method, "WAIT");
            let journal = read_json(&self.journal_path).unwrap();
            assert_eq!(journal["repository_results"]["NEW"], "created:101");
        }
    }

    fn api_step(method: &'static str, endpoint: &str, value: Value) -> GithubStep {
        GithubStep { method, endpoint: endpoint.into(), body: None, response: GithubLifecycleResponse {
            success: true, status: "exit code: 0".into(), stdout: value.to_string(), stderr: String::new(),
        } }
    }

    fn api_error(method: &'static str, endpoint: &str, code: u16) -> GithubStep {
        GithubStep { method, endpoint: endpoint.into(), body: None, response: GithubLifecycleResponse {
            success: false, status: "exit code: 1".into(),
            stdout: format!("{{\"message\":\"original-response-{code}\",\"errors\":[{{\"field\":\"default_branch\"}}]}}"),
            stderr: format!("gh: original-error (HTTP {code})"),
        } }
    }

    fn lifecycle_action() -> RepositoryLifecycleAction {
        RepositoryLifecycleAction {
            repo_id: "NEW".into(), title: "新课程".into(), kind: RepositoryLifecycleKind::Create,
            description: "精确冻结说明".into(), private: false, archived: false, template: false,
            default_branch: "main".into(), readme: Some("# 真实课程映射\n".into()),
            template_repository: Some("Org/template".into()),
            baseline: json!({"exists":false,"remote_url":"https://github.com/Org/NEW.git","readme_sha":"template-sha"}),
        }
    }

    fn lifecycle_metadata(action: &RepositoryLifecycleAction) -> Value {
        json!({"id":101,"full_name":format!("Org/{}", action.repo_id),"description":action.description,
            "private":action.private,"archived":action.archived,"is_template":action.template,
            "default_branch":action.default_branch,"created_at":"2026-09-07T13:54:58Z",
            "template_repository":action.template_repository.as_ref().map(|source| json!({"full_name":source}))})
    }

    fn lifecycle_commit() -> Value { json!({"sha":"04a1f685edd1feefa7b5a8b49fd2c6a3397a1acc","parents":[]}) }

    fn lifecycle_readme(sha: &str, text: &str) -> Value {
        json!({"sha":sha,"type":"file","encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(text.as_bytes())})
    }

    fn lifecycle_fixture() -> (TempDir, Manager, RepositorySyncPreview, RepositoryLifecyclePreview, UpdateExecutionJournal, PathBuf) {
        let (temp, mut manager) = curriculum_rebuild_tests::fixture();
        let registry = seed_registry(temp.path());
        manager = manager.with_registry_remote(registry.to_string_lossy());
        let snapshot = CandidateSnapshot { generated_at:"now".into(),base_url:"test".into(),plans:vec![CandidatePlan {
            plan_id:"plan-a".into(),info:json!({"major_name":"计算机科学与技术","school_name":"计算机学院"}),
            courses:vec![json!({"course_code":"A1","course_name":"程序设计"})],
        }] };
        let mut state = manager.rebuild_from_snapshot(&snapshot, &BTreeMap::new()).unwrap();
        finalize_repository_preview(&mut state, &manager.workspace_identity()).unwrap();
        let mut remote = manager.plan_remote_sync(&state).unwrap();
        remote.organization = "Org".into();
        remote.actions = vec![lifecycle_action()];
        remote.identity_sha256 = lifecycle_identity(&remote).unwrap();
        let identity = canonical_sha256(&json!({"state":state.identity_sha256,"remote":remote.identity_sha256}));
        let operation = format!("curriculum-update-{}", &identity[..20]);
        let path = manager.operations_path.join(format!("{operation}.update.json"));
        let journal = UpdateExecutionJournal {
            schema_version:2,operation_id:operation.clone(),preview_identity_sha256:identity,
            preview_path:manager.operations_path.join(format!("{operation}.update-preview.json")).to_string_lossy().into_owned(),
            status:"failed".into(),stage:"repositories".into(),
            registry_commit:Some(sync_registry(&remote.registry, &operation).unwrap()),
            completed_stages:vec!["registry".into()],repository_results:BTreeMap::new(),
            error:Some("interrupted".into()),created_at:"2026-09-07T13:50:00Z".into(),updated_at:"2026-09-07T13:55:00Z".into(),
        };
        save_update_journal(&path, &journal).unwrap();
        (temp, manager, state, remote, journal, path)
    }

    #[test]
    fn borrowed_preview_hashes_preserve_frozen_identities_and_detect_changes() {
        let (_temp, _manager, mut state, mut remote, journal, _path) = lifecycle_fixture();
        state.summary_lines.push("中文\\\"说明".into());
        state.manifest["unknown"] = json!({"z":[null,true,1.5],"a":"重复"});
        let mut legacy_state = serde_json::to_value(&state).unwrap();
        legacy_state.as_object_mut().unwrap().remove("identity_sha256");
        assert_eq!(repository_preview_identity(&state).unwrap(), canonical_sha256(&legacy_state));
        remote.registry.files.insert("unknown.json".into(), json!({"z":2,"a":[1,1]}));
        let mut legacy_remote = serde_json::to_value(&remote).unwrap();
        legacy_remote.as_object_mut().unwrap().remove("identity_sha256");
        assert_eq!(lifecycle_identity(&remote).unwrap(), canonical_sha256(&legacy_remote));
        let bundle = json!({"state":state,"remote":remote,"identity_sha256":journal.preview_identity_sha256});
        atomic_json(Path::new(&journal.preview_path), &bundle).unwrap();
        let fingerprint = preview_bundle_sha256(&state, &remote, &journal.preview_identity_sha256).unwrap();
        assert_eq!(fingerprint, canonical_sha256(&bundle));
        assert_eq!(fingerprint, crate::json_store::canonical_sha256(Path::new(&journal.preview_path)).unwrap());
        remote.actions[0].readme = None;
        assert_ne!(fingerprint, preview_bundle_sha256(&state, &remote, &journal.preview_identity_sha256).unwrap());
        let mut files = registry_dynamic_tree(&state).unwrap();
        assert!(registry_files_match(&state, &files).unwrap());
        files.insert("extra.json".into(), json!(null));
        assert!(!registry_files_match(&state, &files).unwrap());
        files.remove("extra.json");
        files.get_mut("repository-manifest.json").unwrap()["unknown"] = json!("changed");
        assert!(!registry_files_match(&state, &files).unwrap());
    }

    fn creation_steps(action: &RepositoryLifecycleAction) -> Vec<GithubStep> {
        vec![api_error("GET", "repos/Org/NEW", 404), api_step("POST", "repos/Org/template/generate", lifecycle_metadata(action))]
    }

    fn readiness_steps(action: &RepositoryLifecycleAction, sha: &str, text: &str) -> Vec<GithubStep> {
        vec![api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)),
            api_step("GET", "repos/Org/NEW/commits/main", lifecycle_commit()),
            api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme(sha, text))]
    }

    fn synchronize_steps(action: &RepositoryLifecycleAction) -> Vec<GithubStep> {
        let expected = action.readme.as_ref().unwrap();
        let mut put = api_step("PUT", "repos/Org/NEW/contents/README.md", json!({}));
        put.body = Some(json!({"message":"chore(curriculum): 同步完整课程代码映射","branch":"main","sha":"template-sha",
            "content":base64::engine::general_purpose::STANDARD.encode(expected.as_bytes())}));
        vec![api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("template-sha", "template")),
            api_step("PATCH", "repos/Org/NEW", lifecycle_metadata(action)),
            api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("template-sha", "template")), put]
    }

    fn verification_steps(action: &RepositoryLifecycleAction) -> Vec<GithubStep> {
        vec![api_step("GET", &format!("repos/Org/{}", action.repo_id), lifecycle_metadata(action)),
            api_step("GET", &format!("repos/Org/{}/contents/README.md?ref=main", action.repo_id), lifecycle_readme("target-sha", action.readme.as_ref().unwrap()))]
    }

    #[test]
    fn template_initialization_waits_for_branch_and_readme_before_patch() {
        let (_temp, mut manager, state, remote, journal, path) = lifecycle_fixture();
        let action = &remote.actions[0];
        let mut steps = creation_steps(action);
        steps.extend([api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)),
            api_error("GET", "repos/Org/NEW/commits/main", 409), api_step("WAIT", "", Value::Null)]);
        steps.extend([api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)),
            api_step("GET", "repos/Org/NEW/commits/main", lifecycle_commit()),
            api_error("GET", "repos/Org/NEW/contents/README.md?ref=main", 404), api_step("WAIT", "", Value::Null)]);
        steps.extend(readiness_steps(action, "template-sha", "template"));
        steps.extend(synchronize_steps(action));
        steps.extend(verification_steps(action));
        steps.extend(verification_steps(action));
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        let completed = manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap();
        assert!(github.steps.is_empty());
        assert_eq!(completed.registry_commit, journal.registry_commit);
        assert_eq!(completed.status, "completed");
        assert_eq!(read_json(&path).unwrap()["repository_results"]["NEW"], "completed");
    }

    #[test]
    fn interrupted_creation_resumes_without_create_or_completed_registry_writes() {
        let (_temp, mut manager, state, mut remote, mut journal, old_path) = lifecycle_fixture();
        let mut done = remote.actions[0].clone();
        done.repo_id = "DONE".into();
        done.baseline["remote_url"] = json!("https://github.com/Org/DONE.git");
        remote.actions.push(done.clone());
        remote.identity_sha256 = lifecycle_identity(&remote).unwrap();
        journal.preview_identity_sha256 = canonical_sha256(&json!({"state":state.identity_sha256,"remote":remote.identity_sha256}));
        journal.operation_id = format!("curriculum-update-{}", &journal.preview_identity_sha256[..20]);
        let path = old_path.parent().unwrap().join(format!("{}.update.json", journal.operation_id));
        journal.preview_path = path.with_extension("update-preview.json").to_string_lossy().into_owned();
        journal.repository_results.insert("DONE".into(), "completed".into());
        save_update_journal(&path, &journal).unwrap();
        let action = &remote.actions[0];
        let mut steps = creation_steps(action);
        steps.extend(readiness_steps(action, "template-sha", "template"));
        steps.push(api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("template-sha", "template")));
        steps.push(api_error("PATCH", "repos/Org/NEW", 422));
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        assert!(manager.execute_remote_sync_with(&state, &remote, &mut github).is_err());
        assert!(github.steps.is_empty());
        let failed = read_json(&path).unwrap();
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["repository_results"]["NEW"], "created:101");
        for message in ["exit code: 1", "(HTTP 422)", "original-response-422", "default_branch"] {
            assert!(failed["error"].as_str().unwrap().contains(message));
        }
        let mut steps = vec![api_step("GET", "repos/Org/NEW", lifecycle_metadata(action))];
        steps.extend(readiness_steps(action, "template-sha", "template"));
        steps.extend(synchronize_steps(action));
        steps.extend(verification_steps(action));
        steps.extend(verification_steps(action));
        steps.extend(verification_steps(&done));
        github.steps = steps.into();
        let completed = manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap();
        assert!(github.steps.is_empty());
        assert_eq!(completed.registry_commit, journal.registry_commit);
        assert_eq!(completed.repository_results["DONE"], "completed");
        assert!(completed.error.is_none());
    }

    #[test]
    fn unknown_same_name_and_changed_readme_refuse_all_remote_writes() {
        for checkpoint in [false, true] {
            let (_temp, mut manager, state, remote, mut journal, path) = lifecycle_fixture();
            let action = &remote.actions[0];
            let mut steps = vec![api_step("GET", "repos/Org/NEW", lifecycle_metadata(action))];
            if checkpoint {
                journal.repository_results.insert("NEW".into(), "created:101".into());
                save_update_journal(&path, &journal).unwrap();
                steps.extend(readiness_steps(action, "maintainer-sha", "concurrent maintainer content"));
            }
            let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
            let error = manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap_err();
            assert!(error.to_string().contains(if checkpoint { "README 已变化" } else { "缺少本事务创建凭据" }));
            assert!(github.steps.is_empty());
            assert_eq!(read_json(&path).unwrap()["status"], "failed");
        }
    }

    #[test]
    fn permanent_github_errors_are_not_retried_and_keep_raw_journal_response() {
        for failed_method in ["POST", "GET", "PUT"] {
            let (_temp, mut manager, state, remote, _journal, path) = lifecycle_fixture();
            let action = &remote.actions[0];
            let mut steps = creation_steps(action);
            if failed_method == "POST" {
                *steps.last_mut().unwrap() = api_error("POST", "repos/Org/template/generate", 403);
            } else if failed_method == "GET" {
                steps.extend([api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)),api_error("GET", "repos/Org/NEW/commits/main", 403)]);
            } else {
                steps.extend(readiness_steps(action, "template-sha", "template"));
                steps.extend(synchronize_steps(action));
                *steps.last_mut().unwrap() = api_error("PUT", "repos/Org/NEW/contents/README.md", 403);
            }
            let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
            assert!(manager.execute_remote_sync_with(&state, &remote, &mut github).is_err());
            assert!(github.steps.is_empty());
            let failed = read_json(&path).unwrap();
            for message in ["exit code: 1", "(HTTP 403)", "original-response-403"] {
                assert!(failed["error"].as_str().unwrap().contains(message));
            }
        }
    }

    #[test]
    fn template_initialization_timeout_preserves_creation_checkpoint_without_patch() {
        let (_temp, mut manager, state, remote, _journal, path) = lifecycle_fixture();
        let action = &remote.actions[0];
        let mut steps = creation_steps(action);
        for attempt in 0..31 {
            steps.extend([api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)),api_error("GET", "repos/Org/NEW/commits/main", 404)]);
            if attempt < 30 { steps.push(api_step("WAIT", "", Value::Null)); }
        }
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        assert!(manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap_err().to_string().contains("就绪超时"));
        assert!(github.steps.is_empty());
        assert_eq!(read_json(&path).unwrap()["repository_results"]["NEW"], "created:101");
    }

    #[test]
    fn legacy_empty_repository_failure_requires_all_adoption_evidence() {
        for invalid in ["none", "path", "time", "description", "private", "template", "parents", "readme"] {
            let temporary = TempDir::new().unwrap();
            let path = temporary.path().join("journal.json");
            let action = lifecycle_action();
            let mut journal = UpdateExecutionJournal {
                status:"failed".into(),stage:"repositories".into(),created_at:"2026-09-07T13:50:00Z".into(),updated_at:"2026-09-07T13:55:00Z".into(),
                error:Some(format!("无法更新仓库设置：{}（{}，exit code: 1）\ngh: Validation Failed (HTTP 422)\nCannot update default branch for an empty repository. default_branch", action.title, action.repo_id)),
                ..UpdateExecutionJournal::default()
            };
            save_update_journal(&path, &journal).unwrap();
            let before = fs::read(&path).unwrap();
            let mut metadata = lifecycle_metadata(&action);
            match invalid {
                "path" => metadata["full_name"] = json!("Other/NEW"),
                "time" => metadata["created_at"] = json!("2026-09-01T00:00:00Z"),
                "description" => metadata["description"] = json!("someone else"),
                "private" => metadata["private"] = json!(true),
                "template" => metadata["template_repository"]["full_name"] = json!("Org/other-template"),
                _ => {}
            }
            let mut steps = vec![api_step("GET", "repos/Org/NEW", metadata)];
            if ["none", "parents", "readme"].contains(&invalid) {
                let mut commit = lifecycle_commit();
                if invalid == "parents" { commit["parents"] = json!([{"sha":"other"}]); }
                steps.push(api_step("GET", "repos/Org/NEW/commits/main", commit));
                if invalid != "parents" {
                    steps.push(api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme(if invalid == "readme" { "changed" } else { "template-sha" }, "template")));
                }
            }
            let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
            let result = recover_legacy_creation(&mut github, "Org", &[action], &mut journal, &path);
            assert_eq!(result.is_ok(), invalid == "none");
            assert!(github.steps.is_empty());
            if invalid == "none" {
                assert_eq!(read_json(&path).unwrap()["repository_results"]["NEW"], "created:101");
            } else {
                assert_eq!(fs::read(&path).unwrap(), before);
            }
        }
    }

    #[test]
    fn frozen_identity_failure_does_not_clear_failed_journal() {
        let (_temp, mut manager, state, mut remote, _journal, path) = lifecycle_fixture();
        let before = fs::read(&path).unwrap();
        remote.actions[0].description = "tampered".into();
        let mut github = ScriptedGithub { steps:Default::default(),journal_path:path.clone() };
        assert!(manager.execute_remote_sync_with(&state, &remote, &mut github).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn non_template_creation_writes_real_content_before_setting_default_branch() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("journal.json");
        let mut journal = UpdateExecutionJournal { status:"applying".into(),..UpdateExecutionJournal::default() };
        save_update_journal(&path, &journal).unwrap();
        let mut action = lifecycle_action();
        action.template_repository = None;
        action.baseline["readme_sha"] = Value::Null;
        let expected = action.readme.as_ref().unwrap();
        let mut initial_put = api_step("PUT", "repos/Org/NEW/contents/README.md", json!({}));
        initial_put.body = Some(json!({"message":"chore(curriculum): 初始化完整课程代码映射","branch":action.default_branch,
            "content":base64::engine::general_purpose::STANDARD.encode(expected.as_bytes())}));
        let mut steps = vec![api_error("GET", "repos/Org/NEW", 404),api_step("POST", "orgs/Org/repos", lifecycle_metadata(&action)),
            api_step("GET", "repos/Org/NEW", lifecycle_metadata(&action)),api_error("GET", "repos/Org/NEW/commits/main", 409),
            api_error("GET", "repos/Org/NEW/contents/README.md?ref=main", 404),initial_put,api_step("WAIT", "", Value::Null)];
        steps.extend(readiness_steps(&action, "target-sha", expected));
        steps.extend([api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("target-sha", expected)),
            api_step("PATCH", "repos/Org/NEW", lifecycle_metadata(&action)),
            api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("target-sha", expected))]);
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        apply_repository_lifecycle(&mut github, "Org", &action, &mut journal, &path).unwrap();
        assert!(github.steps.is_empty());
    }

    #[test]
    fn legacy_creation_resumes_through_executor_and_persists_applying_before_writes() {
        let (_temp, mut manager, state, remote, mut journal, path) = lifecycle_fixture();
        let action = &remote.actions[0];
        journal.error = Some(format!("无法更新仓库设置：{}（{}，exit code: 1）\ngh: Validation Failed (HTTP 422)\nCannot update default branch for an empty repository. default_branch", action.title, action.repo_id));
        save_update_journal(&path, &journal).unwrap();
        let mut steps = readiness_steps(action, "template-sha", "template");
        steps.push(api_step("GET", "repos/Org/NEW", lifecycle_metadata(action)));
        steps.extend(readiness_steps(action, "template-sha", "template"));
        steps.extend(synchronize_steps(action));
        steps.extend(verification_steps(action));
        steps.extend(verification_steps(action));
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        let completed = manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap();
        assert!(github.steps.is_empty());
        assert_eq!(completed.registry_commit, journal.registry_commit);
        assert_eq!(completed.repository_results["NEW"], "completed");
    }

    #[test]
    fn readme_change_between_settings_and_put_is_not_overwritten() {
        let (_temp, mut manager, state, remote, _journal, path) = lifecycle_fixture();
        let action = &remote.actions[0];
        let mut steps = creation_steps(action);
        steps.extend(readiness_steps(action, "template-sha", "template"));
        steps.extend([api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("template-sha", "template")),
            api_step("PATCH", "repos/Org/NEW", lifecycle_metadata(action)),
            api_step("GET", "repos/Org/NEW/contents/README.md?ref=main", lifecycle_readme("maintainer-sha", "new maintainer text"))]);
        let mut github = ScriptedGithub { steps:steps.into(),journal_path:path.clone() };
        assert!(manager.execute_remote_sync_with(&state, &remote, &mut github).unwrap_err().to_string().contains("README 已变化"));
        assert!(github.steps.is_empty());
        assert_eq!(read_json(&path).unwrap()["status"], "failed");
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        run_git(cwd, args, None, &[]).unwrap()
    }

    fn seed_registry(root: &Path) -> PathBuf {
        let remote = root.join("registry.git");
        git(
            root,
            &[
                "init",
                "--bare",
                remote.file_name().unwrap().to_str().unwrap(),
            ],
        );
        let work = root.join("registry-work");
        fs::create_dir_all(&work).unwrap();
        git(&work, &["init"]);
        git(&work, &["config", "user.name", "Registry Test"]);
        git(&work, &["config", "user.email", "registry@example.invalid"]);
        fs::write(work.join("README.md"), "registry").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "seed"]);
        git(&work, &["branch", "-M", "main"]);
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["push", "origin", "main"]);
        remote
    }

    #[test]
    fn github_revision_api_only_accepts_exact_https_repository_urls() {
        assert_eq!(github_repository_path("https://github.com/HIT-Fireworks/COURSE-A.git"), Some("HIT-Fireworks/COURSE-A".into()));
        for remote in ["https://github.com.evil.test/HIT-Fireworks/COURSE-A.git", "https://user@github.com/HIT-Fireworks/COURSE-A", "https://github.com/HIT-Fireworks/COURSE-A/tree/main", "http://github.com/HIT-Fireworks/COURSE-A", "https://github.com/HIT-Fireworks/COURSE-A?token=secret"] {
            assert!(github_repository_path(remote).is_none());
        }
    }

    #[test]
    fn readme_mapping_updates_preserve_crlf_and_maintainer_notes() {
        let generated = crate::repository_metadata::readme(&BTreeMap::from([("A2".into(), "新课程".into())])).unwrap();
        let original = "# 自定义说明\r\n\r\n保持原文。\r\n\r\n## 课程代码与原始课程名\r\n\r\n| 课程代码 | 原始课程名 |\r\n|---|---|\r\n| `A1` | 旧课程 |\r\n\r\n维护者备注不能丢失。\r\n\r\n### 特别说明\r\n保留子章节。\r\n\r\n## 贡献者\r\n保留贡献名单。\r\n";
        let updated = merge_managed_course_readme(Some(original), &generated).unwrap();
        assert!(updated.starts_with("# 自定义说明\r\n\r\n保持原文。\r\n\r\n"));
        assert!(updated.contains("| `A2` | 新课程 |"));
        assert!(!updated.contains("| `A1` | 旧课程 |"));
        assert!(updated.ends_with("维护者备注不能丢失。\r\n\r\n### 特别说明\r\n保留子章节。\r\n\r\n## 贡献者\r\n保留贡献名单。\r\n"));
        assert_eq!(merge_managed_course_readme(Some(&updated), &generated).unwrap(), updated);
    }

    #[test]
    fn registry_dynamic_tree_pushes_and_verifies() {
        let (temp, manager) = curriculum_rebuild_tests::fixture();
        let registry = seed_registry(temp.path());
        let snapshot = CandidateSnapshot {
            generated_at: "now".into(),
            base_url: "test".into(),
            plans: vec![CandidatePlan {
                plan_id: "plan-a".into(),
                info: json!({"major_name":"计算机科学与技术","school_name":"计算机学院"}),
                courses: vec![json!({"course_code":"A1","course_name":"程序设计"})],
            }],
        };
        let mut preview = manager
            .rebuild_from_snapshot(&snapshot, &BTreeMap::new())
            .unwrap();
        finalize_repository_preview(&mut preview, &manager.workspace_identity()).unwrap();
        let files = registry_dynamic_tree(&preview).unwrap();
        assert!(files.contains_key("repository-manifest.json"));
        assert!(files
            .keys()
            .any(|path| path.starts_with("curriculum/plans/")));
        let plan = RegistrySyncPlan {
            remote_url: registry.to_string_lossy().to_string(),
            baseline: remote_revision(&registry.to_string_lossy()).unwrap(),
            identity_sha256: canonical_sha256(&json!(files)),
            files,
        };
        let commit = sync_registry(&plan, "test-update").unwrap();
        assert!(is_hex(&commit, 40));
        verify_registry(&plan, Some(&commit)).unwrap();
    }

    #[test]
    fn full_update_execution_syncs_registry_and_local_state() {
        let (temp, mut manager) = curriculum_rebuild_tests::fixture();
        let registry = seed_registry(temp.path());
        manager = manager.with_registry_remote(registry.to_string_lossy());
        let snapshot = CandidateSnapshot {
            generated_at: "now".into(),
            base_url: "test".into(),
            plans: vec![CandidatePlan {
                plan_id: "plan-a".into(),
                info: json!({"major_name":"计算机科学与技术","school_name":"计算机学院"}),
                courses: vec![json!({"course_code":"A1","course_name":"程序设计"})],
            }],
        };
        let mut preview = manager
            .rebuild_from_snapshot(&snapshot, &BTreeMap::new())
            .unwrap();
        finalize_repository_preview(&mut preview, &manager.workspace_identity()).unwrap();
        let remote_preview = manager.plan_remote_sync(&preview).unwrap();
        let journal = manager
            .execute_remote_sync(&preview, &remote_preview)
            .unwrap();
        assert_eq!(journal.status, "completed");
        assert!(journal.completed_stages.contains(&"registry".to_string()));
        assert!(journal
            .completed_stages
            .contains(&"local-state".to_string()));
        assert_eq!(
            manager.manifest["curriculum_records"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn update_journal_persists_preview_bundle_for_restart_verification() {
        let (temp, mut manager) = curriculum_rebuild_tests::fixture();
        let registry = seed_registry(temp.path());
        manager = manager.with_registry_remote(registry.to_string_lossy());
        let snapshot = CandidateSnapshot {
            generated_at: "now".into(),
            base_url: "test".into(),
            plans: vec![CandidatePlan {
                plan_id: "plan-a".into(),
                info: json!({"major_name":"计算机科学与技术","school_name":"计算机学院"}),
                courses: vec![json!({"course_code":"A1","course_name":"程序设计"})],
            }],
        };
        let mut preview = manager
            .rebuild_from_snapshot(&snapshot, &BTreeMap::new())
            .unwrap();
        finalize_repository_preview(&mut preview, &manager.workspace_identity()).unwrap();
        let remote_preview = manager.plan_remote_sync(&preview).unwrap();
        let journal = manager
            .execute_remote_sync(&preview, &remote_preview)
            .unwrap();
        assert!(Path::new(&journal.preview_path).is_file());
        let reloaded =
            Manager::new(manager.workspace()).with_remote_template(manager.remote_template.clone());
        let journals = reloaded.update_journals().unwrap();
        assert_eq!(journals.len(), 1);
        reloaded.verify_update_journal(&journals[0]).unwrap();
    }
}

#[cfg(test)]
#[path = "state_update_tests.rs"]
mod state_update_tests;
fn repository_infrastructure(path: &str) -> bool {
    matches!(path, "README.md" | "LICENSE" | "repository.toml" | ".gitattributes" | ".gitignore")
        || path.starts_with(".github/")
}

fn root_metadata_kind(path: &str) -> Option<&'static str> {
    match path.to_ascii_lowercase().as_str() {
        "readme.md" | "readme" => Some("readme"),
        "license" | "license.md" | "copying" => Some("license"),
        _ => None,
    }
}

fn source_dependency_boundaries(
    object_repo: &Path,
    head: &str,
    tree: &BTreeMap<String, (String, String)>,
    _manifest: &Value,
) -> Result<Vec<BTreeSet<String>>> {
    let markers = ["Cargo.toml", "package.json", "pyproject.toml", "setup.py", "go.mod"];
    let mut roots = BTreeSet::new();
    let mut result = Vec::new();
    for path in tree.keys() {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        if markers.contains(&name) { roots.insert(parent.to_string()); }
        if name.ends_with(".exe") || name.ends_with(".dll") {
            if let Some((package, "bin")) = parent.rsplit_once('/') { roots.insert(package.to_string()); }
        }
        if !name.to_ascii_lowercase().ends_with(".md") { continue; }
        let content = run_git(object_repo, &["cat-file", "blob", &format!("{head}:{path}")], None, &[])?;
        let mut references = Vec::new();
        for part in content.split("](").skip(1) {
            if let Some((target, _)) = part.split_once(')') { references.push(target.trim().trim_matches(['<', '>']).split_whitespace().next().unwrap_or("")); }
        }
        for quote in ["src=\"", "src='"] {
            for part in content.split(quote).skip(1) {
                if let Some(target) = part.split(quote.chars().last().unwrap()).next() { references.push(target); }
            }
        }
        let base = url::Url::parse(&format!("https://repository.invalid/{path}"))?;
        let mut boundary = BTreeSet::from([path.clone()]);
        for reference in references {
            if reference.is_empty() || reference.starts_with('#') || reference.starts_with('/') || reference.contains("://") || reference.starts_with("data:") { continue; }
            let Ok(target) = base.join(reference) else { continue; };
            let encoded = target.path().trim_start_matches('/').replace('+', "%2B");
            let decoded = url::form_urlencoded::parse(format!("path={encoded}").as_bytes()).next().map(|(_, value)| value.into_owned()).unwrap_or_default();
            if tree.contains_key(&decoded) { boundary.insert(decoded); }
            else { boundary.extend(tree.keys().filter(|candidate| candidate.starts_with(&format!("{decoded}/"))).cloned()); }
        }
        if boundary.len() > 1 { result.push(boundary); }
    }
    for root in roots {
        let boundary = tree.keys().filter(|path| root.is_empty() || path.starts_with(&format!("{root}/"))).cloned().collect::<BTreeSet<_>>();
        if boundary.len() > 1 { result.push(boundary); }
    }
    Ok(result)
}

fn validate_resource_layout(manifest: &Value, routes: &Value) -> Result<()> {
    let Some(layout) = manifest.pointer("/policy/resource_layout").and_then(Value::as_object) else { return Ok(()); };
    if layout.get("allow_new_root_categories").and_then(Value::as_bool) != Some(false) { return Ok(()); }
    let categories: HashSet<_> = layout.get("categories").and_then(Value::as_array).context("资料分类规则缺失")?
        .iter().filter_map(Value::as_str).collect();
    if categories.is_empty() { bail!("资料分类不得为空") }
    for file in array_at(routes, "files")? {
        let path = string_field(file, "path");
        if repository_infrastructure(path) { continue; }
        let Some((category, _)) = path.split_once('/') else { bail!("资料必须放入预设分类：{path}"); };
        if !categories.contains(category) { bail!("不得自行新增根级分类：{category}") }
    }
    Ok(())
}

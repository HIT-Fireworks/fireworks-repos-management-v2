use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;
use unicode_normalization::UnicodeNormalization;

const DEFAULT_MANIFEST: &str = "data/repository-manifest.no-collection.v4.json";
const DEFAULT_TOPOLOGY: &str = "config/repository-topology.v4.json";
const DEFAULT_ROUTES: &str = "config/repository-file-routes.v4.json";
const DEFAULT_OPERATIONS: &str = "data/repository-management-operations";
const DEFAULT_REMOTE_TEMPLATE: &str = "https://github.com/{organization}/{repo_id}.git";

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
    pub member_resource_group_ids: Vec<String>,
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
    pub resource_group_ids: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SemanticGroup {
    pub internal_id: String,
    pub title: String,
    pub course_names: Vec<String>,
    pub course_codes: Vec<String>,
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
    pub groups: Vec<SemanticGroup>,
    pub loose_files: Vec<LooseFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemStatus {
    pub offline_ready: bool,
    pub git_available: bool,
    pub github_logged_in: bool,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct Manager {
    workspace: PathBuf,
    manifest_path: PathBuf,
    topology_path: PathBuf,
    routes_path: PathBuf,
    operations_path: PathBuf,
    remote_template: String,
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
            manifest: Value::Null,
            topology: Value::Null,
            routes: Value::Null,
        }
    }

    pub fn with_remote_template(mut self, template: impl Into<String>) -> Self {
        self.remote_template = template.into();
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
        Ok(())
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
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
        let row = self
            .repository_rows()?
            .into_iter()
            .find(|row| row.repo_id == repo_id)
            .context("没有找到这份资料")?;
        if !row.inventory_complete {
            bail!("这份资料还没有完成文件清点，暂时不能拆分。")
        }
        let topology_groups = repositories(&self.topology)?
            .get(repo_id)
            .map(|value| string_array(value, "member_resource_group_ids"))
            .unwrap_or_default();
        let group_ids = if topology_groups.is_empty() {
            self.manifest_repository(repo_id)
                .map(|value| string_array(value, "member_resource_group_ids"))
                .unwrap_or_default()
        } else {
            topology_groups
        };
        let resource_groups = self.resource_group_index();
        let repo_files: Vec<_> = array_at(&self.routes, "files")?
            .iter()
            .filter(|value| string_field(value, "repo_id") == repo_id)
            .cloned()
            .collect();
        let mut groups = Vec::new();
        for id in &group_ids {
            let Some(group) = resource_groups.get(id) else {
                continue;
            };
            let codes = string_array(group, "course_codes");
            let names = string_array(group, "course_names");
            let mut assigned = Vec::new();
            let code_set: HashSet<_> = codes.iter().map(String::as_str).collect();
            for file in &repo_files {
                let file_codes = string_array(file, "course_codes");
                let matches = group_ids
                    .iter()
                    .filter(|candidate| {
                        resource_groups
                            .get(*candidate)
                            .map(|value| {
                                let candidate_codes: HashSet<_> =
                                    string_array(value, "course_codes").into_iter().collect();
                                file_codes.iter().any(|code| candidate_codes.contains(code))
                            })
                            .unwrap_or(false)
                    })
                    .count();
                if matches == 1
                    && file_codes
                        .iter()
                        .any(|code| code_set.contains(code.as_str()))
                {
                    assigned.push(file.clone());
                }
            }
            groups.push(SemanticGroup {
                internal_id: id.to_string(),
                title: string_field(group, "display_name").to_string(),
                course_names: names,
                course_codes: codes,
                file_count: assigned.len(),
                bytes: assigned
                    .iter()
                    .map(|value| value.get("size").and_then(Value::as_u64).unwrap_or(0))
                    .sum(),
                sample_paths: assigned
                    .iter()
                    .filter_map(|value| value.get("path").and_then(Value::as_str))
                    .take(5)
                    .map(ToOwned::to_owned)
                    .collect(),
            });
        }
        groups.sort_by(|left, right| left.title.cmp(&right.title));
        let loose_files = repo_files
            .iter()
            .filter(|file| {
                let file_codes = string_array(file, "course_codes");
                let matches = groups
                    .iter()
                    .filter(|group| {
                        let codes: HashSet<_> =
                            group.course_codes.iter().map(String::as_str).collect();
                        file_codes.iter().any(|code| codes.contains(code.as_str()))
                    })
                    .count();
                matches != 1
            })
            .map(|file| LooseFile {
                internal_path: string_field(file, "path").to_string(),
                title: friendly_path(string_field(file, "path")),
                size: file.get("size").and_then(Value::as_u64).unwrap_or(0),
            })
            .collect();
        Ok(SplitOptions {
            source_repo_id: row.repo_id,
            source_title: row.display_name,
            groups,
            loose_files,
        })
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
            let topology_codes = string_array(topology, "course_codes");
            let codes = if topology_codes.is_empty() {
                manifest
                    .map(|value| string_array(value, "course_codes"))
                    .unwrap_or_default()
            } else {
                topology_codes
            };
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
            let topology_groups = string_array(topology, "member_resource_group_ids");
            let member_groups = if topology_groups.is_empty() {
                manifest
                    .map(|value| string_array(value, "member_resource_group_ids"))
                    .unwrap_or_default()
            } else {
                topology_groups
            };
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
                member_resource_group_ids: member_groups,
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

    fn resource_group_index(&self) -> HashMap<String, &Value> {
        array_at(&self.manifest, "resource_groups")
            .unwrap_or(&[])
            .iter()
            .filter_map(|value| {
                value
                    .get("resource_group_id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_string(), value))
            })
            .collect()
    }

    fn prepare_plan(&self, mut plan: Value) -> Result<PlannedOperation> {
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
        let identity = plan_identity_sha256(&plan);
        plan["core"]["plan_identity_sha256"] = json!(identity);
        Ok(plan_result(PathBuf::new(), plan))
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
        let topology_groups = string_array(&source, "member_resource_group_ids");
        let manifest_groups: BTreeSet<_> = if topology_groups.is_empty() {
            self.manifest_repository(source_repo_id)
                .map(|value| string_array(value, "member_resource_group_ids"))
                .unwrap_or_default()
                .into_iter()
                .collect()
        } else {
            topology_groups.into_iter().collect()
        };
        let mut group_target = HashMap::new();
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
            if target.resource_group_ids.is_empty() && target.paths.is_empty() {
                bail!("每份目标资料至少要包含一组课程或一个文件。")
            }
            for id in &target.resource_group_ids {
                if !manifest_groups.contains(id) {
                    bail!("选择中包含不属于源资料的课程组。")
                }
                if group_target
                    .insert(id.clone(), target.repo_id.clone())
                    .is_some()
                {
                    bail!("同一课程组被分到了两份资料。")
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
        if group_target.keys().cloned().collect::<BTreeSet<_>>() != manifest_groups {
            bail!("还有课程组没有分配，请完成全部选择。")
        }
        let resource_groups = self.resource_group_index();
        let mut group_codes = HashMap::<String, HashSet<String>>::new();
        for id in &manifest_groups {
            group_codes.insert(
                id.clone(),
                resource_groups
                    .get(id)
                    .map(|value| string_array(value, "course_codes").into_iter().collect())
                    .unwrap_or_default(),
            );
        }
        let mut after_routes = self.routes.clone();
        let route_files = after_routes
            .get_mut("files")
            .and_then(Value::as_array_mut)
            .context("路由文件损坏")?;
        let mut moves = Vec::new();
        let mut source_paths = HashSet::new();
        let mut routed_counts = HashMap::<String, usize>::new();
        for file in route_files.iter_mut() {
            if string_field(file, "repo_id") != source_repo_id {
                continue;
            }
            let path = string_field(file, "path").to_string();
            source_paths.insert(path.clone());
            let codes = string_array(file, "course_codes");
            let matching_groups = manifest_groups
                .iter()
                .filter(|id| {
                    group_codes
                        .get(*id)
                        .is_some_and(|group| codes.iter().any(|code| group.contains(code)))
                })
                .collect::<Vec<_>>();
            let semantic_target = if matching_groups.len() == 1 {
                group_target.get(matching_groups[0]).cloned()
            } else {
                None
            };
            let explicit_target = path_target.get(&path).cloned();
            if let (Some(left), Some(right)) = (&semantic_target, &explicit_target) {
                if left != right {
                    bail!("文件“{}”的课程归属与手动选择冲突。", friendly_path(&path))
                }
            }
            let target = explicit_target
                .or(semantic_target)
                .with_context(|| format!("文件“{}”还没有选择去向。", friendly_path(&path)))?;
            file["repo_id"] = json!(target);
            moves.push(json!({
                "source_repo_id": source_repo_id,
                "source_path": path,
                "target_repo_id": target,
                "target_path": path
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
                let target = manifest_groups
                    .iter()
                    .find_map(|id| {
                        group_codes
                            .get(id)
                            .is_some_and(|codes| codes.contains(code))
                            .then(|| group_target.get(id).cloned())
                            .flatten()
                    })
                    .context("课程代码没有分配到目标资料")?;
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
        if path_target.keys().any(|path| !source_paths.contains(path)) {
            bail!("选择中包含不存在的文件。")
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
                    "member_resource_group_ids": target.resource_group_ids,
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
        let mut relocations = Vec::new();
        for indices in by_path.values() {
            let mut sorted = indices.clone();
            sorted.sort_by_key(|index| string_field(&route_files[*index], "repo_id") != preferred);
            for (position, index) in sorted.into_iter().enumerate() {
                let original_repo = string_field(&route_files[index], "repo_id").to_string();
                let original_path = string_field(&route_files[index], "path").to_string();
                let mut target_path = original_path.clone();
                if position > 0 || occupied.contains(&target_path.to_lowercase()) {
                    target_path = format!("merged-from/{original_repo}/{original_path}");
                    let base = target_path.clone();
                    let mut suffix = 1;
                    while occupied.contains(&target_path.to_lowercase()) {
                        suffix += 1;
                        target_path = format!("{base}.conflict-{suffix}");
                    }
                    relocations.push(json!({
                        "source_repo_id":original_repo,
                        "source_path":original_path,
                        "target_path":target_path
                    }));
                }
                safe_path(&target_path)?;
                occupied.insert(target_path.to_lowercase());
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
        let mut member_groups = BTreeSet::new();
        for id in &sources {
            let topology_groups = repos
                .get(id)
                .map(|value| string_array(value, "member_resource_group_ids"))
                .unwrap_or_default();
            if topology_groups.is_empty() {
                if let Some(manifest) = self.manifest_repository(id) {
                    member_groups.extend(string_array(manifest, "member_resource_group_ids"));
                }
            } else {
                member_groups.extend(topology_groups);
            }
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
                "member_resource_group_ids":member_groups,
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
        if plan
            .pointer("/core/workspace_identity/manifest_sha256")
            .and_then(Value::as_str)
            != Some(manifest.as_str())
        {
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
        if Some(topology.as_str()) == before_topology && Some(routes.as_str()) == before_routes {
            "before".to_string()
        } else if Some(topology.as_str()) == after_topology
            && Some(routes.as_str()) == before_routes
        {
            "topology-applied".to_string()
        } else if Some(topology.as_str()) == after_topology && Some(routes.as_str()) == after_routes
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
                    "由薪火资料管理工具创建",
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
        if canonical_sha256(&read_json(&self.topology_path)?) != canonical_sha256(&after_topology) {
            atomic_json(&self.topology_path, &after_topology)?;
            add_stage(journal, "topology");
            atomic_json(journal_path, journal)?;
        }
        if canonical_sha256(&read_json(&self.routes_path)?) != canonical_sha256(&effective_routes) {
            atomic_json(&self.routes_path, &effective_routes)?;
            add_stage(journal, "routes");
            atomic_json(journal_path, journal)?;
        }
        validate_state(&after_topology, &effective_routes, false)?;
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
    let mut group_owner = HashMap::new();
    for (key, repo) in repos {
        safe_repo_id(key)?;
        if string_field(repo, "repo_id") != key {
            bail!("资料索引键不一致")
        }
        let physical_id = string_field(repo, "physical_repository_id");
        if physical_id.is_empty() || !physical.insert(physical_id.to_string()) {
            bail!("资料物理身份缺失或重复")
        }
        for group in string_array(repo, "member_resource_group_ids") {
            if let Some(prior) = group_owner.insert(group.clone(), key.clone()) {
                if prior != *key {
                    bail!("课程资料组同时属于多个仓库")
                }
            }
        }
    }
    let mut paths = HashSet::new();
    for file in array_at(routes, "files")? {
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

fn remote_revision(remote: &str) -> Result<Value> {
    if !remote.contains("://") && !Path::new(remote).exists() {
        return Ok(json!({"exists":false,"head":null,"tree":null,"remote_url":remote}));
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
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("缺少数据文件：{}", path.display()))?
        .read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).with_context(|| format!("数据文件损坏：{}", path.display()))
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("数据路径无效")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn canonical_sha256(value: &Value) -> String {
    let canonical = canonical_json(value);
    let bytes = serde_json::to_vec(&canonical).expect("canonical JSON");
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        _ => value.clone(),
    }
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
mod real_workspace_tests {
    use super::*;

    #[test]
    fn real_workspace_exposes_chinese_split_options_without_internal_input() {
        let mut manager = Manager::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        manager.reload().unwrap();
        let options = manager.split_options("COURSES-RA-531F0B625E8A").unwrap();
        assert_eq!(options.source_title, "材料科学与工程学院");
        assert_eq!(options.groups.len(), 6);
        assert!(options
            .groups
            .iter()
            .any(|group| group.title == "材料热力学"));
        assert!(options
            .groups
            .iter()
            .all(|group| !group.title.starts_with("exact-name-")));
        assert_eq!(
            options
                .groups
                .iter()
                .map(|group| group.file_count)
                .sum::<usize>()
                + options.loose_files.len(),
            10
        );
    }
}

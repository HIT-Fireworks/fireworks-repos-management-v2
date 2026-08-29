use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreResponse {
    pub ok: bool,
    pub mode: String,
    pub result: Option<Value>,
    #[serde(default)]
    pub errors: Vec<CoreFailure>,
    #[serde(default)]
    pub warnings: Vec<Value>,
    #[serde(default)]
    pub next_actions: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct CoreClient {
    workspace: PathBuf,
    organization: String,
    python: String,
    core: PathBuf,
}

impl CoreClient {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        let core = Self::resolve_core_path();
        let python = std::env::var("PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".to_string()
            } else {
                "python3".to_string()
            }
        });
        let organization = std::env::var("FIREWORKS_ORGANIZATION")
            .unwrap_or_else(|_| "HIT-Fireworks".to_string());
        Self {
            workspace,
            organization,
            python,
            core,
        }
    }

    fn resolve_core_path() -> PathBuf {
        if let Some(path) = std::env::var_os("FIREWORKS_MANAGER_CORE") {
            return PathBuf::from(path);
        }
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let development = manifest_root
            .parent()
            .unwrap_or(&manifest_root)
            .join("scripts/fireworks_manager_core.py");
        if development.exists() {
            return development;
        }
        if let Ok(executable) = std::env::current_exe() {
            for ancestor in executable.ancestors() {
                let candidate = ancestor.join("scripts/fireworks_manager_core.py");
                if candidate.exists() {
                    return candidate;
                }
            }
        }
        PathBuf::from("scripts/fireworks_manager_core.py")
    }

    pub fn with_core_path(mut self, core: impl Into<PathBuf>) -> Self {
        self.core = core.into();
        self
    }

    pub fn with_organization(mut self, organization: impl Into<String>) -> Self {
        self.organization = organization.into();
        self
    }

    pub fn invoke(
        &self,
        family: &str,
        kind: &str,
        arguments: Value,
        confirmation: Option<&str>,
    ) -> Result<CoreResponse> {
        let request = json!({
            "schema_version": 1,
            "request_id": "tui",
            "command": {"family": family, "kind": kind, "arguments": arguments},
            "context": {
                "workspace": self.workspace,
                "organization": self.organization,
                "actor": "tui"
            },
            "confirmation": confirmation
        });
        let mut child = Command::new(&self.python)
            .arg(&self.core)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("invoke")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("无法启动 JSON 核心：{}", self.core.display()))?;
        {
            let stdin = child.stdin.as_mut().context("无法写入核心 stdin")?;
            serde_json::to_writer(stdin, &request).context("序列化核心请求失败")?;
        }
        let output = child.wait_with_output().context("等待 JSON 核心失败")?;
        let response: CoreResponse = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "核心输出不是 JSON：{}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
        Ok(response)
    }

    fn successful_result(
        &self,
        family: &str,
        kind: &str,
        arguments: Value,
        confirmation: Option<&str>,
    ) -> Result<Value> {
        let response = self.invoke(family, kind, arguments, confirmation)?;
        if !response.ok {
            let error = response.errors.first();
            bail!(
                "{}: {}",
                error.map(|item| item.code.as_str()).unwrap_or("core_failed"),
                error
                    .map(|item| item.message.as_str())
                    .unwrap_or("核心请求失败")
            );
        }
        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn inspect(&self) -> Result<Health> {
        serde_json::from_value(self.successful_result("query", "inspect", json!({}), None)?)
            .context("解析健康状态失败")
    }

    pub fn search(&self, term: &str) -> Result<Vec<RepositorySummary>> {
        let result = self.successful_result(
            "query",
            "search",
            json!({"term": term, "limit": 500}),
            None,
        )?;
        serde_json::from_value(
            result
                .get("repositories")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .context("解析仓库列表失败")
    }

    pub fn repository(&self, repo_id: &str) -> Result<RepositoryDetail> {
        serde_json::from_value(self.successful_result(
            "query",
            "repository",
            json!({"repo_id": repo_id}),
            None,
        )?)
        .context("解析仓库详情失败")
    }

    pub fn routes(&self, repo_id: Option<&str>) -> Result<RoutesSnapshot> {
        serde_json::from_value(self.successful_result(
            "query",
            "routes",
            json!({"repo_id": repo_id.unwrap_or(""), "limit": 0}),
            None,
        )?)
        .context("解析路由失败")
    }

    pub fn plans(&self) -> Result<Vec<PlanSummary>> {
        let result = self.successful_result("query", "plan", json!({}), None)?;
        serde_json::from_value(result.get("plans").cloned().unwrap_or_else(|| json!([])))
            .context("解析计划列表失败")
    }

    pub fn journals(&self) -> Result<Vec<JournalSummary>> {
        let result = self.successful_result("query", "journals", json!({}), None)?;
        serde_json::from_value(
            result
                .get("journals")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .context("解析 journal 列表失败")
    }

    pub fn plan_split(
        &self,
        source_repo_id: &str,
        targets: &[SplitTarget],
    ) -> Result<PlannedOperation> {
        serde_json::from_value(self.successful_result(
            "plan",
            "split",
            json!({"source_repo_id": source_repo_id, "targets": targets}),
            None,
        )?)
        .context("解析拆分计划失败")
    }

    pub fn plan_detail(&self, operation_id: &str) -> Result<PlannedOperation> {
        let result = self.successful_result(
            "query",
            "plan",
            json!({"operation_id": operation_id}),
            None,
        )?;
        let plan = result.get("plan").cloned().unwrap_or(Value::Null);
        let source_repo_ids = plan
            .pointer("/details/source_repository_heads")
            .and_then(Value::as_object)
            .map(|value| value.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let target_repo_ids = plan
            .pointer("/after/routes/unresolved_repository_heads")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let file_move_count = plan
            .pointer("/details/file_moves")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        Ok(PlannedOperation {
            path: result
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            plan,
            risk: json!({
                "remote_mutation": true,
                "source_repo_ids": source_repo_ids,
                "target_repo_ids": target_repo_ids,
                "file_move_count": file_move_count
            }),
        })
    }

    pub fn plan_merge(
        &self,
        source_repo_ids: &[String],
        target_repo_id: &str,
        display_name: &str,
    ) -> Result<PlannedOperation> {
        serde_json::from_value(self.successful_result(
            "plan",
            "merge",
            json!({
                "source_repo_ids": source_repo_ids,
                "target_repo_id": target_repo_id,
                "display_name": display_name
            }),
            None,
        )?)
        .context("解析合并计划失败")
    }

    pub fn apply(&self, plan: &PlannedOperation, confirmation: &str) -> Result<Value> {
        self.successful_result(
            "execute",
            "apply",
            json!({
                "plan": plan.path,
                "plan_identity_sha256": plan.identity()
            }),
            Some(confirmation),
        )
    }

    pub fn verify(&self, journal: &JournalSummary) -> Result<Value> {
        self.successful_result(
            "execute",
            "verify",
            json!({
                "journal": journal.path,
                "plan_identity_sha256": journal.plan_identity_sha256
            }),
            None,
        )
    }

    pub fn resume(&self, journal: &JournalSummary, confirmation: &str) -> Result<Value> {
        self.successful_result(
            "execute",
            "resume",
            json!({
                "journal": journal.path,
                "plan_identity_sha256": journal.plan_identity_sha256
            }),
            Some(confirmation),
        )
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn core_path(&self) -> &Path {
        &self.core
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    #[serde(skip)]
    pub client: CoreClient,
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
        Self::load_with_client(CoreClient::new(workspace))
    }

    pub fn load_with_client(client: CoreClient) -> Self {
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
            dashboard.logs.push(format!("加载失败：{error}"));
        }
        dashboard
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.health = self.client.inspect()?;
        self.repositories = self.client.search(&self.query)?;
        self.routes = self.client.routes(None)?;
        self.plans = self.client.plans()?;
        self.journals = self.client.journals()?;
        self.logs.push("已从 JSON 核心刷新完整工作台".to_string());
        Ok(())
    }

    pub fn search(&mut self, query: String) -> Result<()> {
        self.query = query;
        self.repositories = self.client.search(&self.query)?;
        self.logs
            .push(format!("搜索完成：{} 条", self.repositories.len()));
        Ok(())
    }

    pub fn detail(&self, index: usize) -> Result<RepositoryDetail> {
        let repository = self.repositories.get(index).context("未选择仓库")?;
        self.client.repository(&repository.repo_id)
    }

    pub fn validate(&self) -> Result<()> {
        if self.health.health != "healthy" {
            bail!(
                "状态不健康：{}",
                self.health
                    .health_message
                    .as_deref()
                    .unwrap_or("未知原因")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_summary_json_contract() {
        let value = json!({
            "repo_id": "A",
            "repo_type": "course",
            "display_name": "课程 A",
            "description": "课程 A",
            "course_codes": ["A1"],
            "course_names": ["课程 A"],
            "member_resource_group_ids": ["g"],
            "unowned_paths": ["README.md"],
            "file_count": 1,
            "bytes": 2,
            "head": null,
            "inventory_complete": false
        });
        let parsed: RepositorySummary = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.repo_id, "A");
        assert_eq!(parsed.unowned_paths, ["README.md"]);
    }

    #[test]
    fn external_workspace_does_not_change_core_location() {
        let client = CoreClient::new(PathBuf::from("external-state"));
        assert!(!client.core_path().starts_with(client.workspace()));
        assert!(client.core_path().ends_with("scripts/fireworks_manager_core.py"));
    }
}

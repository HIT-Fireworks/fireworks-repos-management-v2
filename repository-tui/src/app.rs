use anyhow::anyhow;
use std::collections::BTreeSet;
use std::io::{self, stdout};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Wrap};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthChar;

use crate::curriculum::Decision;
use crate::jwts::{CatalogOption, CrawlSelection, PlanKind};
use crate::state::{
    human_error, CourseAssignment, CourseRepositoryChoice, CurriculumReviewSummary, Dashboard,
    JournalSummary, PlannedOperation, RepositoryDetail, RepositoryLifecyclePreview,
    RepositorySummary, RepositorySyncPreview, SplitOptions, SplitTarget, SystemStatus,
    UpdateExecutionJournal, UpdateSession,
};

const HOME_ITEMS: [&str; 8] = [
    "从教务系统更新数据",
    "查看和搜索资料",
    "管理远端仓库",
    "合并几份资料",
    "拆分一份资料",
    "查看任务记录",
    "系统检查",
    "退出",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    UpdateLogin,
    UpdateKind,
    UpdateGrade,
    UpdateCollege,
    UpdateMajors,
    UpdateDiff,
    UpdateDiffDetail,
    UpdateAssignment,
    UpdateAssignmentSearch,
    UpdateNewRepository,
    UpdatePreview,
    RemotePreview,
    Browse,
    BrowseSearch,
    Detail,
    MergeSelect,
    MergeName,
    SplitSelect,
    SplitCount,
    SplitAssign,
    SplitName,
    Review,
    Tasks,
    TaskDetail,
    System,
    Help,
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewOrigin {
    Merge,
    Split,
}

#[derive(Debug, Clone)]
enum UnifiedTask {
    Repository(JournalSummary),
    Update(UpdateExecutionJournal),
    CurriculumReview(CurriculumReviewSummary),
}
type LoginRunner = fn(&AtomicBool) -> Result<UpdateSession, String>;

fn run_browser_login(cancelled: &AtomicBool) -> Result<UpdateSession, String> {
    let client = crate::browser_login::BrowserLogin::authenticated_client(
        crate::jwts::DEFAULT_HIT_BASE_URL,
        cancelled,
    )
    .map_err(|error| error.to_string())?;
    UpdateSession::from_client(client, PlanKind::Execution).map_err(|error| error.to_string())
}

struct UiState {
    dashboard: Dashboard,
    page: Page,
    home_index: usize,
    browse_index: usize,
    merge_index: usize,
    split_repo_index: usize,
    task_index: usize,
    detail: Option<RepositoryDetail>,
    selected_merge: BTreeSet<String>,
    split_options: Option<SplitOptions>,
    split_target_count: usize,
    split_item_index: usize,
    split_assignments: Vec<Option<usize>>,
    split_names: Vec<String>,
    split_name_index: usize,
    input: String,
    pending_plan: Option<PlannedOperation>,
    review_lines: Vec<String>,
    review_origin: Option<ReviewOrigin>,
    review_choice: usize,
    result_text: String,
    notice: String,
    update_session: Option<UpdateSession>,
    update_kind_index: usize,
    update_grade_index: usize,
    update_college_index: usize,
    update_major_index: usize,
    update_majors: Vec<CatalogOption>,
    selected_majors: BTreeSet<String>,
    update_diff_index: usize,
    diff_detail_scroll: u16,
    pending_assignments: Vec<CourseRepositoryChoice>,
    assignment_targets: Vec<RepositorySummary>,
    assignment_filter: String,
    assignment_index: usize,
    update_preview: Option<RepositorySyncPreview>,
    remote_preview: Option<RepositoryLifecyclePreview>,
    update_journals: Vec<UpdateExecutionJournal>,
    curriculum_reviews: Vec<CurriculumReviewSummary>,
    login_job: Option<LoginJob>,
    login_runner: LoginRunner,
}

struct LoginJob {
    cancelled: Arc<AtomicBool>,
    result: Receiver<Result<UpdateSession, String>>,
}

impl UiState {
    fn new(workspace: std::path::PathBuf) -> Self {
        Self::from_dashboard(Dashboard::load(workspace))
    }

    fn from_dashboard(dashboard: Dashboard) -> Self {
        let notice = dashboard
            .logs
            .last()
            .cloned()
            .unwrap_or_else(|| "请选择要做的事情".to_string());
        let update_journals = dashboard.client.update_journals().unwrap_or_default();
        let curriculum_reviews = dashboard.client.curriculum_reviews().unwrap_or_default();
        Self {
            dashboard,
            page: Page::Home,
            home_index: 0,
            browse_index: 0,
            merge_index: 0,
            split_repo_index: 0,
            task_index: 0,
            detail: None,
            selected_merge: BTreeSet::new(),
            split_options: None,
            split_target_count: 2,
            split_item_index: 0,
            split_assignments: Vec::new(),
            split_names: Vec::new(),
            split_name_index: 0,
            input: String::new(),
            pending_plan: None,
            review_lines: Vec::new(),
            review_origin: None,
            review_choice: 0,
            result_text: String::new(),
            notice,
            update_session: None,
            update_kind_index: 0,
            update_grade_index: 0,
            update_college_index: 0,
            update_major_index: 0,
            update_majors: Vec::new(),
            selected_majors: BTreeSet::new(),
            update_diff_index: 0,
            diff_detail_scroll: 0,
            pending_assignments: Vec::new(),
            assignment_targets: Vec::new(),
            assignment_filter: String::new(),
            assignment_index: 0,
            update_preview: None,
            remote_preview: None,
            update_journals,
            curriculum_reviews,
            login_job: None,
            login_runner: run_browser_login,
        }
    }

    fn refresh(&mut self) {
        match self.dashboard.refresh() {
            Ok(()) => self.notice = "资料已刷新".to_string(),
            Err(error) => self.notice = human_error(&error),
        }
        self.update_journals = self.dashboard.client.update_journals().unwrap_or_default();
        self.curriculum_reviews = self
            .dashboard
            .client
            .curriculum_reviews()
            .unwrap_or_default();
        self.clamp();
    }

    fn clamp(&mut self) {
        self.browse_index = self.browse_index.min(self.dashboard.repositories.len());
        self.merge_index = self.merge_index.min(self.merge_candidates().len());
        self.split_repo_index = self
            .split_repo_index
            .min(self.split_candidates().len().saturating_sub(1));
        self.task_index = self.task_index.min(self.tasks().len().saturating_sub(1));
        let existing: BTreeSet<_> = self
            .dashboard
            .repositories
            .iter()
            .map(|item| item.repo_id.clone())
            .collect();
        self.selected_merge.retain(|id| existing.contains(id));
    }

    fn tasks(&self) -> Vec<UnifiedTask> {
        let mut tasks = self
            .dashboard
            .journals
            .iter()
            .cloned()
            .map(UnifiedTask::Repository)
            .collect::<Vec<_>>();
        tasks.extend(
            self.update_journals
                .iter()
                .cloned()
                .map(UnifiedTask::Update),
        );
        tasks.extend(
            self.curriculum_reviews
                .iter()
                .cloned()
                .map(UnifiedTask::CurriculumReview),
        );
        tasks.sort_by(|left, right| task_updated(right).cmp(task_updated(left)));
        tasks
    }

    fn current_task(&self) -> Option<UnifiedTask> {
        self.tasks().get(self.task_index).cloned()
    }

    fn merge_candidates(&self) -> Vec<usize> {
        self.dashboard
            .repositories
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.inventory_complete.then_some(index))
            .collect()
    }

    fn split_candidates(&self) -> Vec<usize> {
        self.dashboard
            .repositories
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (item.inventory_complete
                    && item.course_codes.len() + item.unowned_paths.len() >= 2)
                    .then_some(index)
            })
            .collect()
    }

    fn selected_merge_repositories(&self) -> Vec<&RepositorySummary> {
        self.dashboard
            .repositories
            .iter()
            .filter(|item| self.selected_merge.contains(&item.repo_id))
            .collect()
    }

    fn clear_update(&mut self) {
        self.cancel_login();
        self.update_session = None;
        self.update_majors.clear();
        self.selected_majors.clear();
        self.update_preview = None;
        self.remote_preview = None;
        self.pending_assignments.clear();
        self.assignment_targets.clear();
        self.assignment_filter.clear();
        self.assignment_index = 0;
        self.diff_detail_scroll = 0;
        self.input.clear();
    }

    fn go_home(&mut self) {
        if let Some(plan) = self.pending_plan.take() {
            let _ = self.dashboard.client.discard_plan(&plan);
        }
        self.clear_update();
        self.page = Page::Home;
        self.detail = None;
        self.split_options = None;
        self.review_lines.clear();
        self.review_origin = None;
        self.notice = "请选择要做的事情".to_string();
    }

    fn open_home_item(&mut self) -> bool {
        match self.home_index {
            0 => self.start_login(),
            1 => {
                self.page = Page::Browse;
                self.browse_index = 0;
            }
            2 => self.open_remote_management(),
            3 => {
                self.page = Page::MergeSelect;
                self.merge_index = 0;
                self.selected_merge.clear();
            }
            4 => {
                self.page = Page::SplitSelect;
                self.split_repo_index = 0;
            }
            5 => {
                self.refresh();
                self.page = Page::Tasks;
                self.task_index = 0;
            }
            6 => self.page = Page::System,
            7 => return true,
            _ => {}
        }
        false
    }

    fn open_remote_management(&mut self) {
        match self
            .dashboard
            .client
            .current_repository_sync_preview()
            .and_then(|preview| {
                let remote = self.dashboard.client.plan_remote_sync(&preview)?;
                Ok((preview, remote))
            }) {
            Ok((state, remote)) => {
                self.update_preview = Some(state);
                self.remote_preview = Some(remote);
                self.page = Page::RemotePreview;
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn start_login(&mut self) {
        self.cancel_login();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let runner = self.login_runner;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(runner(&worker_cancelled));
        });
        self.login_job = Some(LoginJob {
            cancelled,
            result: receiver,
        });
        self.page = Page::UpdateLogin;
        self.notice = "正在打开隔离浏览器……登录完成后会自动继续。按 Esc 取消。".to_string();
    }

    fn cancel_login(&mut self) {
        if let Some(job) = self.login_job.take() {
            job.cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn poll_login(&mut self) {
        let result = match self.login_job.as_ref() {
            Some(job) => job.result.try_recv(),
            None => return,
        };
        match result {
            Ok(Ok(session)) => {
                self.login_job = None;
                self.update_session = Some(session);
                self.update_grade_index = 0;
                self.update_kind_index = 0;
                self.page = Page::UpdateKind;
                self.notice = "登录成功，请选择执行教学计划或培养方案。".to_string();
            }
            Ok(Err(error)) => {
                self.login_job = None;
                self.notice = human_error(&anyhow!(error));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.login_job = None;
                self.notice = "登录窗口连接已结束，请按 Enter 重试。".to_string();
            }
        }
    }

    fn choose_source_kind(&mut self) {
        let kind = if self.update_kind_index == 0 {
            PlanKind::Execution
        } else {
            PlanKind::Curriculum
        };
        let Some(session) = &mut self.update_session else {
            return;
        };
        let result = if session.kind == kind {
            Ok(())
        } else {
            self.dashboard.client.select_source_kind(session, kind)
        };
        match result {
            Ok(()) => {
                self.update_grade_index = 0;
                self.update_college_index = 0;
                self.update_majors.clear();
                self.selected_majors.clear();
                self.page = Page::UpdateGrade;
                self.notice = match kind {
                    PlanKind::Execution => "选择入学年级；执行计划与培养方案版本分别保存。",
                    PlanKind::Curriculum => "选择方案版本；版本年份不代表入学年级。",
                }
                .to_string();
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn choose_grade(&mut self) {
        let Some(session) = &self.update_session else {
            return;
        };
        if session.catalog.grades.is_empty() {
            self.notice = "教务系统没有返回可选年级".to_string();
            return;
        }
        self.update_college_index = 0;
        self.page = Page::UpdateCollege;
    }

    fn choose_college(&mut self) {
        let Some(session) = &self.update_session else {
            return;
        };
        let Some(grade) = session.catalog.grades.get(self.update_grade_index) else {
            return;
        };
        let Some(college) = session.catalog.colleges.get(self.update_college_index) else {
            return;
        };
        match self
            .dashboard
            .client
            .update_majors(session, &grade.code, &college.code)
        {
            Ok(majors) if majors.is_empty() => {
                self.update_majors.clear();
                self.selected_majors.clear();
                self.notice = "学校确认该版本或年级下暂无专业，请选择其他院系".to_string();
            }
            Ok(majors) => {
                self.update_majors = majors;
                self.update_major_index = 0;
                self.selected_majors.clear();
                self.page = Page::UpdateMajors;
                self.notice = "按 Enter 勾选专业；选择“开始抓取”进入下一步".to_string();
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn toggle_major(&mut self) {
        if self.update_major_index == self.update_majors.len() {
            self.start_update_crawl();
            return;
        }
        let Some(major) = self.update_majors.get(self.update_major_index) else {
            return;
        };
        if !self.selected_majors.remove(&major.code) {
            self.selected_majors.insert(major.code.clone());
        }
    }

    fn start_update_crawl(&mut self) {
        let Some(session) = &mut self.update_session else {
            return;
        };
        if self.selected_majors.is_empty() {
            self.notice = "请至少选择一个专业".to_string();
            return;
        }
        let Some(grade) = session.catalog.grades.get(self.update_grade_index).cloned() else {
            return;
        };
        let Some(college) = session
            .catalog
            .colleges
            .get(self.update_college_index)
            .cloned()
        else {
            return;
        };
        let selections = self
            .update_majors
            .iter()
            .filter(|major| self.selected_majors.contains(&major.code))
            .map(|major| CrawlSelection {
                kind: session.kind,
                grade: grade.code.clone(),
                college_code: college.code.clone(),
                college_name: college.name.clone(),
                major_code: major.code.clone(),
                major_name: major.name.clone(),
            })
            .collect::<Vec<_>>();
        self.notice = "正在从教务系统读取课程，请稍候……".to_string();
        match self
            .dashboard
            .client
            .stage_curriculum_update(session, selections)
        {
            Ok(()) => {
                self.update_diff_index = 0;
                self.page = Page::UpdateDiff;
                self.notice = "逐条选择接受教务变化或保留现状".to_string();
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn diff_len(&self) -> usize {
        self.update_session
            .as_ref()
            .and_then(|session| session.diff.as_ref())
            .map(|diff| diff.changes.len())
            .unwrap_or(0)
    }

    fn cycle_diff_decision(&mut self) {
        let Some(session) = &mut self.update_session else {
            return;
        };
        let Some(change) = session.change(self.update_diff_index).cloned() else {
            self.finish_diff_review();
            return;
        };
        let next = match session.decisions.decisions.get(&change.change_id) {
            None => Decision::Accept,
            Some(Decision::Accept) => Decision::Reject,
            Some(Decision::Reject) => Decision::Accept,
        };
        match session.set_decision(self.update_diff_index, next) {
            Ok(()) => self.persist_review(),
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn accept_all_changes(&mut self) {
        if let Some(session) = &mut self.update_session {
            if let Err(error) = session.accept_all() {
                self.notice = human_error(&error);
            } else {
                self.notice = "已选择接受全部教务变化".to_string();
            }
        }
        self.persist_review();
    }

    fn reject_all_changes(&mut self) {
        if let Some(session) = &mut self.update_session {
            if let Err(error) = session.reject_all() {
                self.notice = human_error(&error);
            } else {
                self.notice = "已选择全部保留现状".to_string();
            }
        }
        self.persist_review();
    }

    fn persist_review(&mut self) {
        let Some(session) = &self.update_session else {
            return;
        };
        if session.diff.is_none() {
            return;
        }
        if let Err(error) = self.dashboard.client.save_curriculum_review(session) {
            self.notice = format!("审阅尚未保存：{}", human_error(&error));
        }
    }

    fn finish_diff_review(&mut self) {
        let Some(session) = &self.update_session else {
            return;
        };
        if session.status.pending_decision_count > 0 {
            self.notice = format!(
                "还有 {} 条变化没有决定",
                session.status.pending_decision_count
            );
            return;
        }
        match self.dashboard.client.pending_course_assignments(session) {
            Ok(pending) => {
                self.pending_assignments = pending;
                if self.pending_assignments.is_empty() {
                    self.prepare_update_preview();
                    return;
                }
                match self.dashboard.client.course_assignment_targets() {
                    Ok(targets) => {
                        self.assignment_targets = targets;
                        self.assignment_filter.clear();
                        self.assignment_index = 0;
                        self.page = Page::UpdateAssignment;
                        self.notice =
                            "为新课程选择资料库；同名建议不等于同一课程，不会自动共享旧文件。"
                                .to_string();
                    }
                    Err(error) => self.notice = human_error(&error),
                }
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn assignment_candidates(&self) -> Vec<usize> {
        let query = self.assignment_filter.trim().to_lowercase();
        let suggestions = self
            .pending_assignments
            .first()
            .map(|item| item.suggested_repo_ids.as_slice())
            .unwrap_or(&[]);
        let mut indices = self
            .assignment_targets
            .iter()
            .enumerate()
            .filter(|(_, repo)| {
                query.is_empty()
                    || repo.display_name.to_lowercase().contains(&query)
                    || repo.repo_id.to_lowercase().contains(&query)
                    || repo
                        .course_names
                        .iter()
                        .any(|name| name.to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices
            .sort_by_key(|index| !suggestions.contains(&self.assignment_targets[*index].repo_id));
        indices
    }

    fn choose_course_assignment(&mut self) {
        if self.pending_assignments.is_empty() {
            return;
        }
        if self.assignment_index == 0 {
            self.input = self.assignment_filter.clone();
            self.page = Page::UpdateAssignmentSearch;
            return;
        }
        let candidates = self.assignment_candidates();
        if self.assignment_index == candidates.len() + 1 {
            self.input = self.pending_assignments[0].course_name.clone();
            self.page = Page::UpdateNewRepository;
            return;
        }
        if let Some(index) = candidates.get(self.assignment_index - 1) {
            let repo_id = self.assignment_targets[*index].repo_id.clone();
            self.save_course_assignment(CourseAssignment::Existing { repo_id });
        }
    }

    fn save_course_assignment(&mut self, assignment: CourseAssignment) {
        let Some(course) = self.pending_assignments.first() else {
            return;
        };
        let code = course.course_code.clone();
        let Some(session) = &mut self.update_session else {
            return;
        };
        let result = self
            .dashboard
            .client
            .assign_course(session, &code, assignment)
            .and_then(|()| self.dashboard.client.pending_course_assignments(session));
        match result {
            Ok(pending) => {
                self.pending_assignments = pending;
                self.assignment_filter.clear();
                self.assignment_index = 0;
                self.input.clear();
                if self.pending_assignments.is_empty() {
                    self.prepare_update_preview();
                } else {
                    self.page = Page::UpdateAssignment;
                    self.notice = format!(
                        "归属选择已保存，还有 {} 门新课程待分配。",
                        self.pending_assignments.len()
                    );
                }
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn prepare_update_preview(&mut self) {
        let Some(session) = &mut self.update_session else {
            return;
        };
        match self.dashboard.client.materialize_curriculum_update(session) {
            Ok(preview) => {
                self.update_preview = Some(preview);
                self.remote_preview = None;
                self.page = Page::UpdatePreview;
                self.notice =
                    "本地预览已生成，尚未修改远端。Enter 检查远端，Esc 返回审阅。".to_string();
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn prepare_update_remote(&mut self) {
        let Some(preview) = &self.update_preview else {
            return;
        };
        match self.dashboard.client.plan_remote_sync(preview) {
            Ok(remote) => {
                self.remote_preview = Some(remote);
                self.page = Page::RemotePreview;
                self.notice = "远端检查完成；确认后才写入 Registry 与仓库设置。".to_string();
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn execute_update_preview(&mut self) {
        let (Some(state), Some(remote)) =
            (self.update_preview.as_ref(), self.remote_preview.as_ref())
        else {
            return;
        };
        self.notice = "正在同步注册表和远端仓库，请勿关闭……".to_string();
        match self.dashboard.client.execute_remote_sync(state, remote) {
            Ok(journal) => {
                self.result_text = format!(
                    "教务数据和仓库同步已完成。\n\n培养方案：{} 个\n课程记录：{} 条\n任务：{}",
                    state.plan_count, state.record_count, journal.operation_id
                );
                self.refresh();
                self.page = Page::Result;
            }
            Err(error) => {
                self.result_text = format!(
                    "同步没有完成。\n\n{}\n\n可从“任务记录”安全继续。",
                    human_error(&error)
                );
                self.refresh();
                self.page = Page::Result;
            }
        }
    }

    fn move_current(&mut self, down: bool) {
        let len = match self.page {
            Page::Home => HOME_ITEMS.len(),
            Page::UpdateKind => 2,
            Page::UpdateGrade => self
                .update_session
                .as_ref()
                .map(|session| session.catalog.grades.len())
                .unwrap_or(0),
            Page::UpdateCollege => self
                .update_session
                .as_ref()
                .map(|session| session.catalog.colleges.len())
                .unwrap_or(0),
            Page::UpdateMajors => self.update_majors.len() + 1,
            Page::UpdateDiff => self.diff_len() + 1,
            Page::UpdateAssignment => self.assignment_candidates().len() + 2,
            Page::Browse => self.dashboard.repositories.len() + 1,
            Page::MergeSelect => self.merge_candidates().len() + 1,
            Page::SplitSelect => self.split_candidates().len(),
            Page::SplitAssign => self.split_assignments.len() + 1,
            Page::Tasks => self.tasks().len(),
            Page::Review => 2,
            _ => return,
        };
        let selected = match self.page {
            Page::Home => &mut self.home_index,
            Page::UpdateKind => &mut self.update_kind_index,
            Page::UpdateGrade => &mut self.update_grade_index,
            Page::UpdateCollege => &mut self.update_college_index,
            Page::UpdateMajors => &mut self.update_major_index,
            Page::UpdateDiff => &mut self.update_diff_index,
            Page::UpdateAssignment => &mut self.assignment_index,
            Page::Browse => &mut self.browse_index,
            Page::MergeSelect => &mut self.merge_index,
            Page::SplitSelect => &mut self.split_repo_index,
            Page::SplitAssign => &mut self.split_item_index,
            Page::Tasks => &mut self.task_index,
            Page::Review => &mut self.review_choice,
            _ => return,
        };
        if len == 0 {
            *selected = 0;
        } else if down {
            *selected = (*selected + 1).min(len - 1);
        } else {
            *selected = selected.saturating_sub(1);
        }
    }

    fn open_browse(&mut self) {
        if self.browse_index == 0 {
            self.input = self.dashboard.query.clone();
            self.page = Page::BrowseSearch;
            return;
        }
        let index = self.browse_index - 1;
        match self.dashboard.detail(index) {
            Ok(detail) => {
                self.detail = Some(detail);
                self.page = Page::Detail;
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn toggle_merge(&mut self) {
        let candidates = self.merge_candidates();
        if self.merge_index == candidates.len() {
            if self.selected_merge.len() < 2 {
                self.notice = "请至少选择两份资料".to_string();
                return;
            }
            let selected = self.selected_merge_repositories();
            self.input = if selected.len() == 2 {
                format!("{}与{}", selected[0].display_name, selected[1].display_name)
            } else {
                format!("{}等资料", selected[0].display_name)
            };
            self.page = Page::MergeName;
            return;
        }
        if let Some(index) = candidates.get(self.merge_index) {
            let id = self.dashboard.repositories[*index].repo_id.clone();
            if !self.selected_merge.remove(&id) {
                self.selected_merge.insert(id);
            }
            self.notice = format!("已选择 {} 份资料", self.selected_merge.len());
        }
    }

    fn prepare_merge(&mut self) {
        let name = self.input.trim().to_string();
        if name.is_empty() {
            self.notice = "请填写合并后的资料名称".to_string();
            return;
        }
        let selected = self.selected_merge_repositories();
        if selected.len() < 2 {
            self.page = Page::MergeSelect;
            return;
        }
        let source_ids = selected
            .iter()
            .map(|item| item.repo_id.clone())
            .collect::<Vec<_>>();
        let target_id = source_ids[0].clone();
        let source_names = selected
            .iter()
            .map(|item| item.display_name.clone())
            .collect::<Vec<_>>();
        match self
            .dashboard
            .client
            .plan_merge(&source_ids, &target_id, &name)
        {
            Ok(plan) => {
                let moves = plan
                    .risk
                    .get("file_move_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                self.review_lines = vec![
                    format!("把 {} 份资料合并为“{}”", source_names.len(), name),
                    format!("来源：{}", source_names.join("、")),
                    format!("将整理 {} 个文件", moves),
                    "如有同名文件，会自动放入独立目录，不会覆盖。".to_string(),
                ];
                self.pending_plan = Some(plan);
                self.review_origin = Some(ReviewOrigin::Merge);
                self.review_choice = 0;
                self.page = Page::Review;
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn open_split_source(&mut self) {
        let candidates = self.split_candidates();
        let Some(index) = candidates.get(self.split_repo_index) else {
            self.notice = "没有可拆分的资料".to_string();
            return;
        };
        let repo_id = self.dashboard.repositories[*index].repo_id.clone();
        match self.dashboard.client.split_options(&repo_id) {
            Ok(options) => {
                if options.courses.len() + options.loose_files.len() < 2 {
                    self.notice = "这份资料没有足够的独立内容可拆分".to_string();
                    return;
                }
                self.split_options = Some(options);
                self.split_target_count = 2;
                self.page = Page::SplitCount;
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn adjust_split_count(&mut self, increase: bool) {
        let item_count = self
            .split_options
            .as_ref()
            .map(|options| options.courses.len() + options.loose_files.len())
            .unwrap_or(2);
        let maximum = item_count.min(9).max(2);
        self.split_target_count = if increase {
            (self.split_target_count + 1).min(maximum)
        } else {
            self.split_target_count.saturating_sub(1).max(2)
        };
    }

    fn begin_split_assignment(&mut self) {
        let count = self
            .split_options
            .as_ref()
            .map(|options| options.courses.len() + options.loose_files.len())
            .unwrap_or(0);
        self.split_assignments = vec![None; count];
        self.split_item_index = 0;
        self.split_names = vec![String::new(); self.split_target_count];
        self.page = Page::SplitAssign;
    }

    fn cycle_assignment(&mut self, forward: bool) {
        if self.split_item_index >= self.split_assignments.len() {
            self.finish_assignments();
            return;
        }
        let value = &mut self.split_assignments[self.split_item_index];
        *value = match (*value, forward) {
            (None, true) => Some(0),
            (Some(index), true) if index + 1 < self.split_target_count => Some(index + 1),
            (Some(_), true) => None,
            (None, false) => Some(self.split_target_count - 1),
            (Some(0), false) => None,
            (Some(index), false) => Some(index - 1),
        };
    }

    fn finish_assignments(&mut self) {
        if self.split_assignments.iter().any(Option::is_none) {
            self.notice = "还有内容没有选择去向".to_string();
            return;
        }
        for target in 0..self.split_target_count {
            if !self
                .split_assignments
                .iter()
                .any(|assignment| *assignment == Some(target))
            {
                self.notice = format!("第 {} 份资料还没有内容", target + 1);
                return;
            }
        }
        let options = self.split_options.as_ref().expect("split options");
        for target in 0..self.split_target_count {
            self.split_names[target] = options
                .courses
                .iter()
                .enumerate()
                .find(|(index, _)| self.split_assignments[*index] == Some(target))
                .map(|(_, group)| group.title.clone())
                .unwrap_or_else(|| format!("{}（第{}部分）", options.source_title, target + 1));
        }
        self.split_name_index = 0;
        self.input = self.split_names[0].clone();
        self.page = Page::SplitName;
    }

    fn accept_split_name(&mut self) {
        let value = self.input.trim().to_string();
        if value.is_empty() {
            self.notice = "资料名称不能为空".to_string();
            return;
        }
        self.split_names[self.split_name_index] = value;
        if self.split_name_index + 1 < self.split_target_count {
            self.split_name_index += 1;
            self.input = self.split_names[self.split_name_index].clone();
        } else {
            self.prepare_split();
        }
    }

    fn prepare_split(&mut self) {
        let Some(options) = self.split_options.as_ref() else {
            return;
        };
        let course_count = options.courses.len();
        let mut targets = Vec::new();
        let mut review = vec![format!(
            "把“{}”拆成 {} 份资料",
            options.source_title, self.split_target_count
        )];
        for target_index in 0..self.split_target_count {
            let course_codes = options
                .courses
                .iter()
                .enumerate()
                .filter(|(index, _)| self.split_assignments[*index] == Some(target_index))
                .map(|(_, course)| course.course_code.clone())
                .collect::<Vec<_>>();
            let paths = options
                .loose_files
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    self.split_assignments[course_count + *index] == Some(target_index)
                })
                .map(|(_, file)| file.internal_path.clone())
                .collect::<Vec<_>>();
            let mut semantic_keys = course_codes.clone();
            semantic_keys.extend(paths.iter().map(|path| format!("file:{path}")));
            let repo_id = if target_index == 0 {
                options.source_repo_id.clone()
            } else {
                self.dashboard
                    .client
                    .automatic_repo_id(&self.split_names[target_index], &semantic_keys)
            };
            review.push(format!(
                "“{}”：{} 个课程代码，{} 个独立文件",
                self.split_names[target_index],
                course_codes.len(),
                paths.len()
            ));
            targets.push(SplitTarget {
                repo_id,
                display_name: self.split_names[target_index].clone(),
                course_codes,
                paths,
            });
        }
        match self
            .dashboard
            .client
            .plan_split(&options.source_repo_id, &targets)
        {
            Ok(plan) => {
                review.push("原文件会完整搬到所选资料中，不会丢失。".to_string());
                self.review_lines = review;
                self.pending_plan = Some(plan);
                self.review_origin = Some(ReviewOrigin::Split);
                self.review_choice = 0;
                self.page = Page::Review;
            }
            Err(error) => self.notice = human_error(&error),
        }
    }

    fn cancel_review(&mut self) {
        if let Some(plan) = self.pending_plan.take() {
            let _ = self.dashboard.client.discard_plan(&plan);
        }
        self.page = match self.review_origin {
            Some(ReviewOrigin::Merge) => Page::MergeName,
            Some(ReviewOrigin::Split) => Page::SplitName,
            None => Page::Home,
        };
    }

    fn confirm_review(&mut self) {
        let Some(plan) = self.pending_plan.clone() else {
            return;
        };
        match self.dashboard.client.apply(&plan) {
            Ok(_) => {
                self.pending_plan = None;
                self.result_text = "操作已经完成，并通过了完整性检查。".to_string();
            }
            Err(error) => {
                self.result_text = format!(
                    "操作没有完成。\n\n{}\n\n可从“任务记录”继续。",
                    human_error(&error)
                )
            }
        }
        self.refresh();
        self.page = Page::Result;
    }

    fn open_task(&mut self) {
        if self.current_task().is_some() {
            self.page = Page::TaskDetail;
        }
    }

    fn run_task_action(&mut self) {
        let Some(task) = self.current_task() else {
            return;
        };
        if let UnifiedTask::CurriculumReview(review) = &task {
            match self
                .dashboard
                .client
                .resume_curriculum_review(std::path::Path::new(&review.path))
            {
                Ok(session) => {
                    self.update_session = Some(session);
                    self.update_diff_index = 0;
                    self.diff_detail_scroll = 0;
                    self.update_preview = None;
                    self.remote_preview = None;
                    self.page = Page::UpdateDiff;
                    self.notice = "已恢复本地审阅和归属选择；不需要重新登录教务。".to_string();
                }
                Err(error) => self.notice = human_error(&error),
            }
            return;
        }
        let result: Result<()> = match task {
            UnifiedTask::Repository(journal) => {
                if journal.recovery_state == "resumable" {
                    self.dashboard.client.resume(&journal).map(|_| ())
                } else if journal.recovery_state == "completed" {
                    self.dashboard.client.verify(&journal).map(|_| ())
                } else {
                    Err(anyhow!("这项任务需要人工检查"))
                }
            }
            UnifiedTask::Update(journal) => {
                if journal.status == "completed" {
                    self.dashboard.client.verify_update_journal(&journal)
                } else {
                    self.dashboard
                        .client
                        .resume_remote_sync(&journal)
                        .map(|_| ())
                }
            }
            UnifiedTask::CurriculumReview(_) => unreachable!("review handled above"),
        };
        self.result_text = result
            .map(|_| "任务处理完成。".to_string())
            .unwrap_or_else(|error| human_error(&error));
        self.refresh();
        self.page = Page::Result;
    }

    fn submit_text(&mut self) {
        match self.page {
            Page::BrowseSearch => {
                let query = self.input.trim().to_string();
                match self.dashboard.search(query) {
                    Ok(()) => {
                        self.browse_index = 1.min(self.dashboard.repositories.len());
                        self.page = Page::Browse;
                    }
                    Err(error) => self.notice = human_error(&error),
                }
            }
            Page::UpdateAssignmentSearch => {
                self.assignment_filter = self.input.trim().to_string();
                self.assignment_index = 0;
                self.page = Page::UpdateAssignment;
            }
            Page::UpdateNewRepository => {
                let title = self.input.trim().to_string();
                if title.is_empty() {
                    self.notice = "请输入新资料库的名称".to_string();
                } else {
                    self.save_course_assignment(CourseAssignment::New { title });
                }
            }
            Page::MergeName => self.prepare_merge(),
            Page::SplitName => self.accept_split_name(),
            _ => {}
        }
    }
}

pub fn run_headless(workspace: std::path::PathBuf) -> Result<Dashboard> {
    let dashboard = Dashboard::load(workspace);
    dashboard.validate()?;
    Ok(dashboard)
}

pub fn run(workspace: std::path::PathBuf) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("无法打开管理界面")?;
    terminal.clear()?;
    let result = run_loop(&mut terminal, workspace);
    session.leave();
    result
}

struct TerminalSession {
    active: bool,
}
impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("无法进入交互模式")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { active: true })
    }
    fn leave(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            self.active = false;
        }
    }
}
impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.leave();
    }
}

fn run_loop<B>(terminal: &mut Terminal<B>, workspace: std::path::PathBuf) -> Result<()>
where
    B: ratatui::backend::Backend,
{
    let mut state = UiState::new(workspace);
    loop {
        state.poll_login();
        terminal.draw(|frame| draw(frame, &state))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(&mut state, key) {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn handle_key(state: &mut UiState, key: KeyEvent) -> bool {
    if matches!(
        state.page,
        Page::BrowseSearch
            | Page::MergeName
            | Page::SplitName
            | Page::UpdateAssignmentSearch
            | Page::UpdateNewRepository
    ) {
        match key.code {
            KeyCode::Enter => state.submit_text(),
            KeyCode::Esc
                if matches!(
                    state.page,
                    Page::UpdateAssignmentSearch | Page::UpdateNewRepository
                ) =>
            {
                state.page = Page::UpdateAssignment
            }
            KeyCode::Esc => state.go_home(),
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::Char(character) => state.input.push(character),
            _ => {}
        }
        return false;
    }
    match key.code {
        KeyCode::PageDown if state.page == Page::UpdateDiffDetail => {
            state.diff_detail_scroll = state.diff_detail_scroll.saturating_add(10)
        }
        KeyCode::PageUp if state.page == Page::UpdateDiffDetail => {
            state.diff_detail_scroll = state.diff_detail_scroll.saturating_sub(10)
        }
        KeyCode::Down if state.page == Page::UpdateDiffDetail => {
            state.diff_detail_scroll = state.diff_detail_scroll.saturating_add(1)
        }
        KeyCode::Up if state.page == Page::UpdateDiffDetail => {
            state.diff_detail_scroll = state.diff_detail_scroll.saturating_sub(1)
        }
        KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Right
            if state.page == Page::UpdateDiff =>
        {
            if state.update_diff_index < state.diff_len() {
                state.diff_detail_scroll = 0;
                state.page = Page::UpdateDiffDetail;
            }
        }
        KeyCode::Char('q') | KeyCode::Char('Q') if state.page == Page::Home => return true,
        KeyCode::Esc => match state.page {
            Page::Home => return true,
            Page::Detail => state.page = Page::Browse,
            Page::UpdateGrade => state.page = Page::UpdateKind,
            Page::UpdateCollege => state.page = Page::UpdateGrade,
            Page::UpdateMajors => state.page = Page::UpdateCollege,
            Page::UpdateDiffDetail | Page::UpdateAssignment | Page::UpdatePreview => {
                state.page = Page::UpdateDiff
            }
            Page::RemotePreview if state.update_session.is_some() => {
                state.page = Page::UpdatePreview
            }
            Page::UpdateKind
            | Page::UpdateDiff
            | Page::RemotePreview
            | Page::Browse
            | Page::MergeSelect
            | Page::SplitSelect
            | Page::Tasks
            | Page::System
            | Page::Help => state.go_home(),
            Page::SplitCount => state.page = Page::SplitSelect,
            Page::SplitAssign => state.page = Page::SplitCount,
            Page::Review => state.cancel_review(),
            Page::TaskDetail => state.page = Page::Tasks,
            Page::Result => state.go_home(),
            _ => state.go_home(),
        },
        KeyCode::Down => match state.page {
            Page::SplitCount => state.adjust_split_count(false),
            _ => state.move_current(true),
        },
        KeyCode::Up => match state.page {
            Page::SplitCount => state.adjust_split_count(true),
            _ => state.move_current(false),
        },
        KeyCode::Left if state.page == Page::SplitAssign => state.cycle_assignment(false),
        KeyCode::Right if state.page == Page::SplitAssign => state.cycle_assignment(true),
        KeyCode::Char('a') | KeyCode::Char('A') if state.page == Page::UpdateDiff => {
            state.accept_all_changes()
        }
        KeyCode::Char('r') | KeyCode::Char('R') if state.page == Page::UpdateDiff => {
            state.reject_all_changes()
        }
        KeyCode::Enter => match state.page {
            Page::Home => return state.open_home_item(),
            Page::UpdateLogin => state.start_login(),
            Page::UpdateKind => state.choose_source_kind(),
            Page::UpdateGrade => state.choose_grade(),
            Page::UpdateCollege => state.choose_college(),
            Page::UpdateMajors => state.toggle_major(),
            Page::UpdateDiff => {
                if state.update_diff_index == state.diff_len() {
                    state.finish_diff_review();
                } else {
                    state.cycle_diff_decision();
                }
            }
            Page::UpdateDiffDetail => state.cycle_diff_decision(),
            Page::UpdateAssignment => state.choose_course_assignment(),
            Page::UpdatePreview => state.prepare_update_remote(),
            Page::RemotePreview => state.execute_update_preview(),
            Page::Browse => state.open_browse(),
            Page::Detail => state.page = Page::Browse,
            Page::MergeSelect => state.toggle_merge(),
            Page::SplitSelect => state.open_split_source(),
            Page::SplitCount => state.begin_split_assignment(),
            Page::SplitAssign => state.cycle_assignment(true),
            Page::Review => {
                if state.review_choice == 0 {
                    state.cancel_review()
                } else {
                    state.confirm_review()
                }
            }
            Page::Tasks => state.open_task(),
            Page::TaskDetail => state.run_task_action(),
            Page::System | Page::Help | Page::Result => state.go_home(),
            _ => {}
        },
        KeyCode::Char('?') => state.page = Page::Help,
        _ => {}
    }
    false
}

fn draw(frame: &mut Frame<'_>, state: &UiState) {
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " 薪火仓库管理 ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(page_title(state.page)),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        vertical[0],
    );
    match state.page {
        Page::Home => draw_home(frame, vertical[1], state),
        Page::UpdateLogin => draw_login_status(frame, vertical[1], state),
        Page::UpdateKind => draw_options(
            frame,
            vertical[1],
            "选择教学数据来源",
            &[
                CatalogOption {
                    code: "execution".into(),
                    name: "执行教学计划 — 按入学年级查询课程、模块和毕业要求".into(),
                },
                CatalogOption {
                    code: "curriculum".into(),
                    name: "培养方案 — 按方案版本查询；版本年份不是入学年级".into(),
                },
            ],
            state.update_kind_index,
        ),
        Page::UpdateGrade => draw_options(
            frame,
            vertical[1],
            if state
                .update_session
                .as_ref()
                .is_some_and(|s| s.kind == PlanKind::Execution)
            {
                "选择入学年级"
            } else {
                "选择培养方案版本"
            },
            state
                .update_session
                .as_ref()
                .map(|session| session.catalog.grades.as_slice())
                .unwrap_or(&[]),
            state.update_grade_index,
        ),
        Page::UpdateCollege => draw_options(
            frame,
            vertical[1],
            "选择院系",
            state
                .update_session
                .as_ref()
                .map(|session| session.catalog.colleges.as_slice())
                .unwrap_or(&[]),
            state.update_college_index,
        ),
        Page::UpdateMajors => draw_major_select(frame, vertical[1], state),
        Page::UpdateDiff => draw_diff(frame, vertical[1], state),
        Page::UpdateDiffDetail => draw_diff_detail(frame, vertical[1], state),
        Page::UpdateAssignment => draw_course_assignment(frame, vertical[1], state),
        Page::UpdateAssignmentSearch => draw_text_input(
            frame,
            vertical[1],
            "查找现有资料库",
            "输入资料库名称或已有课程名；留空显示全部",
            &state.input,
        ),
        Page::UpdateNewRepository => draw_text_input(
            frame,
            vertical[1],
            "新建课程资料库",
            "填写名称，生成预览后仍需确认；不会立即创建远端仓库",
            &state.input,
        ),
        Page::UpdatePreview | Page::RemotePreview => draw_update_preview(frame, vertical[1], state),
        Page::Browse => draw_browse(frame, vertical[1], state),
        Page::BrowseSearch => draw_text_input(
            frame,
            vertical[1],
            "搜索资料",
            "输入课程名或资料名称",
            &state.input,
        ),
        Page::Detail => draw_detail(frame, vertical[1], state),
        Page::MergeSelect => draw_merge(frame, vertical[1], state),
        Page::MergeName => draw_text_input(
            frame,
            vertical[1],
            "合并后的名称",
            "可以使用建议名称，也可以修改",
            &state.input,
        ),
        Page::SplitSelect => draw_split_sources(frame, vertical[1], state),
        Page::SplitCount => draw_split_count(frame, vertical[1], state),
        Page::SplitAssign => draw_split_assign(frame, vertical[1], state),
        Page::SplitName => draw_text_input(
            frame,
            vertical[1],
            &format!("第 {} 份资料的名称", state.split_name_index + 1),
            "可以使用建议名称，也可以修改",
            &state.input,
        ),
        Page::Review => draw_review(frame, vertical[1], state),
        Page::Tasks => draw_tasks(frame, vertical[1], state),
        Page::TaskDetail => draw_task_detail(frame, vertical[1], state),
        Page::System => draw_system(frame, vertical[1], state),
        Page::Help => draw_help(frame, vertical[1]),
        Page::Result => draw_result(frame, vertical[1], state),
    }
    frame.render_widget(
        Paragraph::new(clip(
            &state.notice,
            vertical[2].width.saturating_sub(1) as usize,
        )),
        vertical[2],
    );
}

fn page_title(page: Page) -> &'static str {
    match page {
        Page::Home => "首页",
        Page::UpdateLogin
        | Page::UpdateKind
        | Page::UpdateGrade
        | Page::UpdateCollege
        | Page::UpdateMajors
        | Page::UpdateDiff
        | Page::UpdateDiffDetail
        | Page::UpdateAssignment
        | Page::UpdateAssignmentSearch
        | Page::UpdateNewRepository
        | Page::UpdatePreview => "从教务系统更新数据",
        Page::RemotePreview => "管理远端仓库",
        Page::Browse | Page::BrowseSearch | Page::Detail => "查看资料",
        Page::MergeSelect | Page::MergeName => "合并资料向导",
        Page::SplitSelect | Page::SplitCount | Page::SplitAssign | Page::SplitName => {
            "拆分资料向导"
        }
        Page::Review => "确认操作",
        Page::Tasks | Page::TaskDetail => "任务记录",
        Page::System => "系统检查",
        Page::Help => "使用帮助",
        Page::Result => "操作结果",
    }
}

fn draw_home(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new("欢迎使用薪火仓库管理工具\n可以更新教务数据，也可以管理 GitHub 仓库。")
            .alignment(ratatui::layout::Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        chunks[0],
    );
    let lines = HOME_ITEMS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            Line::styled(
                format!(
                    "  {}  {}",
                    if index == state.home_index {
                        "▶"
                    } else {
                        " "
                    },
                    label
                ),
                selected_style(index == state.home_index),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 请选择 ")),
        centered_rect(68, HOME_ITEMS.len() as u16 + 2, chunks[1]),
    );
    frame.render_widget(
        Paragraph::new("↑↓ 选择    Enter 确认    Esc 退出    ? 帮助")
            .alignment(ratatui::layout::Alignment::Center),
        chunks[2],
    );
}

fn draw_login_status(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let status = if state.login_job.is_some() {
        "正在等待浏览器登录完成……"
    } else {
        "登录未完成。按 Enter 重新打开登录窗口。"
    };
    let lines = vec![
        Line::raw(""),
        Line::styled(
            "请在弹出的浏览器中完成哈尔滨工业大学统一身份认证",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw(status),
        Line::raw("程序会自动检测登录结果，无需复制 Cookie。"),
        Line::raw("统一身份认证保存在本机专用浏览器中，可刷新短期教务登录。"),
        Line::raw(""),
        Line::raw("Enter：重试    Esc：取消并返回"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 教务系统登录 "),
            ),
        area,
    );
}

fn draw_options(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    options: &[CatalogOption],
    selected: usize,
) {
    let rows = visible_range(
        selected,
        options.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| Row::new([options[index].name.clone()]).style(selected_style(index == selected)));
    frame.render_widget(
        Table::new(rows, [Constraint::Percentage(100)])
            .header(Row::new([title]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" ↑↓ 选择 · Enter 下一步 "),
            ),
        area,
    );
}

fn draw_major_select(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let sections = Layout::vertical([Constraint::Min(4), Constraint::Length(5)]).split(area);
    let table_area = sections[0];
    let total = state.update_majors.len() + 1;
    let rows = visible_range(
        state.update_major_index,
        total,
        table_area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        if index == state.update_majors.len() {
            return Row::new([
                "开始抓取所选专业".to_string(),
                format!("已选择 {} 个", state.selected_majors.len()),
            ])
            .style(selected_style(index == state.update_major_index));
        }
        let item = &state.update_majors[index];
        Row::new([
            format!(
                "{} {}",
                if state.selected_majors.contains(&item.code) {
                    "☑"
                } else {
                    "☐"
                },
                item.name
            ),
            item.code.clone(),
        ])
        .style(selected_style(index == state.update_major_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [Constraint::Percentage(75), Constraint::Percentage(25)],
        )
        .header(Row::new(["专业全名与培养类型", "专业代码"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 选择需要更新的专业 "),
        ),
        table_area,
    );
    let selected = state
        .update_majors
        .get(state.update_major_index)
        .map(|major| {
            format!(
                "{}\n专业代码：{}。保留官方培养类型；名称相同也不会合并不同专业代码。",
                major.name, major.code
            )
        })
        .unwrap_or_else(|| {
            format!(
                "将读取所选 {} 个专业；其他来源、年级和专业的数据保持不变。",
                state.selected_majors.len()
            )
        });
    frame.render_widget(
        Paragraph::new(selected).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 当前选择完整名称 "),
        ),
        sections[1],
    );
}

fn draw_diff(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(session) = &state.update_session else {
        return;
    };
    let Some(diff) = &session.diff else {
        return;
    };
    let total = diff.changes.len() + 1;
    let rows = visible_range(
        state.update_diff_index,
        total,
        area.height.saturating_sub(4) as usize,
    )
    .map(|index| {
        if index == diff.changes.len() {
            return Row::new([
                "完成审阅并生成仓库变更预览".to_string(),
                format!("未决定 {} 条", session.status.pending_decision_count),
            ])
            .style(selected_style(index == state.update_diff_index));
        }
        let change = &diff.changes[index];
        let decision = match session.decisions.decisions.get(&change.change_id) {
            Some(Decision::Accept) => "接受教务变化",
            Some(Decision::Reject) => "保留现状",
            None => "尚未决定",
        };
        Row::new([
            change.title.clone(),
            change.explanation.clone(),
            decision.to_string(),
        ])
        .style(selected_style(index == state.update_diff_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(40),
                Constraint::Percentage(36),
                Constraint::Percentage(24),
            ],
        )
        .header(Row::new(["变化", "说明", "选择"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Enter 选择 · A 接受全部 · R 保留全部 · V 详情 "),
        ),
        area,
    );
}

fn draw_course_assignment(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(course) = state.pending_assignments.first() else {
        return;
    };
    let sections = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            "{}（{}）\n开课学院：{}\n还有 {} 门新课程需要选择资料库。同名只作为建议，不会合并课程身份或共享旧文件。",
            course.course_name, course.course_code,
            if course.offering_colleges.is_empty() { "未标注".to_string() } else { course.offering_colleges.join("、") },
            state.pending_assignments.len(),
        )).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" 新课程归属 ")),
        sections[0],
    );
    let candidates = state.assignment_candidates();
    let rows = visible_range(
        state.assignment_index,
        candidates.len() + 2,
        sections[1].height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let values = if index == 0 {
            vec![
                "搜索现有资料库".to_string(),
                if state.assignment_filter.is_empty() {
                    "显示全部".to_string()
                } else {
                    state.assignment_filter.clone()
                },
            ]
        } else if index == candidates.len() + 1 {
            vec![
                "新建独立课程资料库".to_string(),
                "填写名称后加入预览，不会立即建仓".to_string(),
            ]
        } else {
            let repo = &state.assignment_targets[candidates[index - 1]];
            let suggested = if course.suggested_repo_ids.contains(&repo.repo_id) {
                "建议 · "
            } else {
                ""
            };
            vec![
                repo.display_name.clone(),
                format!(
                    "{suggested}{} 门课程 · {} 个文件",
                    repo.course_codes.len(),
                    repo.file_count
                ),
            ]
        };
        Row::new(values).style(selected_style(index == state.assignment_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [Constraint::Percentage(65), Constraint::Percentage(35)],
        )
        .header(Row::new(["资料库名称", "现有内容"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ↑↓ 选择 · Enter 保存归属 · Esc 返回审阅 "),
        ),
        sections[1],
    );
    frame.render_widget(
        Paragraph::new(
            "归属选择只保存在本机。全部课程分配后会显示完整变更预览，确认前不修改远端仓库。",
        )
        .wrap(Wrap { trim: false }),
        sections[2],
    );
}

fn academic_field_label(key: &str) -> &str {
    match key {
        "course_code" => "课程代码",
        "course_name" => "课程名称",
        "course_english_name" => "课程英文名",
        "credit" => "学分",
        "total_hours" => "总学时",
        "assessment_method" => "考核方式",
        "course_nature" => "课程性质",
        "course_category" => "课程类别",
        "offering_college" => "开课学院",
        "recommended_year_semester" => "推荐学期",
        "academic_year" => "开课学年",
        "semester" => "开课学期",
        "direction" | "major_direction" => "专业方向",
        "source_kind" => "数据来源",
        "plan_version" => "方案版本",
        "entry_cohort" => "入学年级",
        "department_code" => "院系代码",
        "school_name" => "院系名称",
        "major_code" => "专业代码",
        "major_name" => "专业名称",
        "major_full_name" => "官方专业全名",
        "program_type" => "培养类型",
        "academic_structure" => "模块、毕业要求和备注",
        "module_tree" => "模块树",
        "modules" => "模块要求与课程",
        "requirements" | "graduation_requirements" => "毕业要求",
        "notes" => "方案备注",
        "courses" => "课程记录",
        "hours" => "分项学时",
        "campus" => "校区",
        _ => key,
    }
}

fn append_academic_value(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &serde_json::Value,
    depth: usize,
) {
    let indent = "  ".repeat(depth);
    match value {
        serde_json::Value::Object(fields) if !fields.is_empty() => {
            lines.push(Line::raw(format!("{indent}{label}")));
            for (key, child) in fields {
                append_academic_value(lines, academic_field_label(key), child, depth + 1);
            }
        }
        serde_json::Value::Array(values) if !values.is_empty() => {
            lines.push(Line::raw(format!("{indent}{label}（{} 项）", values.len())));
            for (index, child) in values.iter().enumerate() {
                append_academic_value(lines, &format!("第 {} 项", index + 1), child, depth + 1);
            }
        }
        _ => {
            let text = match value {
                serde_json::Value::Null => "未提供".to_string(),
                serde_json::Value::String(text) if text.is_empty() => "空".to_string(),
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Bool(value) => if *value { "是" } else { "否" }.to_string(),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => "无".to_string(),
                _ => value.to_string(),
            };
            lines.push(Line::raw(format!("{indent}{label}：{text}")));
        }
    }
}

fn draw_diff_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(session) = &state.update_session else {
        return;
    };
    let Some(change) = session.change(state.update_diff_index) else {
        return;
    };
    let decision = match session.decisions.decisions.get(&change.change_id) {
        Some(Decision::Accept) => "接受教务变化",
        Some(Decision::Reject) => "保留现状",
        None => "尚未决定",
    };
    let mut lines = vec![
        Line::styled(
            change.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(change.explanation.clone()),
        Line::raw(format!("当前选择：{decision}")),
        Line::raw(""),
    ];
    append_academic_value(
        &mut lines,
        "当前数据",
        change.before.as_ref().unwrap_or(&serde_json::Value::Null),
        0,
    );
    lines.push(Line::raw(""));
    append_academic_value(
        &mut lines,
        "教务新数据",
        change.after.as_ref().unwrap_or(&serde_json::Value::Null),
        0,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((state.diff_detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" ↑↓ / PgUp / PgDn 滚动 · Enter 切换选择 · Esc 返回 "),
            ),
        area,
    );
}

fn draw_update_preview(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let mut lines = Vec::new();
    if let Some(preview) = &state.update_preview {
        lines.extend(
            preview
                .summary_lines
                .iter()
                .map(|line| Line::raw(format!("• {line}"))),
        );
    }
    if let Some(remote) = &state.remote_preview {
        lines.extend(
            remote
                .summary_lines
                .iter()
                .map(|line| Line::raw(format!("• {line}"))),
        );
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(if state.page == Page::UpdatePreview {
        "Enter：检查远端并继续    Esc：返回审阅（尚未写入远端）"
    } else {
        "Enter：确认并执行完整同步    Esc：返回（尚未写入远端）"
    }));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 将执行以下仓库管理操作 "),
        ),
        area,
    );
}

fn draw_browse(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let total = state.dashboard.repositories.len() + 1;
    let rows = visible_range(
        state.browse_index,
        total,
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        if index == 0 {
            return Row::new([
                "🔍 搜索资料".to_string(),
                "按名称或课程查找".to_string(),
                String::new(),
            ])
            .style(selected_style(state.browse_index == 0));
        }
        let item = &state.dashboard.repositories[index - 1];
        Row::new([
            item.display_name.clone(),
            friendly_type(&item.repo_type).to_string(),
            format!("{} 个文件", item.file_count),
        ])
        .style(selected_style(index == state.browse_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(55),
                Constraint::Percentage(25),
                Constraint::Percentage(20),
            ],
        )
        .header(Row::new(["资料名称", "类型", "内容"]))
        .block(Block::default().borders(Borders::ALL).title(format!(
            " 共 {} 份资料 ",
            state.dashboard.repositories.len()
        ))),
        area,
    );
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(detail) = &state.detail else {
        return;
    };
    let courses = if detail.summary.course_names.is_empty() {
        "未关联课程".to_string()
    } else {
        detail.summary.course_names.join("、")
    };
    frame.render_widget(
        Paragraph::new(format!(
            "资料名称：{}\n\n包含课程：{}\n\n文件数量：{} 个\n占用空间：{}\n文件清点：{}\n\n{}",
            detail.summary.display_name,
            courses,
            detail.summary.file_count,
            human_bytes(detail.summary.bytes),
            if detail.summary.inventory_complete {
                "已完成"
            } else {
                "未完成"
            },
            detail.summary.description
        ))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" 资料详情 ")),
        area,
    );
}

fn draw_merge(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let candidates = state.merge_candidates();
    let total = candidates.len() + 1;
    let rows = visible_range(
        state.merge_index,
        total,
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        if index == candidates.len() {
            return Row::new([
                "下一步：填写合并后的名称".to_string(),
                format!("已选择 {} 份", state.selected_merge.len()),
            ])
            .style(selected_style(index == state.merge_index));
        }
        let item = &state.dashboard.repositories[candidates[index]];
        Row::new([
            format!(
                "{} {}",
                if state.selected_merge.contains(&item.repo_id) {
                    "☑"
                } else {
                    "☐"
                },
                item.display_name
            ),
            format!("{} 个文件", item.file_count),
        ])
        .style(selected_style(index == state.merge_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [Constraint::Percentage(72), Constraint::Percentage(28)],
        )
        .header(Row::new(["按 Enter 勾选或取消", "内容"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 选择要合并的资料 "),
        ),
        area,
    );
}

fn draw_split_sources(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let candidates = state.split_candidates();
    let rows = visible_range(
        state.split_repo_index,
        candidates.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let item = &state.dashboard.repositories[candidates[index]];
        Row::new([
            item.display_name.clone(),
            format!("{} 个课程代码", item.course_codes.len()),
            format!("{} 个文件", item.file_count),
        ])
        .style(selected_style(index == state.split_repo_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(55),
                Constraint::Percentage(25),
                Constraint::Percentage(20),
            ],
        )
        .header(Row::new(["选择一份资料", "可分内容", "文件"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Enter 进入拆分向导 "),
        ),
        area,
    );
}

fn draw_split_count(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let source = state
        .split_options
        .as_ref()
        .map(|options| options.source_title.as_str())
        .unwrap_or("这份资料");
    frame.render_widget(
        Paragraph::new(format!(
            "准备拆分“{}”\n\n要拆成几份？\n\n{} 份\n\n↑ 增加    ↓ 减少    Enter 下一步",
            source, state.split_target_count
        ))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL)),
        centered_rect(72, 12, area),
    );
}

fn draw_split_assign(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(options) = &state.split_options else {
        return;
    };
    let total = state.split_assignments.len() + 1;
    let rows = visible_range(
        state.split_item_index,
        total,
        area.height.saturating_sub(4) as usize,
    )
    .map(|index| {
        if index == state.split_assignments.len() {
            return Row::new([
                "完成分配，进入下一步".to_string(),
                String::new(),
                String::new(),
            ])
            .style(selected_style(index == state.split_item_index));
        }
        let (title, detail) = if index < options.courses.len() {
            let course = &options.courses[index];
            (
                format!("{} · {}", course.course_code, course.title),
                if course.shared_course_codes.is_empty() {
                    format!("{} 个文件 · {}", course.file_count, human_bytes(course.bytes))
                } else {
                    format!("{} 个文件 · 须与 {} 同仓", course.file_count, course.shared_course_codes.join("、"))
                },
            )
        } else {
            let file = &options.loose_files[index - options.courses.len()];
            (file.title.clone(), human_bytes(file.size))
        };
        let assignment = state.split_assignments[index]
            .map(|target| format!("第 {} 份", target + 1))
            .unwrap_or_else(|| "尚未选择".to_string());
        Row::new([title, detail, assignment]).style(selected_style(index == state.split_item_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(52),
                Constraint::Percentage(28),
                Constraint::Percentage(20),
            ],
        )
        .header(Row::new(["课程代码或文件", "内容与共享约束", "放到哪里"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ↑↓ 选择 · ←→ 或 Enter 选择去向 "),
        ),
        area,
    );
}

fn draw_text_input(frame: &mut Frame<'_>, area: Rect, title: &str, prompt: &str, value: &str) {
    frame.render_widget(
        Paragraph::new(format!("{prompt}\n\n> {value}\n\nEnter 确认    Esc 返回"))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} ")),
            ),
        centered_rect(78, 10, area),
    );
}

fn draw_review(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let mut lines = state
        .review_lines
        .iter()
        .map(|line| Line::raw(format!("• {line}")))
        .collect::<Vec<_>>();
    lines.push(Line::raw(""));
    for (index, label) in ["返回修改", "确认执行"].iter().enumerate() {
        lines.push(Line::styled(
            format!(
                "  {}  {}",
                if index == state.review_choice {
                    "▶"
                } else {
                    " "
                },
                label
            ),
            selected_style(index == state.review_choice),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 请确认会发生什么 "),
        ),
        area,
    );
}

fn draw_tasks(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let tasks = state.tasks();
    if tasks.is_empty() {
        frame.render_widget(
            Paragraph::new("目前没有任务记录。")
                .alignment(ratatui::layout::Alignment::Center)
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    let rows = visible_range(
        state.task_index,
        tasks.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        Row::new([
            task_title(&tasks[index]),
            task_status(&tasks[index]).to_string(),
            task_updated(&tasks[index]).to_string(),
        ])
        .style(selected_style(index == state.task_index))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(40),
                Constraint::Percentage(30),
                Constraint::Percentage(30),
            ],
        )
        .header(Row::new(["操作", "状态", "更新时间"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Enter 查看或继续 "),
        ),
        area,
    );
}

fn draw_task_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(task) = state.current_task() else {
        return;
    };
    let content = format!(
        "操作：{}\n状态：{}\n\n{}\n\n按 Enter 检查或继续\nEsc 返回",
        task_title(&task),
        task_status(&task),
        task_error(&task)
    );
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 任务详情 ")),
        area,
    );
}

fn draw_system(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let status: SystemStatus = state.dashboard.client.system_status();
    frame.render_widget(Paragraph::new(format!("浏览资料：{}\nGit 工具：{}\nGitHub 登录：{}\n\n{}\n\n学校登录保存在本机专用浏览器中；管理数据与任务记录不包含登录凭据。", if status.offline_ready { "可以使用" } else { "需要检查" }, if status.git_available { "已安装" } else { "未安装" }, if status.github_logged_in { "已登录" } else { "未登录" }, status.summary)).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" 系统检查 ")), area);
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Paragraph::new("↑↓ 选择，Enter 确认，Esc 返回。\n\n教务更新：在学校浏览器窗口登录，选择执行教学计划的年级或培养方案的版本、院系和专业。官方专业类型与代码会完整显示。\n\n差异审阅：Enter 选择，V 查看新旧字段，A 全部接受，R 全部保留。选择自动保存，可从任务记录恢复。\n\n新课程：每个代码独立选择现有资料库或新建，不因同名自动共享资料。\n\n预览：先检查本地结果，再检查远端，最后确认才执行。学校暂未返回的课程不等于资料应删除。\n\n统一身份认证保存在本机专用浏览器中，用于刷新教务登录；不需要复制 Cookie。").wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" 使用帮助 ")), area);
}

fn draw_result(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    frame.render_widget(
        Paragraph::new(format!("{}\n\n按 Enter 返回首页", state.result_text))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 操作结果 ")),
        centered_rect(78, 14, area),
    );
}

fn task_title(task: &UnifiedTask) -> String {
    match task {
        UnifiedTask::Repository(item) => friendly_operation(item.kind.as_deref().unwrap_or("")),
        UnifiedTask::Update(_) => "教务数据与仓库同步".to_string(),
        UnifiedTask::CurriculumReview(item) => item.title.clone(),
    }
}
fn task_status(task: &UnifiedTask) -> &'static str {
    match task {
        UnifiedTask::Repository(item) => friendly_task_status(&item.recovery_state),
        UnifiedTask::Update(item) if item.status == "completed" => "已完成",
        UnifiedTask::Update(item) if item.status == "failed" => "可以继续",
        UnifiedTask::Update(_) => "处理中",
        UnifiedTask::CurriculumReview(_) => "等待审阅",
    }
}
fn task_updated(task: &UnifiedTask) -> &str {
    match task {
        UnifiedTask::Repository(item) => item.updated_at.as_deref().unwrap_or(""),
        UnifiedTask::Update(item) => &item.updated_at,
        UnifiedTask::CurriculumReview(item) => &item.updated_at,
    }
}
fn task_error(task: &UnifiedTask) -> &str {
    match task {
        UnifiedTask::Repository(item) => item.error.as_deref().unwrap_or("没有记录到错误"),
        UnifiedTask::Update(item) => item.error.as_deref().unwrap_or("没有记录到错误"),
        UnifiedTask::CurriculumReview(_) => "本地审阅已保存，继续时不会修改远端或要求重新登录。",
    }
}
fn friendly_type(value: &str) -> &'static str {
    match value {
        "course" => "课程资料",
        "competition" => "竞赛资料",
        "shared" => "共享资料",
        "template" => "模板",
        "control" => "管理资料",
        _ => "其他资料",
    }
}
fn friendly_operation(value: &str) -> String {
    match value {
        "merge" => "合并资料".to_string(),
        "split" => "拆分资料".to_string(),
        _ => "资料整理".to_string(),
    }
}
fn friendly_task_status(value: &str) -> &'static str {
    match value {
        "completed" => "已完成",
        "resumable" => "可以继续",
        "drifted" => "资料已变化",
        "invalid" => "需要人工检查",
        _ => "处理中",
    }
}
fn human_bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", value as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MB", value as f64 / (1024.0 * 1024.0))
    } else if value >= 1024 {
        format!("{:.1} KB", value as f64 / 1024.0)
    } else {
        format!("{value} B")
    }
}
fn selected_style(selected: bool) -> Style {
    if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    }
}
fn visible_range(selected: usize, len: usize, capacity: usize) -> std::ops::Range<usize> {
    if len == 0 || capacity == 0 {
        return 0..0;
    }
    let capacity = capacity.min(len);
    let start = selected
        .saturating_sub(capacity / 2)
        .min(len.saturating_sub(capacity));
    start..start + capacity
}
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(area.height.saturating_sub(2)).max(3);
    let margin = area.height.saturating_sub(height) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(margin),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
fn clip(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let w = character.width().unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        result.push(character);
        used += w;
    }
    result.push('…');
    result
}
fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Health, Manager, RoutesSnapshot};
    use ratatui::backend::TestBackend;

    fn test_state() -> UiState {
        UiState::from_dashboard(Dashboard {
            client: Manager::new("."),
            health: Health {
                organization: "HIT-Fireworks".to_string(),
                health: "healthy".to_string(),
                repository_count: 2,
                ..Health::default()
            },
            repositories: vec![
                RepositorySummary {
                    repo_id: "A".into(),
                    display_name: "高等数学".into(),
                    repo_type: "course".into(),
                    inventory_complete: true,
                    file_count: 3,
                    course_codes: vec!["A1".into(), "B1".into()],
                    ..RepositorySummary::default()
                },
                RepositorySummary {
                    repo_id: "B".into(),
                    display_name: "线性代数".into(),
                    repo_type: "course".into(),
                    inventory_complete: true,
                    file_count: 2,
                    ..RepositorySummary::default()
                },
            ],
            routes: RoutesSnapshot::default(),
            plans: Vec::new(),
            journals: Vec::new(),
            query: String::new(),
            logs: Vec::new(),
        })
    }
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }
    #[test]
    fn home_contains_full_repository_lifecycle() {
        let state = test_state();
        let backend = TestBackend::new(110, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        assert!(text.contains("从教务系统更新数据"));
        assert!(text.contains("管理远端仓库"));
        assert!(text.contains("合并几份资料"));
        assert!(text.contains("拆分一份资料"));
    }
    #[test]
    fn login_page_never_displays_cookie_input() {
        let mut state = test_state();
        state.page = Page::UpdateLogin;
        state.input = "SESSION=secret".to_string();
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(!text.contains("SESSION=secret"));
        assert!(text.contains("无需复制Cookie"));
    }
    #[test]
    fn first_time_user_can_open_update_with_enter() {
        let mut state = test_state();
        state.login_runner = |_| Err("测试结束".to_string());
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::UpdateLogin);
    }
    #[test]
    fn automatic_login_page_ignores_cookie_typing() {
        let mut state = test_state();
        state.page = Page::UpdateLogin;
        handle_key(&mut state, press(KeyCode::Char('S')));
        assert!(state.input.is_empty());
    }

    #[test]
    fn cancelling_login_does_not_wait_for_worker() {
        let mut state = test_state();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (_sender, receiver) = mpsc::channel::<Result<UpdateSession, String>>();
        state.login_job = Some(LoginJob {
            cancelled: Arc::clone(&cancelled),
            result: receiver,
        });

        let started = std::time::Instant::now();
        state.go_home();

        assert!(cancelled.load(std::sync::atomic::Ordering::Relaxed));
        assert!(started.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn completed_login_opens_grade_selection() {
        let mut state = test_state();
        let (sender, receiver) = mpsc::channel();
        let session = UpdateSession {
            client: None,
            kind: PlanKind::Execution,
            assignments: Default::default(),
            catalog: crate::jwts::CurriculumCatalog {
                grades: vec![CatalogOption {
                    code: "2025".to_string(),
                    name: "2025 级".to_string(),
                }],
                colleges: Vec::new(),
            },
            selections: Vec::new(),
            candidate: None,
            diff: None,
            decisions: crate::curriculum::DecisionSet::default(),
            status: crate::state::CurriculumUpdateStatus::default(),
        };
        sender.send(Ok(session)).unwrap();
        state.page = Page::UpdateLogin;
        state.login_job = Some(LoginJob {
            cancelled: Arc::new(AtomicBool::new(false)),
            result: receiver,
        });

        state.poll_login();

        assert_eq!(state.page, Page::UpdateKind);
        assert_eq!(state.update_session.unwrap().catalog.grades[0].code, "2025");
    }
    #[test]
    fn review_hides_internal_identifiers() {
        let mut state = test_state();
        state.page = Page::Review;
        state.review_lines = vec!["同步课程数据".to_string()];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for forbidden in ["repo_id", "identity", "APPLY", "journal", "JSON"] {
            assert!(!text.contains(forbidden));
        }
    }
    #[test]
    fn unicode_clip_uses_display_width() {
        assert_eq!(display_width("数理A"), 5);
        assert_eq!(clip("数理逻辑", 5), "数理…");
    }

    fn rendered_text(state: &UiState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect()
    }

    #[test]
    fn source_selection_distinguishes_version_from_entry_cohort() {
        let mut state = test_state();
        state.page = Page::UpdateKind;
        let text = rendered_text(&state);
        assert!(text.contains("执行教学计划"));
        assert!(text.contains("培养方案"));
        assert!(text.contains("版本年份不是入学年级"));
        assert_eq!(state.update_kind_index, 0);
        handle_key(&mut state, press(KeyCode::Down));
        assert_eq!(state.update_kind_index, 1);
        handle_key(&mut state, press(KeyCode::Up));
        assert_eq!(state.update_kind_index, 0);
    }

    #[test]
    fn identical_major_names_keep_official_types_and_codes_visible() {
        let mut state = test_state();
        state.page = Page::UpdateMajors;
        state.update_majors = vec![
            CatalogOption {
                code: "13B036".into(),
                name: "人工智能【本】".into(),
            },
            CatalogOption {
                code: "13BE036".into(),
                name: "人工智能【第二学士学位】".into(),
            },
            CatalogOption {
                code: "13BF036".into(),
                name: "人工智能【辅修】".into(),
            },
        ];
        let text = rendered_text(&state);
        for expected in [
            "人工智能【本】",
            "人工智能【第二学士学位】",
            "人工智能【辅修】",
            "13B036",
            "13BE036",
            "13BF036",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        handle_key(&mut state, press(KeyCode::Enter));
        handle_key(&mut state, press(KeyCode::Down));
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(
            state.selected_majors,
            BTreeSet::from(["13B036".into(), "13BE036".into()])
        );
    }

    #[test]
    fn repository_suggestions_do_not_implicitly_assign_a_course() {
        let mut state = test_state();
        state.page = Page::UpdateAssignment;
        state.pending_assignments = vec![CourseRepositoryChoice {
            course_code: "NEW101".into(),
            course_name: "高等数学".into(),
            offering_colleges: vec!["数学学院".into()],
            suggested_repo_ids: vec!["B".into()],
        }];
        state.assignment_targets = state.dashboard.repositories.clone();
        assert_eq!(state.assignment_candidates(), vec![1, 0]);
        let text = rendered_text(&state);
        assert!(text.contains("NEW101") && text.contains("新建独立课程资料库"));
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::UpdateAssignmentSearch);
        assert_eq!(state.pending_assignments.len(), 1);
        state.input = "线性代数".into();
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::UpdateAssignment);
        assert_eq!(state.assignment_candidates(), vec![1]);
        state.assignment_index = 2;
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::UpdateNewRepository);
        assert_eq!(state.input, "高等数学");
        handle_key(&mut state, press(KeyCode::Esc));
        assert_eq!(state.page, Page::UpdateAssignment);
        assert_eq!(state.pending_assignments.len(), 1);
    }

    #[test]
    fn local_update_preview_does_not_claim_to_have_synced_remotes() {
        let mut state = test_state();
        state.page = Page::UpdatePreview;
        state.update_preview = Some(RepositorySyncPreview {
            summary_lines: vec!["新增课程代码：1 个".into()],
            ..RepositorySyncPreview::default()
        });
        let text = rendered_text(&state);
        assert!(text.contains("检查远端并继续"));
        assert!(text.contains("尚未写入远端"));
        assert!(!text.contains("确认并执行完整同步"));
        handle_key(&mut state, press(KeyCode::Esc));
        assert_eq!(state.page, Page::UpdateDiff);
    }

    #[test]
    fn academic_details_preserve_nested_requirements_without_json_input() {
        let mut lines = Vec::new();
        append_academic_value(
            &mut lines,
            "教务新数据",
            &serde_json::json!({
                "credit":4,
                "major_full_name":"软件工程【第二学士学位】",
                "academic_structure":{"requirements":[{"课程类别":"专业核心课","学分":7}]},
                "校方新增约束":"同时满足门数与学分"
            }),
            0,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "学分：4",
            "官方专业全名",
            "第二学士学位",
            "毕业要求",
            "专业核心课",
            "同时满足门数与学分",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
    }
}

use std::collections::BTreeSet;
use std::io::{self, stdout};
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

use crate::state::{
    human_error, Dashboard, JournalSummary, PlannedOperation, RepositoryDetail, RepositorySummary,
    SplitOptions, SplitTarget, SystemStatus,
};

const HOME_ITEMS: [&str; 6] = [
    "查看和搜索资料",
    "合并几份资料",
    "拆分一份资料",
    "查看任务记录",
    "系统检查",
    "退出",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
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
}

impl UiState {
    fn new(workspace: std::path::PathBuf) -> Self {
        let dashboard = Dashboard::load(workspace);
        Self::from_dashboard(dashboard)
    }

    fn from_dashboard(dashboard: Dashboard) -> Self {
        let notice = dashboard
            .logs
            .last()
            .cloned()
            .unwrap_or_else(|| "请选择要做的事情".to_string());
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
        }
    }

    fn refresh(&mut self) {
        match self.dashboard.refresh() {
            Ok(()) => self.notice = "资料已刷新".to_string(),
            Err(error) => self.notice = human_error(&error),
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.browse_index = self.browse_index.min(self.dashboard.repositories.len());
        self.merge_index = self.merge_index.min(self.merge_candidates().len());
        self.split_repo_index = self
            .split_repo_index
            .min(self.split_candidates().len().saturating_sub(1));
        self.task_index = self
            .task_index
            .min(self.dashboard.journals.len().saturating_sub(1));
        let existing: BTreeSet<_> = self
            .dashboard
            .repositories
            .iter()
            .map(|item| item.repo_id.clone())
            .collect();
        self.selected_merge.retain(|id| existing.contains(id));
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
                    && (item.member_resource_group_ids.len() >= 2 || item.unowned_paths.len() >= 2))
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

    fn current_journal(&self) -> Option<&JournalSummary> {
        self.dashboard.journals.get(self.task_index)
    }

    fn go_home(&mut self) {
        if let Some(plan) = self.pending_plan.take() {
            let _ = self.dashboard.client.discard_plan(&plan);
        }
        self.page = Page::Home;
        self.detail = None;
        self.split_options = None;
        self.review_lines.clear();
        self.review_origin = None;
        self.input.clear();
        self.notice = "请选择要做的事情".to_string();
    }

    fn open_home_item(&mut self) -> bool {
        match self.home_index {
            0 => {
                self.page = Page::Browse;
                self.browse_index = 0;
            }
            1 => {
                self.page = Page::MergeSelect;
                self.merge_index = 0;
                self.selected_merge.clear();
            }
            2 => {
                self.page = Page::SplitSelect;
                self.split_repo_index = 0;
            }
            3 => {
                self.refresh();
                self.page = Page::Tasks;
                self.task_index = 0;
            }
            4 => self.page = Page::System,
            5 => return true,
            _ => {}
        }
        false
    }

    fn move_current(&mut self, down: bool) {
        let len = match self.page {
            Page::Home => HOME_ITEMS.len(),
            Page::Browse => self.dashboard.repositories.len() + 1,
            Page::MergeSelect => self.merge_candidates().len() + 1,
            Page::SplitSelect => self.split_candidates().len(),
            Page::SplitAssign => self.split_assignments.len() + 1,
            Page::Tasks => self.dashboard.journals.len(),
            Page::Review => 2,
            _ => return,
        };
        let selected = match self.page {
            Page::Home => &mut self.home_index,
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
            let default_name = if selected.len() == 2 {
                format!("{}与{}", selected[0].display_name, selected[1].display_name)
            } else {
                format!("{}等资料", selected[0].display_name)
            };
            self.input = default_name;
            self.page = Page::MergeName;
            return;
        }
        let Some(index) = candidates.get(self.merge_index) else {
            return;
        };
        let id = self.dashboard.repositories[*index].repo_id.clone();
        if !self.selected_merge.remove(&id) {
            self.selected_merge.insert(id);
        }
        self.notice = format!("已选择 {} 份资料", self.selected_merge.len());
    }

    fn prepare_merge(&mut self) {
        let name = self.input.trim().to_string();
        if name.is_empty() {
            self.notice = "请填写合并后的资料名称".to_string();
            return;
        }
        let selected = self.selected_merge_repositories();
        if selected.len() < 2 {
            self.notice = "请至少选择两份资料".to_string();
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
                let count = options.groups.len() + options.loose_files.len();
                if count < 2 {
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
            .map(|options| options.groups.len() + options.loose_files.len())
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
            .map(|options| options.groups.len() + options.loose_files.len())
            .unwrap_or(0);
        self.split_assignments = vec![None; count];
        self.split_item_index = 0;
        self.split_names = vec![String::new(); self.split_target_count];
        self.page = Page::SplitAssign;
        self.notice = "用左右方向键选择每项资料的去向".to_string();
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
            let first_group = options
                .groups
                .iter()
                .enumerate()
                .find(|(index, _)| self.split_assignments[*index] == Some(target))
                .map(|(_, group)| group.title.clone());
            self.split_names[target] = first_group
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
            return;
        }
        self.prepare_split();
    }

    fn prepare_split(&mut self) {
        let Some(options) = self.split_options.as_ref() else {
            self.notice = "拆分信息已失效，请重新开始".to_string();
            self.go_home();
            return;
        };
        let group_count = options.groups.len();
        let mut targets = Vec::new();
        let mut review = vec![format!(
            "把“{}”拆成 {} 份资料",
            options.source_title, self.split_target_count
        )];
        for target_index in 0..self.split_target_count {
            let group_ids = options
                .groups
                .iter()
                .enumerate()
                .filter(|(index, _)| self.split_assignments[*index] == Some(target_index))
                .map(|(_, group)| group.internal_id.clone())
                .collect::<Vec<_>>();
            let paths = options
                .loose_files
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    self.split_assignments[group_count + *index] == Some(target_index)
                })
                .map(|(_, file)| file.internal_path.clone())
                .collect::<Vec<_>>();
            let mut semantic_keys = group_ids.clone();
            semantic_keys.extend(paths.iter().map(|path| format!("file:{path}")));
            let repo_id = if target_index == 0 {
                options.source_repo_id.clone()
            } else {
                self.dashboard
                    .client
                    .automatic_repo_id(&self.split_names[target_index], &semantic_keys)
            };
            review.push(format!(
                "“{}”：{} 个课程组，{} 个零散文件",
                self.split_names[target_index],
                group_ids.len(),
                paths.len()
            ));
            targets.push(SplitTarget {
                repo_id,
                display_name: self.split_names[target_index].clone(),
                resource_group_ids: group_ids,
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
        self.notice = "没有执行任何修改".to_string();
    }

    fn confirm_review(&mut self) {
        let Some(plan) = self.pending_plan.clone() else {
            self.notice = "操作预览已失效，请重新开始".to_string();
            self.go_home();
            return;
        };
        self.notice = "正在安全地处理资料，请稍候……".to_string();
        match self.dashboard.client.apply(&plan) {
            Ok(_) => {
                self.pending_plan = None;
                self.result_text = "操作已经完成，并通过了本地完整性检查。".to_string();
                self.refresh();
                self.page = Page::Result;
            }
            Err(error) => {
                self.result_text = format!(
                    "操作没有完成。\n\n{}\n\n已完成的步骤记录在“任务记录”中，可以从那里安全继续。",
                    human_error(&error)
                );
                self.refresh();
                self.page = Page::Result;
            }
        }
    }

    fn open_task(&mut self) {
        if self.current_journal().is_some() {
            self.page = Page::TaskDetail;
        } else {
            self.notice = "目前没有任务记录".to_string();
        }
    }

    fn run_task_action(&mut self) {
        let Some(journal) = self.current_journal().cloned() else {
            return;
        };
        let result = if journal.recovery_state == "resumable" {
            self.dashboard.client.resume(&journal)
        } else if journal.recovery_state == "completed" {
            self.dashboard.client.verify(&journal)
        } else {
            self.notice = "这项任务需要人工检查，不能自动继续".to_string();
            return;
        };
        match result {
            Ok(_) => self.result_text = "任务处理完成。".to_string(),
            Err(error) => self.result_text = human_error(&error),
        }
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
                        self.notice = format!("找到 {} 份资料", self.dashboard.repositories.len());
                    }
                    Err(error) => self.notice = human_error(&error),
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
        Page::BrowseSearch | Page::MergeName | Page::SplitName
    ) {
        match key.code {
            KeyCode::Enter => state.submit_text(),
            KeyCode::Esc => {
                state.input.clear();
                state.page = match state.page {
                    Page::BrowseSearch => Page::Browse,
                    Page::MergeName => Page::MergeSelect,
                    Page::SplitName => Page::SplitAssign,
                    _ => Page::Home,
                };
            }
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::Char(character) => state.input.push(character),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') if state.page == Page::Home => return true,
        KeyCode::Esc => match state.page {
            Page::Home => return true,
            Page::Detail => state.page = Page::Browse,
            Page::MergeSelect | Page::SplitSelect | Page::Tasks | Page::System | Page::Help => {
                state.go_home()
            }
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
        KeyCode::Enter => match state.page {
            Page::Home => return state.open_home_item(),
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
                " 薪火资料管理 ",
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
            "可以直接使用建议名称，也可以修改",
            &state.input,
        ),
        Page::SplitSelect => draw_split_sources(frame, vertical[1], state),
        Page::SplitCount => draw_split_count(frame, vertical[1], state),
        Page::SplitAssign => draw_split_assign(frame, vertical[1], state),
        Page::SplitName => draw_text_input(
            frame,
            vertical[1],
            &format!("第 {} 份资料的名称", state.split_name_index + 1),
            "可以直接使用建议名称，也可以修改",
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
        Paragraph::new("欢迎使用薪火资料管理工具\n不需要写代码，也不需要记命令。")
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
            let prefix = if index == state.home_index {
                "▶"
            } else {
                " "
            };
            Line::styled(
                format!("  {prefix}  {label}"),
                selected_style(index == state.home_index),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 请选择 ")),
        centered_rect(64, HOME_ITEMS.len() as u16 + 2, chunks[1]),
    );
    frame.render_widget(
        Paragraph::new("↑↓ 选择    Enter 确认    Esc 退出    ? 帮助")
            .alignment(ratatui::layout::Alignment::Center),
        chunks[2],
    );
}

fn draw_browse(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let capacity = area.height.saturating_sub(3) as usize;
    let total = state.dashboard.repositories.len() + 1;
    let rows = visible_range(state.browse_index, total, capacity).map(|index| {
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
        .header(
            Row::new(["资料名称", "类型", "内容"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            " 共 {} 份资料 · Enter 查看 ",
            state.dashboard.repositories.len()
        ))),
        area,
    );
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(detail) = &state.detail else {
        frame.render_widget(Paragraph::new("没有可显示的资料"), area);
        return;
    };
    let courses = if detail.summary.course_names.is_empty() {
        "未关联课程".to_string()
    } else {
        detail.summary.course_names.join("、")
    };
    let content = format!(
        "资料名称：{}\n\n包含课程：{}\n\n文件数量：{} 个\n占用空间：{}\n文件清点：{}\n\n{}\n\n按 Enter 或 Esc 返回",
        detail.summary.display_name,
        courses,
        detail.summary.file_count,
        human_bytes(detail.summary.bytes),
        if detail.summary.inventory_complete { "已完成" } else { "未完成" },
        if detail.summary.description.is_empty() { "" } else { detail.summary.description.as_str() }
    );
    frame.render_widget(
        Paragraph::new(content)
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
        let mark = if state.selected_merge.contains(&item.repo_id) {
            "☑"
        } else {
            "☐"
        };
        Row::new([
            format!("{mark} {}", item.display_name),
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
    if candidates.is_empty() {
        frame.render_widget(
            Paragraph::new("目前没有适合拆分的资料。\n\n只有已经完成文件清点、并且包含多个独立内容组的资料才能拆分。\n\n按 Esc 返回。")
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" 拆分资料 ")),
            area,
        );
        return;
    }
    let rows = visible_range(
        state.split_repo_index,
        candidates.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let item = &state.dashboard.repositories[candidates[index]];
        Row::new([
            item.display_name.clone(),
            format!("{} 个课程组", item.member_resource_group_ids.len()),
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
    let content = format!(
        "准备拆分“{}”\n\n要拆成几份？\n\n                 {} 份\n\n↑ 增加    ↓ 减少    Enter 下一步    Esc 返回",
        source, state.split_target_count
    );
    frame.render_widget(
        Paragraph::new(content)
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" 选择数量 ")),
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
        let (title, detail) = if index < options.groups.len() {
            let group = &options.groups[index];
            (
                group.title.clone(),
                format!("{} 个文件 · {}", group.file_count, human_bytes(group.bytes)),
            )
        } else {
            let file = &options.loose_files[index - options.groups.len()];
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
        .header(Row::new(["课程组或文件", "内容", "放到哪里"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ↑↓ 选择项目 · ←→ 或 Enter 选择去向 "),
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
    lines.push(Line::raw(
        "确认后会修改 GitHub 上的资料。开始前还会再次检查远端是否变化。",
    ));
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
    if state.dashboard.journals.is_empty() {
        frame.render_widget(
            Paragraph::new("目前没有任务记录。\n\n按 Esc 返回首页。")
                .alignment(ratatui::layout::Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" 任务记录 ")),
            area,
        );
        return;
    }
    let rows = visible_range(
        state.task_index,
        state.dashboard.journals.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let item = &state.dashboard.journals[index];
        Row::new([
            friendly_operation(item.kind.as_deref().unwrap_or("")),
            friendly_task_status(&item.recovery_state).to_string(),
            item.updated_at.clone().unwrap_or_default(),
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
    let Some(item) = state.current_journal() else {
        return;
    };
    let action = match item.recovery_state.as_str() {
        "resumable" => "按 Enter 安全地继续上次操作",
        "completed" => "按 Enter 检查最终结果",
        "drifted" => "资料已经变化，不能自动继续。请联系维护人员。",
        _ => "任务记录需要人工检查。",
    };
    let content = format!(
        "操作：{}\n状态：{}\n\n{}\n\n{}\n\nEsc 返回",
        friendly_operation(item.kind.as_deref().unwrap_or("")),
        friendly_task_status(&item.recovery_state),
        item.error.as_deref().unwrap_or("没有记录到错误"),
        action
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
    let content = format!(
        "浏览资料：{}\nGit 工具：{}\nGitHub 登录：{}\n\n{}\n\n说明：即使没有 Git 或尚未登录 GitHub，也可以正常查看资料；只有合并、拆分时才需要。\n\n按 Enter 或 Esc 返回首页",
        if status.offline_ready { "可以使用" } else { "需要检查" },
        if status.git_available { "已安装" } else { "未安装" },
        if status.github_logged_in { "已登录" } else { "未登录" },
        status.summary
    );
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 系统检查 ")),
        area,
    );
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(
            "只需要记住三个按键：\n\n  ↑↓  选择\n  Enter  确认\n  Esc  返回\n\n合并资料：在列表中按 Enter 勾选至少两份，再填写合并后的名称。\n\n拆分资料：选择一份资料、选择要拆成几份，再逐项选择每个课程组或文件的去向。\n\n最后都会先显示结果预览；选择“确认执行”之前，不会修改任何远端资料。\n\n按 Enter 或 Esc 返回首页",
        )
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" 使用帮助 ")),
        area,
    );
}

fn draw_result(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    frame.render_widget(
        Paragraph::new(format!("{}\n\n按 Enter 返回首页", state.result_text))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 操作结果 ")),
        centered_rect(76, 12, area),
    );
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
    let vertical_margin = area.height.saturating_sub(height) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(vertical_margin),
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
    let target = width.saturating_sub(1);
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if used + character_width > target {
            break;
        }
        result.push(character);
        used += character_width;
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
                    repo_id: "A".to_string(),
                    display_name: "高等数学".to_string(),
                    repo_type: "course".to_string(),
                    inventory_complete: true,
                    file_count: 3,
                    member_resource_group_ids: vec!["g1".into(), "g2".into()],
                    ..RepositorySummary::default()
                },
                RepositorySummary {
                    repo_id: "B".to_string(),
                    display_name: "线性代数".to_string(),
                    repo_type: "course".to_string(),
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
    fn first_time_user_can_open_browse_with_enter_only() {
        let mut state = test_state();
        assert_eq!(state.page, Page::Home);
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::Browse);
        assert_eq!(state.browse_index, 0);
    }

    #[test]
    fn merge_selection_uses_arrows_and_enter() {
        let mut state = test_state();
        state.home_index = 1;
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::MergeSelect);
        handle_key(&mut state, press(KeyCode::Enter));
        handle_key(&mut state, press(KeyCode::Down));
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.selected_merge.len(), 2);
        handle_key(&mut state, press(KeyCode::Down));
        handle_key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.page, Page::MergeName);
        assert!(!state.input.is_empty());
    }

    #[test]
    fn review_screen_hides_internal_identifiers_and_challenges() {
        let mut state = test_state();
        state.page = Page::Review;
        state.review_lines = vec!["把两份资料合并为“数学资料”".to_string()];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let compact = rendered
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(compact.contains("确认执行"));
        assert!(!rendered.contains("repo_id"));
        assert!(!rendered.contains("identity"));
        assert!(!rendered.contains("APPLY"));
        assert!(!rendered.contains("journal"));
    }

    #[test]
    fn home_is_fully_chinese_and_explains_keys() {
        let state = test_state();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let compact = rendered
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(compact.contains("查看和搜索资料"));
        assert!(compact.contains("合并几份资料"));
        assert!(compact.contains("拆分一份资料"));
        assert!(compact.contains("↑↓选择"));
        assert!(!rendered.contains("Repositories"));
    }

    #[test]
    fn chinese_input_redraws() {
        let mut state = test_state();
        state.page = Page::BrowseSearch;
        handle_key(&mut state, press(KeyCode::Char('数')));
        assert_eq!(state.input, "数");
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains('数'));
    }

    #[test]
    fn unicode_clip_uses_display_width() {
        assert_eq!(display_width("数理A"), 5);
        assert_eq!(clip("数理逻辑", 5), "数理…");
    }
}

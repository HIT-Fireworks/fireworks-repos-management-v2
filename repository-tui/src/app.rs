use std::collections::BTreeSet;
use std::io::{self, stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthChar;

use crate::state::{
    Dashboard, JournalSummary, PlannedOperation, RepositoryDetail, RepositorySummary,
    RoutesSnapshot, SplitTarget,
};

pub fn run_headless(workspace: std::path::PathBuf) -> Result<Dashboard> {
    let dashboard = Dashboard::load(workspace);
    dashboard.validate()?;
    Ok(dashboard)
}

pub fn run(workspace: std::path::PathBuf) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("创建终端绘制器失败")?;
    terminal.clear().context("清理终端失败")?;
    let result = run_loop(&mut terminal, workspace);
    session.leave();
    result
}

struct TerminalSession {
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("启用终端 raw mode 失败")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("进入备用屏幕失败");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Repositories,
    Routes,
    Plans,
    Journals,
}

impl Tab {
    fn next(self) -> Self {
        match self {
            Self::Repositories => Self::Routes,
            Self::Routes => Self::Plans,
            Self::Plans => Self::Journals,
            Self::Journals => Self::Repositories,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Repositories => Self::Journals,
            Self::Routes => Self::Repositories,
            Self::Plans => Self::Routes,
            Self::Journals => Self::Plans,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Repositories => "Repositories",
            Self::Routes => "Routes",
            Self::Plans => "Plans",
            Self::Journals => "Journals",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    Search,
    MergeTarget,
    MergeDisplayName { target_repo_id: String },
    SplitSpec { source_repo_id: String },
    Apply,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Modal {
    None,
    RepositoryDetail,
    Health,
    Help,
    Command,
    PlanDetail,
    JournalDetail,
    Input(InputMode),
}

struct UiState {
    dashboard: Dashboard,
    tab: Tab,
    modal: Modal,
    selected_repository: usize,
    selected_route: usize,
    selected_plan: usize,
    selected_journal: usize,
    repository_detail: Option<RepositoryDetail>,
    plan_detail: Option<PlannedOperation>,
    merge_sources: BTreeSet<String>,
    input: String,
    notice: String,
}

impl UiState {
    fn new(workspace: std::path::PathBuf) -> Self {
        let dashboard = Dashboard::load(workspace);
        let notice = dashboard.logs.last().cloned().unwrap_or_default();
        Self {
            dashboard,
            tab: Tab::Repositories,
            modal: Modal::None,
            selected_repository: 0,
            selected_route: 0,
            selected_plan: 0,
            selected_journal: 0,
            repository_detail: None,
            plan_detail: None,
            merge_sources: BTreeSet::new(),
            input: String::new(),
            notice,
        }
    }

    fn selected_repository(&self) -> Option<&RepositorySummary> {
        self.dashboard.repositories.get(self.selected_repository)
    }

    fn selected_journal(&self) -> Option<&JournalSummary> {
        self.dashboard.journals.get(self.selected_journal)
    }

    fn selected_len(&self) -> usize {
        match self.tab {
            Tab::Repositories => self.dashboard.repositories.len(),
            Tab::Routes => route_count(&self.dashboard.routes),
            Tab::Plans => self.dashboard.plans.len(),
            Tab::Journals => self.dashboard.journals.len(),
        }
    }

    fn selected_index_mut(&mut self) -> &mut usize {
        match self.tab {
            Tab::Repositories => &mut self.selected_repository,
            Tab::Routes => &mut self.selected_route,
            Tab::Plans => &mut self.selected_plan,
            Tab::Journals => &mut self.selected_journal,
        }
    }

    fn clamp_selections(&mut self) {
        self.selected_repository = self
            .selected_repository
            .min(self.dashboard.repositories.len().saturating_sub(1));
        self.selected_route = self
            .selected_route
            .min(route_count(&self.dashboard.routes).saturating_sub(1));
        self.selected_plan = self
            .selected_plan
            .min(self.dashboard.plans.len().saturating_sub(1));
        self.selected_journal = self
            .selected_journal
            .min(self.dashboard.journals.len().saturating_sub(1));
        let existing: BTreeSet<_> = self
            .dashboard
            .repositories
            .iter()
            .map(|item| item.repo_id.as_str())
            .collect();
        self.merge_sources
            .retain(|repo_id| existing.contains(repo_id.as_str()));
    }

    fn refresh(&mut self) {
        match self.dashboard.refresh() {
            Ok(()) => self.notice = "状态已从 JSON 核心刷新".to_string(),
            Err(error) => self.notice = format!("刷新失败：{error}"),
        }
        self.clamp_selections();
    }

    fn move_selection(&mut self, down: bool) {
        let len = self.selected_len();
        let selected = self.selected_index_mut();
        if down {
            *selected = (*selected + 1).min(len.saturating_sub(1));
        } else {
            *selected = selected.saturating_sub(1);
        }
    }

    fn open_repository_detail(&mut self) {
        match self.dashboard.detail(self.selected_repository) {
            Ok(detail) => {
                self.repository_detail = Some(detail);
                self.modal = Modal::RepositoryDetail;
            }
            Err(error) => self.notice = format!("详情失败：{error}"),
        }
    }

    fn open_plan_detail(&mut self) {
        let Some(summary) = self.dashboard.plans.get(self.selected_plan) else {
            self.notice = "没有可审阅计划".to_string();
            return;
        };
        if !summary.valid {
            self.notice = format!(
                "计划无效：{}",
                summary.error.as_deref().unwrap_or("未知错误")
            );
            return;
        }
        let Some(operation_id) = summary.operation_id.as_deref() else {
            self.notice = "计划缺少 operation_id".to_string();
            return;
        };
        match self.dashboard.client.plan_detail(operation_id) {
            Ok(plan) => {
                self.plan_detail = Some(plan);
                self.modal = Modal::PlanDetail;
            }
            Err(error) => self.notice = format!("读取计划失败：{error}"),
        }
    }

    fn open_journal_detail(&mut self) {
        if self.selected_journal().is_some() {
            self.modal = Modal::JournalDetail;
        } else {
            self.notice = "没有 journal".to_string();
        }
    }

    fn toggle_merge_source(&mut self) {
        let Some(repo_id) = self.selected_repository().map(|item| item.repo_id.clone()) else {
            return;
        };
        if !self.merge_sources.remove(&repo_id) {
            self.merge_sources.insert(repo_id);
        }
        self.notice = format!("已选择 {} 个合并源仓库", self.merge_sources.len());
    }

    fn start_command(&mut self) {
        if self.tab != Tab::Repositories {
            self.notice = "计划命令面板只从 Repositories 打开".to_string();
            return;
        }
        self.modal = Modal::Command;
    }

    fn start_merge(&mut self) {
        if self.merge_sources.len() < 2 {
            self.notice = "先用 Space 选择至少两个完整库存仓库".to_string();
            self.modal = Modal::None;
            return;
        }
        self.input = self.merge_sources.iter().next().cloned().unwrap_or_default();
        self.modal = Modal::Input(InputMode::MergeTarget);
    }

    fn start_split(&mut self) {
        let Some(repository) = self.selected_repository() else {
            self.notice = "未选择拆分源仓库".to_string();
            self.modal = Modal::None;
            return;
        };
        if !repository.inventory_complete {
            self.notice = "拆分要求源仓库完整库存已冻结".to_string();
            self.modal = Modal::None;
            return;
        }
        let source_repo_id = repository.repo_id.clone();
        self.input.clear();
        self.modal = Modal::Input(InputMode::SplitSpec { source_repo_id });
    }

    fn accept_plan(&mut self, plan: PlannedOperation) {
        let operation_id = plan.operation_id().to_string();
        self.plan_detail = Some(plan);
        if let Err(error) = self.dashboard.refresh() {
            self.notice = format!("计划已生成，但刷新失败：{error}");
        } else {
            self.notice = format!("冻结计划已生成：{operation_id}");
            if let Some(index) = self
                .dashboard
                .plans
                .iter()
                .position(|item| item.operation_id.as_deref() == Some(operation_id.as_str()))
            {
                self.selected_plan = index;
            }
        }
        self.tab = Tab::Plans;
        self.modal = Modal::PlanDetail;
    }

    fn submit_input(&mut self) {
        let mode = match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::Input(mode) => mode,
            other => {
                self.modal = other;
                return;
            }
        };
        let raw_value = std::mem::take(&mut self.input);
        let value = match mode {
            InputMode::Apply | InputMode::Resume => raw_value,
            _ => raw_value.trim().to_string(),
        };
        match mode {
            InputMode::Search => match self.dashboard.search(value) {
                Ok(()) => {
                    self.selected_repository = 0;
                    self.notice = format!(
                        "找到 {} 个仓库",
                        self.dashboard.repositories.len()
                    );
                }
                Err(error) => self.notice = format!("搜索失败：{error}"),
            },
            InputMode::MergeTarget => {
                if value.is_empty() {
                    self.notice = "合并目标 repo_id 不能为空".to_string();
                    self.modal = Modal::Input(InputMode::MergeTarget);
                    return;
                }
                self.modal = Modal::Input(InputMode::MergeDisplayName {
                    target_repo_id: value,
                });
            }
            InputMode::MergeDisplayName { target_repo_id } => {
                let sources = self.merge_sources.iter().cloned().collect::<Vec<_>>();
                match self
                    .dashboard
                    .client
                    .plan_merge(&sources, &target_repo_id, &value)
                {
                    Ok(plan) => self.accept_plan(plan),
                    Err(error) => self.notice = format!("生成合并计划失败：{error}"),
                }
            }
            InputMode::SplitSpec { source_repo_id } => match parse_split_spec(&value) {
                Ok(targets) => match self
                    .dashboard
                    .client
                    .plan_split(&source_repo_id, &targets)
                {
                    Ok(plan) => self.accept_plan(plan),
                    Err(error) => self.notice = format!("生成拆分计划失败：{error}"),
                },
                Err(error) => {
                    self.notice = error;
                    self.input = value;
                    self.modal = Modal::Input(InputMode::SplitSpec { source_repo_id });
                }
            },
            InputMode::Apply => {
                let Some(plan) = self.plan_detail.clone() else {
                    self.notice = "未加载计划详情".to_string();
                    return;
                };
                let expected = plan.confirmation_phrase().to_string();
                if !exact_challenge(&value, &expected) {
                    self.notice = "确认短语不匹配；未执行任何变更".to_string();
                    self.input = value;
                    self.modal = Modal::Input(InputMode::Apply);
                    return;
                }
                match self.dashboard.client.apply(&plan, &value) {
                    Ok(_) => {
                        self.notice = format!("操作已完成：{}", plan.operation_id());
                        self.tab = Tab::Journals;
                        self.modal = Modal::None;
                        self.refresh();
                        if let Some(index) = self.dashboard.journals.iter().position(|item| {
                            item.operation_id.as_deref() == Some(plan.operation_id())
                        }) {
                            self.selected_journal = index;
                            self.modal = Modal::JournalDetail;
                        }
                    }
                    Err(error) => {
                        self.notice = format!("执行失败：{error}");
                        self.modal = Modal::PlanDetail;
                    }
                }
            }
            InputMode::Resume => {
                let Some(journal) = self.selected_journal().cloned() else {
                    self.notice = "未选择 journal".to_string();
                    return;
                };
                let expected = journal.confirmation_phrase.clone().unwrap_or_default();
                if !exact_challenge(&value, &expected) {
                    self.notice = "恢复短语不匹配；未执行任何变更".to_string();
                    self.input = value;
                    self.modal = Modal::Input(InputMode::Resume);
                    return;
                }
                match self.dashboard.client.resume(&journal, &value) {
                    Ok(_) => {
                        self.notice = format!(
                            "恢复完成：{}",
                            journal.operation_id.as_deref().unwrap_or("unknown")
                        );
                        self.modal = Modal::None;
                        self.refresh();
                        self.modal = Modal::JournalDetail;
                    }
                    Err(error) => {
                        self.notice = format!("恢复失败：{error}");
                        self.modal = Modal::JournalDetail;
                    }
                }
            }
        }
        self.clamp_selections();
    }

    fn start_apply(&mut self) {
        let Some(plan) = self.plan_detail.as_ref() else {
            self.notice = "未加载计划详情".to_string();
            return;
        };
        let state = self
            .dashboard
            .plans
            .iter()
            .find(|item| item.operation_id.as_deref() == Some(plan.operation_id()))
            .and_then(|item| item.state.as_deref());
        if state != Some("before") {
            self.notice = format!("计划当前状态不可 apply：{}", state.unwrap_or("unknown"));
            return;
        }
        self.input.clear();
        self.modal = Modal::Input(InputMode::Apply);
    }

    fn start_resume(&mut self) {
        let Some(journal) = self.selected_journal() else {
            self.notice = "未选择 journal".to_string();
            return;
        };
        if journal.recovery_state != "resumable" {
            self.notice = format!("journal 不可恢复：{}", journal.recovery_state);
            return;
        }
        self.input.clear();
        self.modal = Modal::Input(InputMode::Resume);
    }

    fn verify_journal(&mut self) {
        let Some(journal) = self.selected_journal().cloned() else {
            self.notice = "未选择 journal".to_string();
            return;
        };
        if journal.recovery_state != "completed" {
            self.notice = "只有 completed journal 可验证".to_string();
            return;
        }
        match self.dashboard.client.verify(&journal) {
            Ok(_) => self.notice = format!(
                "验证通过：{}",
                journal.operation_id.as_deref().unwrap_or("unknown")
            ),
            Err(error) => self.notice = format!("验证失败：{error}"),
        }
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
    if matches!(state.modal, Modal::Input(_)) {
        match key.code {
            KeyCode::Enter => state.submit_input(),
            KeyCode::Esc => {
                state.input.clear();
                state.modal = Modal::None;
            }
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                state.input.push(character)
            }
            _ => {}
        }
        return false;
    }

    match state.modal {
        Modal::RepositoryDetail | Modal::Health | Modal::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('b') | KeyCode::Enter => state.modal = Modal::None,
            _ => {}
        },
        Modal::Command => match key.code {
            KeyCode::Char('m') => state.start_merge(),
            KeyCode::Char('s') => state.start_split(),
            KeyCode::Esc | KeyCode::Char('b') => state.modal = Modal::None,
            _ => {}
        },
        Modal::PlanDetail => match key.code {
            KeyCode::Char('a') => state.start_apply(),
            KeyCode::Esc | KeyCode::Char('b') => state.modal = Modal::None,
            _ => {}
        },
        Modal::JournalDetail => match key.code {
            KeyCode::Char('r') => state.start_resume(),
            KeyCode::Char('v') => state.verify_journal(),
            KeyCode::Esc | KeyCode::Char('b') => state.modal = Modal::None,
            _ => {}
        },
        Modal::Input(_) => unreachable!(),
        Modal::None => match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Tab => {
                state.tab = state.tab.next();
                state.repository_detail = None;
                state.plan_detail = None;
            }
            KeyCode::BackTab => {
                state.tab = state.tab.previous();
                state.repository_detail = None;
                state.plan_detail = None;
            }
            KeyCode::Down | KeyCode::Char('j') => state.move_selection(true),
            KeyCode::Up | KeyCode::Char('k') => state.move_selection(false),
            KeyCode::Enter => match state.tab {
                Tab::Repositories => state.open_repository_detail(),
                Tab::Plans => state.open_plan_detail(),
                Tab::Journals => state.open_journal_detail(),
                Tab::Routes => {}
            },
            KeyCode::Char(' ') if state.tab == Tab::Repositories => {
                state.toggle_merge_source()
            }
            KeyCode::Char('/') if state.tab == Tab::Repositories => {
                state.input = state.dashboard.query.clone();
                state.modal = Modal::Input(InputMode::Search);
            }
            KeyCode::Char('p') => state.start_command(),
            KeyCode::Char('h') => state.modal = Modal::Health,
            KeyCode::Char('?') => state.modal = Modal::Help,
            KeyCode::Char('r') => state.refresh(),
            _ => {}
        },
    }
    false
}

fn parse_split_spec(value: &str) -> std::result::Result<Vec<SplitTarget>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("拆分目标规格不能为空".to_string());
    }
    let targets = if value.starts_with('[') {
        serde_json::from_str::<Vec<SplitTarget>>(value)
            .map_err(|error| format!("拆分 JSON 无效：{error}"))?
    } else {
        let mut targets = Vec::new();
        for segment in value.split(';').map(str::trim).filter(|item| !item.is_empty()) {
            let fields = segment.splitn(4, '|').collect::<Vec<_>>();
            if fields.len() != 4 {
                return Err(
                    "紧凑格式必须是 repo_id|展示名|group1,group2|path1,path2；目标间用 ; 分隔"
                        .to_string(),
                );
            }
            let repo_id = fields[0].trim().to_string();
            let display_name = if fields[1].trim().is_empty() {
                repo_id.clone()
            } else {
                fields[1].trim().to_string()
            };
            let resource_group_ids = split_csv(fields[2]);
            let paths = split_csv(fields[3]);
            targets.push(SplitTarget {
                repo_id,
                display_name,
                resource_group_ids,
                paths,
            });
        }
        targets
    };
    if targets.len() < 2 {
        return Err("拆分至少需要两个目标仓库".to_string());
    }
    if targets.iter().any(|target| {
        target.repo_id.trim().is_empty()
            || (target.resource_group_ids.is_empty() && target.paths.is_empty())
    }) {
        return Err("每个拆分目标必须有 repo_id，且至少分配一个资源组或路径".to_string());
    }
    Ok(targets)
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn exact_challenge(value: &str, expected: &str) -> bool {
    !expected.is_empty() && value == expected
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
    draw_header(frame, vertical[0], state);
    match state.tab {
        Tab::Repositories => draw_repositories(frame, vertical[1], state),
        Tab::Routes => draw_routes(frame, vertical[1], state),
        Tab::Plans => draw_plans(frame, vertical[1], state),
        Tab::Journals => draw_journals(frame, vertical[1], state),
    }
    frame.render_widget(
        Paragraph::new(clip(
            &state.notice,
            vertical[2].width.saturating_sub(1) as usize,
        )),
        vertical[2],
    );
    draw_modal(frame, area, state);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let health = &state.dashboard.health;
    let status_color = if health.health == "healthy" {
        Color::Green
    } else {
        Color::Red
    };
    let mut spans = vec![
        Span::styled(
            " 薪火仓库管理中心 ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(&health.health, Style::default().fg(status_color)),
        Span::raw("  "),
    ];
    for tab in [
        Tab::Repositories,
        Tab::Routes,
        Tab::Plans,
        Tab::Journals,
    ] {
        let style = if tab == state.tab {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {} ", tab.label()), style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_repositories(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    if area.width < 78 {
        let lines = visible_range(
            state.selected_repository,
            state.dashboard.repositories.len(),
            area.height.saturating_sub(2) as usize,
        )
        .map(|index| {
            let repository = &state.dashboard.repositories[index];
            let marker = if state.merge_sources.contains(&repository.repo_id) {
                "[x]"
            } else {
                "[ ]"
            };
            let prefix = if index == state.selected_repository { ">" } else { " " };
            Line::from(format!(
                "{prefix}{marker} {} · {} · {} files\n  {}",
                repository.display_name,
                repository.repo_type,
                repository.file_count,
                repository.repo_id
            ))
        })
        .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(repository_title(state)),
            ),
            area,
        );
        return;
    }
    let rows = visible_range(
        state.selected_repository,
        state.dashboard.repositories.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let repository = &state.dashboard.repositories[index];
        let style = selected_style(index == state.selected_repository);
        let marker = if state.merge_sources.contains(&repository.repo_id) {
            "[x]"
        } else {
            "[ ]"
        };
        Row::new(vec![
            Cell::from(marker),
            Cell::from(repository.display_name.clone()),
            Cell::from(repository.repo_type.clone()),
            Cell::from(repository.file_count.to_string()),
            Cell::from(repository.repo_id.clone()),
        ])
        .style(style)
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Percentage(36),
                Constraint::Length(14),
                Constraint::Length(8),
                Constraint::Percentage(48),
            ],
        )
        .header(
            Row::new(["选", "语义名称", "类型", "文件", "repo_id"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(repository_title(state)),
        ),
        area,
    );
}

fn repository_title(state: &UiState) -> String {
    format!(
        " 仓库 {} · 搜索={} · Space 多选 · p 计划 ",
        state.dashboard.repositories.len(),
        if state.dashboard.query.is_empty() {
            "全部"
        } else {
            &state.dashboard.query
        }
    )
}

fn route_count(routes: &RoutesSnapshot) -> usize {
    routes.file_routes.len() + routes.course_code_routes.len()
}

fn route_row(routes: &RoutesSnapshot, index: usize) -> (String, String, String) {
    if let Some(route) = routes.file_routes.get(index) {
        return (
            "file".to_string(),
            route.get("repo_id")
                .and_then(ValueExt::string)
                .unwrap_or("—")
                .to_string(),
            format!(
                "{} · {}",
                route
                    .get("path")
                    .and_then(ValueExt::string)
                    .unwrap_or("—"),
                route
                    .get("route_kind")
                    .and_then(ValueExt::string)
                    .unwrap_or("unclassified")
            ),
        );
    }
    let code_index = index.saturating_sub(routes.file_routes.len());
    let route = routes.course_code_routes.get(code_index);
    (
        "course".to_string(),
        route
            .and_then(|item| item.get("repo_id"))
            .and_then(ValueExt::string)
            .unwrap_or("—")
            .to_string(),
        route
            .and_then(|item| item.get("course_code"))
            .and_then(ValueExt::string)
            .unwrap_or("—")
            .to_string(),
    )
}

trait ValueExt {
    fn string(&self) -> Option<&str>;
}

impl ValueExt for serde_json::Value {
    fn string(&self) -> Option<&str> {
        self.as_str()
    }
}

fn draw_routes(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let routes = &state.dashboard.routes;
    let total = route_count(routes);
    let rows = visible_range(
        state.selected_route,
        total,
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let (kind, repo_id, value) = route_row(routes, index);
        Row::new([kind, repo_id, value]).style(selected_style(index == state.selected_route))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Percentage(38),
                Constraint::Percentage(62),
            ],
        )
        .header(
            Row::new(["类型", "repo_id", "路由"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Routes · files={} · course_codes={} ",
            routes.file_total, routes.course_code_total
        ))),
        area,
    );
}

fn draw_plans(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let rows = visible_range(
        state.selected_plan,
        state.dashboard.plans.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let plan = &state.dashboard.plans[index];
        Row::new([
            plan.operation_id.as_deref().unwrap_or("invalid").to_string(),
            plan.kind.as_deref().unwrap_or("—").to_string(),
            plan.state.as_deref().unwrap_or("invalid").to_string(),
            if plan.valid {
                short_hash(plan.plan_identity_sha256.as_deref().unwrap_or(""))
            } else {
                plan.error.as_deref().unwrap_or("invalid").to_string()
            },
        ])
        .style(selected_style(index == state.selected_plan))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(35),
                Constraint::Length(10),
                Constraint::Length(18),
                Constraint::Percentage(55),
            ],
        )
        .header(
            Row::new(["operation_id", "kind", "state", "identity / error"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Plans · Enter 审阅 · a 仅在详情中执行 "),
        ),
        area,
    );
}

fn draw_journals(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let rows = visible_range(
        state.selected_journal,
        state.dashboard.journals.len(),
        area.height.saturating_sub(3) as usize,
    )
    .map(|index| {
        let journal = &state.dashboard.journals[index];
        Row::new([
            journal
                .operation_id
                .as_deref()
                .unwrap_or("invalid")
                .to_string(),
            journal.kind.as_deref().unwrap_or("—").to_string(),
            journal.status.clone(),
            journal.recovery_state.clone(),
            journal.error.as_deref().unwrap_or("").to_string(),
        ])
        .style(selected_style(index == state.selected_journal))
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(30),
                Constraint::Length(9),
                Constraint::Length(12),
                Constraint::Length(14),
                Constraint::Percentage(35),
            ],
        )
        .header(
            Row::new(["operation_id", "kind", "status", "recovery", "error"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Journals · Enter 详情 · r 恢复 / v 验证仅在详情中 "),
        ),
        area,
    );
}

fn draw_modal(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    match &state.modal {
        Modal::None => {}
        Modal::RepositoryDetail => draw_repository_detail(frame, area, state),
        Modal::Health => draw_health(frame, area, state),
        Modal::Help => draw_help(frame, area),
        Modal::Command => draw_command(frame, area, state),
        Modal::PlanDetail => draw_plan_detail(frame, area, state),
        Modal::JournalDetail => draw_journal_detail(frame, area, state),
        Modal::Input(mode) => draw_input(frame, area, state, mode),
    }
}

fn draw_repository_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let popup = centered_rect(88, area.height.saturating_sub(6).min(26), area);
    frame.render_widget(Clear, popup);
    let content = if let Some(detail) = &state.repository_detail {
        vec![
            format!("语义: {}", detail.summary.display_name),
            format!("repo_id: {}", detail.summary.repo_id),
            format!("类型: {}", detail.summary.repo_type),
            format!("description: {}", detail.summary.description),
            format!(
                "physical_repository_id: {}",
                detail.physical_repository_id.as_deref().unwrap_or("—")
            ),
            format!("课程代码: {}", detail.summary.course_codes.join(", ")),
            format!("原始课程名: {}", detail.summary.course_names.join(", ")),
            format!(
                "资源组: {}",
                detail.summary.member_resource_group_ids.join(", ")
            ),
            format!("无归属路径: {}", detail.summary.unowned_paths.join(", ")),
            format!(
                "文件 / 容量: {} / {} B",
                detail.summary.file_count, detail.summary.bytes
            ),
            format!(
                "库存: {}",
                if detail.summary.inventory_complete {
                    "完整"
                } else {
                    "未完整"
                }
            ),
            format!("HEAD: {}", detail.summary.head.as_deref().unwrap_or("未冻结")),
        ]
        .join("\n")
    } else {
        "未加载详情".to_string()
    };
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 仓库详情 · Esc/b 返回 ")),
        popup,
    );
}

fn draw_health(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let popup = centered_rect(80, area.height.saturating_sub(8).min(24), area);
    frame.render_widget(Clear, popup);
    let content = serde_json::to_string_pretty(&state.dashboard.health).unwrap_or_default();
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 健康状态 · Esc/b 返回 ")),
        popup,
    );
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(82, area.height.saturating_sub(8).min(25), area);
    frame.render_widget(Clear, popup);
    let content = "Tab / Shift-Tab  切换 Repositories / Routes / Plans / Journals\nj/k 或方向键      移动\nEnter             打开详情\n/                 搜索仓库\nSpace             多选合并源仓库\np                 计划命令面板（split / merge）\nh                 健康状态\nr                 刷新只读快照\n?                 帮助\nq                 退出\n\n计划详情：a 输入核心返回的精确 APPLY challenge。\njournal 详情：r 仅恢复 resumable journal；v 仅验证 completed journal。\nTUI 不直接改写状态文件或调用 GitHub；所有变更都通过 JSON 核心。";
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 帮助 ")),
        popup,
    );
}

fn draw_command(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let popup = centered_rect(68, 10, area);
    frame.render_widget(Clear, popup);
    let selected = state.merge_sources.iter().cloned().collect::<Vec<_>>().join(", ");
    let source = state
        .selected_repository()
        .map(|item| item.repo_id.as_str())
        .unwrap_or("—");
    frame.render_widget(
        Paragraph::new(format!(
            "[m] 合并 Space 已选仓库（{}）\n[s] 拆分当前仓库（{}）\n\n已选源: {}\n\nEsc/b 取消",
            state.merge_sources.len(),
            source,
            if selected.is_empty() { "—" } else { &selected }
        ))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" 计划命令面板 ")),
        popup,
    );
}

fn draw_plan_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let popup = centered_rect(92, area.height.saturating_sub(4).min(30), area);
    frame.render_widget(Clear, popup);
    let content = if let Some(plan) = &state.plan_detail {
        let kind = plan.plan.get("kind").and_then(ValueExt::string).unwrap_or("—");
        let sources = plan
            .risk
            .get("source_repo_ids")
            .map(|value| compact_json(value))
            .unwrap_or_else(|| "[]".to_string());
        let targets = plan
            .risk
            .get("target_repo_ids")
            .map(|value| compact_json(value))
            .unwrap_or_else(|| "[]".to_string());
        let moves = plan
            .risk
            .get("file_move_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let state_value = state
            .dashboard
            .plans
            .iter()
            .find(|item| item.operation_id.as_deref() == Some(plan.operation_id()))
            .and_then(|item| item.state.as_deref())
            .unwrap_or("unknown");
        format!(
            "operation_id: {}\nkind: {}\nstate: {}\nplan identity: {}\nconfirmation: {}\nsources: {}\ntargets: {}\nfile moves: {}\nplan path: {}\n\n风险：将创建或更新目标 GitHub 仓库并原子切换 topology/routes；远端、actor、Registry、源 commit/tree、目标 HEAD 和 workspace identity 任一漂移即停止。\n\n[a] 输入精确 confirmation 并执行 · Esc/b 返回",
            plan.operation_id(),
            kind,
            state_value,
            plan.identity(),
            plan.confirmation_phrase(),
            sources,
            targets,
            moves,
            plan.path
        )
    } else {
        "未加载计划详情".to_string()
    };
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 冻结计划审阅 ")),
        popup,
    );
}

fn draw_journal_detail(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let popup = centered_rect(88, area.height.saturating_sub(8).min(24), area);
    frame.render_widget(Clear, popup);
    let content = if let Some(journal) = state.selected_journal() {
        format!(
            "operation_id: {}\nkind: {}\nstatus: {}\nrecovery: {}\nplan identity: {}\nresume challenge: {}\nupdated_at: {}\nerror: {}\njournal path: {}\n\n[r] 仅在 resumable 时输入精确 challenge 恢复\n[v] 仅在 completed 时验证最终状态和远端 HEAD\nEsc/b 返回",
            journal.operation_id.as_deref().unwrap_or("invalid"),
            journal.kind.as_deref().unwrap_or("—"),
            journal.status,
            journal.recovery_state,
            journal.plan_identity_sha256.as_deref().unwrap_or("—"),
            journal.confirmation_phrase.as_deref().unwrap_or("—"),
            journal.updated_at.as_deref().unwrap_or("—"),
            journal.error.as_deref().unwrap_or("—"),
            journal.path
        )
    } else {
        "未选择 journal".to_string()
    };
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Journal 详情 ")),
        popup,
    );
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, state: &UiState, mode: &InputMode) {
    let height = if matches!(mode, InputMode::SplitSpec { .. }) {
        area.height.saturating_sub(6).min(24)
    } else {
        10
    };
    let popup = centered_rect(88, height, area);
    frame.render_widget(Clear, popup);
    let (title, prompt) = input_prompt(state, mode);
    frame.render_widget(
        Paragraph::new(format!("{}\n\n> {}", prompt, state.input))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        popup,
    );
}

fn input_prompt(state: &UiState, mode: &InputMode) -> (String, String) {
    match mode {
        InputMode::Search => (
            " 搜索 · Enter 提交 · Esc 取消 ".to_string(),
            "按 repo_id、语义、课程代码或原始课程名搜索；空值显示全部。".to_string(),
        ),
        InputMode::MergeTarget => (
            " 合并向导 1/2 · 目标 repo_id ".to_string(),
            format!(
                "源仓库：{}\n目标可为一个源仓库，也可为新的空仓库。",
                state.merge_sources.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        ),
        InputMode::MergeDisplayName { target_repo_id } => (
            " 合并向导 2/2 · 展示名 ".to_string(),
            format!("目标 repo_id：{target_repo_id}\n输入稳定上位语义展示名。"),
        ),
        InputMode::SplitSpec { source_repo_id } => {
            let repository = state
                .dashboard
                .repositories
                .iter()
                .find(|item| &item.repo_id == source_repo_id);
            let groups = repository
                .map(|item| item.member_resource_group_ids.join(","))
                .unwrap_or_default();
            let paths = repository
                .map(|item| item.unowned_paths.join(","))
                .unwrap_or_default();
            (
                " 拆分向导 · 完整互斥分区 ".to_string(),
                format!(
                    "源：{source_repo_id}\n必须恰好分配全部资源组：{}\n必须显式裁决无归属路径：{}\n\n紧凑格式：repo_id|展示名|group1,group2|path1,path2;repo_id2|展示名2|group3|\n路径含逗号等特殊字符时可输入 SplitTarget JSON 数组。",
                    if groups.is_empty() { "—" } else { &groups },
                    if paths.is_empty() { "—" } else { &paths }
                ),
            )
        }
        InputMode::Apply => {
            let expected = state
                .plan_detail
                .as_ref()
                .map(PlannedOperation::confirmation_phrase)
                .unwrap_or("");
            (
                " 危险操作确认 · APPLY ".to_string(),
                format!(
                    "已审阅当前 plan identity 后，原样输入：\n{expected}\n\n任何其他输入都不会执行。"
                ),
            )
        }
        InputMode::Resume => {
            let expected = state
                .selected_journal()
                .and_then(|item| item.confirmation_phrase.as_deref())
                .unwrap_or("");
            (
                " 恢复确认 · RESUME ".to_string(),
                format!(
                    "核心已将该 journal 标记为 resumable。原样输入：\n{expected}\n\n漂移或无效 journal 不允许恢复。"
                ),
            )
        }
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

fn selected_style(selected: bool) -> Style {
    if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    }
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
    let ellipsis_width = display_width("…");
    let target = width.saturating_sub(ellipsis_width);
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

fn short_hash(value: &str) -> String {
    value.chars().take(12).collect()
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CoreClient, Health, RoutesSnapshot};
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn test_state() -> UiState {
        UiState {
            dashboard: Dashboard {
                client: CoreClient::new(PathBuf::from(".")),
                health: Health {
                    organization: "HIT-Fireworks".to_string(),
                    health: "healthy".to_string(),
                    repository_count: 100,
                    course_route_count: 2618,
                    file_route_count: 3857,
                    ..Health::default()
                },
                repositories: Vec::new(),
                routes: RoutesSnapshot::default(),
                plans: Vec::new(),
                journals: Vec::new(),
                query: String::new(),
                logs: Vec::new(),
            },
            tab: Tab::Repositories,
            modal: Modal::None,
            selected_repository: 0,
            selected_route: 0,
            selected_plan: 0,
            selected_journal: 0,
            repository_detail: None,
            plan_detail: None,
            merge_sources: BTreeSet::new(),
            input: String::new(),
            notice: String::new(),
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn search_accepts_first_character_and_redraws() {
        let mut state = test_state();
        assert!(!handle_key(&mut state, press(KeyCode::Char('/'))));
        assert_eq!(state.modal, Modal::Input(InputMode::Search));
        assert!(!handle_key(&mut state, press(KeyCode::Char('A'))));
        assert_eq!(state.input, "A");
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains('A'));
    }

    #[test]
    fn search_accepts_multibyte_character_and_redraws() {
        let mut state = test_state();
        handle_key(&mut state, press(KeyCode::Char('/')));
        handle_key(&mut state, press(KeyCode::Char('数')));
        assert_eq!(state.input, "数");
        let backend = TestBackend::new(120, 40);
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
    fn tabs_cover_complete_workspace() {
        let mut state = test_state();
        for expected in [Tab::Routes, Tab::Plans, Tab::Journals, Tab::Repositories] {
            handle_key(&mut state, press(KeyCode::Tab));
            assert_eq!(state.tab, expected);
        }
    }

    #[test]
    fn split_compact_spec_preserves_partition_fields() {
        let targets = parse_split_spec(
            "A1|课程一|g1,g2|README.md;A2|课程二|g3|notes/other.txt",
        )
        .unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].resource_group_ids, ["g1", "g2"]);
        assert_eq!(targets[1].paths, ["notes/other.txt"]);
    }

    #[test]
    fn confirmation_is_exact_and_case_sensitive() {
        assert!(exact_challenge("APPLY operation-1", "APPLY operation-1"));
        assert!(!exact_challenge("apply operation-1", "APPLY operation-1"));
        assert!(!exact_challenge("APPLY operation-1 ", "APPLY operation-1"));
    }

    #[test]
    fn shift_tab_cycles_workspace_backwards() {
        let mut state = test_state();
        handle_key(&mut state, press(KeyCode::BackTab));
        assert_eq!(state.tab, Tab::Journals);
        handle_key(&mut state, press(KeyCode::BackTab));
        assert_eq!(state.tab, Tab::Plans);
    }

    #[test]
    fn plan_command_is_only_available_from_repositories() {
        let mut state = test_state();
        state.tab = Tab::Routes;
        handle_key(&mut state, press(KeyCode::Char('p')));
        assert_eq!(state.modal, Modal::None);
        assert!(state.notice.contains("Repositories"));
        state.tab = Tab::Repositories;
        handle_key(&mut state, press(KeyCode::Char('p')));
        assert_eq!(state.modal, Modal::Command);
    }

    #[test]
    fn input_escape_cancels_without_calling_core() {
        let mut state = test_state();
        state.modal = Modal::Input(InputMode::Apply);
        state.input = "APPLY operation-1".to_string();
        handle_key(&mut state, press(KeyCode::Esc));
        assert_eq!(state.modal, Modal::None);
        assert!(state.input.is_empty());
    }

    #[test]
    fn apply_challenge_preserves_trailing_space_as_mismatch() {
        let mut state = test_state();
        state.plan_detail = Some(PlannedOperation {
            path: "unused.plan.json".to_string(),
            plan: serde_json::json!({
                "operation_id": "operation-1",
                "core": {
                    "plan_identity_sha256": "identity",
                    "confirmation_phrase": "APPLY operation-1"
                }
            }),
            risk: serde_json::json!({}),
        });
        state.modal = Modal::Input(InputMode::Apply);
        state.input = "APPLY operation-1 ".to_string();
        state.submit_input();
        assert_eq!(state.modal, Modal::Input(InputMode::Apply));
        assert_eq!(state.input, "APPLY operation-1 ");
        assert!(state.notice.contains("不匹配"));
    }

    #[test]
    fn unicode_clip_uses_display_width() {
        assert_eq!(display_width("数理A"), 5);
        assert_eq!(clip("数理逻辑", 5), "数理…");
    }
}

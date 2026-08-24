use std::{collections::HashMap, path::PathBuf, time::Duration};

use gpui::{
    App, Bounds, ClickEvent, Context, Corner, Div, FocusHandle, Focusable, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, ScrollAnchor, ScrollHandle, SharedString, Styled, Window,
    WindowBounds, WindowOptions, actions, anchored, deferred, div, point, prelude::*, px, relative,
    size,
};

use crate::{
    bd::BdClient,
    herdr::{self, AgentInfo, AgentKind},
    model::{DashboardData, EpicSummary, Issue, WorkState},
    theme,
};

actions!(dashboard, [FocusSearch]);

pub struct Dashboard {
    bd: BdClient,
    data: DashboardData,
    selected: Option<String>,
    message: Option<String>,
    dashboard_scroll: ScrollHandle,
    completed_toast: Vec<Issue>,
    focus_handle: FocusHandle,
    search_open: bool,
    search_query: String,
    agent_menu_for: Option<String>,
    launching_agent: Option<String>,
    agent_notice: Option<(String, bool, String)>,
    inspector_height: Option<Pixels>,
    inspector_resize: Option<(Pixels, Pixels)>,
    agents: HashMap<String, AgentInfo>,
    hovered_working: Option<String>,
    agent_previews: HashMap<String, Vec<String>>,
    working_menu_for: Option<String>,
}

#[derive(Clone)]
struct WorkingItem {
    issue: Issue,
    epic_title: Option<String>,
    anchor: Option<ScrollAnchor>,
    agent: Option<AgentInfo>,
}

impl Dashboard {
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        let bd = BdClient::new(project);
        Self::start_auto_refresh(cx);
        Self::start_preview_refresh(cx);
        match bd.load() {
            Ok(data) => Self {
                bd,
                data,
                selected: None,
                message: None,
                dashboard_scroll: ScrollHandle::new(),
                completed_toast: Vec::new(),
                focus_handle: cx.focus_handle(),
                search_open: false,
                search_query: String::new(),
                agent_menu_for: None,
                launching_agent: None,
                agent_notice: None,
                inspector_height: None,
                inspector_resize: None,
                agents: HashMap::new(),
                hovered_working: None,
                agent_previews: HashMap::new(),
                working_menu_for: None,
            },
            Err(error) => Self {
                bd,
                data: DashboardData::default(),
                selected: None,
                message: Some(error.to_string()),
                dashboard_scroll: ScrollHandle::new(),
                completed_toast: Vec::new(),
                focus_handle: cx.focus_handle(),
                search_open: false,
                search_query: String::new(),
                agent_menu_for: None,
                launching_agent: None,
                agent_notice: None,
                inspector_height: None,
                inspector_resize: None,
                agents: HashMap::new(),
                hovered_working: None,
                agent_previews: HashMap::new(),
                working_menu_for: None,
            },
        }
    }

    fn start_auto_refresh(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let Some(client) = this.update(cx, |dashboard, _| dashboard.bd.clone()).ok() else {
                    break;
                };
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let result = client.load();
                        let agents = result
                            .as_ref()
                            .map(|data| herdr::discover_agents(&data.issues, client.project()))
                            .unwrap_or_default();
                        (result, agents)
                    })
                    .await;
                if this
                    .update(cx, |dashboard, cx| {
                        dashboard.agents = result.1;
                        dashboard.apply_load(result.0, true, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn start_preview_refresh(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(650))
                    .await;
                let Some((bead_id, agent, cwd)) = this
                    .update(cx, |dashboard, _| {
                        let bead_id = dashboard.hovered_working.clone()?;
                        let agent = dashboard.agents.get(&bead_id)?.clone();
                        Some((bead_id, agent, dashboard.bd.project().to_path_buf()))
                    })
                    .ok()
                    .flatten()
                else {
                    continue;
                };
                let name = agent.name.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { herdr::read_agent_preview(&name, &cwd) })
                    .await;
                if let Ok(lines) = result {
                    this.update(cx, |dashboard, cx| {
                        if dashboard.hovered_working.as_deref() == Some(bead_id.as_str()) {
                            dashboard.agent_previews.insert(bead_id, lines);
                            cx.notify();
                        }
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn apply_load(
        &mut self,
        result: anyhow::Result<DashboardData>,
        show_completions: bool,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(data) => {
                if show_completions {
                    for issue in &data.issues {
                        let just_closed = issue.status == "closed"
                            && self
                                .data
                                .issue(&issue.id)
                                .is_some_and(|previous| previous.status != "closed");
                        if just_closed
                            && !self.completed_toast.iter().any(|item| item.id == issue.id)
                        {
                            self.completed_toast.push(issue.clone());
                        }
                    }
                }
                self.data = data;
                self.message = None;
            }
            Err(error) => self.message = Some(error.to_string()),
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let result = self.bd.load();
        self.apply_load(result, true, cx);
    }

    fn refresh(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn open_search(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn clear_search(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.search_query.clear();
        self.search_open = false;
        self.dashboard_scroll
            .set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    fn search_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.search_open {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                self.search_query.clear();
                self.search_open = false;
            }
            "backspace" => {
                self.search_query.pop();
            }
            "enter" => self.search_open = false,
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = &event.keystroke.key_char {
                    if text.chars().all(|character| !character.is_control()) {
                        self.search_query.push_str(text);
                    }
                }
            }
            _ => return,
        }
        self.dashboard_scroll
            .set_offset(gpui::point(px(0.), px(0.)));
        cx.stop_propagation();
        cx.notify();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id);
        cx.notify();
    }

    fn dismiss_inspector(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        self.inspector_resize = None;
        cx.notify();
    }

    fn begin_inspector_resize(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let height = self
            .inspector_height
            .unwrap_or(window.bounds().size.height * 0.52);
        self.inspector_resize = Some((event.position.y, height));
        cx.stop_propagation();
    }

    fn dismiss_completion_toast(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.completed_toast.clear();
        cx.notify();
    }

    fn cycle_priority(&mut self, id: String, current: u8, cx: &mut Context<Self>) {
        let next = (current + 1) % 5;
        if let Err(error) = self.bd.set_priority(&id, next) {
            self.message = Some(error.to_string());
        } else {
            self.reload(cx);
            self.selected = Some(id);
        }
        cx.notify();
    }

    fn move_to_epic(&mut self, id: String, parent: Option<String>, cx: &mut Context<Self>) {
        if let Err(error) = self.bd.set_parent(&id, parent.as_deref()) {
            self.message = Some(error.to_string());
        } else {
            self.reload(cx);
            self.selected = Some(id);
        }
        cx.notify();
    }

    fn hover_working(&mut self, id: String, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hovered_working = Some(id);
        } else if self.hovered_working.as_deref() == Some(id.as_str()) {
            self.hovered_working = None;
        }
        cx.notify();
    }

    fn toggle_working_menu(
        &mut self,
        id: String,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.working_menu_for = if self.working_menu_for.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    fn close_working_menu(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.working_menu_for = None;
        cx.notify();
    }

    fn open_working_inspector(&mut self, id: String, cx: &mut Context<Self>) {
        self.working_menu_for = None;
        self.selected = Some(id);
        cx.notify();
    }

    fn refresh_working_preview(&mut self, id: String, cx: &mut Context<Self>) {
        self.working_menu_for = None;
        self.agent_previews.remove(&id);
        self.hovered_working = Some(id);
        cx.notify();
    }

    fn focus_working_agent(&mut self, name: String, cx: &mut Context<Self>) {
        self.working_menu_for = None;
        let cwd = self.bd.project().to_path_buf();
        let task = cx
            .background_executor()
            .spawn(async move { herdr::focus_agent(&name, &cwd) });
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |dashboard, cx| {
                    dashboard.message = Some(error.to_string());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn toggle_agent_menu(
        &mut self,
        id: String,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agent_menu_for = if self.agent_menu_for.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        self.agent_notice = None;
        cx.notify();
    }

    fn close_agent_menu(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.agent_menu_for = None;
        cx.notify();
    }

    fn launch_agent(
        &mut self,
        id: String,
        kind: AgentKind,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.launching_agent.is_some() {
            return;
        }
        let Some(issue) = self.data.issue(&id).cloned() else {
            return;
        };
        let cwd = self.bd.project().to_path_buf();
        let label = kind.label(model.as_deref());
        self.agent_menu_for = None;
        self.launching_agent = Some(id.clone());
        self.agent_notice = Some((id.clone(), false, format!("Starting {label}…")));
        cx.notify();

        let task = cx
            .background_executor()
            .spawn(async move { herdr::launch_agent(&issue, kind, model.as_deref(), &cwd) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |dashboard, cx| {
                dashboard.launching_agent = None;
                dashboard.agent_notice = Some(match result {
                    Ok(name) => (id.clone(), false, format!("Started {label} as {name}")),
                    Err(error) => (id.clone(), true, error.to_string()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn priority_pill(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = issue.id.clone();
        let priority = issue.priority;
        div()
            .id(SharedString::from(format!("priority:{}", issue.id)))
            .px_1()
            .rounded_sm()
            .bg(theme::priority(priority).opacity(0.14))
            .border_1()
            .border_color(theme::priority(priority).opacity(0.38))
            .text_size(px(9.))
            .text_color(theme::priority(priority))
            .cursor_pointer()
            .hover(|style| style.bg(theme::priority(priority).opacity(0.25)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.cycle_priority(id.clone(), priority, cx);
            }))
            .child(format!("P{priority}"))
    }

    fn state_badge(&self, state: WorkState) -> Div {
        let (label, color) = match state {
            WorkState::Ready => ("READY", theme::ready()),
            WorkState::Blocked => ("BLOCKED", theme::blocked()),
            WorkState::InProgress => ("IN PROGRESS", theme::progress()),
            WorkState::Closed => ("CLOSED", theme::muted()),
            WorkState::Other => ("OTHER", theme::muted()),
        };
        div()
            .px_1()
            .rounded_sm()
            .bg(color.opacity(0.12))
            .border_1()
            .border_color(color.opacity(0.3))
            .text_size(px(9.))
            .text_color(color)
            .child(label)
    }

    fn issue_row(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let state = self.data.state(&issue.id);
        let id = issue.id.clone();
        let state_color = match state {
            WorkState::Ready => theme::ready(),
            WorkState::Blocked => theme::blocked(),
            WorkState::InProgress => theme::progress(),
            _ => theme::muted(),
        };
        div()
            .id(SharedString::from(format!("issue:{}", issue.id)))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
            .child(
                div()
                    .w(px(3.))
                    .h(px(18.))
                    .rounded_full()
                    .bg(state_color.opacity(0.8)),
            )
            .child(self.priority_pill(issue, cx))
            .child(
                div()
                    .w(px(96.))
                    .min_w(px(72.))
                    .text_xs()
                    .text_color(theme::muted())
                    .truncate()
                    .child(issue.id.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme::text())
                    .truncate()
                    .child(issue.title.clone()),
            )
            .child(self.state_badge(state))
    }

    fn epic_card(
        &self,
        summary: &EpicSummary,
        anchor: ScrollAnchor,
        query: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let epic_id = summary.epic.id.clone();
        let epic_matches = issue_matches(&summary.epic, query);
        let active: Vec<_> = summary
            .children
            .iter()
            .filter(|child| {
                if query.is_empty() || epic_matches {
                    child.status != "closed"
                } else {
                    issue_matches(child, query)
                }
            })
            .collect();
        let progress = summary.progress();
        div()
            .id(SharedString::from(format!("epic-card:{}", summary.epic.id)))
            .anchor_scroll(Some(anchor))
            .w(px(360.))
            .min_w(px(300.))
            .flex_grow()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_lg()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .id(SharedString::from(format!("epic:{}", summary.epic.id)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.select(epic_id.clone(), cx)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(summary.epic.id.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .truncate()
                            .child(summary.epic.title.clone()),
                    )
                    .child(self.priority_pill(&summary.epic, cx))
                    .child(div().text_sm().text_color(theme::muted()).child(format!(
                        "{}/{}",
                        summary.closed,
                        summary.children.len()
                    ))),
            )
            .child(
                div()
                    .w_full()
                    .h(px(3.))
                    .rounded_full()
                    .bg(theme::border())
                    .child(
                        div()
                            .h_full()
                            .w(relative(progress))
                            .rounded_full()
                            .bg(theme::accent()),
                    ),
            )
            .children(active.into_iter().map(|issue| self.issue_row(issue, cx)))
            .when(summary.children.is_empty(), |card| {
                card.child(
                    div()
                        .py_4()
                        .text_sm()
                        .text_color(theme::muted())
                        .child("No beads in this epic"),
                )
            })
            .when(summary.closed > 0, |card| {
                card.child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(theme::muted())
                        .child(format!("✓ {} completed", summary.closed)),
                )
            })
    }

    fn ungrouped_card(&self, query: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .w(px(360.))
            .min_w(px(300.))
            .flex_grow()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_lg()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Ungrouped"),
            )
            .children(
                self.data
                    .ungrouped
                    .iter()
                    .filter(|issue| {
                        if query.is_empty() {
                            issue.status != "closed"
                        } else {
                            issue_matches(issue, query)
                        }
                    })
                    .map(|issue| self.issue_row(issue, cx)),
            )
            .when(self.data.ungrouped.is_empty(), |card| {
                card.child(
                    div()
                        .py_4()
                        .text_sm()
                        .text_color(theme::muted())
                        .child("Everything has a home"),
                )
            })
    }

    fn working_preview(&self, item: &WorkingItem, agent: &AgentInfo) -> impl IntoElement + use<> {
        let lines = self
            .agent_previews
            .get(&item.issue.id)
            .cloned()
            .unwrap_or_default();
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .offset(point(px(0.), px(34.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .w(px(500.))
                        .max_h(px(230.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::progress().opacity(0.4))
                        .shadow_lg()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(theme::border())
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(format!("{} · {}", agent.kind, item.issue.id)),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(agent_status_color(&agent.status)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(agent_status_color(&agent.status))
                                        .child(agent.status.clone()),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("preview:{}", item.issue.id)))
                                .max_h(px(180.))
                                .overflow_y_scroll()
                                .p_3()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .font_family("monospace")
                                .text_xs()
                                .text_color(theme::text())
                                .when(lines.is_empty(), |body| {
                                    body.child(
                                        div()
                                            .text_color(theme::muted())
                                            .child("Reading agent output…"),
                                    )
                                })
                                .children(lines.into_iter().map(|line| div().w_full().child(line))),
                        ),
                ),
        )
        .with_priority(2)
    }

    fn working_actions(
        &self,
        issue_id: &str,
        agent: &AgentInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let focus_name = agent.name.clone();
        let inspect_id = issue_id.to_owned();
        let refresh_id = issue_id.to_owned();
        deferred(
            anchored()
                .anchor(Corner::TopRight)
                .offset(point(px(0.), px(26.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .w(px(190.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_working_menu))
                        .child(
                            div()
                                .id("go-to-working-agent")
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(theme::text())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.focus_working_agent(focus_name.clone(), cx)
                                }))
                                .child("Go to agent"),
                        )
                        .child(
                            div()
                                .id("open-working-inspector")
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(theme::text())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_working_inspector(inspect_id.clone(), cx)
                                }))
                                .child("Open bead inspector"),
                        )
                        .child(
                            div()
                                .id("refresh-working-preview")
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(theme::text())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.refresh_working_preview(refresh_id.clone(), cx)
                                }))
                                .child("Refresh preview"),
                        ),
                ),
        )
        .with_priority(3)
    }

    fn working_strip(
        &self,
        items: Vec<WorkingItem>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .h(px(42.))
            .min_h(px(42.))
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::progress())
                    .child("WORKING"),
            )
            .child(div().w(px(1.)).h(px(16.)).bg(theme::border()))
            .child(
                div()
                    .id("working-scroll")
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .gap_2()
                    .overflow_x_scroll()
                    .children(items.into_iter().map(|item| {
                        let id = item.issue.id.clone();
                        let click_id = id.clone();
                        let hover_id = id.clone();
                        let menu_id = id.clone();
                        let anchor = item.anchor.clone();
                        let agent = item.agent.clone();
                        let is_hovered = self.hovered_working.as_deref() == Some(id.as_str());
                        let menu_open = self.working_menu_for.as_deref() == Some(id.as_str());
                        let subtitle = item
                            .epic_title
                            .as_deref()
                            .map(|epic| format!(" · {epic}"))
                            .unwrap_or_else(|| " · ungrouped".into());
                        div()
                            .id(SharedString::from(format!("working:{id}")))
                            .max_w(px(360.))
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .bg(theme::progress().opacity(0.08))
                            .border_1()
                            .border_color(theme::progress().opacity(0.2))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::progress().opacity(0.16)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(anchor) = &anchor {
                                    anchor.scroll_to(window, cx);
                                } else {
                                    this.select(click_id.clone(), cx);
                                }
                            }))
                            .child(div().size(px(6.)).rounded_full().bg(theme::progress()))
                            .child(
                                div()
                                    .id(SharedString::from(format!("working-preview-trigger:{id}")))
                                    .px_1()
                                    .rounded_sm()
                                    .bg(theme::progress().opacity(0.14))
                                    .border_1()
                                    .border_color(theme::progress().opacity(0.3))
                                    .text_xs()
                                    .text_color(theme::progress())
                                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                        this.hover_working(hover_id.clone(), *hovered, cx)
                                    }))
                                    .child(item.issue.id.clone())
                                    .when(is_hovered && agent.is_some() && !menu_open, |badge| {
                                        badge.child(
                                            self.working_preview(&item, agent.as_ref().unwrap()),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::text())
                                    .child(format!("{}{}", item.issue.title, subtitle)),
                            )
                            .when_some(agent.clone(), |working, agent| {
                                working.child(
                                    div()
                                        .id(SharedString::from(format!("working-menu:{id}")))
                                        .px_1()
                                        .rounded_sm()
                                        .text_sm()
                                        .text_color(theme::muted())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .on_click(cx.listener(move |this, event, window, cx| {
                                            this.toggle_working_menu(
                                                menu_id.clone(),
                                                event,
                                                window,
                                                cx,
                                            )
                                        }))
                                        .child("•••")
                                        .when(menu_open, |button| {
                                            button.child(self.working_actions(
                                                &item.issue.id,
                                                &agent,
                                                cx,
                                            ))
                                        }),
                                )
                            })
                    })),
            )
    }

    fn completion_toast(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.completed_toast.len();
        div()
            .absolute()
            .right_4()
            .bottom_4()
            .w(px(340.))
            .max_h(px(260.))
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::ready().opacity(0.45))
            .shadow_lg()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().size(px(7.)).rounded_full().bg(theme::ready()))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if count == 1 {
                                "Bead completed".into()
                            } else {
                                format!("{count} beads completed")
                            }),
                    )
                    .child(
                        div()
                            .id("dismiss-completion-toast")
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(theme::muted())
                            .cursor_pointer()
                            .hover(|style| {
                                style.bg(theme::surface_hover()).text_color(theme::text())
                            })
                            .on_click(cx.listener(Self::dismiss_completion_toast))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .id("completion-toast-list")
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.completed_toast.iter().map(|issue| {
                        div()
                            .px_2()
                            .py_1()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(theme::muted()).child(issue.id.clone()))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text())
                                    .child(issue.title.clone()),
                            )
                    })),
            )
    }

    fn agent_option(
        &self,
        issue_id: &str,
        kind: AgentKind,
        model: Option<&str>,
        title: &'static str,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = issue_id.to_owned();
        let model = model.map(str::to_owned);
        div()
            .id(SharedString::from(format!("launch:{title}")))
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.launch_agent(id.clone(), kind, model.clone(), cx)
            }))
            .child(div().text_sm().text_color(theme::text()).child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(description),
            )
    }

    fn agent_menu(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        deferred(
            anchored()
                .anchor(Corner::TopRight)
                .offset(point(px(0.), px(28.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .w(px(280.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_agent_menu))
                        .child(self.agent_option(
                            &issue.id,
                            AgentKind::Claude,
                            Some("claude-fable-5"),
                            "Claude · Fable",
                            "Fast, focused implementation",
                            cx,
                        ))
                        .child(self.agent_option(
                            &issue.id,
                            AgentKind::Claude,
                            Some("claude-opus-5"),
                            "Claude · Opus 5",
                            "Deeper reasoning for difficult work",
                            cx,
                        ))
                        .child(self.agent_option(
                            &issue.id,
                            AgentKind::Pi,
                            None,
                            "Pi",
                            "Launch with the Pi coding harness",
                            cx,
                        )),
                ),
        )
        .with_priority(1)
    }

    fn inspector(
        &self,
        issue: &Issue,
        bottom: bool,
        inspector_height: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let current_epic = self
            .data
            .epics
            .iter()
            .find(|epic| epic.children.iter().any(|child| child.id == issue.id))
            .map(|epic| epic.epic.id.as_str());
        let can_move = issue.issue_type != "epic";
        let can_launch = can_move && issue.status != "closed";
        let menu_open = self.agent_menu_for.as_deref() == Some(issue.id.as_str());
        let is_launching = self.launching_agent.as_deref() == Some(issue.id.as_str());
        let notice = self
            .agent_notice
            .as_ref()
            .filter(|(id, _, _)| id == &issue.id)
            .cloned();
        div()
            .w(px(340.))
            .min_w(px(340.))
            .h_full()
            .when(bottom, |panel| panel.w_full().min_w_0().h(inspector_height))
            .flex()
            .flex_col()
            .bg(theme::surface())
            .when(bottom, |panel| panel.border_t_1())
            .when(!bottom, |panel| panel.border_l_1())
            .border_color(theme::border())
            .when(bottom, |panel| {
                panel.child(
                    div()
                        .id("inspector-resize-handle")
                        .w_full()
                        .h(px(7.))
                        .min_h(px(7.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_ns_resize()
                        .hover(|style| style.bg(theme::accent().opacity(0.18)))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_inspector_resize))
                        .child(
                            div()
                                .w(px(36.))
                                .h(px(2.))
                                .rounded_full()
                                .bg(theme::muted().opacity(0.45)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme::muted())
                            .child(format!("{}  ·  {}", issue.id, issue.issue_type)),
                    )
                    .child(
                        div()
                            .id("close-inspector")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme::muted())
                            .hover(|style| {
                                style.bg(theme::surface_hover()).text_color(theme::text())
                            })
                            .on_click(cx.listener(Self::dismiss_inspector))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(issue.title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(self.priority_pill(issue, cx))
                                    .child(self.state_badge(self.data.state(&issue.id)))
                                    .child(div().flex_1())
                                    .when(can_launch, |row| {
                                        let id = issue.id.clone();
                                        row.child(
                                            div()
                                                .id("agent-menu-button")
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(theme::progress().opacity(0.4))
                                                .bg(theme::progress().opacity(0.1))
                                                .text_xs()
                                                .text_color(theme::progress())
                                                .cursor_pointer()
                                                .hover(|style| {
                                                    style.bg(theme::progress().opacity(0.18))
                                                })
                                                .on_click(cx.listener(
                                                    move |this, event, window, cx| {
                                                        cx.stop_propagation();
                                                        this.toggle_agent_menu(
                                                            id.clone(),
                                                            event,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .child(if is_launching {
                                                    "Starting…"
                                                } else {
                                                    "Start work ▾"
                                                })
                                                .when(menu_open && !is_launching, |button| {
                                                    button.child(self.agent_menu(issue, cx))
                                                }),
                                        )
                                    }),
                            )
                            .when_some(notice, |header, (_, is_error, message)| {
                                header.child(
                                    div()
                                        .text_xs()
                                        .text_color(if is_error {
                                            theme::danger()
                                        } else {
                                            theme::muted()
                                        })
                                        .child(message),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child("DESCRIPTION"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .line_height(relative(1.45))
                                    .text_color(theme::text())
                                    .child(if issue.description.is_empty() {
                                        "No description.".into()
                                    } else {
                                        issue.description.clone()
                                    }),
                            ),
                    )
                    .when_some(issue.assignee.clone(), |panel, assignee| {
                        panel.child(
                            div()
                                .text_sm()
                                .text_color(theme::muted())
                                .child(format!("Assigned to {assignee}")),
                        )
                    })
                    .when(!issue.labels.is_empty(), |panel| {
                        panel.child(div().flex().flex_wrap().gap_2().children(
                            issue.labels.iter().map(|label| {
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme::accent().opacity(0.12))
                                    .text_xs()
                                    .text_color(theme::accent())
                                    .child(label.clone())
                            }),
                        ))
                    })
                    .when(can_move, |panel| {
                        panel.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child("MOVE TO EPIC"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(
                                            self.data
                                                .epics
                                                .iter()
                                                .filter(|epic| epic.epic.status != "closed")
                                                .map(|epic| {
                                                    let bead_id = issue.id.clone();
                                                    let epic_id = epic.epic.id.clone();
                                                    let selected =
                                                        current_epic == Some(epic.epic.id.as_str());
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "move:{}",
                                                            epic.epic.id
                                                        )))
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_md()
                                                        .border_1()
                                                        .border_color(if selected {
                                                            theme::accent()
                                                        } else {
                                                            theme::border()
                                                        })
                                                        .bg(if selected {
                                                            theme::accent().opacity(0.14)
                                                        } else {
                                                            theme::background()
                                                        })
                                                        .text_xs()
                                                        .text_color(if selected {
                                                            theme::accent()
                                                        } else {
                                                            theme::muted()
                                                        })
                                                        .cursor_pointer()
                                                        .hover(|style| {
                                                            style
                                                                .border_color(theme::accent())
                                                                .text_color(theme::text())
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.move_to_epic(
                                                                    bead_id.clone(),
                                                                    Some(epic_id.clone()),
                                                                    cx,
                                                                )
                                                            },
                                                        ))
                                                        .child(epic.epic.title.clone())
                                                }),
                                        )
                                        .child({
                                            let bead_id = issue.id.clone();
                                            div()
                                                .id("move-ungrouped")
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(theme::border())
                                                .bg(theme::background())
                                                .text_xs()
                                                .text_color(theme::muted())
                                                .cursor_pointer()
                                                .hover(|style| {
                                                    style
                                                        .border_color(theme::accent())
                                                        .text_color(theme::text())
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.move_to_epic(bead_id.clone(), None, cx)
                                                }))
                                                .child("Ungrouped")
                                        }),
                                ),
                        )
                    }),
            )
    }
}

impl Focusable for Dashboard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn issue_matches(issue: &Issue, query: &str) -> bool {
    query.is_empty()
        || issue.id.to_lowercase().contains(query)
        || issue.title.to_lowercase().contains(query)
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self.data.stats();
        let bottom_inspector = window.bounds().size.width < px(820.);
        let selected = self
            .selected
            .as_deref()
            .and_then(|id| self.data.issue(id))
            .cloned();
        let inspector_height = self
            .inspector_height
            .unwrap_or(window.bounds().size.height * 0.52)
            .max(px(220.))
            .min(window.bounds().size.height * 0.85);
        if bottom_inspector && selected.is_some() {
            let dashboard = cx.entity();
            window.on_mouse_event(move |event: &MouseMoveEvent, _, window, cx| {
                if !event.dragging() {
                    return;
                }
                let max_height = window.bounds().size.height * 0.85;
                dashboard.update(cx, |dashboard, cx| {
                    if let Some((start_y, start_height)) = dashboard.inspector_resize {
                        dashboard.inspector_height = Some(
                            (start_height + start_y - event.position.y)
                                .max(px(220.))
                                .min(max_height),
                        );
                        cx.notify();
                    }
                });
            });
            let dashboard = cx.entity();
            window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                dashboard.update(cx, |dashboard, _| {
                    dashboard.inspector_resize = None;
                });
            });
        }
        let project = self
            .bd
            .project()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned();
        let epic_anchors: Vec<_> = self
            .data
            .epics
            .iter()
            .map(|_| ScrollAnchor::for_handle(self.dashboard_scroll.clone()))
            .collect();
        let mut working = Vec::new();
        for (epic, anchor) in self.data.epics.iter().zip(&epic_anchors) {
            for issue in &epic.children {
                if self.data.state(&issue.id) == WorkState::InProgress {
                    working.push(WorkingItem {
                        issue: issue.clone(),
                        epic_title: Some(epic.epic.title.clone()),
                        anchor: Some(anchor.clone()),
                        agent: self.agents.get(&issue.id).cloned(),
                    });
                }
            }
        }
        for issue in &self.data.ungrouped {
            if self.data.state(&issue.id) == WorkState::InProgress {
                working.push(WorkingItem {
                    issue: issue.clone(),
                    epic_title: None,
                    anchor: None,
                    agent: self.agents.get(&issue.id).cloned(),
                });
            }
        }
        let query = self.search_query.trim().to_lowercase();
        let visible_epics: Vec<_> = self
            .data
            .epics
            .iter()
            .zip(epic_anchors)
            .filter(|(epic, _)| {
                query.is_empty()
                    || issue_matches(&epic.epic, &query)
                    || epic
                        .children
                        .iter()
                        .any(|issue| issue_matches(issue, &query))
            })
            .collect();
        let show_ungrouped = !self.data.ungrouped.is_empty()
            && (query.is_empty()
                || self
                    .data
                    .ungrouped
                    .iter()
                    .any(|issue| issue_matches(issue, &query)));
        let has_results = !visible_epics.is_empty() || show_ungrouped;

        div()
            .size_full()
            .relative()
            .flex()
            .when(bottom_inspector, |root| root.flex_col())
            .bg(theme::background())
            .text_color(theme::text())
            .font_family("Inter")
            .id("dashboard")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_search))
            .on_key_down(cx.listener(Self::search_key_down))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(46.))
                            .px_4()
                            .flex()
                            .items_center()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme::border())
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("beadsctrl"),
                            )
                            .child(div().text_xs().text_color(theme::muted()).child(project))
                            .child(div().w(px(1.)).h(px(18.)).bg(theme::border()))
                            .child(stat("open", stats.open, theme::text()))
                            .child(stat("ready", stats.ready, theme::ready()))
                            .child(stat("working", stats.in_progress, theme::progress()))
                            .child(stat("closed", stats.closed, theme::muted()))
                            .child(div().flex_1())
                            .when(self.search_open || !query.is_empty(), |toolbar| {
                                toolbar.child(
                                    div()
                                        .id("search-field")
                                        .w(px(230.))
                                        .h(px(28.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded_md()
                                        .bg(theme::background())
                                        .border_1()
                                        .border_color(theme::accent().opacity(0.5))
                                        .cursor_text()
                                        .on_click(cx.listener(Self::open_search))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .text_color(if query.is_empty() {
                                                    theme::muted()
                                                } else {
                                                    theme::text()
                                                })
                                                .child(if query.is_empty() {
                                                    "Filter issues and epics…".into()
                                                } else if self.search_open {
                                                    format!("⌕  {}|", self.search_query)
                                                } else {
                                                    format!("⌕  {}", self.search_query)
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id("clear-search")
                                                .text_sm()
                                                .text_color(theme::muted())
                                                .cursor_pointer()
                                                .on_click(cx.listener(Self::clear_search))
                                                .child("×"),
                                        ),
                                )
                            })
                            .when(!self.search_open && query.is_empty(), |toolbar| {
                                toolbar.child(
                                    div()
                                        .id("open-search")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .on_click(cx.listener(Self::open_search))
                                        .child("⌕  Ctrl+F"),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(format!("{} total", stats.total)),
                            )
                            .child(
                                div()
                                    .id("refresh")
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_sm()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.bg(theme::surface_hover()).text_color(theme::text())
                                    })
                                    .on_click(cx.listener(Self::refresh))
                                    .child("↻"),
                            ),
                    )
                    .when(query.is_empty() && !working.is_empty(), |main| {
                        main.child(self.working_strip(working, cx))
                    })
                    .child(
                        div()
                            .id("dashboard-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .track_scroll(&self.dashboard_scroll)
                            .p_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_start()
                                    .gap_3()
                                    .children(visible_epics.into_iter().map(|(epic, anchor)| {
                                        self.epic_card(epic, anchor, &query, cx)
                                    }))
                                    .when(show_ungrouped, |grid| {
                                        grid.child(self.ungrouped_card(&query, cx))
                                    })
                                    .when(!has_results, |grid| {
                                        grid.child(
                                            div()
                                                .w_full()
                                                .py_8()
                                                .text_sm()
                                                .text_color(theme::muted())
                                                .child(format!(
                                                    "No beads or epics match ‘{}’",
                                                    query
                                                )),
                                        )
                                    }),
                            ),
                    )
                    .when_some(self.message.clone(), |main, message| {
                        main.child(
                            div()
                                .px_5()
                                .py_3()
                                .border_t_1()
                                .border_color(theme::danger().opacity(0.35))
                                .bg(theme::danger().opacity(0.08))
                                .text_sm()
                                .text_color(theme::danger())
                                .child(message),
                        )
                    }),
            )
            .when_some(selected, |root, issue| {
                root.child(self.inspector(&issue, bottom_inspector, inspector_height, cx))
            })
            .when(!self.completed_toast.is_empty(), |root| {
                root.child(self.completion_toast(cx))
            })
    }
}

fn agent_status_color(status: &str) -> Hsla {
    match status {
        "working" => theme::progress(),
        "blocked" => theme::blocked(),
        "idle" | "done" => theme::ready(),
        _ => theme::muted(),
    }
}

fn stat(label: &'static str, value: usize, color: Hsla) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap_1()
        .text_xs()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(color)
                .child(value.to_string()),
        )
        .child(div().text_color(theme::muted()).child(label))
}

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-f", FocusSearch, None)]);
}

pub fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1360.), px(860.)),
            cx,
        ))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("beadsctrl".into()),
            ..Default::default()
        }),
        app_id: Some("beadsctrl".into()),
        ..Default::default()
    }
}

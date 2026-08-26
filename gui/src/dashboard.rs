use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    App, Bounds, ClickEvent, Context, Corner, Div, FocusHandle, Focusable, Hsla,
    IntoElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, ScrollAnchor, ScrollHandle, SharedString,
    Stateful, Styled, Window, WindowBounds, WindowOptions, actions, anchored, deferred, div,
    point, prelude::*, px, relative, size,
};

use crate::{
    agents::AgentScan,
    bd::BdClient,
    herdr::{self, AgentInfo, AgentKind},
    model::{BlocksNode, DashboardData, EpicSummary, Issue, WorkState},
    queue::{self, QueueEntry},
    theme,
};

actions!(dashboard, [FocusSearch]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DashboardFilter {
    All,
    Open,
    Ready,
    Working,
    Closed,
}

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
    filter: DashboardFilter,
    agent_menu_for: Option<String>,
    priority_menu_for: Option<String>,
    row_menu_for: Option<String>,
    launching_agent: Option<String>,
    agent_notice: Option<(String, bool, String)>,
    inspector_height: Option<Pixels>,
    inspector_resize: Option<(Pixels, Pixels)>,
    agents: HashMap<String, AgentInfo>,
    hovered_working: Option<String>,
    agent_previews: HashMap<String, Vec<String>>,
    working_menu_for: Option<String>,
    queue: Vec<QueueEntry>,
    queue_auto: bool,
    queue_paused: Option<String>,
    queue_launching: Option<String>,
    // Beads we launched an agent for that has not yet run `bd update --claim`,
    // keyed by bead id. See queue::Claim.
    optimistic_claims: HashMap<String, queue::Claim>,
}

#[derive(Clone)]
struct WorkingItem {
    issue: Issue,
    anchor: Option<ScrollAnchor>,
    agent: Option<AgentInfo>,
}

#[derive(Clone)]
struct DraggedQueueEntry {
    id: String,
    label: String,
}

struct QueueChipPreview {
    entry: DraggedQueueEntry,
}

impl Render for QueueChipPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::accent())
            .shadow_lg()
            .text_xs()
            .text_color(theme::text())
            .font_family("Inter")
            .child(format!("{} · {}", self.entry.id, self.entry.label))
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Dashboard {
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        let bd = BdClient::new(project);
        Self::start_auto_refresh(cx);
        Self::start_preview_refresh(cx);
        let queue = queue::load_queue(bd.project());
        let queue_auto = queue::load_auto(bd.project());
        let (data, message) = match bd.load() {
            Ok(data) => (data, None),
            Err(error) => (DashboardData::default(), Some(error.to_string())),
        };
        Self {
            bd,
            data,
            selected: None,
            message,
            dashboard_scroll: ScrollHandle::new(),
            completed_toast: Vec::new(),
            focus_handle: cx.focus_handle(),
            search_open: false,
            search_query: String::new(),
            filter: DashboardFilter::All,
            agent_menu_for: None,
            priority_menu_for: None,
            row_menu_for: None,
            launching_agent: None,
            agent_notice: None,
            inspector_height: None,
            inspector_resize: None,
            agents: HashMap::new(),
            hovered_working: None,
            agent_previews: HashMap::new(),
            working_menu_for: None,
            queue,
            queue_auto,
            queue_paused: None,
            queue_launching: None,
            optimistic_claims: HashMap::new(),
        }
    }

    fn start_auto_refresh(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // What each transcript and pane was last seen doing never has to
            // be worked out twice, so the scan lives across ticks and
            // round-trips through each spawn.
            let mut scan = AgentScan::default();
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let Some(client) = this.update(cx, |dashboard, _| dashboard.bd.clone()).ok() else {
                    break;
                };
                let taken = std::mem::take(&mut scan);
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let mut scan = taken;
                        let result = client.load();
                        let agents = result
                            .as_ref()
                            .map(|data| scan.run(data, client.project()))
                            .unwrap_or_default();
                        (result, agents, scan)
                    })
                    .await;
                scan = result.2;
                if this
                    .update(cx, |dashboard, cx| {
                        dashboard.agents = result.1;
                        dashboard.apply_load(result.0, true, cx);
                        dashboard.queue_tick(cx);
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
                let target = agent.target.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { herdr::read_agent_preview(&target, &cwd) })
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

    // Deck membership: a bead we just launched (optimistic claim, shows
    // before the herdr tab even exists) or an in-progress bead with a live
    // herdr agent — one of ours, or a tab somebody opened by hand that we
    // attributed to the bead. In-progress beads with no agent — e.g. stale
    // imports that were never closed — stay out of the deck.
    fn is_working(&self, id: &str) -> bool {
        self.optimistic_claims.contains_key(id)
            || (self.data.state(id) == WorkState::InProgress && self.agents.contains_key(id))
    }

    fn apply_load(
        &mut self,
        result: anyhow::Result<DashboardData>,
        show_completions: bool,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(data) => {
                let agents = &self.agents;
                let expired = queue::expire_claims(&mut self.optimistic_claims, &data, |id| {
                    agents.contains_key(id)
                });
                for (id, claim) in expired {
                    // The agent we launched is gone and never claimed its
                    // bead. Releasing the deck is what unwedges the queue, but
                    // the bead was popped when it launched: put it back at the
                    // front and pause, so a half-done tree is not handed to
                    // the next bead behind the user's back.
                    let Some(entry) = claim.entry else { continue };
                    // Straight off disk: the TUI writes this file too, and the
                    // in-memory copy is a tick old by now.
                    let mut queue = queue::load_queue(self.bd.project());
                    if queue::position(&queue, &id).is_none() {
                        queue.insert(0, entry);
                        let _ = queue::save_queue(self.bd.project(), &queue);
                    }
                    self.queue = queue;
                    self.queue_paused = Some(format!("{id} was never claimed"));
                }
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

    fn toggle_priority_menu(&mut self, id: String, cx: &mut Context<Self>) {
        self.priority_menu_for = if self.priority_menu_for.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    fn close_priority_menu(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.priority_menu_for = None;
        cx.notify();
    }

    fn choose_priority(&mut self, id: String, priority: u8, cx: &mut Context<Self>) {
        self.priority_menu_for = None;
        if let Err(error) = self.bd.set_priority(&id, priority) {
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

    fn focus_working_agent(&mut self, target: String, cx: &mut Context<Self>) {
        self.working_menu_for = None;
        let cwd = self.bd.project().to_path_buf();
        let task = cx
            .background_executor()
            .spawn(async move { herdr::focus_agent(&target, &cwd) });
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

    // The TUI writes the same queue file, so every tick re-reads it before
    // deciding anything.
    fn queue_tick(&mut self, cx: &mut Context<Self>) {
        self.queue = queue::load_queue(self.bd.project());
        if queue::prune(&mut self.queue, &self.data) {
            let _ = queue::save_queue(self.bd.project(), &self.queue);
        }
        if self.queue_auto && self.queue_paused.is_none() {
            self.launch_next_queued(cx);
        }
        cx.notify();
    }

    // An agent whose bead is no longer in progress does not count: agents
    // outlive their bead (the tab stays open after the bead closes). An
    // untracked tab counts like any other — it holds the same working tree,
    // so launching into it would put two agents on one checkout.
    fn deck_busy(&self) -> bool {
        self.launching_agent.is_some()
            || self.queue_launching.is_some()
            || !self.optimistic_claims.is_empty()
            || self
                .agents
                .keys()
                .any(|id| self.data.state(id) == WorkState::InProgress)
    }

    fn launch_next_queued(&mut self, cx: &mut Context<Self>) {
        if self.deck_busy() {
            return;
        }
        let Some(entry) = queue::next_ready(&self.queue, &self.data).cloned() else {
            return;
        };
        let Some(issue) = self.data.issue(&entry.id).cloned() else {
            return;
        };
        self.queue.retain(|item| item.id != entry.id);
        let _ = queue::save_queue(self.bd.project(), &self.queue);
        self.queue_launching = Some(entry.id.clone());
        self.optimistic_claims
            .insert(entry.id.clone(), queue::Claim::new(Some(entry.clone())));
        let label = entry.kind.label(entry.model.as_deref());
        self.agent_notice = Some((entry.id.clone(), false, format!("Queue: starting {label}…")));
        cx.notify();

        let cwd = self.bd.project().to_path_buf();
        let task_entry = entry.clone();
        let task = cx.background_executor().spawn(async move {
            herdr::launch_agent(&issue, entry.kind, entry.model.as_deref(), &cwd)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |dashboard, cx| {
                dashboard.queue_launching = None;
                match result {
                    Ok(name) => {
                        // The tab exists from here on, so a missing agent from
                        // now on means it went away rather than never arrived.
                        if let Some(claim) = dashboard.optimistic_claims.get_mut(&task_entry.id) {
                            claim.launched = true;
                        }
                        dashboard.agent_notice =
                            Some((task_entry.id.clone(), false, format!("Queue: started {name}")));
                    }
                    Err(error) => {
                        // Back at the front and pause: the queue retries once
                        // the user resumes, and never silently drops work.
                        dashboard.optimistic_claims.remove(&task_entry.id);
                        dashboard.queue.insert(0, task_entry.clone());
                        let _ = queue::save_queue(dashboard.bd.project(), &dashboard.queue);
                        dashboard.queue_paused = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_queue(
        &mut self,
        id: String,
        kind: AgentKind,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.agent_menu_for = None;
        self.row_menu_for = None;
        if let Some(index) = queue::position(&self.queue, &id) {
            self.queue.remove(index);
        } else {
            let cwd = queue::canonical_project(self.bd.project())
                .to_string_lossy()
                .into_owned();
            self.queue.push(QueueEntry {
                cwd,
                id,
                kind,
                model,
                at: now_millis(),
            });
        }
        let _ = queue::save_queue(self.bd.project(), &self.queue);
        cx.notify();
    }

    fn unqueue(&mut self, id: &str, cx: &mut Context<Self>) {
        self.queue.retain(|entry| entry.id != id);
        let _ = queue::save_queue(self.bd.project(), &self.queue);
        cx.notify();
    }

    fn move_queued(&mut self, dragged: &str, before: Option<&str>, cx: &mut Context<Self>) {
        let Some(from) = queue::position(&self.queue, dragged) else {
            return;
        };
        let entry = self.queue.remove(from);
        let to = before
            .and_then(|id| queue::position(&self.queue, id))
            .unwrap_or(self.queue.len());
        self.queue.insert(to, entry);
        let _ = queue::save_queue(self.bd.project(), &self.queue);
        cx.notify();
    }

    fn toggle_queue_auto(&mut self, cx: &mut Context<Self>) {
        self.queue_auto = !self.queue_auto;
        let _ = queue::save_auto(self.bd.project(), self.queue_auto);
        cx.notify();
    }

    fn resume_queue(&mut self, cx: &mut Context<Self>) {
        self.queue_paused = None;
        cx.notify();
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
        self.row_menu_for = None;
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
        self.row_menu_for = None;
        self.launching_agent = Some(id.clone());
        self.optimistic_claims
            .insert(id.clone(), queue::Claim::new(None));
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
                    Ok(name) => {
                        if let Some(claim) = dashboard.optimistic_claims.get_mut(&id) {
                            claim.launched = true;
                        }
                        (id.clone(), false, format!("Started {label} as {name}"))
                    }
                    Err(error) => {
                        dashboard.optimistic_claims.remove(&id);
                        (id.clone(), true, error.to_string())
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn priority_pill_base(pill: Stateful<Div>, priority: u8) -> Stateful<Div> {
        pill.px_1()
            .rounded_sm()
            .bg(theme::priority(priority).opacity(0.14))
            .border_1()
            .border_color(theme::priority(priority).opacity(0.38))
            .text_size(px(9.))
            .text_color(theme::priority(priority))
            .cursor_pointer()
            .hover(|style| style.bg(theme::priority(priority).opacity(0.25)))
            .child(format!("P{priority}"))
    }

    fn priority_pill(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = issue.id.clone();
        let priority = issue.priority;
        let pill = div().id(SharedString::from(format!("priority:{}", issue.id)));
        Self::priority_pill_base(pill, priority).on_click(cx.listener(
            move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.cycle_priority(id.clone(), priority, cx);
            },
        ))
    }

    // Same pill, but clicking it opens a dropdown to jump straight to a
    // priority instead of cycling through them one at a time.
    fn priority_pill_editable(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = issue.id.clone();
        let priority = issue.priority;
        let menu_open = self.priority_menu_for.as_deref() == Some(issue.id.as_str());
        let pill = div().id(SharedString::from(format!("priority-menu:{}", issue.id)));
        Self::priority_pill_base(pill, priority)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_priority_menu(id.clone(), cx);
            }))
            .when(menu_open, |pill| {
                pill.child(self.priority_menu(&issue.id, priority, cx))
            })
    }

    fn priority_menu(
        &self,
        issue_id: &str,
        current: u8,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .offset(point(px(0.), px(20.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .w(px(72.))
                        .rounded_md()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_priority_menu))
                        .children((0..5).map(|priority| {
                            let id = issue_id.to_owned();
                            let selected = priority == current;
                            div()
                                .id(SharedString::from(format!(
                                    "priority-option:{issue_id}:{priority}"
                                )))
                                .px_2()
                                .py_1()
                                .flex()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.choose_priority(id.clone(), priority, cx);
                                }))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(theme::priority(priority))
                                        .child(format!("P{priority}")),
                                )
                                .when(selected, |row| {
                                    row.child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::accent())
                                            .child("✓"),
                                    )
                                })
                        })),
                ),
        )
        .with_priority(1)
    }

    fn issue_matches_filter(&self, issue: &Issue) -> bool {
        match self.filter {
            DashboardFilter::All => issue.status != "closed",
            DashboardFilter::Open => matches!(
                self.data.state(&issue.id),
                WorkState::Ready | WorkState::Blocked
            ),
            DashboardFilter::Ready => self.data.state(&issue.id) == WorkState::Ready,
            DashboardFilter::Working => self.data.state(&issue.id) == WorkState::InProgress,
            DashboardFilter::Closed => self.data.state(&issue.id) == WorkState::Closed,
        }
    }

    fn set_filter(&mut self, filter: DashboardFilter, cx: &mut Context<Self>) {
        self.filter = if self.filter == filter {
            DashboardFilter::All
        } else {
            filter
        };
        cx.notify();
    }

    fn filter_stat(
        &self,
        label: &'static str,
        value: usize,
        color: Hsla,
        filter: DashboardFilter,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.filter == filter;
        div()
            .id(SharedString::from(format!("filter:{label}")))
            .px_2()
            .py_1()
            .flex()
            .items_baseline()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                color.opacity(0.45)
            } else {
                gpui::transparent_black()
            })
            .when(selected, |stat| stat.bg(color.opacity(0.12)))
            .cursor_pointer()
            .hover(|style| style.bg(color.opacity(0.1)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            .text_xs()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(value.to_string()),
            )
            .child(div().text_color(theme::muted()).child(label))
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
        let can_menu = issue.issue_type != "epic" && issue.status != "closed";
        let row_menu_open = can_menu && self.row_menu_for.as_deref() == Some(issue.id.as_str());
        let menu_id = issue.id.clone();
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
            // Blocked beads fade back so the ready ones carry the card.
            .when(state == WorkState::Blocked, |row| row.opacity(0.5))
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
            .when(can_menu, |row| {
                row.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.row_menu_for = Some(menu_id.clone());
                        cx.notify();
                    }),
                )
            })
            .when(row_menu_open, |row| {
                row.child(self.agent_menu(issue, Corner::TopLeft, cx))
            })
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
            .when_some(queue::position(&self.queue, &issue.id), |row, index| {
                row.child(self.queued_badge(index))
            })
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
                self.issue_matches_filter(child)
                    && (query.is_empty() || epic_matches || issue_matches(child, query))
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
                        self.issue_matches_filter(issue)
                            && (query.is_empty() || issue_matches(issue, query))
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

    fn closed_card(
        &self,
        issues: Vec<&Issue>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .w_full()
            .max_w(px(900.))
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
                    .pb_2()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Recently closed"),
            )
            .children(issues.into_iter().map(|issue| {
                let id = issue.id.clone();
                let closed_at = issue
                    .closed_at
                    .as_deref()
                    .or(issue.updated_at.as_deref())
                    .map(format_closed_at)
                    .unwrap_or_else(|| "Unknown date".into());
                div()
                    .id(SharedString::from(format!("closed:{}", issue.id)))
                    .w_full()
                    .px_2()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .child(
                        div()
                            .w(px(116.))
                            .flex_none()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(closed_at),
                    )
                    .child(
                        div()
                            .w(px(100.))
                            .flex_none()
                            .truncate()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(issue.id.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme::text())
                            .child(issue.title.clone()),
                    )
                    .child(self.state_badge(WorkState::Closed))
            }))
    }

    fn working_preview(&self, item: &WorkingItem, agent: &AgentInfo) -> impl IntoElement + use<> {
        let lines = self
            .agent_previews
            .get(&item.issue.id)
            .cloned()
            .unwrap_or_default();
        deferred(
            anchored()
                .anchor(Corner::BottomLeft)
                .offset(point(px(0.), px(-10.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
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
                                        .child(match agent.external {
                                            true => format!(
                                                "{} · {} · external tab",
                                                agent.kind, item.issue.id
                                            ),
                                            false => format!("{} · {}", agent.kind, item.issue.id),
                                        }),
                                )
                                .child(div().flex_1())
                                .when_some(agent.context_tokens, |header, tokens| {
                                    header.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(format!("{} ctx", format_tokens(tokens))),
                                    )
                                })
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
        let focus_target = agent.target.clone();
        let inspect_id = issue_id.to_owned();
        let refresh_id = issue_id.to_owned();
        deferred(
            anchored()
                .anchor(Corner::BottomRight)
                .offset(point(px(0.), px(-10.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
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
                                    this.focus_working_agent(focus_target.clone(), cx)
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

    fn deck_item(&self, item: WorkingItem, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = item.issue.id.clone();
        let click_id = id.clone();
        let hover_id = id.clone();
        let menu_id = id.clone();
        let anchor = item.anchor.clone();
        let agent = item.agent.clone();
        let is_hovered = self.hovered_working.as_deref() == Some(id.as_str());
        let menu_open = self.working_menu_for.as_deref() == Some(id.as_str());
        let status_color = agent
            .as_ref()
            .map(|agent| agent_status_color(&agent.status))
            .unwrap_or_else(theme::muted);
        div()
            .id(SharedString::from(format!("working:{id}")))
            .w_full()
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
            .child(div().size(px(6.)).rounded_full().bg(status_color))
            .child(
                div()
                    .id(SharedString::from(format!("working-preview-trigger:{id}")))
                    .px_1()
                    .rounded_sm()
                    .bg(theme::progress().opacity(0.14))
                    .text_xs()
                    .text_color(theme::progress())
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_working(hover_id.clone(), *hovered, cx)
                    }))
                    .child(item.issue.id.clone())
                    .when(is_hovered && agent.is_some() && !menu_open, |badge| {
                        badge.child(self.working_preview(&item, agent.as_ref().unwrap()))
                    }),
            )
            .when(
                agent.as_ref().is_some_and(|agent| agent.external),
                |row| {
                    row.child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(9.))
                            .text_color(theme::muted())
                            .child("external"),
                    )
                },
            )
            .when_some(
                agent.as_ref().and_then(|agent| agent.context_tokens),
                |row, tokens| {
                    row.child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(9.))
                            .text_color(theme::muted())
                            .child(format!("{} ctx", format_tokens(tokens))),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme::text())
                    .child(item.issue.title.clone()),
            )
            .when_some(agent.clone(), |chip, agent| {
                chip.child(
                    div()
                        .id(SharedString::from(format!("working-menu:{id}")))
                        .px_1()
                        .rounded_sm()
                        .text_sm()
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::raised_hover()))
                        .on_click(cx.listener(move |this, event, window, cx| {
                            this.toggle_working_menu(menu_id.clone(), event, window, cx)
                        }))
                        .child("•••")
                        .when(menu_open, |button| {
                            button.child(self.working_actions(&item.issue.id, &agent, cx))
                        }),
                )
            })
    }

    fn queue_row(
        &self,
        index: usize,
        entry: &QueueEntry,
        next_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = entry.id.clone();
        let ready = self.data.state(&id) == WorkState::Ready;
        let is_next = next_id == Some(id.as_str());
        let harness = entry.kind.label(entry.model.as_deref());
        let title = self
            .data
            .issue(&id)
            .map(|issue| issue.title.clone())
            .unwrap_or_else(|| "(unknown bead)".into());
        let drag = DraggedQueueEntry {
            id: id.clone(),
            label: title.clone(),
        };
        let drop_id = id.clone();
        let click_id = id.clone();
        let remove_id = id.clone();
        div()
            .id(SharedString::from(format!("queued:{id}")))
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised_hover()))
            .on_drag(drag, |dragged, _, _, cx| {
                cx.new(|_| QueueChipPreview {
                    entry: dragged.clone(),
                })
            })
            .drag_over::<DraggedQueueEntry>(|style, _, _, _| {
                style.bg(theme::accent().opacity(0.12))
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedQueueEntry, _, cx| {
                this.move_queued(&dragged.id, Some(&drop_id), cx);
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.select(click_id.clone(), cx)))
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .text_xs()
                    .text_color(if is_next { theme::ready() } else { theme::muted() })
                    .child(format!("{}", index + 1)),
            )
            .child(
                div()
                    .w(px(110.))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(id.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(if ready { theme::text() } else { theme::muted() })
                    .child(title),
            )
            .when(!ready, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(theme::blocked())
                        .child("blocked"),
                )
            })
            .child(div().text_xs().text_color(theme::muted()).child(harness))
            .child(
                div()
                    .id(SharedString::from(format!("unqueue:{id}")))
                    .px_1()
                    .rounded_sm()
                    .text_xs()
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::raised_hover()).text_color(theme::text()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.unqueue(&remove_id, cx);
                    }))
                    .child("×"),
            )
    }

    // Why the queue stopped, with the one click that starts it again. Shown
    // beside the idle status row, or under a busy one — a pause the user
    // cannot see is a queue that looks broken.
    fn queue_paused_notice(&self, error: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme::danger())
                    .child(format!("queue paused · {error}")),
            )
            .child(
                div()
                    .id("queue-resume")
                    .flex_none()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::raised_hover()))
                    .on_click(cx.listener(|this, _, _, cx| this.resume_queue(cx)))
                    .child("resume"),
            )
    }

    // The deck: a status row (what's running now, or a Run-next button when
    // idle) with the auto/manual toggle on the right, and the queue as a
    // vertical list underneath.
    fn deck_console(
        &self,
        items: Vec<WorkingItem>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let next_id = queue::next_ready(&self.queue, &self.data).map(|entry| entry.id.clone());
        let next_title = next_id
            .as_deref()
            .and_then(|id| self.data.issue(id))
            .map(|issue| issue.title.clone());
        let idle = items.is_empty() && !self.deck_busy();
        let can_run_next = idle && next_id.is_some() && self.queue_paused.is_none();
        let has_work = !items.is_empty();
        div()
            .flex()
            .flex_col()
            .px_4()
            .py_2()
            .gap_2()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(28.))
                    .when(has_work, |row| {
                        row.items_start().child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(9.))
                                        .text_color(theme::muted())
                                        .child("NOW WORKING"),
                                )
                                .child(
                                    div()
                                        .id("deck-playing")
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .max_h(px(120.))
                                        .overflow_y_scroll()
                                        .children(
                                            items.into_iter().map(|item| self.deck_item(item, cx)),
                                        ),
                                ),
                        )
                    })
                    .when(!has_work, |row| {
                        row.child({
                            let left = div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2();
                            if let Some(error) = self.queue_paused.clone() {
                                left.child(self.queue_paused_notice(error, cx))
                            } else if can_run_next {
                                left.child(
                                    div()
                                        .id("queue-run-next")
                                        .flex_none()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(theme::accent().opacity(0.9))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::background())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::accent()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.launch_next_queued(cx)
                                        }))
                                        .child("Run next"),
                                )
                                .when_some(next_title, |row, title| {
                                    row.child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(title),
                                    )
                                })
                            } else {
                                left.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child("nothing running"),
                                )
                            }
                        })
                    })
                    .child(
                        div()
                            .id("queue-mode")
                            .flex_none()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(if self.queue_auto {
                                theme::ready().opacity(0.4)
                            } else {
                                theme::border()
                            })
                            .text_xs()
                            .text_color(if self.queue_auto {
                                theme::ready()
                            } else {
                                theme::muted()
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::raised_hover()))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_queue_auto(cx)))
                            .child(if self.queue_auto { "auto" } else { "manual" }),
                    ),
            )
            .when_some(
                self.queue_paused.clone().filter(|_| has_work),
                |console, error| console.child(self.queue_paused_notice(error, cx)),
            )
            .when(!self.queue.is_empty(), |console| {
                console.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(theme::muted())
                                .child("QUEUE"),
                        )
                        .child(
                            div()
                                .id("deck-queue")
                                .flex()
                                .flex_col()
                                .max_h(px(180.))
                                .overflow_y_scroll()
                                .children(self.queue.iter().enumerate().map(
                                    |(index, entry)| {
                                        self.queue_row(index, entry, next_id.as_deref(), cx)
                                    },
                                ))
                                .child(
                                    div()
                                        .id("queue-append")
                                        .h(px(10.))
                                        .rounded_md()
                                        .drag_over::<DraggedQueueEntry>(|style, _, _, _| {
                                            style.bg(theme::accent().opacity(0.12))
                                        })
                                        .on_drop(cx.listener(
                                            |this, dragged: &DraggedQueueEntry, _, cx| {
                                                this.move_queued(&dragged.id, None, cx);
                                            },
                                        )),
                                ),
                        ),
                )
            })
    }

    fn completion_toast(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.completed_toast.len();
        div()
            .absolute()
            .right_4()
            .bottom(px(104.))
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

    fn queue_option(
        &self,
        issue_id: &str,
        kind: AgentKind,
        model: Option<&str>,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = issue_id.to_owned();
        let model = model.map(str::to_owned);
        div()
            .id(SharedString::from(format!("queue:{title}")))
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_queue(id.clone(), kind, model.clone(), cx)
            }))
            .child(div().text_sm().text_color(theme::text()).child(title))
    }

    fn queued_badge(&self, index: usize) -> Div {
        div()
            .px_1()
            .rounded_sm()
            .bg(theme::accent().opacity(0.12))
            .border_1()
            .border_color(theme::accent().opacity(0.3))
            .text_size(px(9.))
            .text_color(theme::accent())
            .child(format!("♪ {}", index + 1))
    }

    fn agent_menu(
        &self,
        issue: &Issue,
        corner: Corner,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let queued_at = queue::position(&self.queue, &issue.id);
        deferred(
            anchored()
                .anchor(corner)
                .offset(point(px(0.), px(26.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .w(px(280.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_agent_menu))
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(px(9.))
                                .text_color(theme::muted())
                                .child("START NOW"),
                        )
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
                        ))
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .border_t_1()
                                .border_color(theme::border())
                                .text_size(px(9.))
                                .text_color(theme::muted())
                                .child("ADD TO QUEUE · starts when nothing is working"),
                        )
                        .when_some(queued_at, |menu, index| {
                            let id = issue.id.clone();
                            menu.child(
                                div()
                                    .id("unqueue-menu")
                                    .px_3()
                                    .py_2()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.agent_menu_for = None;
                                        this.row_menu_for = None;
                                        this.unqueue(&id, cx);
                                    }))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme::text())
                                            .child(format!("Unqueue (currently #{})", index + 1)),
                                    ),
                            )
                        })
                        .when(queued_at.is_none(), |menu| {
                            menu.child(self.queue_option(
                                &issue.id,
                                AgentKind::Claude,
                                Some("claude-fable-5"),
                                "Claude · Fable",
                                cx,
                            ))
                            .child(self.queue_option(
                                &issue.id,
                                AgentKind::Claude,
                                Some("claude-opus-5"),
                                "Claude · Opus 5",
                                cx,
                            ))
                            .child(self.queue_option(&issue.id, AgentKind::Pi, None, "Pi", cx))
                        }),
                ),
        )
        .with_priority(1)
    }

    fn dependency_row(
        &self,
        section: &str,
        node: &BlocksNode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let issue = self.data.issue(&node.id).cloned();
        let known = issue.is_some();
        let (mark, mark_color) = match issue.as_ref().map(|issue| issue.status.as_str()) {
            Some("closed") => ("✓", theme::muted()),
            Some("in_progress") => ("◐", theme::progress()),
            Some("open") => match self.data.state(&node.id) {
                WorkState::Blocked => ("○", theme::blocked()),
                _ => ("○", theme::ready()),
            },
            _ => ("·", theme::muted()),
        };
        let select_id = node.id.clone();
        div()
            .id(SharedString::from(format!("{section}:{}", node.id)))
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .px_1()
            .rounded_sm()
            .when(known, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(select_id.clone(), cx)
                    }))
            })
            .child(
                div()
                    .flex_none()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(theme::muted())
                    .child(node.prefix.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(mark_color)
                    .child(mark),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(if known { theme::accent() } else { theme::muted() })
                    .child(node.id.clone()),
            )
            .when_some(issue, |row, issue| {
                let closed = issue.status == "closed";
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(9.))
                        .text_color(theme::priority(issue.priority))
                        .child(format!("P{}", issue.priority)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if closed { theme::muted() } else { theme::text() })
                        .child(issue.title.clone()),
                )
            })
            .when(node.cycle, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme::danger())
                        .child("↺ cycle"),
                )
            })
    }

    // What this bead is holding up, as a tree: direct dependents at the root,
    // their dependents nested below — mirroring the TUI's BLOCKS section.
    fn blocks_section(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let nodes = self.data.blocks_tree(&issue.id);
        self.dependency_tree_section("BLOCKS", "blocks", &nodes, cx)
    }

    // What is holding this bead up, as a tree: direct blockers at the root,
    // their blockers nested below.
    fn blocked_by_section(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let nodes = self.data.blocked_by_tree(&issue.id);
        self.dependency_tree_section("BLOCKED BY", "blocked-by", &nodes, cx)
    }

    fn dependency_tree_section(
        &self,
        label: &'static str,
        section: &'static str,
        nodes: &[BlocksNode],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(theme::muted()).child(label))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(nodes.is_empty(), |list| {
                        list.child(
                            div()
                                .px_1()
                                .text_xs()
                                .text_color(theme::muted())
                                .child("none"),
                        )
                    })
                    .children(
                        nodes
                            .iter()
                            .map(|node| self.dependency_row(section, node, cx)),
                    ),
            )
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
                                    .child(self.priority_pill_editable(issue, cx))
                                    .child(self.state_badge(self.data.state(&issue.id)))
                                    .when_some(
                                        queue::position(&self.queue, &issue.id),
                                        |row, index| row.child(self.queued_badge(index)),
                                    )
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
                                                    button.child(self.agent_menu(
                                                        issue,
                                                        Corner::TopRight,
                                                        cx,
                                                    ))
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
                    .child(self.blocked_by_section(issue, cx))
                    .child(self.blocks_section(issue, cx))
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
        // Leave room for macOS traffic lights when content extends into the
        // transparent titlebar. Other platforms retain the original inset.
        let titlebar_left_padding = if cfg!(target_os = "macos") {
            px(80.)
        } else {
            px(16.)
        };
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
                if self.is_working(&issue.id) {
                    working.push(WorkingItem {
                        issue: issue.clone(),
                        anchor: Some(anchor.clone()),
                        agent: self.agents.get(&issue.id).cloned(),
                    });
                }
            }
        }
        for issue in &self.data.ungrouped {
            if self.is_working(&issue.id) {
                working.push(WorkingItem {
                    issue: issue.clone(),
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
                (self.issue_matches_filter(&epic.epic)
                    && (query.is_empty() || issue_matches(&epic.epic, &query)))
                    || epic.children.iter().any(|issue| {
                        self.issue_matches_filter(issue)
                            && (query.is_empty() || issue_matches(issue, &query))
                    })
            })
            .collect();
        let show_ungrouped = self.data.ungrouped.iter().any(|issue| {
            self.issue_matches_filter(issue)
                && (query.is_empty() || issue_matches(issue, &query))
        });
        let mut closed_issues: Vec<_> = self
            .data
            .issues
            .iter()
            .filter(|issue| {
                issue.status == "closed" && (query.is_empty() || issue_matches(issue, &query))
            })
            .collect();
        closed_issues.sort_by(|left, right| {
            closure_time(right)
                .cmp(closure_time(left))
                .then_with(|| right.id.cmp(&left.id))
        });
        closed_issues.truncate(100);
        let has_results = if self.filter == DashboardFilter::Closed {
            !closed_issues.is_empty()
        } else {
            !visible_epics.is_empty() || show_ungrouped
        };

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
            // Inspector resize drag: handlers live on the root element (which
            // spans the window) — Window::on_mouse_event may only be called
            // during paint and panics when registered from render.
            .when(bottom_inspector, |root| {
                root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    if !event.dragging() {
                        return;
                    }
                    if let Some((start_y, start_height)) = this.inspector_resize {
                        let max_height = window.bounds().size.height * 0.85;
                        this.inspector_height = Some(
                            (start_height + start_y - event.position.y)
                                .max(px(220.))
                                .min(max_height),
                        );
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, _| {
                        this.inspector_resize = None;
                    }),
                )
            })
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
                            .pl(titlebar_left_padding)
                            .pr_4()
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
                            .child(self.filter_stat(
                                "open",
                                stats.open,
                                theme::text(),
                                DashboardFilter::Open,
                                cx,
                            ))
                            .child(self.filter_stat(
                                "ready",
                                stats.ready,
                                theme::ready(),
                                DashboardFilter::Ready,
                                cx,
                            ))
                            .child(self.filter_stat(
                                "working",
                                stats.in_progress,
                                theme::progress(),
                                DashboardFilter::Working,
                                cx,
                            ))
                            .child(self.filter_stat(
                                "closed",
                                stats.closed,
                                theme::muted(),
                                DashboardFilter::Closed,
                                cx,
                            ))
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
                                    .when(self.filter != DashboardFilter::Closed, |grid| {
                                        grid.children(visible_epics.into_iter().map(
                                            |(epic, anchor)| {
                                                self.epic_card(epic, anchor, &query, cx)
                                            },
                                        ))
                                        .when(show_ungrouped, |grid| {
                                            grid.child(self.ungrouped_card(&query, cx))
                                        })
                                    })
                                    .when(
                                        self.filter == DashboardFilter::Closed && has_results,
                                        |grid| grid.child(self.closed_card(closed_issues, cx)),
                                    )
                                    .when(!has_results, |grid| {
                                        grid.child(
                                            div()
                                                .w_full()
                                                .py_8()
                                                .text_sm()
                                                .text_color(theme::muted())
                                                .child(if query.is_empty() {
                                                    "No beads match this filter".to_owned()
                                                } else {
                                                    format!("No beads match ‘{}’", query)
                                                }),
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
                    })
                    .child(self.deck_console(working, cx)),
            )
            .when_some(selected, |root, issue| {
                root.child(self.inspector(&issue, bottom_inspector, inspector_height, cx))
            })
            .when(!self.completed_toast.is_empty(), |root| {
                root.child(self.completion_toast(cx))
            })
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1e6)
    } else if tokens >= 10_000 {
        format!("{}k", tokens / 1_000)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1e3)
    } else {
        tokens.to_string()
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

fn closure_time(issue: &Issue) -> &str {
    issue
        .closed_at
        .as_deref()
        .or(issue.updated_at.as_deref())
        .unwrap_or("")
}

fn format_closed_at(timestamp: &str) -> String {
    match (timestamp.get(..10), timestamp.get(11..16)) {
        (Some(date), Some(time)) => format!("{date} {time}"),
        _ => timestamp.to_owned(),
    }
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
            appears_transparent: true,
            ..Default::default()
        }),
        app_id: Some("beadsctrl".into()),
        ..Default::default()
    }
}

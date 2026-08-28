use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, Context, Corner, Div, Entity, FocusHandle, Focusable,
    HighlightStyle, Hsla, IntoElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollAnchor, ScrollHandle,
    ScrollWheelEvent, SharedString, Stateful, Styled, StyledText, Window, WindowBounds,
    WindowOptions, actions, anchored, canvas, deferred, div, fill, point, prelude::*, px, relative,
    size,
};

use crate::{
    agents::AgentScan,
    bd::BdClient,
    herdr::{self, AgentInfo, AgentKind},
    model::{BlocksNode, DashboardData, EpicSummary, Issue, WorkState},
    queue::{self, QueueEntry},
    terminal::ChatTerminal,
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

#[derive(Clone, Copy)]
enum ClosedEvent<'a> {
    Issue(&'a Issue),
    EpicCompleted(&'a EpicSummary),
}

impl<'a> ClosedEvent<'a> {
    fn issue(self) -> &'a Issue {
        match self {
            Self::Issue(issue) => issue,
            Self::EpicCompleted(summary) => &summary.epic,
        }
    }

    fn timestamp(self) -> &'a str {
        match self {
            Self::Issue(issue) => closure_time(issue),
            Self::EpicCompleted(summary) => summary.completed_at().unwrap_or(""),
        }
    }

    fn is_epic_completion(self) -> bool {
        matches!(self, Self::EpicCompleted(_))
    }
}

pub struct Dashboard {
    bd: BdClient,
    data: DashboardData,
    selected: Option<String>,
    message: Option<String>,
    dashboard_scroll: ScrollHandle,
    // Incrementing this invalidates the previous Linux kinetic-scroll task.
    #[cfg(target_os = "linux")]
    scroll_momentum_generation: u64,
    completed_toast: Vec<Issue>,
    focus_handle: FocusHandle,
    search_open: bool,
    search_query: String,
    filter: DashboardFilter,
    starred_only: bool,
    desc_edit: Option<DescEdit>,
    agent_menu_for: Option<String>,
    priority_menu_for: Option<String>,
    state_menu_for: Option<String>,
    close_dialog_for: Option<String>,
    close_reason: String,
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
    // Right-clicked row in the closed view, showing its context menu.
    closed_menu_for: Option<String>,
    // Every embedded chat terminal started this session. Closing the pane
    // hides it; the PTYs stay alive until a convo is explicitly killed.
    chats: Vec<Entity<ChatTerminal>>,
    active_chat: usize,
    chat_open: bool,
    // The convo-switcher dropdown under the pane header title.
    chat_menu_open: bool,
}

// In-flight description edit. The buffer is detached from the issue so the
// 2-second auto-refresh can swap `data` underneath without stomping typing.
struct DescEdit {
    id: String,
    buffer: String,
    // Byte offset into `buffer`, always on a char boundary.
    cursor: usize,
    // Selection anchor; a selection spans anchor..cursor in either direction.
    anchor: Option<usize>,
    // A mouse drag that started inside the editor is extending the selection.
    dragging: bool,
}

impl DescEdit {
    fn new(id: String, buffer: String) -> Self {
        let cursor = buffer.len();
        Self {
            id,
            buffer,
            cursor,
            anchor: None,
            dragging: false,
        }
    }

    fn selection(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor.filter(|anchor| *anchor != self.cursor)?;
        Some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    fn selected_text(&self) -> Option<String> {
        self.selection().map(|range| self.buffer[range].to_owned())
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.cursor = range.start;
        self.buffer.replace_range(range, "");
        self.anchor = None;
        true
    }

    fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.buffer.len();
    }

    // Called before every cursor motion: shift extends the selection from
    // where the cursor was, plain motion drops it.
    fn begin_move(&mut self, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
    }

    fn set_cursor(&mut self, index: usize, extend: bool) {
        let mut index = index.min(self.buffer.len());
        while !self.buffer.is_char_boundary(index) {
            index -= 1;
        }
        self.begin_move(extend);
        self.cursor = index;
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(previous) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= previous.len_utf8();
            self.buffer.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.start;
            self.anchor = None;
            return;
        }
        self.anchor = None;
        if let Some(previous) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= previous.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.end;
            self.anchor = None;
            return;
        }
        self.anchor = None;
        if let Some(next) = self.buffer[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn extend_left(&mut self) {
        self.begin_move(true);
        if let Some(previous) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= previous.len_utf8();
        }
    }

    fn extend_right(&mut self) {
        self.begin_move(true);
        if let Some(next) = self.buffer[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn line_start(&self) -> usize {
        self.buffer[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }

    fn line_end(&self) -> usize {
        self.buffer[self.cursor..]
            .find('\n')
            .map_or(self.buffer.len(), |index| self.cursor + index)
    }

    fn move_vertical(&mut self, up: bool) {
        let column = self.buffer[self.line_start()..self.cursor].chars().count();
        let start = self.line_start();
        let target_start = if up {
            if start == 0 {
                self.cursor = 0;
                return;
            }
            self.buffer[..start - 1]
                .rfind('\n')
                .map_or(0, |index| index + 1)
        } else {
            let end = self.line_end();
            if end == self.buffer.len() {
                self.cursor = end;
                return;
            }
            end + 1
        };
        let target_line = &self.buffer[target_start..];
        let target_len = target_line.find('\n').unwrap_or(target_line.len());
        let mut offset = 0;
        for character in self.buffer[target_start..target_start + target_len]
            .chars()
            .take(column)
        {
            offset += character.len_utf8();
        }
        self.cursor = target_start + offset;
    }
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
            #[cfg(target_os = "linux")]
            scroll_momentum_generation: 0,
            completed_toast: Vec::new(),
            focus_handle: cx.focus_handle(),
            search_open: false,
            search_query: String::new(),
            filter: DashboardFilter::All,
            starred_only: false,
            desc_edit: None,
            agent_menu_for: None,
            priority_menu_for: None,
            state_menu_for: None,
            close_dialog_for: None,
            close_reason: String::new(),
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
            closed_menu_for: None,
            chats: Vec::new(),
            active_chat: 0,
            chat_open: false,
            chat_menu_open: false,
        }
    }

    fn dashboard_scroll_wheel(
        &mut self,
        _event: &ScrollWheelEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Cocoa supplies momentum events itself. GPUI's Linux backends do not,
        // so add a short decaying tail to wheel and touchpad deltas here only.
        #[cfg(target_os = "linux")]
        {
            let delta = _event.delta.pixel_delta(_window.line_height());
            let delta_y = if delta.y == Pixels::ZERO {
                delta.x
            } else {
                delta.y
            };
            if delta_y == Pixels::ZERO {
                return;
            }

            self.scroll_momentum_generation = self.scroll_momentum_generation.wrapping_add(1);
            let generation = self.scroll_momentum_generation;
            let handle = self.dashboard_scroll.clone();
            let mut velocity = delta_y * 0.32;

            _cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    velocity *= 0.84;
                    if velocity.abs() < px(0.35) {
                        break;
                    }

                    let keep_going = this
                        .update(cx, |dashboard, cx| {
                            if dashboard.scroll_momentum_generation != generation {
                                return false;
                            }

                            let offset = handle.offset();
                            let next_y = (offset.y + velocity)
                                .clamp(-handle.max_offset().height, Pixels::ZERO);
                            if next_y == offset.y {
                                return false;
                            }

                            handle.set_offset(point(offset.x, next_y));
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !keep_going {
                        break;
                    }
                }
            })
            .detach();
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
                        theme::refresh();
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
        if self.desc_edit.is_some() {
            return;
        }
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

    fn start_desc_edit(&mut self, issue: &Issue, window: &mut Window, cx: &mut Context<Self>) {
        self.desc_edit = Some(DescEdit::new(issue.id.clone(), issue.description.clone()));
        self.search_open = false;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn cancel_desc_edit(&mut self, cx: &mut Context<Self>) {
        self.desc_edit = None;
        cx.notify();
    }

    fn save_desc_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.desc_edit.take() else {
            return;
        };
        if let Err(error) = self.bd.set_description(&edit.id, &edit.buffer) {
            self.message = Some(error.to_string());
            self.desc_edit = Some(edit);
        } else {
            self.reload(cx);
            self.selected = Some(edit.id);
        }
        cx.notify();
    }

    fn desc_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.desc_edit.is_none() {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "escape" => {
                self.cancel_desc_edit(cx);
                cx.stop_propagation();
                return;
            }
            "enter" if modifiers.platform => {
                self.save_desc_edit(cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let Some(edit) = self.desc_edit.as_mut() else {
            return;
        };
        match event.keystroke.key.as_str() {
            "enter" => edit.insert("\n"),
            "backspace" => edit.backspace(),
            "delete" => edit.delete(),
            "left" if modifiers.platform => {
                edit.begin_move(modifiers.shift);
                edit.cursor = edit.line_start();
            }
            "right" if modifiers.platform => {
                edit.begin_move(modifiers.shift);
                edit.cursor = edit.line_end();
            }
            "left" if modifiers.shift => edit.extend_left(),
            "right" if modifiers.shift => edit.extend_right(),
            "left" => edit.move_left(),
            "right" => edit.move_right(),
            "up" => {
                edit.begin_move(modifiers.shift);
                edit.move_vertical(true);
            }
            "down" => {
                edit.begin_move(modifiers.shift);
                edit.move_vertical(false);
            }
            "home" => {
                edit.begin_move(modifiers.shift);
                edit.cursor = edit.line_start();
            }
            "end" => {
                edit.begin_move(modifiers.shift);
                edit.cursor = edit.line_end();
            }
            "a" if modifiers.platform => edit.select_all(),
            "c" if modifiers.platform => {
                if let Some(text) = edit.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                // Nothing changed on screen; skip the notify below.
                cx.stop_propagation();
                return;
            }
            "x" if modifiers.platform => {
                if let Some(text) = edit.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    edit.delete_selection();
                }
            }
            "v" if modifiers.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    edit.insert(&text);
                }
            }
            _ if !modifiers.control && !modifiers.alt && !modifiers.platform => {
                let Some(text) = event.keystroke.key_char.clone() else {
                    return;
                };
                if text.chars().any(|character| character.is_control()) {
                    return;
                }
                edit.insert(&text);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn search_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.close_dialog_for.is_some() {
            match event.keystroke.key.as_str() {
                "escape" => self.cancel_close(cx),
                "backspace" => {
                    self.close_reason.pop();
                    cx.notify();
                }
                "enter" => self.confirm_close(cx),
                _ if !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform =>
                {
                    if let Some(text) = &event.keystroke.key_char
                        && text.chars().all(|character| !character.is_control())
                    {
                        self.close_reason.push_str(text);
                        cx.notify();
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        if self.desc_edit.is_some() {
            self.desc_key_down(event, cx);
            return;
        }
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
        if self
            .desc_edit
            .as_ref()
            .is_some_and(|edit| edit.id != id)
        {
            self.desc_edit = None;
        }
        self.selected = Some(id);
        cx.notify();
    }

    fn dismiss_inspector(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        self.inspector_resize = None;
        self.desc_edit = None;
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

    fn toggle_state_menu(&mut self, id: String, cx: &mut Context<Self>) {
        self.state_menu_for = if self.state_menu_for.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    fn close_state_menu(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state_menu_for = None;
        cx.notify();
    }

    fn choose_status(
        &mut self,
        id: String,
        current: String,
        status: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.state_menu_for = None;
        if current == status {
            cx.notify();
            return;
        }
        if status == "closed" {
            self.close_dialog_for = Some(id);
            self.close_reason.clear();
            cx.notify();
            return;
        }

        let result = if current == "closed" {
            self.bd.reopen(&id).and_then(|_| {
                if status == "open" {
                    Ok(())
                } else {
                    self.bd.set_status(&id, status)
                }
            })
        } else {
            self.bd.set_status(&id, status)
        };
        if let Err(error) = result {
            self.message = Some(error.to_string());
        } else {
            self.reload(cx);
            self.selected = Some(id);
        }
        cx.notify();
    }

    fn toggle_star(&mut self, id: String, starred: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.bd.set_starred(&id, !starred) {
            self.message = Some(error.to_string());
        } else {
            self.reload(cx);
            // Unstarring the last starred epic would leave the filter showing
            // nothing — drop back to the full grid instead.
            if self.starred_only
                && !self
                    .data
                    .epics
                    .iter()
                    .any(|summary| summary.epic.starred() && summary.epic.status != "closed")
            {
                self.starred_only = false;
            }
        }
        cx.notify();
    }

    fn cancel_close(&mut self, cx: &mut Context<Self>) {
        self.close_dialog_for = None;
        self.close_reason.clear();
        cx.notify();
    }

    fn confirm_close(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.close_dialog_for.take() else {
            return;
        };
        let reason = self.close_reason.trim();
        if let Err(error) = self.bd.close(&id, (!reason.is_empty()).then_some(reason)) {
            self.message = Some(error.to_string());
        } else {
            self.reload(cx);
            self.selected = Some(id);
        }
        self.close_reason.clear();
        cx.notify();
    }

    fn toggle_starred_only(&mut self, cx: &mut Context<Self>) {
        self.starred_only = !self.starred_only;
        self.dashboard_scroll
            .set_offset(gpui::point(px(0.), px(0.)));
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
                        dashboard.agent_notice = Some((
                            task_entry.id.clone(),
                            false,
                            format!("Queue: started {name}"),
                        ));
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

    fn chat_labelled(&self, label: &str, cx: &App) -> Option<usize> {
        self.chats
            .iter()
            .position(|chat| chat.read(cx).label == label)
    }

    fn activate_chat(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_chat = index;
        self.chat_open = true;
        self.chat_menu_open = false;
        if let Some(chat) = self.chats.get(index) {
            let focus = chat.read(cx).focus_handle(cx);
            window.focus(&focus);
        }
        cx.notify();
    }

    fn chat_about(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.closed_menu_for = None;
        // One conversation per bead: a second "chat about it" resumes it.
        if let Some(index) = self.chat_labelled(&id, cx) {
            self.activate_chat(index, window, cx);
            return;
        }
        let Some(issue) = self.data.issue(&id).cloned() else {
            return;
        };
        match ChatTerminal::open(&issue, self.bd.project(), cx) {
            Ok(chat) => {
                self.chats.push(chat);
                self.activate_chat(self.chats.len() - 1, window, cx);
            }
            Err(error) => {
                self.message = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn open_designer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.chat_labelled("designer", cx) {
            self.activate_chat(index, window, cx);
            return;
        }
        match ChatTerminal::open_designer(self.bd.project(), cx) {
            Ok(chat) => {
                self.chats.push(chat);
                self.activate_chat(self.chats.len() - 1, window, cx);
            }
            Err(error) => {
                self.message = Some(error.to_string());
                cx.notify();
            }
        }
    }

    // × on the pane hides it; every conversation stays alive for the session.
    fn close_chat(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.chat_open = false;
        self.chat_menu_open = false;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    // Dropping the entity shuts its PTY event loop down via ChatTerminal's
    // Drop — this is the one gesture that actually ends a conversation.
    fn kill_chat(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.chats.len() {
            return;
        }
        self.chats.remove(index);
        if self.chats.is_empty() {
            self.chat_open = false;
            self.chat_menu_open = false;
            window.focus(&self.focus_handle);
        } else if self.active_chat >= self.chats.len() {
            self.active_chat = self.chats.len() - 1;
        } else if index < self.active_chat {
            self.active_chat -= 1;
        }
        cx.notify();
    }

    fn close_closed_menu(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.closed_menu_for = None;
        cx.notify();
    }

    fn closed_row_menu(&self, issue_id: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = issue_id.to_owned();
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .offset(point(px(0.), px(24.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .w(px(240.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_closed_menu))
                        .child(
                            div()
                                .id("chat-about-bead")
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.chat_about(id.clone(), window, cx);
                                }))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::text())
                                        .child("Chat about it"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child("Claude · Fable, right here in a quick terminal"),
                                ),
                        ),
                ),
        )
        .with_priority(2)
    }

    // The docked chat pane: header identifying the bead, the terminal view
    // underneath. A side pane on wide windows; on narrow ones a bottom pane
    // under the inspector and deck, mirroring the inspector's docking rule.
    // The dropdown under the pane title: every live conversation, with a
    // kill button per row, plus the designer entry point.
    fn chat_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let has_designer = self.chat_labelled("designer", cx).is_some();
        let rows: Vec<_> = self
            .chats
            .iter()
            .enumerate()
            .map(|(index, chat)| {
                let chat = chat.read(cx);
                (index, chat.label.clone(), chat.subtitle.clone(), chat.exited())
            })
            .collect();
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .offset(point(px(0.), px(30.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .w(px(300.))
                        .rounded_lg()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.chat_menu_open = false;
                            cx.notify();
                        }))
                        .children(rows.into_iter().map(|(index, label, subtitle, exited)| {
                            let active = index == self.active_chat;
                            div()
                                .id(SharedString::from(format!("chat-switch:{index}")))
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .when(active, |row| row.bg(theme::surface()))
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.activate_chat(index, window, cx);
                                }))
                                .child(div().size(px(6.)).rounded_full().bg(if exited {
                                    theme::muted()
                                } else {
                                    theme::accent()
                                }))
                                .child(div().text_sm().text_color(theme::text()).child(label))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child(subtitle),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("chat-kill:{index}")))
                                        .px_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style
                                                .bg(theme::raised_hover())
                                                .text_color(theme::danger())
                                        })
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.kill_chat(index, window, cx);
                                        }))
                                        .child("×"),
                                )
                        }))
                        .when(!has_designer, |menu| {
                            menu.child(
                                div()
                                    .id("chat-switch-designer")
                                    .px_3()
                                    .py_2()
                                    .border_t_1()
                                    .border_color(theme::border())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.chat_menu_open = false;
                                        this.open_designer(window, cx);
                                    }))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme::text())
                                            .child("✎ New designer chat"),
                                    ),
                            )
                        }),
                ),
        )
        .with_priority(3)
    }

    fn chat_panel(
        &self,
        chat: Entity<ChatTerminal>,
        bottom: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (label, subtitle, exited) = {
            let chat = chat.read(cx);
            (chat.label.clone(), chat.subtitle.clone(), chat.exited())
        };
        div()
            .flex()
            .flex_col()
            .bg(theme::background())
            .when(bottom, |pane| {
                pane.w_full()
                    .h(px(300.))
                    .max_h(relative(0.45))
                    .border_t_1()
            })
            .when(!bottom, |pane| {
                pane.w(px(600.)).min_w(px(420.)).h_full().border_l_1()
            })
            .border_color(theme::border())
            .child(
                div()
                    // Matches the 46px toolbar so the two header rows line up
                    // across the pane border.
                    .h(px(46.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().size(px(7.)).rounded_full().bg(if exited {
                        theme::muted()
                    } else {
                        theme::accent()
                    }))
                    .child(
                        div()
                            .id("chat-title")
                            .px_1()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.chat_menu_open = !this.chat_menu_open;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(format!("Chat · {label}")),
                            )
                            .child(div().text_xs().text_color(theme::muted()).child("▾"))
                            .when(self.chat_menu_open, |title| {
                                title.child(self.chat_switcher(cx))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(subtitle),
                    )
                    .child(self.badge("FABLE", theme::accent()))
                    .child(
                        div()
                            .id("close-chat")
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
                            .on_click(cx.listener(Self::close_chat))
                            .child("×"),
                    ),
            )
            .child(div().flex_1().min_h_0().child(chat))
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
    fn priority_pill_editable(
        &self,
        issue: &Issue,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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

    // Header toggle that narrows the grid to starred epics only. Lives next
    // to the state filters but is independent of them — both can be active.
    fn starred_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let starred = self
            .data
            .epics
            .iter()
            .filter(|summary| summary.epic.starred() && summary.epic.status != "closed")
            .count();
        let selected = self.starred_only;
        let color = theme::star();
        div()
            .id("filter:starred")
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
            .on_click(cx.listener(|this, _, _, cx| this.toggle_starred_only(cx)))
            .text_xs()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if starred > 0 { color } else { theme::muted() })
                    .child(format!("★ {starred}")),
            )
            .child(div().text_color(theme::muted()).child("starred"))
    }

    fn state_badge(&self, state: WorkState) -> Div {
        let (label, color) = match state {
            WorkState::Ready => ("READY", theme::ready()),
            WorkState::Blocked => ("BLOCKED", theme::blocked()),
            WorkState::InProgress => ("IN PROGRESS", theme::progress()),
            WorkState::Closed => ("CLOSED", theme::muted()),
            WorkState::Other => ("OTHER", theme::muted()),
        };
        self.badge(label, color)
    }

    fn badge(&self, label: &'static str, color: Hsla) -> Div {
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

    fn state_badge_editable(
        &self,
        issue: &Issue,
        location: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let menu_key = format!("{location}:{}", issue.id);
        let toggle_key = menu_key.clone();
        let state = self.data.state(&issue.id);
        let menu_open = self.state_menu_for.as_deref() == Some(menu_key.as_str());
        self.state_badge(state)
            .id(SharedString::from(format!("state-menu:{menu_key}")))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_state_menu(toggle_key.clone(), cx);
            }))
            .when(menu_open, |badge| badge.child(self.state_menu(issue, cx)))
    }

    fn state_menu(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let options = [
            ("open", "OPEN", theme::ready()),
            ("in_progress", "IN PROGRESS", theme::progress()),
            ("closed", "CLOSED", theme::muted()),
        ];
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .offset(point(px(0.), px(20.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .w(px(132.))
                        .rounded_md()
                        .bg(theme::background())
                        .border_1()
                        .border_color(theme::border())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(Self::close_state_menu))
                        .children(options.into_iter().map(|(status, label, color)| {
                            let id = issue.id.clone();
                            let current = issue.status.clone();
                            let selected = current == status;
                            div()
                                .id(SharedString::from(format!(
                                    "state-option:{}:{status}",
                                    issue.id
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
                                    this.choose_status(id.clone(), current.clone(), status, cx);
                                }))
                                .child(div().text_size(px(10.)).text_color(color).child(label))
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
            .child(self.state_badge_editable(issue, "row", cx))
    }

    fn description_section(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let editing = self
            .desc_edit
            .as_ref()
            .filter(|edit| edit.id == issue.id.as_str());
        let issue_for_edit = issue.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(theme::muted()).child("DESCRIPTION"))
                    .when(editing.is_none(), |header| {
                        header.child(
                            div()
                                .id(SharedString::from(format!("edit-desc:{}", issue.id)))
                                .text_xs()
                                .text_color(theme::muted())
                                .cursor_pointer()
                                .hover(|style| style.text_color(theme::text()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.start_desc_edit(&issue_for_edit, window, cx);
                                }))
                                .child("✎"),
                        )
                    })
                    .when(editing.is_some(), |header| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme::muted())
                                .child("⌘⏎ save · esc cancel"),
                        )
                    }),
            )
            .map(|section| match editing {
                Some(edit) => {
                    let styled = StyledText::new(edit.buffer.clone()).with_highlights(
                        edit.selection().map(|range| {
                            (
                                range,
                                HighlightStyle {
                                    background_color: Some(theme::accent().opacity(0.35)),
                                    ..Default::default()
                                },
                            )
                        }),
                    );
                    let layout = styled.layout().clone();
                    let down_layout = layout.clone();
                    let move_layout = layout.clone();
                    let cursor = edit.cursor;
                    section
                        .child(
                            div()
                                .id("desc-editor")
                                .p_2()
                                .rounded_md()
                                .bg(theme::background())
                                .border_1()
                                .border_color(theme::accent().opacity(0.5))
                                .text_sm()
                                .line_height(relative(1.45))
                                .text_color(theme::text())
                                .cursor_text()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        let Some(edit) = this.desc_edit.as_mut() else {
                                            return;
                                        };
                                        let index = match down_layout.index_for_position(event.position)
                                        {
                                            Ok(index) | Err(index) => index,
                                        };
                                        edit.set_cursor(index, event.modifiers.shift);
                                        if !event.modifiers.shift {
                                            edit.anchor = Some(edit.cursor);
                                        }
                                        edit.dragging = true;
                                        cx.notify();
                                    }),
                                )
                                .on_mouse_move(cx.listener(
                                    move |this, event: &MouseMoveEvent, _, cx| {
                                        let Some(edit) = this.desc_edit.as_mut() else {
                                            return;
                                        };
                                        if !edit.dragging {
                                            return;
                                        }
                                        if !event.dragging() {
                                            edit.dragging = false;
                                            return;
                                        }
                                        let index = match move_layout.index_for_position(event.position)
                                        {
                                            Ok(index) | Err(index) => index,
                                        };
                                        edit.set_cursor(index, true);
                                        cx.notify();
                                    },
                                ))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseUpEvent, _, _| {
                                        if let Some(edit) = this.desc_edit.as_mut() {
                                            edit.dragging = false;
                                        }
                                    }),
                                )
                                .child(styled)
                                // Painted after the text so the caret can ask the
                                // finished layout where the cursor index landed.
                                .child(canvas(
                                    |_, _, _| (),
                                    move |_, _, window, _| {
                                        if let Some(position) = layout.position_for_index(cursor) {
                                            window.paint_quad(fill(
                                                gpui::Bounds::new(
                                                    position,
                                                    size(px(2.), layout.line_height()),
                                                ),
                                                theme::accent(),
                                            ));
                                        }
                                    },
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id("save-desc")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme::accent().opacity(0.4))
                                        .bg(theme::accent().opacity(0.1))
                                        .text_xs()
                                        .text_color(theme::accent())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::accent().opacity(0.18)))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.save_desc_edit(cx);
                                        }))
                                        .child("Save"),
                                )
                                .child(
                                    div()
                                        .id("cancel-desc")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme::border())
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.cancel_desc_edit(cx);
                                        }))
                                        .child("Cancel"),
                                ),
                        )
                }
                None => section.child(
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
            })
    }

    fn star_button(&self, epic: &Issue, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = epic.id.clone();
        let starred = epic.starred();
        div()
            .id(SharedString::from(format!("star:{}", epic.id)))
            .flex_none()
            .text_sm()
            .text_color(if starred {
                theme::star()
            } else {
                theme::muted().opacity(0.5)
            })
            .cursor_pointer()
            .hover(|style| style.text_color(theme::star()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_star(id.clone(), starred, cx);
            }))
            .child(if starred { "★" } else { "☆" })
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
                    .child(self.star_button(&summary.epic, cx))
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
        events: Vec<ClosedEvent<'_>>,
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
                    .child("Recent completions"),
            )
            .children(events.into_iter().map(|event| {
                let issue = event.issue();
                let id = issue.id.clone();
                let menu_id = issue.id.clone();
                let menu_open = self.closed_menu_for.as_deref() == Some(issue.id.as_str());
                let event_kind = if event.is_epic_completion() {
                    "epic-complete"
                } else {
                    "closed"
                };
                let completed_at = if event.timestamp().is_empty() {
                    "Unknown date".into()
                } else {
                    format_closed_at(event.timestamp())
                };
                div()
                    .id(SharedString::from(format!("{event_kind}:{}", issue.id)))
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
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.closed_menu_for = Some(menu_id.clone());
                            cx.notify();
                        }),
                    )
                    .when(menu_open, |row| row.child(self.closed_row_menu(&issue.id, cx)))
                    .child(
                        div()
                            .w(px(116.))
                            .flex_none()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(completed_at),
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
                    .child(if event.is_epic_completion() {
                        self.badge("✓ EPIC COMPLETE", theme::ready()).into_any_element()
                    } else {
                        self.state_badge_editable(issue, "closed", cx).into_any_element()
                    })
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
            .when(agent.as_ref().is_some_and(|agent| agent.external), |row| {
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
            })
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
            .on_drop(
                cx.listener(move |this, dragged: &DraggedQueueEntry, _, cx| {
                    this.move_queued(&dragged.id, Some(&drop_id), cx);
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(click_id.clone(), cx)))
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .text_xs()
                    .text_color(if is_next {
                        theme::ready()
                    } else {
                        theme::muted()
                    })
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
    fn queue_paused_notice(
        &self,
        error: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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
                            let left = div().flex_1().min_w_0().flex().items_center().gap_2();
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
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.launch_next_queued(cx)
                                            }),
                                        )
                                        .child("Run next"),
                                )
                                .when_some(
                                    next_title,
                                    |row, title| {
                                        row.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .text_color(theme::muted())
                                                .child(title),
                                        )
                                    },
                                )
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
                                .children(self.queue.iter().enumerate().map(|(index, entry)| {
                                    self.queue_row(index, entry, next_id.as_deref(), cx)
                                }))
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

    fn close_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = self.close_dialog_for.clone().unwrap_or_default();
        let has_reason = !self.close_reason.is_empty();
        div()
            .id("close-dialog-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.55))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_close(cx)))
            .child(
                div()
                    .id("close-dialog")
                    .w(px(440.))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border())
                    .shadow_lg()
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .text_base()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(format!("Close {id}?")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("Optionally record why this bead is being closed."),
                    )
                    .child(
                        div()
                            .id("close-reason")
                            .h(px(38.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded_md()
                            .bg(theme::background())
                            .border_1()
                            .border_color(theme::accent().opacity(0.55))
                            .cursor_text()
                            .text_sm()
                            .text_color(if has_reason {
                                theme::text()
                            } else {
                                theme::muted()
                            })
                            .child(if has_reason {
                                format!("{}|", self.close_reason)
                            } else {
                                "Close reason (optional)…|".to_owned()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("cancel-close")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.cancel_close(cx)
                                    }))
                                    .child("Cancel"),
                            )
                            .child(
                                div()
                                    .id("confirm-close")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(theme::danger().opacity(0.16))
                                    .border_1()
                                    .border_color(theme::danger().opacity(0.4))
                                    .text_xs()
                                    .text_color(theme::danger())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::danger().opacity(0.25)))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.confirm_close(cx)
                                    }))
                                    .child("Close bead"),
                            ),
                    ),
            )
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
                            .child(self.queue_option(
                                &issue.id,
                                AgentKind::Pi,
                                None,
                                "Pi",
                                cx,
                            ))
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
                    .on_click(cx.listener(move |this, _, _, cx| this.select(select_id.clone(), cx)))
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
                    .text_color(if known {
                        theme::accent()
                    } else {
                        theme::muted()
                    })
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
                        .text_color(if closed {
                            theme::muted()
                        } else {
                            theme::text()
                        })
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
    fn blocked_by_section(
        &self,
        issue: &Issue,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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
                                    .child(self.state_badge_editable(issue, "inspector", cx))
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
                    .child(self.description_section(issue, cx))
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
                (!self.starred_only || epic.epic.starred())
                    && ((self.issue_matches_filter(&epic.epic)
                        && (query.is_empty() || issue_matches(&epic.epic, &query)))
                        || epic.children.iter().any(|issue| {
                            self.issue_matches_filter(issue)
                                && (query.is_empty() || issue_matches(issue, &query))
                        }))
            })
            .collect();
        // Starring is an epic-level concept, so the loose-beads card sits out
        // of starred-only mode entirely.
        let show_ungrouped = !self.starred_only
            && self.data.ungrouped.iter().any(|issue| {
                self.issue_matches_filter(issue)
                    && (query.is_empty() || issue_matches(issue, &query))
            });
        // The closed view is an event stream, not just a status query. An epic
        // completion is inferred when all of its child beads are closed; the
        // epic itself can (and normally does) remain open.
        let mut closed_events: Vec<_> = self
            .data
            .issues
            .iter()
            .filter(|issue| {
                issue.status == "closed"
                    && (query.is_empty() || issue_matches(issue, &query))
                    // A completed epic gets one richer completion event rather
                    // than a duplicate literal CLOSED row.
                    && !(issue.issue_type == "epic"
                        && self
                            .data
                            .epics
                            .iter()
                            .any(|summary| summary.epic.id == issue.id && summary.is_complete()))
            })
            .map(ClosedEvent::Issue)
            .chain(
                self.data
                    .epics
                    .iter()
                    .filter(|summary| {
                        summary.is_complete()
                            && (query.is_empty() || issue_matches(&summary.epic, &query))
                    })
                    .map(ClosedEvent::EpicCompleted),
            )
            .collect();
        closed_events.sort_by(|left, right| {
            right
                .timestamp()
                .cmp(left.timestamp())
                .then_with(|| right.issue().id.cmp(&left.issue().id))
        });
        closed_events.truncate(100);
        let has_results = if self.filter == DashboardFilter::Closed {
            !closed_events.is_empty()
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
                            .child(self.starred_toggle(cx))
                            .child(
                                div()
                                    .id("open-designer")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style
                                            .bg(theme::surface_hover())
                                            .text_color(theme::accent())
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_designer(window, cx)
                                    }))
                                    .child("✎ designer"),
                            )
                            .when(!self.chats.is_empty() && !self.chat_open, |toolbar| {
                                let count = self.chats.len();
                                toolbar.child(
                                    div()
                                        .id("reopen-chats")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme::accent().opacity(0.35))
                                        .text_xs()
                                        .text_color(theme::accent())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::accent().opacity(0.1)))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let index = this.active_chat.min(
                                                this.chats.len().saturating_sub(1),
                                            );
                                            this.activate_chat(index, window, cx);
                                        }))
                                        .child(if count == 1 {
                                            "1 chat".to_owned()
                                        } else {
                                            format!("{count} chats")
                                        }),
                                )
                            })
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
                            .on_scroll_wheel(cx.listener(Self::dashboard_scroll_wheel))
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
                                        |grid| grid.child(self.closed_card(closed_events, cx)),
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
            .when_some(
                self.chat_open
                    .then(|| self.chats.get(self.active_chat).cloned())
                    .flatten(),
                |root, chat| root.child(self.chat_panel(chat, bottom_inspector, cx)),
            )
            .when(!self.completed_toast.is_empty(), |root| {
                root.child(self.completion_toast(cx))
            })
            .when(self.close_dialog_for.is_some(), |root| {
                root.child(self.close_dialog(cx))
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

#[cfg(test)]
mod tests {
    use super::DescEdit;

    fn edit(buffer: &str, cursor: usize) -> DescEdit {
        let mut edit = DescEdit::new("x".into(), buffer.into());
        edit.cursor = cursor;
        edit
    }

    #[test]
    fn editing_stays_on_char_boundaries() {
        let mut edit = edit("héllo", 5);
        edit.backspace();
        assert_eq!(edit.buffer, "hélo");
        edit.move_left();
        edit.move_left();
        assert_eq!(edit.cursor, 1);
        edit.insert("ü");
        assert_eq!(edit.buffer, "hüélo");
        edit.delete();
        assert_eq!(edit.buffer, "hülo");
        edit.move_right();
        edit.insert("!");
        assert_eq!(edit.buffer, "hül!o");
    }

    #[test]
    fn selection_replaces_collapses_and_normalizes_direction() {
        let mut edit = edit("hello world", 6);
        // Drag backwards: anchor right of cursor still yields a forward range.
        edit.set_cursor(6, false);
        edit.anchor = Some(6);
        edit.set_cursor(0, true);
        assert_eq!(edit.selection(), Some(0..6));
        assert_eq!(edit.selected_text().as_deref(), Some("hello "));
        edit.insert("bye ");
        assert_eq!(edit.buffer, "bye world");
        assert_eq!(edit.cursor, 4);
        assert_eq!(edit.selection(), None);

        edit.select_all();
        edit.backspace();
        assert_eq!(edit.buffer, "");

        // Plain left/right collapse a selection to its edge instead of moving.
        let mut collapse = DescEdit::new("x".into(), "abc".into());
        collapse.cursor = 0;
        collapse.set_cursor(2, true);
        collapse.move_right();
        assert_eq!((collapse.cursor, collapse.selection()), (2, None));
    }

    #[test]
    fn vertical_moves_keep_the_column_and_clamp_short_lines() {
        let mut edit = edit("first line\nlonger second\nab", 4);
        edit.move_vertical(false);
        assert_eq!(&edit.buffer[edit.cursor..edit.cursor + 2], "er");
        edit.move_vertical(false);
        // Third line is shorter than the column; cursor clamps to its end.
        assert_eq!(edit.cursor, edit.buffer.len());
        edit.move_vertical(true);
        edit.move_vertical(true);
        assert_eq!(edit.cursor, 2);
        edit.move_vertical(true);
        assert_eq!(edit.cursor, 0, "up from the first line goes to the start");
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

// An embedded terminal for quick "chat about this bead" conversations: a
// local PTY running Claude Code, with alacritty_terminal keeping the VT state
// and gpui painting the visible grid. Deliberately minimal — no scrollback UI,
// no selection — herdr remains the home for real agent sessions.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::Scroll,
    index::{Column, Line, Point as GridPoint, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term, TermMode, cell::Flags, test::TermSize},
    tty::{self, Options as PtyOptions, Shell},
    vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb as AnsiRgb},
};
use anyhow::{Context as _, Result};
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontStyle, FontWeight,
    Hsla, IntoElement, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent, Styled, TextRun,
    UnderlineStyle, Window, canvas, div, font, point, prelude::*, px,
};

use crate::{model::Issue, theme};

const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 12.;
const LINE_HEIGHT: f32 = 17.;

// Everything the terminal itself watches for from the PTY thread. Rendering
// polls `dirty` instead of waking gpui from a foreign thread.
#[derive(Clone)]
struct EventProxy {
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    // Filled in after the event loop exists; PtyWrite answers (color queries,
    // size reports) go straight back to the PTY.
    sender: Arc<Mutex<Option<EventLoopSender>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::Exit | TermEvent::ChildExit(_) => {
                self.exited.store(true, Ordering::Relaxed);
                self.dirty.store(true, Ordering::Relaxed);
            }
            TermEvent::PtyWrite(text) => {
                if let Some(sender) = self.sender.lock().unwrap().as_ref() {
                    let _ = sender.send(Msg::Input(text.into_bytes().into()));
                }
            }
            TermEvent::Wakeup => self.dirty.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
}

pub struct ChatTerminal {
    // What the pane header and convo dropdown call this chat: a bead id, or
    // "designer" for the global bead-designer conversation.
    pub label: String,
    pub subtitle: String,
    term: Arc<FairMutex<Term<EventProxy>>>,
    sender: EventLoopSender,
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    // Grid size last sent to the PTY, shared with the paint closure that
    // measures the real panel and re-fits the grid.
    grid: Arc<Mutex<(usize, usize)>>,
    // Sub-line scroll remainder, so smooth trackpad deltas add up instead of
    // being truncated away.
    scroll_remainder: f32,
    // Where the grid actually landed on screen and how wide a cell is, written
    // by the paint pass so mouse positions can be mapped back to cells.
    layout: Arc<Mutex<Option<(Bounds<Pixels>, Pixels)>>>,
    // A left-button drag is extending the selection.
    selecting: bool,
    focus_handle: FocusHandle,
}

// The discussion counterpart to herdr's implementation prompt: load the bead,
// summarize, then wait — never start editing on its own.
fn chat_prompt(issue: &Issue) -> String {
    format!(
        "I want to chat about bead {id} (\"{title}\") from this project's bd issue tracker. \
         Start by running `bd show {id}` to load its details, then give me a brief summary \
         of what this bead was about and wait for my questions. This is a discussion — \
         don't change any code unless I explicitly ask.",
        id = issue.id,
        title = issue.title,
    )
}

// Same role as the TUI's designer tab: turn discussion into well-scoped
// beads, never implement anything.
fn designer_prompt() -> String {
    [
        "You are the bead designer for this repository. Your job is to turn ideas and \
         discussion into well-scoped beads using the `bd` CLI — you never implement \
         anything yourself.",
        "Start by running `bd ready --json --limit 0` and `bd list --json --status \
         in_progress --limit 0` to see the current state, then wait for direction.",
        "When given an idea, work it out into one or more beads with `bd create`, each \
         with a clear description and acceptance criteria a separate worker agent could \
         implement without further context. Split large work into an epic with dependent \
         beads (`bd dep`), and set sensible priorities.",
        "Do not edit source files, do not claim beads, and do not close beads you did \
         not author — implementation is done by worker agents that consume ready beads.",
    ]
    .join(" ")
}

impl ChatTerminal {
    pub fn open(issue: &Issue, cwd: &Path, cx: &mut App) -> Result<Entity<Self>> {
        Self::spawn(
            issue.id.clone(),
            issue.title.clone(),
            chat_prompt(issue),
            cwd,
            cx,
        )
    }

    pub fn open_designer(cwd: &Path, cx: &mut App) -> Result<Entity<Self>> {
        Self::spawn(
            "designer".into(),
            "turn ideas into beads".into(),
            designer_prompt(),
            cwd,
            cx,
        )
    }

    fn spawn(
        label: String,
        subtitle: String,
        prompt: String,
        cwd: &Path,
        cx: &mut App,
    ) -> Result<Entity<Self>> {
        let dirty = Arc::new(AtomicBool::new(true));
        let exited = Arc::new(AtomicBool::new(false));
        let sender_slot = Arc::new(Mutex::new(None));
        let proxy = EventProxy {
            dirty: dirty.clone(),
            exited: exited.clone(),
            sender: sender_slot.clone(),
        };

        let (cols, rows) = (100, 28);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            proxy.clone(),
        )));
        let window_size = WindowSize {
            num_cols: cols as u16,
            num_lines: rows as u16,
            cell_width: 8,
            cell_height: LINE_HEIGHT as u16,
        };
        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        let options = PtyOptions {
            shell: Some(Shell::new(
                "claude".into(),
                vec!["--model".into(), "claude-fable-5".into(), prompt],
            )),
            working_directory: Some(cwd.to_path_buf()),
            drain_on_exit: false,
            env,
        };
        let pty = tty::new(&options, window_size, 0)
            .context("could not spawn claude — is it on your PATH?")?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .context("could not start terminal IO loop")?;
        let sender = event_loop.channel();
        *sender_slot.lock().unwrap() = Some(sender.clone());
        event_loop.spawn();

        Ok(cx.new(|cx| {
            // The PTY reader thread cannot touch gpui, so it only flips
            // `dirty`; this poll turns that into repaints at ~30fps.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    let alive = this.update(cx, |terminal: &mut Self, cx| {
                        if terminal.dirty.swap(false, Ordering::Relaxed) {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
            Self {
                label,
                subtitle,
                term,
                sender,
                dirty,
                exited,
                grid: Arc::new(Mutex::new((cols, rows))),
                scroll_remainder: 0.,
                layout: Arc::new(Mutex::new(None)),
                selecting: false,
                focus_handle: cx.focus_handle(),
            }
        }))
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let mut bytes = Vec::new();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        // A raw newline would submit mid-paste in most TUI inputs.
        bytes.extend_from_slice(text.replace('\n', "\r").as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        self.write(bytes);
    }

    // Mouse position → grid cell, using the bounds the last paint recorded.
    // Positions outside the grid clamp to its edge so drags keep selecting.
    fn grid_point(&self, position: gpui::Point<Pixels>) -> Option<(GridPoint, Side)> {
        let (bounds, cell_width) = (*self.layout.lock().unwrap())?;
        let (cols, rows) = *self.grid.lock().unwrap();
        let x = (f32::from(position.x - bounds.origin.x) / f32::from(cell_width)).max(0.);
        let y = f32::from(position.y - bounds.origin.y) / LINE_HEIGHT;
        let column = (x as usize).min(cols.saturating_sub(1));
        let row = (y.max(0.) as usize).min(rows.saturating_sub(1));
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        let offset = self.term.lock().grid().display_offset() as i32;
        Some((GridPoint::new(Line(row as i32 - offset), Column(column)), side))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        let kind = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.term.lock().selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if !event.dragging() {
            self.selecting = false;
            return;
        }
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform {
            match event.keystroke.key.as_str() {
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.paste(&text);
                    }
                    cx.stop_propagation();
                }
                "c" => {
                    self.copy_selection(cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }
        let mode = *self.term.lock().mode();
        if let Some(bytes) = key_bytes(&event.keystroke, mode) {
            // Typing snaps back to the live view and drops the selection,
            // like any terminal.
            {
                let mut term = self.term.lock();
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
            }
            self.scroll_remainder = 0.;
            self.write(bytes);
            cx.stop_propagation();
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y * 3.,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / LINE_HEIGHT,
        };
        self.scroll_remainder += lines;
        let whole = self.scroll_remainder.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.scroll_remainder -= whole as f32;
        let mut term = self.term.lock();
        let mode = *term.mode();
        // Same precedence as a real terminal: apps that asked for mouse
        // reporting (Claude Code does) get wheel events and scroll their own
        // view; other alt-screen TUIs get arrow keys; a plain shell scrolls
        // our scrollback.
        if mode.intersects(TermMode::MOUSE_MODE) {
            drop(term);
            let button = if whole > 0 { 64 } else { 65 };
            let mut bytes = Vec::new();
            for _ in 0..whole.unsigned_abs() {
                if mode.contains(TermMode::SGR_MOUSE) {
                    bytes.extend_from_slice(format!("\x1b[<{button};1;1M").as_bytes());
                } else {
                    bytes.extend_from_slice(&[0x1b, b'[', b'M', 32 + button, 33, 33]);
                }
            }
            self.write(bytes);
        } else if mode.contains(TermMode::ALT_SCREEN) {
            let arrow: &[u8] = match (whole > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            drop(term);
            self.write(arrow.repeat(whole.unsigned_abs() as usize));
        } else {
            term.scroll_display(Scroll::Delta(whole));
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Drop for ChatTerminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

impl Focusable for ChatTerminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let term = self.term.clone();
        let sender = self.sender.clone();
        let grid = self.grid.clone();
        let layout = self.layout.clone();
        div()
            .id("chat-terminal")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .cursor_text()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(
                div().flex_1().min_h_0().p_2().child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, cx| {
                            paint_terminal(&term, &sender, &grid, &layout, bounds, window, cx)
                        },
                    )
                    .size_full(),
                ),
            )
            .when(self.exited(), |root| {
                root.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(theme::border())
                        .text_xs()
                        .text_color(theme::muted())
                        .child("claude exited — close this panel with ×"),
                )
            })
    }
}

// One uniform stretch of cells: the shaping input for a TextRun.
struct RunKey {
    color: Hsla,
    background: Option<Hsla>,
    weight: FontWeight,
    italic: bool,
    underline: bool,
}

fn paint_terminal(
    term: &Arc<FairMutex<Term<EventProxy>>>,
    sender: &EventLoopSender,
    grid: &Arc<Mutex<(usize, usize)>>,
    layout: &Arc<Mutex<Option<(Bounds<Pixels>, Pixels)>>>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let base_font = font(FONT_FAMILY);
    let font_size = px(FONT_SIZE);
    let line_height = px(LINE_HEIGHT);
    let probe = TextRun {
        len: 1,
        font: base_font.clone(),
        color: theme::text(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let cell_width = window
        .text_system()
        .shape_line("M".into(), font_size, std::slice::from_ref(&probe), None)
        .width;
    if f32::from(cell_width) <= 0. {
        return;
    }
    *layout.lock().unwrap() = Some((bounds, cell_width));
    let cols = ((f32::from(bounds.size.width) / f32::from(cell_width)) as usize).max(2);
    let rows = ((f32::from(bounds.size.height) / f32::from(line_height)) as usize).max(2);
    {
        let mut last = grid.lock().unwrap();
        if *last != (cols, rows) {
            *last = (cols, rows);
            term.lock().resize(TermSize::new(cols, rows));
            let _ = sender.send(Msg::Resize(WindowSize {
                num_cols: cols as u16,
                num_lines: rows as u16,
                cell_width: f32::from(cell_width) as u16,
                cell_height: f32::from(line_height) as u16,
            }));
        }
    }

    // Copy the visible grid out under the lock, then shape and paint without
    // stalling the PTY reader thread.
    let mut lines: Vec<(String, Vec<TextRun>, Vec<RunKey>)> = Vec::new();
    lines.resize_with(rows, || (String::new(), Vec::new(), Vec::new()));
    {
        let term = term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let cursor = content.cursor;
        let colors = content.colors;
        let selection = content.selection;
        for cell in content.display_iter {
            let row = cell.point.line.0 + offset;
            if row < 0 || row >= rows as i32 {
                continue;
            }
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut fg = fg_color(cell.fg, colors);
            let mut bg = bg_color(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                let swapped_bg = Some(fg);
                fg = bg.unwrap_or_else(theme::background);
                bg = swapped_bg;
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.opacity(0.6);
            }
            if selection.is_some_and(|range| range.contains(cell.point)) {
                bg = Some(theme::accent().opacity(0.35));
            }
            let at_cursor = cursor.shape != CursorShape::Hidden && cell.point == cursor.point;
            if at_cursor {
                bg = Some(theme::accent().opacity(0.85));
                fg = theme::background();
            }
            let key = RunKey {
                color: fg,
                background: bg,
                weight: if cell.flags.contains(Flags::BOLD) {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                },
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            };
            let character = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            let (text, runs, keys) = &mut lines[row as usize];
            text.push(character);
            let mut added = character.len_utf8();
            if let Some(zerowidth) = cell.zerowidth() {
                for extra in zerowidth {
                    text.push(*extra);
                    added += extra.len_utf8();
                }
            }
            match keys.last() {
                Some(last)
                    if last.color == key.color
                        && last.background == key.background
                        && last.weight == key.weight
                        && last.italic == key.italic
                        && last.underline == key.underline =>
                {
                    runs.last_mut().unwrap().len += added;
                }
                _ => {
                    let mut run_font = base_font.clone();
                    run_font.weight = key.weight;
                    if key.italic {
                        run_font.style = FontStyle::Italic;
                    }
                    runs.push(TextRun {
                        len: added,
                        font: run_font,
                        color: key.color,
                        background_color: key.background,
                        underline: key.underline.then(|| UnderlineStyle {
                            thickness: px(1.),
                            color: Some(key.color),
                            wavy: false,
                        }),
                        strikethrough: None,
                    });
                    keys.push(key);
                }
            }
        }
    }

    for (row, (text, runs, _)) in lines.into_iter().enumerate() {
        if text.trim().is_empty() && runs.iter().all(|run| run.background_color.is_none()) {
            continue;
        }
        let origin = bounds.origin + point(px(0.), line_height * row as f32);
        let shaped = window
            .text_system()
            .shape_line(text.into(), font_size, &runs, None);
        let _ = shaped.paint_background(origin, line_height, window, cx);
        let _ = shaped.paint(origin, line_height, window, cx);
    }
}

fn key_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let modifiers = keystroke.modifiers;
    // xterm-style modifier parameter for cursor keys: shift 1, alt 2, ctrl 4.
    let parameter = 1
        + modifiers.shift as u8
        + (modifiers.alt as u8) * 2
        + (modifiers.control as u8) * 4;
    let cursor = |letter: char| {
        if parameter > 1 {
            format!("\x1b[1;{parameter}{letter}").into_bytes()
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let bytes = match keystroke.key.as_str() {
        // A bare terminal cannot distinguish modified enter from enter (both
        // are \r), so every modified enter becomes meta-enter — the encoding
        // TUIs like Claude Code read as "insert newline, don't submit".
        "enter" if modifiers.alt || modifiers.control || modifiers.shift => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "backspace" if modifiers.alt => b"\x1b\x7f".to_vec(),
        "backspace" => vec![0x7f],
        "tab" if modifiers.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        key if modifiers.control => {
            let character = key.chars().next()?;
            let byte = match character {
                'a'..='z' => character as u8 - b'a' + 1,
                '@' | ' ' => 0,
                '[' => 27,
                '\\' => 28,
                ']' => 29,
                '^' => 30,
                '_' | '-' => 31,
                _ => return None,
            };
            vec![byte]
        }
        _ => {
            let text = keystroke.key_char.as_deref().filter(|text| !text.is_empty())?;
            let mut bytes = Vec::new();
            if modifiers.alt {
                bytes.push(0x1b);
            }
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
    };
    Some(bytes)
}

fn fg_color(color: AnsiColor, colors: &alacritty_terminal::term::color::Colors) -> Hsla {
    match color {
        AnsiColor::Spec(rgb) => hsla_from(rgb),
        AnsiColor::Named(named) => colors[named]
            .map(hsla_from)
            .unwrap_or_else(|| named_default(named)),
        AnsiColor::Indexed(index) => colors[index as usize]
            .map(hsla_from)
            .unwrap_or_else(|| indexed_default(index)),
    }
}

// None means "the terminal's own background": nothing is painted, so the
// panel background shows through.
fn bg_color(color: AnsiColor, colors: &alacritty_terminal::term::color::Colors) -> Option<Hsla> {
    match color {
        AnsiColor::Named(NamedColor::Background) => None,
        other => Some(fg_color(other, colors)),
    }
}

fn hsla_from(rgb: AnsiRgb) -> Hsla {
    gpui::rgb(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32).into()
}

fn rgb(value: u32) -> Hsla {
    gpui::rgb(value).into()
}

// The 16-color palette, tuned to sit on the app's dark theme.
fn ansi16(index: u8) -> Hsla {
    match index {
        0 => rgb(0x1a2132),
        1 => rgb(0xff647c),
        2 => rgb(0x71d9a6),
        3 => rgb(0xe4c76a),
        4 => rgb(0x70a5ff),
        5 => rgb(0x9b8cff),
        6 => rgb(0x68c7c1),
        7 => rgb(0xd7dce8),
        8 => rgb(0x5a6478),
        9 => rgb(0xff8598),
        10 => rgb(0x8ce8bb),
        11 => rgb(0xf2d98a),
        12 => rgb(0x8fb8ff),
        13 => rgb(0xb3a7ff),
        14 => rgb(0x86d9d4),
        _ => rgb(0xf2f4f9),
    }
}

fn named_default(named: NamedColor) -> Hsla {
    match named {
        NamedColor::Foreground | NamedColor::BrightForeground => theme::text(),
        NamedColor::DimForeground => theme::text().opacity(0.6),
        NamedColor::Background => theme::background(),
        NamedColor::Cursor => theme::accent(),
        NamedColor::Black => ansi16(0),
        NamedColor::Red => ansi16(1),
        NamedColor::Green => ansi16(2),
        NamedColor::Yellow => ansi16(3),
        NamedColor::Blue => ansi16(4),
        NamedColor::Magenta => ansi16(5),
        NamedColor::Cyan => ansi16(6),
        NamedColor::White => ansi16(7),
        NamedColor::BrightBlack => ansi16(8),
        NamedColor::BrightRed => ansi16(9),
        NamedColor::BrightGreen => ansi16(10),
        NamedColor::BrightYellow => ansi16(11),
        NamedColor::BrightBlue => ansi16(12),
        NamedColor::BrightMagenta => ansi16(13),
        NamedColor::BrightCyan => ansi16(14),
        NamedColor::BrightWhite => ansi16(15),
        NamedColor::DimBlack => ansi16(0).opacity(0.6),
        NamedColor::DimRed => ansi16(1).opacity(0.6),
        NamedColor::DimGreen => ansi16(2).opacity(0.6),
        NamedColor::DimYellow => ansi16(3).opacity(0.6),
        NamedColor::DimBlue => ansi16(4).opacity(0.6),
        NamedColor::DimMagenta => ansi16(5).opacity(0.6),
        NamedColor::DimCyan => ansi16(6).opacity(0.6),
        NamedColor::DimWhite => ansi16(7).opacity(0.6),
    }
}

fn indexed_default(index: u8) -> Hsla {
    match index {
        0..=15 => ansi16(index),
        16..=231 => {
            let value = index - 16;
            let level = |component: u8| -> u32 {
                if component == 0 { 0 } else { 55 + 40 * component as u32 }
            };
            let (r, g, b) = (value / 36, (value / 6) % 6, value % 6);
            rgb((level(r) << 16) | (level(g) << 8) | level(b))
        }
        _ => {
            let gray = 8 + 10 * (index as u32 - 232);
            rgb((gray << 16) | (gray << 8) | gray)
        }
    }
}

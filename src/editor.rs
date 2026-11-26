//! A multi-line text editor view whose right-hand column shows the result of
//! each line, computed live by the engine.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, ContentMask, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, Font, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, PathPromptOptions, Pixels, Point, Render,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, font, point, prelude::*, px, relative, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::ai;
use crate::engine::{self, Hl, LineOutput, Plot, Value};

actions!(
    editor,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        PageUp,
        PageDown,
        SelectAll,
        Newline,
        Tab,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        CopyWithResults,
        NewSheet,
        Open,
        Save,
        SaveAs,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        CancelCompletion,
        AskAi,
    ]
);

// ---------------------------------------------------------------- theme ---

pub const FONT_FAMILY: &str = "Segoe UI";
const BASE_FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT_RATIO: f32 = 1.75;
const PAD_TOP: f32 = 14.0;
const PAD_X: f32 = 18.0;
const GUTTER: f32 = 34.0;
const HL_SLIDE_SECS: f32 = 0.14;
const HL_FADE_SECS: f32 = 0.28;
const MAX_COMPLETIONS: usize = 6;

pub mod theme {
    use gpui::{Rgba, rgb};
    pub fn bg() -> Rgba {
        rgb(0x1b1c20)
    }
    pub fn results_bg() -> Rgba {
        rgb(0x202126)
    }
    pub fn text() -> Rgba {
        rgb(0xe4e4e7)
    }
    pub fn muted() -> Rgba {
        rgb(0x6b6f7a)
    }
    pub fn faint() -> Rgba {
        rgb(0x44474f)
    }
    pub fn divider() -> Rgba {
        rgb(0x2c2e34)
    }
    pub fn current_line() -> Rgba {
        rgb(0x23252b)
    }
    pub fn accent() -> Rgba {
        rgb(0x5fd4a0)
    }
    pub fn accent_hover() -> Rgba {
        rgb(0x9af0c6)
    }
    pub fn number() -> Rgba {
        rgb(0xf2c27b)
    }
    pub fn unit() -> Rgba {
        rgb(0x7cc7ff)
    }
    pub fn operator() -> Rgba {
        rgb(0x9aa0ab)
    }
    pub fn keyword() -> Rgba {
        rgb(0xc59bf2)
    }
    pub fn variable() -> Rgba {
        rgb(0xff9e8a)
    }
    pub fn function() -> Rgba {
        rgb(0x82aaff)
    }
    pub fn comment() -> Rgba {
        rgb(0x5f6370)
    }
    pub fn ai() -> Rgba {
        rgb(0xb8a6ff)
    }
    pub fn error() -> Rgba {
        rgb(0xff8a80)
    }
    pub fn label() -> Rgba {
        rgb(0xa1a6b3)
    }
}

fn hl_color(hl: Option<Hl>) -> Hsla {
    match hl {
        None => theme::text(),
        Some(Hl::Number) => theme::number(),
        Some(Hl::Unit) => theme::unit(),
        Some(Hl::Operator) => theme::operator(),
        Some(Hl::Keyword) => theme::keyword(),
        Some(Hl::Variable) => theme::variable(),
        Some(Hl::Function) => theme::function(),
        Some(Hl::Comment) => theme::comment(),
        Some(Hl::Label) => theme::label(),
        Some(Hl::Ai) => theme::ai(),
    }
    .into()
}

// --------------------------------------------------------------- editor ---

#[derive(Clone)]
struct Snapshot {
    content: String,
    selection: Range<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

/// Geometry from the last frame, used for hit-testing.
#[derive(Clone, Copy)]
struct Metrics {
    bounds: Bounds<Pixels>,
    text_left: Pixels,
    top: Pixels,
    line_height: Pixels,
    max_scroll: Pixels,
}

/// Variable suggestions for the name being typed at the cursor.
struct Completion {
    /// Byte range of the partly typed name.
    range: Range<usize>,
    /// (name as defined, its current value).
    items: Vec<(String, Option<String>)>,
    selected: usize,
    /// The typed text already is a complete name; Enter then keeps its
    /// newline meaning unless another item was picked.
    exact: bool,
    /// Length (in chars) of the typed prefix, for highlighting.
    typed_chars: usize,
}

pub struct Editor {
    focus_handle: FocusHandle,
    content: String,
    line_starts: Vec<usize>,
    outputs: Vec<LineOutput>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    goal_x: Option<Pixels>,
    scroll_y: Pixels,
    autoscroll: bool,
    zoom: f32,

    metrics: Option<Metrics>,
    result_boxes: Vec<(Bounds<Pixels>, usize)>,
    hovered_result: Option<usize>,
    completion: Option<Completion>,
    completion_boxes: Vec<(Bounds<Pixels>, usize)>,
    /// Claude's answers by normalised question, plus in-flight and failed asks.
    ai_cache: HashMap<String, ai::Answer>,
    ai_pending: HashSet<String>,
    ai_errors: HashMap<String, String>,
    ai_model: String,
    /// Cursor line at the last render; leaving a `?` line asks Claude.
    last_line: usize,

    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    last_edit: Option<(EditKind, Instant)>,

    blink_on: bool,
    last_activity: Instant,
    /// Animated position (in lines) of the current-line highlight.
    hl_from: f32,
    hl_to: f32,
    hl_start: Instant,
    flash: Option<(usize, Instant)>,
    status: Option<(SharedString, Instant)>,

    path: PathBuf,
    save_task: Option<Task<()>>,
    _blink_task: Task<()>,
}

use crate::data_dir;

const WELCOME: &str = include_str!("welcome.txt");
/// Remembers which sheet was open (and the cursor) between launches.
const LAST_SHEET_FILE: &str = "last-sheet.txt";

/// The sheet that was open when Tallypad last saved, if it still exists.
fn last_sheet() -> Option<(PathBuf, usize)> {
    let state = std::fs::read_to_string(data_dir().join(LAST_SHEET_FILE)).ok()?;
    let mut lines = state.lines();
    let path = PathBuf::from(lines.next()?.trim());
    let cursor = lines
        .next()
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);
    path.is_file().then_some((path, cursor))
}

impl Editor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (path, cursor) = last_sheet().unwrap_or_else(|| (data_dir().join("sheet.txt"), 0));
        let content = std::fs::read_to_string(&path)
            .map(|s| s.replace("\r\n", "\n"))
            .unwrap_or_else(|_| WELCOME.to_string());

        let blink_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                let alive = this.update(cx, |editor, cx| {
                    let idle = editor.last_activity.elapsed() > Duration::from_millis(500);
                    editor.blink_on = !idle || !editor.blink_on;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let mut editor = Editor {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            line_starts: vec![0],
            outputs: Vec::new(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            goal_x: None,
            scroll_y: px(0.),
            autoscroll: false,
            zoom: 1.0,
            metrics: None,
            result_boxes: Vec::new(),
            hovered_result: None,
            completion: None,
            completion_boxes: Vec::new(),
            ai_cache: ai::load_cache(),
            ai_pending: HashSet::new(),
            ai_errors: HashMap::new(),
            ai_model: ai::load_model(),
            last_line: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            blink_on: true,
            last_activity: Instant::now(),
            hl_from: 0.,
            hl_to: 0.,
            hl_start: Instant::now(),
            flash: None,
            status: None,
            path,
            save_task: None,
            _blink_task: blink_task,
        };
        editor.set_content(content);
        let mut cursor = cursor.min(editor.content.len());
        while !editor.content.is_char_boundary(cursor) {
            cursor -= 1;
        }
        editor.selected_range = cursor..cursor;
        editor.autoscroll = true;
        editor.last_line = editor.line_of(cursor);
        editor
    }

    // ------------------------------------------------------- text model ---

    fn set_content(&mut self, content: String) {
        self.content = content;
        self.recompute();
        let end = self.content.len();
        self.selected_range = end.min(self.selected_range.start)..end.min(self.selected_range.end);
    }

    fn recompute(&mut self) {
        self.line_starts = std::iter::once(0)
            .chain(self.content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let cache = &self.ai_cache;
        self.outputs = engine::evaluate_document_with(&self.content, &|q| {
            cache
                .get(&ai::cache_key(q))
                .map(|a| (a.expression.clone(), a.answer.clone()))
        });
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_starts[line];
        let end = self
            .line_starts
            .get(line + 1)
            .map(|s| s - 1)
            .unwrap_or(self.content.len());
        start..end
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn font(&self) -> Font {
        font(FONT_FAMILY)
    }

    fn font_size(&self) -> Pixels {
        px(BASE_FONT_SIZE * self.zoom)
    }

    fn line_height(&self) -> Pixels {
        px((BASE_FONT_SIZE * self.zoom * LINE_HEIGHT_RATIO).round())
    }

    fn shape_plain(&self, text: &str, window: &mut Window) -> ShapedLine {
        let run = TextRun {
            len: text.len(),
            font: self.font(),
            color: theme::text().into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if text.is_empty() { vec![] } else { vec![run] };
        window.text_system().shape_line(
            SharedString::from(text.to_string()),
            self.font_size(),
            &runs,
            None,
        )
    }

    fn x_for_offset(&self, offset: usize, window: &mut Window) -> Pixels {
        let line = self.line_of(offset);
        let range = self.line_range(line);
        let shaped = self.shape_plain(&self.content[range.clone()], window);
        shaped.x_for_index(offset - range.start)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content[..offset]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content[offset..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| offset + i)
            .unwrap_or(self.content.len())
    }

    fn word_left(&self, offset: usize) -> usize {
        let before: Vec<(usize, char)> = self.content[..offset].char_indices().collect();
        let mut i = before.len();
        if i > 0 && before[i - 1].1 == '\n' {
            return before[i - 1].0;
        }
        while i > 0 && before[i - 1].1.is_whitespace() && before[i - 1].1 != '\n' {
            i -= 1;
        }
        if i == 0 {
            return 0;
        }
        let class = char_class(before[i - 1].1);
        while i > 0 && char_class(before[i - 1].1) == class && before[i - 1].1 != '\n' {
            i -= 1;
        }
        before.get(i).map(|(p, _)| *p).unwrap_or(offset)
    }

    fn word_right(&self, offset: usize) -> usize {
        let mut iter = self.content[offset..].char_indices().peekable();
        if let Some((_, '\n')) = iter.peek() {
            return offset + 1;
        }
        while let Some((_, c)) = iter.peek() {
            if c.is_whitespace() && *c != '\n' {
                iter.next();
            } else {
                break;
            }
        }
        let Some((_, first)) = iter.peek().copied() else {
            return self.content.len();
        };
        let class = char_class(first);
        while let Some((i, c)) = iter.peek().copied() {
            if char_class(c) != class || c == '\n' {
                return offset + i;
            }
            iter.next();
        }
        self.content.len()
    }

    fn word_range_at(&self, offset: usize) -> Range<usize> {
        let line = self.line_range(self.line_of(offset));
        let text = &self.content[line.clone()];
        for (start, word) in text.split_word_bound_indices() {
            let s = line.start + start;
            let e = s + word.len();
            if offset >= s && offset <= e && !word.trim().is_empty() {
                return s..e;
            }
        }
        offset..offset
    }

    // ------------------------------------------------------- selection ---

    fn touch(&mut self) {
        self.last_activity = Instant::now();
        self.blink_on = true;
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.completion = None;
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.goal_x = None;
        self.autoscroll = true;
        self.touch();
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.completion = None;
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.goal_x = None;
        self.autoscroll = true;
        self.touch();
        cx.notify();
    }

    fn vertical_target(&mut self, delta: isize, window: &mut Window) -> (usize, Pixels) {
        let cursor = self.cursor_offset();
        let line = self.line_of(cursor);
        let goal = match self.goal_x {
            Some(x) => x,
            None => self.x_for_offset(cursor, window),
        };
        let target = line as isize + delta;
        let offset = if target < 0 {
            0
        } else if target as usize >= self.line_count() {
            self.content.len()
        } else {
            let range = self.line_range(target as usize);
            let shaped = self.shape_plain(&self.content[range.clone()], window);
            range.start + shaped.closest_index_for_x(goal).min(range.len())
        };
        (offset, goal)
    }

    fn move_vertical(
        &mut self,
        delta: isize,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !select && !self.selected_range.is_empty() && delta.abs() == 1 {
            let edge = if delta < 0 {
                self.selected_range.start
            } else {
                self.selected_range.end
            };
            self.selected_range = edge..edge;
            self.selection_reversed = false;
        }
        let (offset, goal) = self.vertical_target(delta, window);
        if select {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        self.goal_x = Some(goal);
    }

    fn page_lines(&self) -> isize {
        self.metrics
            .map(|m| ((m.bounds.size.height / m.line_height) as isize - 2).max(1))
            .unwrap_or(20)
    }

    // --------------------------------------------------------- editing ---

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selection: self.selected_range.clone(),
        }
    }

    fn record_undo(&mut self, kind: EditKind) {
        let coalesce = match self.last_edit {
            Some((last, at)) => {
                last == kind && kind != EditKind::Other && at.elapsed() < Duration::from_millis(900)
            }
            None => false,
        };
        if !coalesce {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > 500 {
                self.undo_stack.remove(0);
            }
        }
        self.redo_stack.clear();
        self.last_edit = Some((kind, Instant::now()));
    }

    fn replace(&mut self, range: Range<usize>, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        self.record_undo(kind);
        self.content.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        let was_open = self.completion.is_some();
        self.after_edit(cx);
        if kind == EditKind::Typing || (kind == EditKind::Deleting && was_open) {
            self.refresh_completion();
        } else {
            self.completion = None;
        }
    }

    // -------------------------------------------------------------- AI ---

    pub fn ai_model(&self) -> &str {
        &self.ai_model
    }

    pub fn set_ai_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.ai_model = model.to_string();
        ai::save_model(model);
        let name = ai::MODELS
            .iter()
            .find(|m| m.0 == model)
            .map(|m| m.2)
            .unwrap_or(model);
        self.set_status(format!("Questions now go to {name}"), cx);
    }

    /// Re-asks questions that failed (e.g. after adding an API key).
    pub fn retry_ai(&mut self, cx: &mut Context<Self>) {
        let failed: Vec<usize> = (0..self.outputs.len())
            .filter(|&i| {
                self.outputs[i]
                    .question
                    .as_ref()
                    .is_some_and(|q| self.ai_errors.contains_key(&ai::cache_key(q)))
            })
            .collect();
        self.ai_errors.clear();
        for line in failed {
            self.ask_ai(line, false, cx);
        }
        cx.notify();
    }

    /// The lines above `line`, with their results, as context for Claude.
    fn ai_context(&self, line: usize) -> String {
        let mut rows: Vec<String> = Vec::new();
        let mut size = 0;
        for i in (0..line).rev() {
            let text = self.content[self.line_range(i)].trim();
            if text.is_empty() {
                continue;
            }
            let row = match self.outputs.get(i).and_then(|o| o.display.as_ref()) {
                Some(d) => format!("{text} = {d}"),
                None => text.to_string(),
            };
            size += row.len();
            if size > 6000 || rows.len() >= 80 {
                break;
            }
            rows.push(row);
        }
        rows.reverse();
        rows.join("\n")
    }

    fn ask_ai(&mut self, line: usize, force: bool, cx: &mut Context<Self>) {
        let Some(question) = self.outputs.get(line).and_then(|o| o.question.clone()) else {
            return;
        };
        let key = ai::cache_key(&question);
        if self.ai_pending.contains(&key) || (!force && self.ai_cache.contains_key(&key)) {
            return;
        }
        let Some(api_key) = ai::api_key() else {
            self.ai_errors
                .insert(key, "add an API key in the AI menu".into());
            cx.notify();
            return;
        };
        let context = self.ai_context(line);
        let model = self.ai_model.clone();
        self.ai_errors.remove(&key);
        self.ai_pending.insert(key.clone());
        let task = cx
            .background_executor()
            .spawn(async move { ai::ask(&api_key, &model, &question, &context) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |editor, cx| {
                editor.ai_pending.remove(&key);
                match result {
                    Ok(answer) => {
                        editor.ai_cache.insert(key, answer);
                        ai::save_cache(&editor.ai_cache);
                        editor.recompute();
                    }
                    Err(err) => {
                        editor.ai_errors.insert(key, err);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Ctrl+Enter: re-asks a `?` line, or turns any other line into one
    /// ("20 added 30" -> "? 20 added 30") and asks Claude.
    fn ask_again(&mut self, _: &AskAi, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_of(self.cursor_offset());
        if self.outputs.get(line).is_some_and(|o| o.question.is_some()) {
            self.ask_ai(line, true, cx);
            return;
        }
        let range = self.line_range(line);
        let text = &self.content[range.clone()];
        if text.trim().is_empty() {
            self.set_status("Type a question, then press Ctrl+Enter", cx);
            return;
        }
        let indent = text.len() - text.trim_start().len();
        let at = range.start + indent;
        let cursor = self.cursor_offset() + 2;
        self.replace(at..at, "? ", EditKind::Other, cx);
        self.selected_range = cursor..cursor;
        self.ask_ai(line, true, cx);
    }

    /// Results-column text for a line without a value: Claude's status or a
    /// non-numeric answer.
    fn line_note(&self, output: &LineOutput, is_cursor_line: bool) -> Option<(String, Hsla)> {
        if let Some(note) = &output.note {
            let color = if output.plot.is_some() {
                theme::faint()
            } else {
                theme::accent()
            };
            return Some((note.clone(), color.into()));
        }
        if output.unclear && output.question.is_none() {
            return Some((
                "not sure · Ctrl+Enter asks Claude".into(),
                theme::faint().into(),
            ));
        }
        let question = output.question.as_ref()?;
        let key = ai::cache_key(question);
        if self.ai_pending.contains(&key) {
            return Some((
                format!("asking {}…", ai::short_name(&self.ai_model)),
                theme::ai().into(),
            ));
        }
        if let Some(err) = self.ai_errors.get(&key) {
            return Some((format!("⚠ {err}"), theme::error().into()));
        }
        if !self.ai_cache.contains_key(&key) {
            let hint = if is_cursor_line {
                "↵ ask Claude"
            } else {
                "Ctrl+Enter to ask"
            };
            return Some((hint.into(), theme::faint().into()));
        }
        None
    }

    /// What the graph panel shows for the cursor's position.
    pub fn graph(&self) -> Graph {
        let mut line = self.line_of(self.cursor_offset());
        if let Some(plot) = self.outputs.get(line).and_then(|o| o.plot.clone()) {
            return Graph::Plot(plot);
        }
        let is_break = |i: usize| {
            let t = self.content[self.line_range(i)].trim();
            t.is_empty() || t.starts_with('#')
        };
        // On a blank line right under a block, chart the block above.
        if is_break(line) && line > 0 && !is_break(line - 1) {
            line -= 1;
            if let Some(plot) = self.outputs.get(line).and_then(|o| o.plot.clone()) {
                return Graph::Plot(plot);
            }
        }
        if is_break(line) {
            return Graph::Empty;
        }
        let mut start = line;
        while start > 0 && !is_break(start - 1) {
            start -= 1;
        }
        let mut end = line;
        while end + 1 < self.line_count() && !is_break(end + 1) {
            end += 1;
        }
        let bars: Vec<(String, f64, String)> = (start..=end)
            .filter_map(|i| {
                let o = self.outputs.get(i)?;
                if o.summary {
                    return None;
                }
                let value = o.value.as_ref()?;
                let display = o.display.clone()?;
                let text = self.content[self.line_range(i)].trim();
                let label = match (&o.defines, text.split_once(':')) {
                    (Some(name), _) => name.as_str(),
                    (None, Some((l, _))) if !l.trim().is_empty() => l.trim(),
                    _ => text.trim_start_matches('?').trim(),
                };
                let label: String = if label.chars().count() > 18 {
                    label.chars().take(17).chain(std::iter::once('…')).collect()
                } else {
                    label.to_string()
                };
                Some((label, value.shown(), display))
            })
            .collect();
        if bars.is_empty() {
            Graph::Empty
        } else {
            Graph::Bars(bars)
        }
    }

    // ------------------------------------------------------ completion ---

    /// Suggests variables defined above the cursor whose names start with
    /// the word(s) just typed.
    fn refresh_completion(&mut self) {
        self.completion = None;
        if !self.selected_range.is_empty() || self.marked_range.is_some() {
            return;
        }
        let cursor = self.cursor_offset();
        let line = self.line_of(cursor);
        let start = self.line_starts[line];
        // Later definitions win; keep the most recent value.
        let mut vars: Vec<(String, Option<String>)> = Vec::new();
        for output in self.outputs.iter().take(line) {
            if let Some(name) = &output.defines {
                let key = name.to_lowercase();
                vars.retain(|(n, _)| n.to_lowercase() != key);
                vars.push((name.clone(), output.display.clone()));
            }
        }
        if vars.is_empty() {
            return;
        }
        let names: Vec<&str> = vars.iter().map(|(n, _)| n.as_str()).collect();
        if let Some(m) = match_names(&self.content[start..cursor], &names) {
            self.completion = Some(Completion {
                range: start + m.start..cursor,
                items: m.items.iter().map(|&i| vars[i].clone()).collect(),
                selected: 0,
                exact: m.exact,
                typed_chars: self.content[start + m.start..cursor].chars().count(),
            });
        }
    }

    fn accept_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(c) = self.completion.take() else {
            return false;
        };
        let name = c.items[c.selected].0.clone();
        self.replace(c.range, &name, EditKind::Other, cx);
        true
    }

    fn move_completion(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(c) = self.completion.as_mut() else {
            return false;
        };
        let n = c.items.len() as isize;
        c.selected = (c.selected as isize + delta).rem_euclid(n) as usize;
        self.touch();
        cx.notify();
        true
    }

    fn cancel_completion(&mut self, _: &CancelCompletion, _: &mut Window, cx: &mut Context<Self>) {
        if self.completion.take().is_some() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.recompute();
        self.goal_x = None;
        self.autoscroll = true;
        self.touch();
        self.schedule_save(cx);
        cx.notify();
    }

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let kind = if text.chars().all(|c| !c.is_whitespace()) || text == " " {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        self.replace(self.selected_range.clone(), text, kind, cx);
    }

    fn delete_selection_or(&mut self, target: usize, cx: &mut Context<Self>) {
        let range = if self.selected_range.is_empty() {
            let c = self.cursor_offset();
            c.min(target)..c.max(target)
        } else {
            self.selected_range.clone()
        };
        self.replace(range, "", EditKind::Deleting, cx);
    }

    fn restore(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        self.content = snap.content;
        self.selected_range = snap.selection;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_edit = None;
        self.completion = None;
        self.after_edit(cx);
    }

    // ----------------------------------------------------------- files ---

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            this.update(cx, |editor, _| editor.save_now()).ok();
        }));
    }

    pub fn save_now(&mut self) -> bool {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let ok = std::fs::write(&self.path, &self.content).is_ok();
        if ok {
            let state = format!("{}\n{}\n", self.path.display(), self.cursor_offset());
            let _ = std::fs::write(data_dir().join(LAST_SHEET_FILE), state);
        }
        ok
    }

    /// Puts the cursor at the end of `line` (0-based).
    pub fn move_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        let line = line.min(self.line_count() - 1);
        let end = self.line_range(line).end;
        self.move_to(end, cx);
    }

    /// Re-evaluates every line (e.g. after exchange rates change).
    pub fn refresh_results(&mut self, cx: &mut Context<Self>) {
        self.recompute();
        cx.notify();
    }

    /// Transient status message, if one is still showing.
    pub fn status(&self) -> Option<SharedString> {
        self.status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(2))
            .map(|(s, _)| s.clone())
    }

    pub fn summary(&self) -> String {
        format!("{}  ·  {} lines", self.file_name(), self.line_count())
    }

    pub fn set_status(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Some((text.into(), Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2100))
                .await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn load_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.save_now();
                self.record_undo(EditKind::Other);
                self.path = path;
                self.selected_range = 0..0;
                self.scroll_y = px(0.);
                self.set_content(text.replace("\r\n", "\n"));
                self.after_edit(cx);
                self.set_status("Opened", cx);
            }
            Err(err) => self.set_status(format!("Couldn't open file: {err}"), cx),
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sheet.txt".into())
    }

    // ----------------------------------------------------- action glue ---

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.previous_boundary(self.cursor_offset());
        self.delete_selection_or(target, cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.next_boundary(self.cursor_offset());
        self.delete_selection_or(target, cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.word_left(self.cursor_offset());
        self.delete_selection_or(target, cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.word_right(self.cursor_offset());
        self.delete_selection_or(target, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_completion(-1, cx) {
            return;
        }
        self.move_vertical(-1, false, window, cx);
    }

    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_completion(1, cx) {
            return;
        }
        self.move_vertical(1, false, window, cx);
    }

    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-self.page_lines(), false, window, cx);
    }

    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(self.page_lines(), false, window, cx);
    }

    fn word_left_action(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_left(self.cursor_offset()), cx);
    }

    fn word_right_action(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_right(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, true, window, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_left(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_right(self.cursor_offset()), cx);
    }

    fn smart_home(&self) -> usize {
        let range = self.line_range(self.line_of(self.cursor_offset()));
        let text = &self.content[range.clone()];
        let indent = text.len() - text.trim_start().len();
        let first = range.start + indent;
        if self.cursor_offset() == first {
            range.start
        } else {
            first
        }
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.smart_home(), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.line_range(self.line_of(self.cursor_offset())).end;
        self.move_to(end, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.smart_home(), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.line_range(self.line_of(self.cursor_offset())).end;
        self.select_to(end, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.completion = None;
        self.touch();
        cx.notify();
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &self.completion {
            // "hours" typed in full: Enter is a newline unless the user
            // arrowed to a longer name like "hours worked".
            if !(c.exact && c.selected == 0) && self.accept_completion(cx) {
                return;
            }
            self.completion = None;
        }
        // Keep the indentation of the current line.
        let range = self.line_range(self.line_of(self.cursor_offset()));
        let text = &self.content[range];
        let indent: String = text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.insert(&format!("\n{indent}"), cx);
    }

    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.accept_completion(cx) {
            return;
        }
        self.insert("    ", cx);
    }

    fn selected_or_line(&self) -> (Range<usize>, String) {
        if self.selected_range.is_empty() {
            let line = self.line_of(self.cursor_offset());
            let range = self.line_range(line);
            let end = (range.end + 1).min(self.content.len());
            let mut text = self.content[range.start..end].to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            (range.start..end, text)
        } else {
            let r = self.selected_range.clone();
            (r.clone(), self.content[r].to_string())
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let (_, text) = self.selected_or_line();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let (range, text) = self.selected_or_line();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.replace(range, "", EditKind::Other, cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace(self.selected_range.clone(), &text, EditKind::Other, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snap) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(snap, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snap) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(snap, cx);
        }
    }

    fn copy_with_results(&mut self, _: &CopyWithResults, _: &mut Window, cx: &mut Context<Self>) {
        let lines: Vec<&str> = self.content.split('\n').collect();
        let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut out = String::new();
        for (line, output) in lines.iter().zip(&self.outputs) {
            match &output.display {
                Some(result) => {
                    let pad = width - line.chars().count();
                    out.push_str(&format!("{line}{}  = {result}\n", " ".repeat(pad)));
                }
                None => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        cx.write_to_clipboard(ClipboardItem::new_string(out));
        self.set_status("Copied sheet with results", cx);
    }

    fn new_sheet(&mut self, _: &NewSheet, _: &mut Window, cx: &mut Context<Self>) {
        self.save_now();
        self.record_undo(EditKind::Other);
        let mut n = 1;
        let dir = data_dir();
        while dir.join(format!("sheet-{n}.txt")).exists() {
            n += 1;
        }
        self.path = dir.join(format!("sheet-{n}.txt"));
        self.content.clear();
        self.selected_range = 0..0;
        self.scroll_y = px(0.);
        self.after_edit(cx);
        self.set_status("New sheet", cx);
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open sheet".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |editor, cx| editor.load_file(path, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.save_now() {
            self.set_status(format!("Saved {}", self.path.display()), cx);
        } else {
            self.set_status("Save failed", cx);
        }
    }

    fn save_as(&mut self, _: &SaveAs, _: &mut Window, cx: &mut Context<Self>) {
        let dir = self
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(data_dir);
        let rx = cx.prompt_for_new_path(&dir, Some(&self.file_name()));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                this.update(cx, |editor, cx| {
                    editor.path = path;
                    if editor.save_now() {
                        editor.set_status(format!("Saved {}", editor.path.display()), cx);
                    } else {
                        editor.set_status("Save failed", cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = (self.zoom + 0.1).min(2.5);
        cx.notify();
    }

    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = (self.zoom - 0.1).max(0.6);
        cx.notify();
    }

    fn zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        cx.notify();
    }

    // ----------------------------------------------------------- mouse ---

    fn result_at(&self, position: Point<Pixels>) -> Option<usize> {
        self.result_boxes
            .iter()
            .find(|(b, _)| b.contains(&position))
            .map(|(_, line)| *line)
    }

    fn offset_for_position(&self, position: Point<Pixels>, window: &mut Window) -> usize {
        let Some(m) = self.metrics else {
            return 0;
        };
        let y = position.y - m.top + self.scroll_y;
        let line = (y / m.line_height).floor();
        if line < 0. {
            return 0;
        }
        let line = line as usize;
        if line >= self.line_count() {
            return self.content.len();
        }
        let range = self.line_range(line);
        let shaped = self.shape_plain(&self.content[range.clone()], window);
        let x = (position.x - m.text_left).max(px(0.));
        range.start + shaped.closest_index_for_x(x).min(range.len())
    }

    fn copy_result(&mut self, line: usize, cx: &mut Context<Self>) {
        if let Some(text) = self.outputs.get(line).and_then(|o| o.display.clone()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.flash = Some((line, Instant::now()));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1000))
                    .await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
            cx.notify();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if let Some(&(_, i)) = self
            .completion_boxes
            .iter()
            .find(|(b, _)| b.contains(&event.position))
            && let Some(c) = self.completion.as_mut()
            && i < c.items.len()
        {
            c.selected = i;
            self.accept_completion(cx);
            return;
        }
        if let Some(line) = self.result_at(event.position) {
            self.copy_result(line, cx);
            return;
        }
        let offset = self.offset_for_position(event.position, window);
        match event.click_count {
            2 => {
                let word = self.word_range_at(offset);
                self.selected_range = word;
                self.selection_reversed = false;
                self.touch();
                cx.notify();
            }
            3 => {
                let line = self.line_range(self.line_of(offset));
                let end = (line.end + 1).min(self.content.len());
                self.selected_range = line.start..end;
                self.selection_reversed = false;
                self.touch();
                cx.notify();
            }
            _ => {
                self.is_selecting = true;
                if event.modifiers.shift {
                    self.select_to(offset, cx);
                } else {
                    self.move_to(offset, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            let offset = self.offset_for_position(event.position, window);
            self.select_to(offset, cx);
            return;
        }
        let hovered = self.result_at(event.position);
        if hovered != self.hovered_result {
            self.hovered_result = hovered;
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.line_height());
        let max = self.metrics.map(|m| m.max_scroll).unwrap_or(px(0.));
        self.scroll_y = (self.scroll_y - delta.y).max(px(0.)).min(max);
        cx.notify();
    }

    // ---------------------------------------------------------- footer ---

    /// Sum of the selected lines, or of every non-aggregate line.
    pub fn footer_total(&self) -> Option<(&'static str, String)> {
        let (label, lines): (&'static str, Range<usize>) = if self.selected_range.is_empty() {
            ("Total", 0..self.outputs.len())
        } else {
            let a = self.line_of(self.selected_range.start);
            let b = self.line_of(self.selected_range.end);
            if a == b {
                ("Total", 0..self.outputs.len())
            } else {
                ("Selection", a..b + 1)
            }
        };
        let mut acc: Option<Value> = None;
        for output in &self.outputs[lines] {
            let Some(v) = &output.value else { continue };
            if (label == "Total" && output.aggregate) || v.percent {
                continue;
            }
            acc = Some(match acc {
                None => v.clone(),
                Some(a) => a.add(v).unwrap_or(a),
            });
        }
        acc.and_then(|v| v.format().ok()).map(|s| (label, s))
    }
}

/// Data for the graph panel.
pub enum Graph {
    /// A `plot` line under the cursor.
    Plot(Plot),
    /// The values in the cursor's block: (label, number, formatted).
    Bars(Vec<(String, f64, String)>),
    Empty,
}

struct NameMatch {
    /// Byte offset in the typed text where the name starts.
    start: usize,
    /// Indices of matching names, best first.
    items: Vec<usize>,
    /// The typed text is itself one of the names.
    exact: bool,
}

/// Matches the end of `before` (the line up to the cursor) against variable
/// names, case-insensitively. Multi-word names match across spaces, so
/// "hourly r" suggests "hourly rate".
fn match_names(before: &str, names: &[&str]) -> Option<NameMatch> {
    if before.is_empty() || before.ends_with(char::is_whitespace) {
        return None;
    }
    if before.trim_start().starts_with('#')
        || before.contains("//")
        || before.matches('"').count() % 2 == 1
    {
        return None;
    }
    let lower = |s: &str| s.chars().flat_map(char::to_lowercase).collect::<String>();
    let names_lower: Vec<String> = names.iter().map(|n| lower(n)).collect();
    let mut prev: Option<char> = None;
    for (i, c) in before.char_indices() {
        let boundary = c.is_alphabetic() && !prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
        prev = Some(c);
        if !boundary || before.len() - i > 64 {
            continue;
        }
        let typed = lower(&before[i..]);
        let mut items: Vec<usize> = (0..names.len())
            .filter(|&k| names_lower[k].starts_with(&typed))
            .collect();
        if items.is_empty() {
            continue;
        }
        let exact = items.iter().any(|&k| names_lower[k] == typed);
        if exact && items.len() == 1 {
            return None;
        }
        // Exact match first, then shorter names, then alphabetical.
        items.sort_by_key(|&k| {
            (
                names_lower[k] != typed,
                names[k].len(),
                names_lower[k].clone(),
            )
        });
        items.truncate(MAX_COMPLETIONS);
        return Some(NameMatch {
            start: i,
            items,
            exact,
        });
    }
    None
}

fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Builds coloured text runs for a line from engine highlight spans.
fn build_runs(
    len: usize,
    spans: &[(Range<usize>, Hl)],
    marked: Option<Range<usize>>,
    font: &Font,
) -> Vec<TextRun> {
    if len == 0 {
        return Vec::new();
    }
    let mut cuts = vec![0, len];
    for (r, _) in spans {
        if r.end <= len {
            cuts.push(r.start);
            cuts.push(r.end);
        }
    }
    if let Some(m) = &marked {
        cuts.push(m.start.min(len));
        cuts.push(m.end.min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut runs = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        let hl = spans
            .iter()
            .find(|(r, _)| r.start <= a && a < r.end)
            .map(|(_, h)| *h);
        let color = hl_color(hl);
        let underline = marked
            .as_ref()
            .filter(|m| m.start <= a && a < m.end)
            .map(|_| UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: false,
            });
        runs.push(TextRun {
            len: b - a,
            font: font.clone(),
            color,
            background_color: None,
            underline,
            strikethrough: None,
        });
    }
    runs
}

// -------------------------------------------------------- IME plumbing ---

impl Editor {
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Enter and Tab are handled by actions; ignore raw control characters.
        if new_text.chars().any(|c| c.is_control()) {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let kind = if new_text.chars().all(|c| !c.is_whitespace()) || new_text == " " {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        self.replace(range, new_text, kind, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.record_undo(EditKind::Typing);
        self.completion = None;
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.recompute();
        self.autoscroll = true;
        self.touch();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let m = self.metrics?;
        let range = self.range_from_utf16(&range_utf16);
        let line = self.line_of(range.start);
        let y = m.top + m.line_height * line as f32 - self.scroll_y;
        let x0 = self.x_for_offset(range.start, window);
        let x1 = self.x_for_offset(range.end.max(range.start), window);
        Some(Bounds::from_corners(
            point(m.text_left + x0, y),
            point(m.text_left + x1, y + m.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_for_position(point, window);
        Some(self.offset_to_utf16(offset))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// -------------------------------------------------------------- render ---

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Leaving a `?` line asks Claude about it.
        let line = self.line_of(self.cursor_offset());
        if line != self.last_line {
            let previous = std::mem::replace(&mut self.last_line, line);
            self.ask_ai(previous, false, cx);
        }
        let title = format!("{} — Tallypad", self.file_name());
        window.set_window_title(&title);

        div()
            .id("tallypad-editor")
            .key_context("Editor")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(FONT_FAMILY)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::word_left_action))
            .on_action(cx.listener(Self::word_right_action))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::copy_with_results))
            .on_action(cx.listener(Self::new_sheet))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::cancel_completion))
            .on_action(cx.listener(Self::ask_again))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(EditorElement {
                        editor: cx.entity(),
                    }),
            )
    }
}

// ------------------------------------------------------------- element ---

struct EditorElement {
    editor: Entity<Editor>,
}

struct LinePaint {
    shaped: ShapedLine,
    origin: Point<Pixels>,
}

/// The variable-completion popup under the cursor.
struct Popup {
    bounds: Bounds<Pixels>,
    bg: PaintQuad,
    selected: Option<PaintQuad>,
    rows: Vec<LinePaint>,
    row_height: Pixels,
    hitbox: Hitbox,
}

pub struct PrepaintState {
    popup: Option<Popup>,
    lines: Vec<LinePaint>,
    gutter: Vec<LinePaint>,
    results: Vec<LinePaint>,
    placeholder: Option<LinePaint>,
    selections: Vec<PaintQuad>,
    current_line: Option<PaintQuad>,
    band_marker: Option<PaintQuad>,
    result_pill: Option<PaintQuad>,
    cursor: Option<PaintQuad>,
    results_bg: PaintQuad,
    divider: PaintQuad,
    text_clip: Bounds<Pixels>,
    results_clip: Bounds<Pixels>,
    text_hitbox: Hitbox,
    result_hitboxes: Vec<Hitbox>,
    line_height: Pixels,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let line_height = editor.line_height();
        let font_size = editor.font_size();
        let font_ = editor.font();
        let zoom = editor.zoom;

        let result_width = (bounds.size.width * 0.36).max(px(150.)).min(px(440.));
        let divider_x = bounds.right() - result_width;
        let gutter_right = bounds.left() + px(GUTTER * zoom);
        let text_left = gutter_right + px(14.);
        let text_right = divider_x - px(10.);
        let result_right = bounds.right() - px(PAD_X);
        let top = bounds.top() + px(PAD_TOP);

        let line_count = editor.line_count();
        let content_height = line_height * line_count as f32 + px(PAD_TOP * 2.);
        let max_scroll = (content_height - bounds.size.height).max(px(0.));

        // Keep the cursor in view after keyboard movement / edits.
        let cursor_offset = editor.cursor_offset();
        let cursor_line = editor.line_of(cursor_offset);
        let mut scroll_y = editor.scroll_y;
        if editor.autoscroll {
            let cy = line_height * cursor_line as f32;
            let view_h = bounds.size.height - px(PAD_TOP * 2.);
            if cy < scroll_y {
                scroll_y = cy;
            } else if cy + line_height > scroll_y + view_h {
                scroll_y = cy + line_height - view_h;
            }
        }
        scroll_y = scroll_y.max(px(0.)).min(max_scroll);

        let first = ((scroll_y - px(PAD_TOP)) / line_height).floor().max(0.) as usize;
        let visible = (bounds.size.height / line_height).ceil() as usize + 2;
        let last = (first + visible).min(line_count);

        let selection = editor.selected_range.clone();
        let marked = editor.marked_range.clone();
        let is_focused = editor.focus_handle.is_focused(window);
        let flash = editor
            .flash
            .filter(|(_, at)| at.elapsed() < Duration::from_millis(900))
            .map(|(line, _)| line);
        let hovered = editor.hovered_result;
        let show_cursor = is_focused && editor.blink_on && selection.is_empty();

        // The current-line band glides to the cursor's line and spans both
        // columns, so it's obvious which answer belongs to the line you're on.
        let target = cursor_line as f32;
        let ease = |start: Instant, dur: f32| {
            let t = (start.elapsed().as_secs_f32() / dur).min(1.);
            (t, 1. - (1. - t).powi(3))
        };
        let (hl_from, hl_start) = if target != editor.hl_to {
            let (_, e) = ease(editor.hl_start, HL_SLIDE_SECS);
            (
                editor.hl_from + (editor.hl_to - editor.hl_from) * e,
                Instant::now(),
            )
        } else {
            (editor.hl_from, editor.hl_start)
        };
        let (slide_t, slide) = ease(hl_start, HL_SLIDE_SECS);
        let (fade_t, fade) = ease(hl_start, HL_FADE_SECS);
        let hl_pos = hl_from + (target - hl_from) * slide;
        if slide_t < 1. || fade_t < 1. {
            window.request_animation_frame();
        }
        let band_y = top + line_height * hl_pos - scroll_y;
        let current_line = is_focused.then(|| {
            fill(
                Bounds::from_corners(
                    point(bounds.left(), band_y),
                    point(bounds.right(), band_y + line_height),
                ),
                theme::current_line(),
            )
        });
        let band_marker = is_focused.then(|| {
            fill(
                Bounds::new(point(bounds.left(), band_y), size(px(2.), line_height)),
                theme::accent(),
            )
        });
        let mut result_pill = None;

        let mut lines = Vec::new();
        let mut gutter = Vec::new();
        let mut results = Vec::new();
        let mut selections = Vec::new();
        let mut result_rects = Vec::new();
        let mut cursor = None;
        let mut popup_anchor = None;

        let text_system = window.text_system().clone();
        let space_w = px(BASE_FONT_SIZE * zoom * 0.35);

        for i in first..last {
            let range = editor.line_range(i);
            let text = &editor.content[range.clone()];
            let y = top + line_height * i as f32 - scroll_y;
            let output = editor.outputs.get(i);

            let local_marked = marked.as_ref().and_then(|m| {
                (m.start >= range.start && m.end <= range.end)
                    .then(|| m.start - range.start..m.end - range.start)
            });
            // Lines without a result are prose: only keep comment/label colours.
            let spans: Vec<(Range<usize>, Hl)> = match output {
                Some(o) if o.display.is_some() || o.question.is_some() || o.plot.is_some() => {
                    o.spans.clone()
                }
                Some(o) => o
                    .spans
                    .iter()
                    .filter(|(_, h)| matches!(h, Hl::Comment | Hl::Label))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            let runs = build_runs(text.len(), &spans, local_marked, &font_);
            let shaped = text_system.shape_line(
                SharedString::from(text.to_string()),
                font_size,
                &runs,
                None,
            );

            // Selection
            if selection.start <= range.end && selection.end >= range.start && !selection.is_empty()
            {
                let a = selection.start.max(range.start) - range.start;
                let b = selection.end.min(range.end) - range.start;
                let x0 = shaped.x_for_index(a);
                let mut x1 = shaped.x_for_index(b);
                if selection.end > range.end {
                    x1 += space_w;
                }
                if x1 > x0 {
                    selections.push(fill(
                        Bounds::from_corners(
                            point(text_left + x0, y),
                            point(text_left + x1, y + line_height),
                        ),
                        rgba(0x4c7dff44),
                    ));
                }
            }

            if i == cursor_line {
                if let Some(c) = &editor.completion
                    && c.range.start >= range.start
                    && c.range.end <= range.end
                {
                    popup_anchor = Some((
                        text_left + shaped.x_for_index(c.range.start - range.start),
                        y,
                    ));
                }
                if show_cursor {
                    let x = shaped.x_for_index(cursor_offset - range.start);
                    let inset = (line_height * 0.18).round();
                    cursor = Some(fill(
                        Bounds::new(
                            point(text_left + x - px(1.), y + inset),
                            size(px(2.), line_height - inset * 2.),
                        ),
                        theme::accent(),
                    ));
                }
            }

            // Line number
            let number = SharedString::from((i + 1).to_string());
            let num_color: Hsla = if i == cursor_line && is_focused {
                theme::muted().into()
            } else {
                theme::faint().into()
            };
            let num_run = TextRun {
                len: number.len(),
                font: font_.clone(),
                color: num_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let num_shaped = text_system.shape_line(number, font_size * 0.78, &[num_run], None);
            let num_y = y + (line_height - font_size * 0.78 * LINE_HEIGHT_RATIO) / 2.;
            gutter.push(LinePaint {
                origin: point(gutter_right - num_shaped.width, num_y),
                shaped: num_shaped,
            });

            // Result
            if let Some(display) = output.and_then(|o| o.display.as_ref()) {
                let (label, color): (SharedString, Hsla) = if flash == Some(i) {
                    ("Copied".into(), theme::muted().into())
                } else if hovered == Some(i) || (i == cursor_line && is_focused) {
                    (display.clone().into(), theme::accent_hover().into())
                } else {
                    (display.clone().into(), theme::accent().into())
                };
                let mut result_font = font_.clone();
                result_font.weight = gpui::FontWeight::SEMIBOLD;
                let run = TextRun {
                    len: label.len(),
                    font: result_font,
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped_result = text_system.shape_line(label, font_size, &[run], None);
                let x = result_right - shaped_result.width;
                let rect = Bounds::from_corners(
                    point(x.max(divider_x + px(8.)) - px(6.), y),
                    point(result_right + px(6.), y + line_height),
                );
                result_rects.push((rect, i));
                if i == cursor_line && is_focused {
                    let mut tint: Hsla = theme::accent().into();
                    tint.a = 0.16 * fade;
                    let inset = (line_height * 0.14).round();
                    result_pill = Some(
                        fill(
                            Bounds::from_corners(
                                point(x - px(10.), y + inset),
                                point(result_right + px(10.), y + line_height - inset),
                            ),
                            tint,
                        )
                        .corner_radii(px(6.)),
                    );
                }
                results.push(LinePaint {
                    origin: point(x, y),
                    shaped: shaped_result,
                });
            } else if let Some((note, color)) =
                output.and_then(|o| editor.line_note(o, i == cursor_line && is_focused))
            {
                let size_ = font_size * 0.86;
                let run = TextRun {
                    len: note.len(),
                    font: font_.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped_note = text_system.shape_line(note.into(), size_, &[run], None);
                results.push(LinePaint {
                    origin: point(
                        (result_right - shaped_note.width).max(divider_x + px(10.)),
                        y,
                    ),
                    shaped: shaped_note,
                });
            }

            lines.push(LinePaint {
                shaped,
                origin: point(text_left, y),
            });
        }

        let placeholder = (editor.content.is_empty()).then(|| {
            let hint = "Type a calculation… e.g.  $1,200 rent + 15%";
            let run = TextRun {
                len: hint.len(),
                font: font_.clone(),
                color: theme::faint().into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            LinePaint {
                shaped: text_system.shape_line(hint.into(), font_size, &[run], None),
                origin: point(text_left, top - scroll_y),
            }
        });

        let results_bg = fill(
            Bounds::from_corners(point(divider_x, bounds.top()), bounds.bottom_right()),
            theme::results_bg(),
        );
        let divider = fill(
            Bounds::new(
                point(divider_x, bounds.top()),
                size(px(1.), bounds.size.height),
            ),
            theme::divider(),
        );
        let text_clip = Bounds::from_corners(
            point(bounds.left(), bounds.top()),
            point(text_right, bounds.bottom()),
        );
        let results_clip = Bounds::from_corners(
            point(divider_x + px(1.), bounds.top()),
            bounds.bottom_right(),
        );

        let text_hitbox = window.insert_hitbox(text_clip, HitboxBehavior::Normal);
        let result_hitboxes = result_rects
            .iter()
            .map(|(r, _)| window.insert_hitbox(*r, HitboxBehavior::Normal))
            .collect();

        // Completion popup: below the typed name, or above it near the bottom.
        let mut popup = None;
        let mut completion_boxes = Vec::new();
        if let (Some(c), Some((ax, ay)), true) = (&editor.completion, popup_anchor, is_focused) {
            let row_height = (line_height * 0.9).round();
            let name_size = font_size * 0.92;
            let value_size = font_size * 0.8;
            let pad = px(5.);
            let mut bold = font_.clone();
            bold.weight = gpui::FontWeight::SEMIBOLD;
            let run = |len: usize, font: &Font, color: Hsla| TextRun {
                len,
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped_rows: Vec<(ShapedLine, Option<ShapedLine>)> = c
                .items
                .iter()
                .map(|(name, value)| {
                    let split = name
                        .char_indices()
                        .nth(c.typed_chars)
                        .map(|(b, _)| b)
                        .unwrap_or(name.len());
                    let runs: Vec<TextRun> = [
                        run(split, &bold, theme::accent().into()),
                        run(name.len() - split, &font_, theme::text().into()),
                    ]
                    .into_iter()
                    .filter(|r| r.len > 0)
                    .collect();
                    let n = text_system.shape_line(name.clone().into(), name_size, &runs, None);
                    let v = value.as_ref().map(|v| {
                        text_system.shape_line(
                            v.clone().into(),
                            value_size,
                            &[run(v.len(), &font_, theme::muted().into())],
                            None,
                        )
                    });
                    (n, v)
                })
                .collect();
            let content_w = shaped_rows
                .iter()
                .map(|(n, v)| n.width + v.as_ref().map(|v| v.width + px(28.)).unwrap_or_default())
                .fold(px(170.), |a, b| a.max(b));
            let width = (content_w + pad * 2. + px(16.)).min(bounds.size.width - px(16.));
            let height = row_height * shaped_rows.len() as f32 + pad * 2.;
            let x = (ax - px(12.))
                .min(bounds.right() - width - px(8.))
                .max(bounds.left() + px(8.));
            let below = ay + line_height + px(2.);
            let y = if below + height > bounds.bottom() - px(4.) {
                (ay - height - px(2.)).max(bounds.top())
            } else {
                below
            };
            let popup_bounds = Bounds::new(point(x, y), size(width, height));
            let mut selected = None;
            let mut rows = Vec::new();
            for (k, (n, v)) in shaped_rows.into_iter().enumerate() {
                let row_y = y + pad + row_height * k as f32;
                let row = Bounds::new(point(x + pad, row_y), size(width - pad * 2., row_height));
                completion_boxes.push((row, k));
                if k == c.selected {
                    let mut tint: Hsla = theme::accent().into();
                    tint.a = 0.16;
                    selected = Some(fill(row, tint).corner_radii(px(5.)));
                }
                rows.push(LinePaint {
                    origin: point(x + pad + px(8.), row_y),
                    shaped: n,
                });
                if let Some(v) = v {
                    rows.push(LinePaint {
                        origin: point(x + width - pad - px(8.) - v.width, row_y),
                        shaped: v,
                    });
                }
            }
            popup = Some(Popup {
                bounds: popup_bounds,
                bg: fill(popup_bounds, rgba(0x25272dff))
                    .corner_radii(px(8.))
                    .border_widths(px(1.))
                    .border_color(rgba(0x3a3d45ff)),
                selected,
                rows,
                row_height,
                hitbox: window.insert_hitbox(popup_bounds, HitboxBehavior::Normal),
            });
        }

        let metrics = Metrics {
            bounds,
            text_left,
            top,
            line_height,
            max_scroll,
        };
        self.editor.update(cx, |editor, _| {
            editor.metrics = Some(metrics);
            editor.scroll_y = scroll_y;
            editor.autoscroll = false;
            editor.hl_from = hl_from;
            editor.hl_to = target;
            editor.hl_start = hl_start;
            editor.result_boxes = result_rects;
            editor.completion_boxes = completion_boxes;
        });

        PrepaintState {
            popup,
            lines,
            gutter,
            results,
            placeholder,
            selections,
            current_line,
            band_marker,
            result_pill,
            cursor,
            results_bg,
            divider,
            text_clip,
            results_clip,
            text_hitbox,
            result_hitboxes,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        window.set_cursor_style(CursorStyle::IBeam, &state.text_hitbox);
        for hitbox in &state.result_hitboxes {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        if let Some(p) = &state.popup {
            window.set_cursor_style(CursorStyle::PointingHand, &p.hitbox);
        }

        let line_height = state.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(state.results_bg.clone());
            if let Some(q) = state.current_line.take() {
                window.paint_quad(q);
            }
            if let Some(q) = state.band_marker.take() {
                window.paint_quad(q);
            }
            window.paint_quad(state.divider.clone());

            window.with_content_mask(
                Some(ContentMask {
                    bounds: state.text_clip,
                }),
                |window| {
                    for q in state.selections.drain(..) {
                        window.paint_quad(q);
                    }
                    for g in &state.gutter {
                        g.shaped.paint(g.origin, line_height, window, cx).ok();
                    }
                    if let Some(p) = &state.placeholder {
                        p.shaped.paint(p.origin, line_height, window, cx).ok();
                    }
                    for l in &state.lines {
                        l.shaped.paint(l.origin, line_height, window, cx).ok();
                    }
                    if let Some(c) = state.cursor.take() {
                        window.paint_quad(c);
                    }
                },
            );

            window.with_content_mask(
                Some(ContentMask {
                    bounds: state.results_clip,
                }),
                |window| {
                    if let Some(pill) = state.result_pill.take() {
                        window.paint_quad(pill);
                    }
                    for r in &state.results {
                        r.shaped.paint(r.origin, line_height, window, cx).ok();
                    }
                },
            );

            if let Some(p) = state.popup.take() {
                window.paint_shadows(
                    p.bounds,
                    px(8.).into(),
                    &[BoxShadow {
                        color: gpui::hsla(0., 0., 0., 0.45),
                        offset: point(px(0.), px(6.)),
                        blur_radius: px(18.),
                        spread_radius: px(0.),
                    }],
                );
                window.paint_quad(p.bg);
                if let Some(q) = p.selected {
                    window.paint_quad(q);
                }
                for r in &p.rows {
                    r.shaped.paint(r.origin, p.row_height, window, cx).ok();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::match_names;

    fn names(before: &str) -> Option<(usize, Vec<&'static str>, bool)> {
        const VARS: &[&str] = &["hourly rate", "hours", "hours worked", "Rent", "x"];
        match_names(before, VARS)
            .map(|m| (m.start, m.items.iter().map(|&i| VARS[i]).collect(), m.exact))
    }

    #[test]
    fn completes_variable_prefixes() {
        assert_eq!(
            names("hou"),
            Some((0, vec!["hours", "hourly rate", "hours worked"], false))
        );
        assert_eq!(
            names("pay * hourly r"),
            Some((6, vec!["hourly rate"], false))
        );
        assert_eq!(names("$5 + re"), Some((5, vec!["Rent"], false)));
        // A complete name with longer siblings still offers them.
        assert_eq!(
            names("hours"),
            Some((0, vec!["hours", "hours worked"], true))
        );
        // Complete and unique, a trailing space, comments: nothing.
        assert_eq!(names("rent"), None);
        assert_eq!(names("hourly "), None);
        assert_eq!(names("// hou"), None);
        assert_eq!(names("# hou"), None);
        assert_eq!(names("2hou"), None);
        assert_eq!(names("zz"), None);
    }
}

//! The viewer. Two modes only: one step per screen, and a dive into the
//! full diff behind the current step. Keys: n/p step, Enter dive/back,
//! q quit.
//!
//! Slide anatomy: a rounded page frame, a one-line claim with an accent
//! bar, then the excerpt — syntax highlighted, diff shown as background
//! tint (added green, deleted red, changed words darker + bold), context
//! dimmed, notes hanging off their lines rustc-style. No signs and no
//! line numbers on slides; those live in the dive. A filmstrip along the
//! bottom keeps the whole deck in view, and the status bar always says
//! how much of the diff the deck has actually touched.

use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::deck::{Anchor, Deck, Group, Note, Severity, Step};
use crate::diff::{
    ExcerptRow, FileDiff, LineKind, Repo, Segment, emphasize_hunk, excerpt, file_diff, load_diff,
};
use crate::highlight::{Lang, highlight};
use crate::seen::{SeenStore, changed_count, hunk_hash};

pub fn run(deck: Deck, repo: Repo, deck_key: Option<String>) -> Result<()> {
    let files = load_diff(&repo, deck.base.as_deref())?;
    let coverage = Coverage::compute(&deck, &files);
    let outline = outline_of(&deck);
    let git_dir = repo.git_dir();
    let seen = SeenStore::load(git_dir.clone());
    let hashes = hash_cache(&files);
    let covered = covered_map(&deck, &files, &hashes, &repo);
    // Pick up where the reader left off last time.
    let resumed = deck_key
        .as_deref()
        .and_then(|key| crate::resume::load(&git_dir, key))
        .filter(|&s| s > 0 && s < deck.steps.len());
    let mut app = App {
        deck,
        repo,
        files,
        coverage,
        outline,
        step: resumed.unwrap_or(0),
        mode: Mode::Steps,
        notes_view: NotesView::Panel,
        files_view: true,
        hide_seen: false,
        file_cache: HashMap::new(),
        seen,
        dive_file: None,
        sidebar_area: Rect::default(),
        focus: Focus::Slides,
        ask: None,
        help: false,
        toast: resumed.map(|s| format!("resumed at slide {}", s + 1)),
        sidebar_cursor: 0,
        sidebar_items: Vec::new(),
        strip_boxes: Vec::new(),
        sidebar_hits: Vec::new(),
        git_dir,
        deck_key,
        hashes,
        covered,
    };
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = app.event_loop(&mut terminal);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

enum Mode {
    Steps,
    Dive { scroll: usize, cursor: usize },
}

/// What a sidebar row does when clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SideTarget {
    /// Jump to a slide.
    Step(usize),
    /// Open the dive on a file the deck does not point at.
    File(String),
}

/// Where key input goes on the steps screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Slides,
    Sidebar,
}

/// One actionable sidebar row, for keyboard navigation.
#[derive(Clone)]
struct SideItem {
    /// Index into the sidebar's display rows.
    row: usize,
    target: SideTarget,
    /// Set when the row is a file — `v` toggles it seen wholesale.
    file: Option<String>,
}

/// How the speaker notes are shown. `s` cycles panel → popup → hidden.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NotesView {
    Panel,
    Popup,
    Hidden,
}

impl NotesView {
    fn next(self) -> NotesView {
        match self {
            NotesView::Panel => NotesView::Popup,
            NotesView::Popup => NotesView::Hidden,
            NotesView::Hidden => NotesView::Panel,
        }
    }
}

struct App {
    deck: Deck,
    repo: Repo,
    files: Vec<FileDiff>,
    coverage: Coverage,
    step: usize,
    mode: Mode,
    notes_view: NotesView,
    /// File sidebar on the left, toggled with `f`; hidden on narrow panes.
    files_view: bool,
    /// `h`: drop fully seen files from the list — the endgame view where
    /// only what is left to review remains.
    hide_seen: bool,
    /// The deck's sections and the files each one points at.
    outline: Vec<Section>,
    file_cache: HashMap<String, Option<Vec<String>>>,
    /// Which changed lines the reader has marked seen. Local, persisted.
    seen: SeenStore,
    /// When set, the dive shows this file instead of the current step's.
    dive_file: Option<String>,
    /// Where the sidebar was last drawn, for wheel routing.
    sidebar_area: Rect,
    /// Keyboard focus on the steps screen; Tab switches.
    focus: Focus,
    /// Open question input (`a`): the buffer being typed.
    ask: Option<String>,
    /// Keyboard shortcut overlay (`?`); any key closes it.
    help: bool,
    /// One-shot status message (e.g. "copied"), cleared on the next key.
    toast: Option<String>,
    /// Cursor into `sidebar_items` while the sidebar has focus.
    sidebar_cursor: usize,
    /// Actionable sidebar rows, top to bottom, refreshed on every draw.
    sidebar_items: Vec<SideItem>,
    /// Filmstrip hit areas, refreshed on every draw: (box, step index).
    strip_boxes: Vec<(Rect, usize)>,
    /// Sidebar hit areas, refreshed on every draw.
    sidebar_hits: Vec<(Rect, SideTarget)>,
    git_dir: Option<std::path::PathBuf>,
    /// Identity of the open deck, for the resume record.
    deck_key: Option<String>,
    /// Per-file (hunk hash, changed count), computed once — hashing the
    /// whole diff on every draw is what made the sidebar feel slow.
    hashes: HashMap<String, Vec<(String, usize)>>,
    /// Per file, the changed lines some slide's excerpt shows — "what the
    /// deck explains", as opposed to the rest of the diff.
    covered: HashMap<String, std::collections::HashSet<(String, usize)>>,
}

/// Which changed lines the deck's excerpts actually display, per file.
fn covered_map(
    deck: &Deck,
    files: &[FileDiff],
    hashes: &HashMap<String, Vec<(String, usize)>>,
    repo: &Repo,
) -> HashMap<String, std::collections::HashSet<(String, usize)>> {
    let mut covered: HashMap<String, std::collections::HashSet<(String, usize)>> =
        HashMap::new();
    let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    for step in &deck.steps {
        let Some(at) = step.anchor() else { continue };
        let lines = cache
            .entry(at.file.clone())
            .or_insert_with(|| repo.read_file(&at.file).ok());
        let (start, end) = at.range();
        let rows = excerpt(file_diff(files, &at.file), lines.as_deref(), start, end);
        let pairs = hashes.get(&at.file);
        let set = covered.entry(at.file.clone()).or_default();
        for row in rows {
            if let (Some(h), Some(i)) = (row.hunk_idx, row.changed_idx)
                && let Some((hash, _)) = pairs.and_then(|p| p.get(h))
            {
                set.insert((hash.clone(), i));
            }
        }
    }
    covered
}

fn hash_cache(files: &[FileDiff]) -> HashMap<String, Vec<(String, usize)>> {
    files
        .iter()
        .map(|fd| {
            (
                fd.new_path.clone(),
                fd.hunks
                    .iter()
                    .map(|h| (hunk_hash(h), changed_count(h)))
                    .collect(),
            )
        })
        .collect()
}

/// A run of slides under one headline slide, and the files they point at.
/// `level` 1 is a section, 2 a subsection; files under a headline make
/// the third tier.
struct Section {
    title: String,
    headline_step: usize,
    level: u8,
    /// (path, first step that points at it)
    files: Vec<(String, usize)>,
}

fn outline_of(deck: &Deck) -> Vec<Section> {
    let mut sections = vec![Section {
        title: String::new(),
        headline_step: 0,
        level: 1,
        files: Vec::new(),
    }];
    for (i, step) in deck.steps.iter().enumerate() {
        if i > 0
            && let Step::Cover { what, level, .. } = step {
                sections.push(Section {
                    title: what.clone(),
                    headline_step: i,
                    level: (*level).clamp(1, 2),
                    files: Vec::new(),
                });
                continue;
            }
        if let Some(at) = step.anchor() {
            let section = sections.last_mut().unwrap();
            if !section.files.iter().any(|(f, _)| f == &at.file) {
                section.files.push((at.file.clone(), i));
            }
        }
    }
    sections.retain(|s| !s.files.is_empty() || !s.title.is_empty());
    sections
}

const ACCENT: Color = Color::Cyan;
const ADD_BG: Color = Color::Rgb(16, 56, 28);
const ADD_EMPH_BG: Color = Color::Rgb(22, 92, 42);
const DEL_BG: Color = Color::Rgb(68, 26, 26);
const DEL_EMPH_BG: Color = Color::Rgb(112, 40, 40);

/// How much of the diff the deck actually points at. Shown permanently:
/// a ten-step tour of a ten-thousand-line change must say so itself.
struct Coverage {
    lines_touched: usize,
    lines_total: usize,
    files_touched: usize,
    files_total: usize,
    /// Biggest changed files no step points at: (basename, added lines).
    untouched_top: Vec<(String, usize)>,
}

impl Coverage {
    fn compute(deck: &Deck, files: &[FileDiff]) -> Coverage {
        let mut ranges: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
        for step in &deck.steps {
            if let Some(at) = step.anchor() {
                ranges.entry(at.file.as_str()).or_default().push(at.range());
            }
        }
        let mut cov = Coverage {
            lines_touched: 0,
            lines_total: 0,
            files_touched: 0,
            files_total: files.len(),
            untouched_top: Vec::new(),
        };
        let mut untouched: Vec<(String, usize)> = Vec::new();
        for fd in files {
            let added = fd.added();
            cov.lines_total += added;
            let file_ranges = ranges.get(fd.new_path.as_str());
            let touched = match file_ranges {
                None => 0,
                Some(rs) => fd
                    .hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == LineKind::Add)
                    .filter_map(|l| l.new_no)
                    .filter(|no| rs.iter().any(|(lo, hi)| no >= lo && no <= hi))
                    .count(),
            };
            cov.lines_touched += touched;
            if file_ranges.is_some() {
                cov.files_touched += 1;
            } else if added > 0 {
                let base = fd.new_path.rsplit('/').next().unwrap_or(&fd.new_path);
                untouched.push((base.to_string(), added));
            }
        }
        untouched.sort_by_key(|(_, added)| std::cmp::Reverse(*added));
        untouched.truncate(3);
        cov.untouched_top = untouched;
        cov
    }

    fn summary(&self) -> String {
        format!(
            "covers +{}/+{} · {}/{} files",
            self.lines_touched, self.lines_total, self.files_touched, self.files_total
        )
    }

    /// Compact form for the status bar; the file breakdown lives in the
    /// sidebar and on the map slide.
    fn summary_short(&self) -> String {
        format!("covers +{}/+{}", self.lines_touched, self.lines_total)
    }
}

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut saved_step = self.step;
        loop {
            // Persist the position whenever it moved, so a reopened deck
            // starts where this one left off.
            if self.step != saved_step {
                saved_step = self.step;
                if let Some(key) = &self.deck_key {
                    crate::resume::save(&self.git_dir, key, self.step);
                }
            }
            terminal.draw(|frame| self.draw(frame))?;
            // Handle everything already queued before redrawing once — a
            // wheel burst is many events but should cost one draw.
            loop {
                if self.handle_event(event::read()?)? {
                    return Ok(());
                }
                if !event::poll(std::time::Duration::ZERO)? {
                    break;
                }
            }
        }
    }

    /// Returns true when the app should quit.
    fn handle_event(&mut self, event: Event) -> Result<bool> {
        let key = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => key,
            Event::Mouse(mouse) => {
                let over_sidebar = self.sidebar_area.contains(Position {
                    x: mouse.column,
                    y: mouse.row,
                });
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.on_click(mouse.column, mouse.row);
                    }
                    MouseEventKind::ScrollDown => match &mut self.mode {
                        Mode::Steps if over_sidebar => self.sidebar_move(3),
                        Mode::Steps => {
                            if self.step + 1 < self.deck.steps.len() {
                                self.step += 1;
                            }
                        }
                        Mode::Dive { cursor, .. } => *cursor = cursor.saturating_add(3),
                    },
                    MouseEventKind::ScrollUp => match &mut self.mode {
                        Mode::Steps if over_sidebar => self.sidebar_move(-3),
                        Mode::Steps => {
                            self.step = self.step.saturating_sub(1);
                        }
                        Mode::Dive { cursor, .. } => *cursor = cursor.saturating_sub(3),
                    },
                    _ => {}
                }
                return Ok(false);
            }
            _ => return Ok(false),
        };
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return Ok(true);
        }
        // The question input swallows every key while open — `q` is a
        // letter here, not quit.
        if let Some(buf) = &mut self.ask {
            match key.code {
                KeyCode::Esc => self.ask = None,
                KeyCode::Enter => {
                    let question = self.ask.take().unwrap_or_default();
                    let prompt = self.build_ask_prompt(&question);
                    self.toast = Some(deliver_ask(&prompt));
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return Ok(false);
        }
        self.toast = None;
        if self.help {
            self.help = false;
            return Ok(false);
        }
        if key.code == KeyCode::Char('?') {
            self.help = true;
            return Ok(false);
        }
        if key.code == KeyCode::Char('q') {
            return Ok(true);
        }
        match &mut self.mode {
            Mode::Steps if self.focus == Focus::Sidebar => match key.code {
                KeyCode::Char('n' | 'j') | KeyCode::Down => {
                    self.sidebar_cursor = (self.sidebar_cursor + 1)
                        .min(self.sidebar_items.len().saturating_sub(1));
                }
                KeyCode::Char('p' | 'k') | KeyCode::Up => {
                    self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
                }
                KeyCode::Enter => {
                    if let Some(item) = self.sidebar_items.get(self.sidebar_cursor).cloned() {
                        match item.target {
                            SideTarget::Step(step) => {
                                self.step = step;
                                self.focus = Focus::Slides;
                            }
                            SideTarget::File(file) => {
                                self.dive_file = Some(file);
                                self.mode = Mode::Dive {
                                    scroll: 0,
                                    cursor: 0,
                                };
                            }
                        }
                    }
                }
                KeyCode::Char('v') => {
                    if let Some(path) = self
                        .sidebar_items
                        .get(self.sidebar_cursor)
                        .and_then(|i| i.file.clone())
                        && let Some(fd) = file_diff(&self.files, &path).cloned()
                    {
                        self.seen.toggle_file(&fd);
                        // Marking flows downward — unless hide-seen will
                        // drop this row, in which case the vanishing row
                        // is the advance and the cursor stays put.
                        let (s, t) = self
                            .seen
                            .progress_cached(&path, self.hashes_of(&path));
                        let vanishes = self.hide_seen && t > 0 && s == t;
                        if !vanishes {
                            self.sidebar_cursor = (self.sidebar_cursor + 1)
                                .min(self.sidebar_items.len().saturating_sub(1));
                        }
                    }
                }
                KeyCode::Char('f') => {
                    self.files_view = false;
                    self.focus = Focus::Slides;
                }
                KeyCode::Char('h') => self.hide_seen = !self.hide_seen,
                KeyCode::Tab | KeyCode::Esc | KeyCode::Left | KeyCode::Right => {
                    self.focus = Focus::Slides;
                }
                _ => {}
            },
            Mode::Steps => match key.code {
                KeyCode::Char('n' | 'j' | ' ') | KeyCode::Right => {
                    if self.step + 1 < self.deck.steps.len() {
                        self.step += 1;
                    }
                }
                KeyCode::Char('p' | 'k') | KeyCode::Left => {
                    self.step = self.step.saturating_sub(1);
                }
                // ↑/↓ go straight into the file list — the sidebar is the
                // vertical axis, slides the horizontal one.
                KeyCode::Down => self.sidebar_move(1),
                KeyCode::Up => self.sidebar_move(-1),
                KeyCode::Char('s') => self.notes_view = self.notes_view.next(),
                KeyCode::Char('f') => self.files_view = !self.files_view,
                KeyCode::Char('h') => self.hide_seen = !self.hide_seen,
                KeyCode::Char('v') => self.slide_toggle_seen(),
                KeyCode::Char('a') => self.ask = Some(String::new()),
                KeyCode::Tab => {
                    if self.files_view && !self.sidebar_items.is_empty() {
                        self.focus = Focus::Sidebar;
                    }
                }
                KeyCode::Enter => self.enter_dive(),
                KeyCode::Esc => {
                    if self.notes_view == NotesView::Popup {
                        self.notes_view = NotesView::Panel;
                    } else {
                        return Ok(true);
                    }
                }
                _ => {}
            },
            Mode::Dive { cursor, .. } => match key.code {
                KeyCode::Char('n' | 'j') | KeyCode::Down => {
                    *cursor = cursor.saturating_add(1);
                }
                KeyCode::Char('p' | 'k') | KeyCode::Up => {
                    *cursor = cursor.saturating_sub(1);
                }
                KeyCode::Char('v') => self.dive_toggle_line(),
                KeyCode::Char('V') => self.dive_toggle_hunk(),
                KeyCode::Enter | KeyCode::Esc => {
                    self.mode = Mode::Steps;
                    self.dive_file = None;
                }
                _ => {}
            },
        }
        Ok(false)
    }

    fn current(&self) -> &Step {
        &self.deck.steps[self.step]
    }

    /// Move the file-list cursor, taking focus if the slides had it.
    /// Entry lands relative to the current slide's file, so stepping off
    /// the slide into the list reads as one continuous motion.
    fn sidebar_move(&mut self, delta: isize) {
        if !self.files_view || self.sidebar_items.is_empty() {
            return;
        }
        let base = if self.focus == Focus::Sidebar {
            self.sidebar_cursor as isize
        } else {
            self.focus = Focus::Sidebar;
            self.current()
                .anchor()
                .map(|a| a.file.clone())
                .and_then(|f| {
                    self.sidebar_items
                        .iter()
                        .position(|i| i.file.as_deref() == Some(f.as_str()))
                })
                .unwrap_or(self.sidebar_cursor) as isize
        };
        let last = self.sidebar_items.len() as isize - 1;
        self.sidebar_cursor = (base + delta).clamp(0, last) as usize;
    }

    fn file_lines(&mut self, path: &str) -> Option<&[String]> {
        if !self.file_cache.contains_key(path) {
            let loaded = self.repo.read_file(path).ok();
            self.file_cache.insert(path.to_string(), loaded);
        }
        self.file_cache.get(path).unwrap().as_deref()
    }

    // ---- drawing ----------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        match self.mode {
            Mode::Steps => {
                let area = frame.area();
                let [content, status] =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
                let sidebar_w = if self.files_view && content.width >= 130 {
                    42
                } else if self.files_view && content.width >= 100 {
                    34
                } else {
                    0
                };
                let [side, main] = Layout::horizontal([
                    Constraint::Length(sidebar_w),
                    Constraint::Min(1),
                ])
                .areas(content);
                if sidebar_w > 0 {
                    self.draw_sidebar(frame, side);
                } else {
                    self.sidebar_hits.clear();
                }
                // Fixed geometry, derived from the terminal size alone:
                // the page never moves or resizes between slides.
                let band_rows = self
                    .outline
                    .iter()
                    .filter(|s| !s.title.is_empty())
                    .map(|s| s.level)
                    .max()
                    .unwrap_or(0) as u16;
                let geo = Geometry::of(main, band_rows);
                let [_, page_row, notes_row, _, strip] = Layout::vertical([
                    Constraint::Fill(1),
                    Constraint::Length(geo.page_h),
                    Constraint::Length(geo.notes_h),
                    Constraint::Fill(2),
                    Constraint::Length(geo.strip_h),
                ])
                .areas(main);
                let page = center_h(page_row, geo.page_w);
                frame.render_widget(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().dim()),
                    page,
                );
                let inner = page.inner(Margin {
                    horizontal: 3,
                    vertical: 1,
                });
                self.draw_step(frame, inner);
                let notes_text = self.current().speaker_notes().map(str::to_string);
                if self.notes_view == NotesView::Panel
                    && let Some(text) = &notes_text {
                        draw_speaker_notes(frame, notes_row, geo.page_w, text);
                    }
                if geo.strip_h > 0 {
                    self.draw_filmstrip(frame, strip);
                }
                self.draw_status(frame, status);
                if self.notes_view == NotesView::Popup
                    && let Some(text) = &notes_text {
                        draw_notes_popup(frame, area, text);
                    }
                if let Some(buf) = &self.ask {
                    let context = match self.current().anchor() {
                        Some(at) => format!(
                            "slide {}/{} · {at}",
                            self.step + 1,
                            self.deck.steps.len()
                        ),
                        None => format!("slide {}/{}", self.step + 1, self.deck.steps.len()),
                    };
                    draw_ask_popup(frame, area, buf, &context);
                }
                if self.help {
                    draw_help(frame, area);
                }
            }
            Mode::Dive { .. } => {
                let [body, status] =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                        .areas(frame.area());
                self.draw_dive(frame, body);
                let (s, t) = self.seen_overall();
                let left = format!(" dive · seen {s}/{t}");
                let hint = "v line · V hunk · n/p move · ? keys · enter back · q quit ";
                let [l, r] = Layout::horizontal([
                    Constraint::Min(1),
                    Constraint::Length(display_width(hint) as u16),
                ])
                .areas(status);
                frame.render_widget(Paragraph::new(left).style(Style::new().dim()), l);
                frame.render_widget(Paragraph::new(hint).style(Style::new().dim()), r);
                if self.help {
                    draw_help(frame, body);
                }
            }
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let step = self.current();
        // seen before the anchor: a long path may truncate, the progress
        // meter may not.
        let (s, t) = self.seen_overall();
        let mut left = format!(
            " {}/{} · seen {s}/{t} · {}",
            self.step + 1,
            self.deck.steps.len(),
            step.type_name()
        );
        if let Some(at) = step.anchor() {
            left.push_str(&format!(" · {at}"));
        }
        if let Some(toast) = &self.toast {
            left.push_str(&format!("  ▸ {toast}"));
        }
        let right = "n/p · v seen · a ask · ? keys · enter diff · q quit ";
        let [l, r] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(display_width(right) as u16),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(left).style(Style::new().dim()), l);
        frame.render_widget(Paragraph::new(right).style(Style::new().dim()), r);
    }

    /// One little numbered box per step, current one lit — the outline the
    /// reader keeps in the corner of their eye. Clicking a box jumps there.
    fn draw_filmstrip(&mut self, frame: &mut Frame, area: Rect) {
        self.strip_boxes.clear();
        let n = self.deck.steps.len();
        let pitch: u16 = 7; // box width 6 + gap 1
        let max_boxes = usize::from((area.width.saturating_sub(4) + 1) / pitch).max(1);
        let (start, end) = if n <= max_boxes {
            (0, n)
        } else {
            let s = self
                .step
                .saturating_sub(max_boxes / 2)
                .min(n - max_boxes);
            (s, s + max_boxes)
        };
        let total = (end - start) as u16 * pitch - 1;
        let x0 = area.x + area.width.saturating_sub(total) / 2;
        let boxes_y = area.y + area.height.saturating_sub(3);
        let band_rows = area.height.saturating_sub(3);
        for level in 1..=band_rows.min(2) {
            self.draw_section_bands(
                frame,
                area.y + level - 1,
                level as u8,
                x0,
                pitch,
                start,
                end,
            );
        }
        for (j, i) in (start..end).enumerate() {
            let rect = Rect {
                x: x0 + j as u16 * pitch,
                y: boxes_y,
                width: 6,
                height: 3,
            };
            self.strip_boxes.push((rect, i));
            let style = if i == self.step {
                Style::new().fg(ACCENT).bold()
            } else {
                Style::new().dim()
            };
            frame.render_widget(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(style),
                rect,
            );
            frame.render_widget(
                Paragraph::new(format!("{}", i + 1)).centered().style(style),
                rect.inner(Margin {
                    horizontal: 1,
                    vertical: 1,
                }),
            );
        }
        // Continuation marks when the window does not reach an end.
        let mid_y = boxes_y + 1;
        if start > 0 && x0 >= area.x + 2 {
            frame.render_widget(
                Paragraph::new("…").style(Style::new().dim()),
                Rect {
                    x: x0 - 2,
                    y: mid_y,
                    width: 1,
                    height: 1,
                },
            );
        }
        if end < n {
            frame.render_widget(
                Paragraph::new("…").style(Style::new().dim()),
                Rect {
                    x: (x0 + total + 1).min(area.x + area.width - 1),
                    y: mid_y,
                    width: 1,
                    height: 1,
                },
            );
        }
    }

    /// One row of bands for headlines of `level`, spanning their slides,
    /// current chain lit. `…` where the window cuts a band short.
    /// Clicking a band jumps to its headline slide.
    #[allow(clippy::too_many_arguments)]
    fn draw_section_bands(
        &mut self,
        frame: &mut Frame,
        y: u16,
        level: u8,
        x0: u16,
        pitch: u16,
        win_start: usize,
        win_end: usize,
    ) {
        let n = self.deck.steps.len();
        let cur_l1_step = self
            .outline
            .iter()
            .filter(|s| s.level == 1 && !s.title.is_empty() && s.headline_step <= self.step)
            .map(|s| s.headline_step)
            .next_back()
            .unwrap_or(0);
        // A subsection is only "current" inside the current section.
        let current_section = self.outline.iter().rposition(|s| {
            s.level == level
                && !s.title.is_empty()
                && s.headline_step <= self.step
                && (level == 1 || s.headline_step >= cur_l1_step)
        });
        for (si, section) in self.outline.iter().enumerate() {
            if section.title.is_empty() || section.level != level {
                continue;
            }
            // A band runs until the next headline at its level or above.
            let sec_end = self
                .outline
                .iter()
                .skip(si + 1)
                .find(|s| !s.title.is_empty() && s.level <= level)
                .map(|s| s.headline_step)
                .unwrap_or(n);
            let s = section.headline_step.max(win_start);
            let e = sec_end.min(win_end);
            if s >= e {
                continue;
            }
            let x = x0 + (s - win_start) as u16 * pitch;
            let width = (e - s) as u16 * pitch - 1;
            let clipped_left = section.headline_step < win_start;
            let clipped_right = sec_end > win_end;
            let lead = if clipped_left { '…' } else { '╶' };
            let trail = if clipped_right { '…' } else { '╴' };
            let label = fit(&section.title, usize::from(width).saturating_sub(4));
            let pad = usize::from(width)
                .saturating_sub(display_width(&label) + 3);
            let text = format!("{lead} {label} {}{trail}", "─".repeat(pad));
            let style = if Some(si) == current_section {
                Style::new().fg(ACCENT).bold()
            } else {
                Style::new().dim()
            };
            let rect = Rect {
                x,
                y,
                width,
                height: 1,
            };
            self.strip_boxes.push((rect, section.headline_step));
            frame.render_widget(Paragraph::new(text).style(style), rect);
        }
    }

    fn on_click(&mut self, x: u16, y: u16) {
        if !matches!(self.mode, Mode::Steps) {
            return;
        }
        let pos = Position { x, y };
        if let Some(&(_, step)) = self.strip_boxes.iter().find(|(rect, _)| rect.contains(pos)) {
            self.step = step;
            return;
        }
        let hit = self
            .sidebar_hits
            .iter()
            .find(|(rect, _)| rect.contains(pos))
            .map(|(_, target)| target.clone());
        match hit {
            Some(SideTarget::Step(step)) => {
                self.step = step;
            }
            Some(SideTarget::File(file)) => {
                self.dive_file = Some(file);
                self.mode = Mode::Dive {
                    scroll: 0,
                    cursor: 0,
                };
            }
            None => {}
        }
    }

    /// The changed-files panel: the deck's sections with the files each one
    /// points at, GitHub-files-tab style. Clicking a row jumps to the first
    /// slide about that file.
    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        self.sidebar_hits.clear();
        self.sidebar_items.clear();
        self.sidebar_area = area;
        frame.render_widget(
            Block::new()
                .borders(ratatui::widgets::Borders::RIGHT)
                .border_style(if self.focus == Focus::Sidebar {
                    Style::new().fg(ACCENT)
                } else {
                    Style::new().dim()
                }),
            area,
        );
        let inner_w = usize::from(area.width.saturating_sub(2));
        let current_file = self.current().anchor().map(|a| a.file.clone());
        // Nearest headline at or before the current slide, and its parent
        // section when the nearest one is a subsection.
        let nearest = self
            .outline
            .iter()
            .rposition(|s| !s.title.is_empty() && s.headline_step <= self.step);
        let parent = nearest.filter(|&i| self.outline[i].level == 2).and_then(|i| {
            self.outline[..i]
                .iter()
                .rposition(|s| s.level == 1 && !s.title.is_empty())
        });

        // A file row: shortened path, +/- counts, seen progress.
        let hashes = &self.hashes;
        let covered = &self.covered;
        let file_row = |path: &str,
                        indent: &str,
                        is_current: bool,
                        dim_row: bool,
                        seen: &SeenStore,
                        files: &[FileDiff]|
         -> Line<'static> {
            let fd = file_diff(files, path);
            let counts = fd.map(|fd| (fd.added(), fd.deleted()));
            let (s, t) = hashes
                .get(path)
                .map(|pairs| seen.progress_cached(path, pairs))
                .unwrap_or((0, 0));
            // Three facts per file: what the deck explains (and whether
            // that part is seen), the file's whole change, and how much
            // unexplained change is still unseen.
            let mut suffix: Vec<Span<'static>> = Vec::new();
            if t > 0 {
                if s == t {
                    suffix.push(Span::styled(" ✓".to_string(), Style::new().green()));
                } else {
                    let cov = covered.get(path);
                    let cov_tot = cov.map(|c| c.len()).unwrap_or(0);
                    let cov_seen = cov
                        .map(|c| {
                            c.iter()
                                .filter(|(h, i)| seen.is_seen(path, h, *i))
                                .count()
                        })
                        .unwrap_or(0);
                    let unexplained_unseen = (t - s).saturating_sub(cov_tot - cov_seen);
                    if cov_tot > 0 {
                        if cov_seen == cov_tot {
                            suffix.push(Span::styled(
                                format!(" ◆{cov_tot}✓"),
                                Style::new().green(),
                            ));
                        } else {
                            suffix.push(Span::styled(
                                format!(" ◆{cov_seen}/{cov_tot}"),
                                Style::new().fg(ACCENT),
                            ));
                        }
                    }
                    if unexplained_unseen > 0 {
                        suffix.push(Span::styled(
                            format!(" {unexplained_unseen}"),
                            Style::new().yellow(),
                        ));
                    }
                }
            }
            let count_text = match counts {
                Some((a, d)) if d > 0 => format!(" +{a} -{d}"),
                Some((a, _)) => format!(" +{a}"),
                None => String::new(),
            };
            let suffix_w: usize = suffix.iter().map(|s| display_width(&s.content)).sum();
            let name_w = inner_w
                .saturating_sub(count_text.len() + suffix_w + indent.len() + 1);
            let marker = if is_current { "▎" } else { " " };
            let dim_if = |s: Style| if dim_row { s.add_modifier(Modifier::DIM) } else { s };
            let mut spans = vec![
                Span::raw(indent.to_string()),
                Span::styled(
                    marker.to_string(),
                    if is_current { Style::new().fg(ACCENT) } else { Style::new() },
                ),
                Span::styled(
                    fit(path, name_w),
                    dim_if(if is_current { Style::new().bold() } else { Style::new() }),
                ),
            ];
            if let Some((a, d)) = counts {
                spans.push(Span::styled(format!(" +{a}"), Style::new().green().dim()));
                if d > 0 {
                    spans.push(Span::styled(format!(" -{d}"), Style::new().red().dim()));
                }
            }
            spans.extend(suffix);
            Line::from(spans)
        };

        // (line, click target, is-current-file)
        let mut rows: Vec<(Line, Option<SideTarget>, bool)> = Vec::new();
        let mut items: Vec<SideItem> = Vec::new();
        rows.push((
            Line::default(), // header, filled in once the hidden count is known
            None,
            false,
        ));
        let seen_store = &self.seen;
        let fully_seen = |path: &str| -> bool {
            hashes
                .get(path)
                .map(|pairs| {
                    let (s, t) = seen_store.progress_cached(path, pairs);
                    t > 0 && s == t
                })
                .unwrap_or(false)
        };
        let mut hidden = 0usize;
        for (si, section) in self.outline.iter().enumerate() {
            if !section.title.is_empty() {
                if section.level == 1 {
                    rows.push((Line::default(), None, false));
                }
                let style = if Some(si) == nearest {
                    Style::new().fg(ACCENT).bold()
                } else if Some(si) == parent {
                    Style::new().fg(ACCENT)
                } else if section.level == 1 {
                    Style::new().bold()
                } else {
                    Style::new()
                };
                let indent = if section.level == 1 { " " } else { "   " };
                items.push(SideItem {
                    row: rows.len(),
                    target: SideTarget::Step(section.headline_step),
                    file: None,
                });
                rows.push((
                    Line::from(format!(
                        "{indent}{}",
                        fit(&section.title, inner_w.saturating_sub(indent.len()))
                    ))
                    .style(style),
                    Some(SideTarget::Step(section.headline_step)),
                    false,
                ));
            } else {
                rows.push((Line::default(), None, false));
            }
            let file_indent = if section.level == 1 { "  " } else { "    " };
            for (path, first_step) in &section.files {
                if self.hide_seen && fully_seen(path) {
                    hidden += 1;
                    continue;
                }
                let is_current = current_file.as_deref() == Some(path.as_str());
                items.push(SideItem {
                    row: rows.len(),
                    target: SideTarget::Step(*first_step),
                    file: Some(path.clone()),
                });
                rows.push((
                    file_row(path, file_indent, is_current, false, &self.seen, &self.files),
                    Some(SideTarget::Step(*first_step)),
                    is_current,
                ));
            }
        }

        // Files the deck never points at — reachable, so "not looked at
        // yet" has somewhere to be looked at.
        let pointed: std::collections::HashSet<&str> = self
            .outline
            .iter()
            .flat_map(|s| s.files.iter().map(|(p, _)| p.as_str()))
            .collect();
        let mut rest: Vec<&FileDiff> = self
            .files
            .iter()
            .filter(|fd| !pointed.contains(fd.new_path.as_str()))
            .filter(|fd| fd.added() + fd.deleted() > 0)
            .collect();
        rest.sort_by_key(|fd| std::cmp::Reverse(fd.added() + fd.deleted()));
        if !rest.is_empty() {
            rows.push((Line::default(), None, false));
            rows.push((
                Line::from(format!(" not in deck ({})", rest.len()))
                    .style(Style::new().dim().italic()),
                None,
                false,
            ));
            for fd in rest {
                if self.hide_seen && fully_seen(&fd.new_path) {
                    hidden += 1;
                    continue;
                }
                let is_current = self.dive_file.as_deref() == Some(fd.new_path.as_str());
                items.push(SideItem {
                    row: rows.len(),
                    target: SideTarget::File(fd.new_path.clone()),
                    file: Some(fd.new_path.clone()),
                });
                rows.push((
                    file_row(&fd.new_path, "  ", is_current, true, &self.seen, &self.files),
                    Some(SideTarget::File(fd.new_path.clone())),
                    is_current,
                ));
            }
        }

        rows[0].0 = Line::from(format!(
            "{}{}",
            if self.focus == Focus::Sidebar {
                " j/k · enter open · v seen".to_string()
            } else {
                format!(" {} · tab", self.coverage.summary_short())
            },
            if self.hide_seen {
                format!(" · hiding {hidden} ✓")
            } else {
                String::new()
            },
        ))
        .style(Style::new().dim());

        // The focused row: the keyboard cursor when the sidebar has focus,
        // otherwise the current slide's file.
        self.sidebar_cursor = self.sidebar_cursor.min(items.len().saturating_sub(1));
        let focus_row = if self.focus == Focus::Sidebar {
            let row = items.get(self.sidebar_cursor).map(|i| i.row).unwrap_or(0);
            if let Some((line, _, _)) = rows.get_mut(row) {
                *line = line.clone().style(Style::new().bg(Color::DarkGray));
            }
            row
        } else {
            rows.iter().position(|(_, _, cur)| *cur).unwrap_or(0)
        };
        self.sidebar_items = items;

        // Keep the focused row in view.
        let height = usize::from(area.height);
        let offset = if rows.len() <= height {
            0
        } else {
            focus_row.saturating_sub(height / 2).min(rows.len() - height)
        };
        for (i, (line, target, _)) in rows.into_iter().enumerate().skip(offset).take(height) {
            let y = area.y + (i - offset) as u16;
            let rect = Rect {
                x: area.x,
                y,
                width: area.width.saturating_sub(1),
                height: 1,
            };
            if let Some(target) = target {
                self.sidebar_hits.push((rect, target));
            }
            frame.render_widget(Paragraph::new(line), rect);
        }
    }

    fn draw_step(&mut self, frame: &mut Frame, area: Rect) {
        match self.current().clone() {
            Step::Cover { what, bullets, .. } => self.draw_cover(frame, area, &what, &bullets),
            Step::Point { at, claim, notes, .. } => {
                self.draw_excerpt_slide(frame, area, &at, Some(&claim), &notes, None)
            }
            Step::Risk {
                at,
                claim,
                severity,
                notes,
                ..
            } => self.draw_excerpt_slide(frame, area, &at, Some(&claim), &notes, Some(severity)),
            Step::BeforeAfter { at, claim, .. } => {
                self.draw_before_after(frame, area, &at, claim.as_deref())
            }
            Step::Map { groups, .. } => self.draw_map(frame, area, &groups),
        }
    }

    fn draw_cover(&self, frame: &mut Frame, area: Rect, what: &str, bullets: &[String]) {
        // The first slide carries the deck title; later cover-shaped
        // slides are section headlines and stand on their own.
        let mut lines = if self.step == 0 {
            vec![
                Line::from(self.deck.title.clone().bold().fg(ACCENT)),
                Line::default(),
                Line::from(what.to_string().bold()),
            ]
        } else {
            vec![Line::from(what.to_string().bold().fg(ACCENT))]
        };
        if !bullets.is_empty() {
            lines.push(Line::default());
            for b in bullets {
                lines.push(Line::from(vec![
                    Span::styled("• ", Style::new().fg(ACCENT)),
                    Span::raw(b.clone()),
                ]));
            }
        }
        let height = (lines.len() as u16 + 2).min(area.height);
        let block = centered(area, 76.min(area.width.saturating_sub(2)), height);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).centered(),
            block,
        );
    }

    /// point and risk: claim with an accent bar, then the excerpt with
    /// notes hanging off their lines.
    fn draw_excerpt_slide(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        at: &Anchor,
        claim: Option<&str>,
        notes: &[Note],
        severity: Option<Severity>,
    ) {
        let body = draw_claim(frame, area, claim, severity);
        let rows = self.rows_for(at);
        if rows.is_empty() {
            render_note(frame, body, &format!("cannot read {}", at.file));
            return;
        }
        // Which of the shown changed lines are already marked seen.
        let hashes = self.hashes_of(&at.file);
        let seen_flags: Vec<bool> = rows
            .iter()
            .map(|r| match (r.hunk_idx, r.changed_idx) {
                (Some(h), Some(i)) => self.seen.is_seen(&at.file, &hashes[h].0, i),
                _ => false,
            })
            .collect();
        let shown_changed = seen_flags
            .iter()
            .zip(&rows)
            .filter(|(_, r)| r.changed_idx.is_some())
            .count();
        let shown_seen = seen_flags
            .iter()
            .zip(&rows)
            .filter(|(s, r)| **s && r.changed_idx.is_some())
            .count();

        let lang = Lang::from_path(&at.file);
        let mut header = file_header(&at.file);
        if shown_changed > 0 {
            if shown_seen == shown_changed {
                header.push_span(Span::styled(" · ✓ seen", Style::new().green()));
            } else if shown_seen > 0 {
                header.push_span(Span::styled(
                    format!(" · {shown_seen}/{shown_changed} seen"),
                    Style::new().dim(),
                ));
            }
        }
        let mut lines = vec![header];
        lines.extend(excerpt_lines(&rows, lang, notes, body.width, &seen_flags));
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn rows_for(&mut self, at: &Anchor) -> Vec<ExcerptRow> {
        let (start, end) = at.range();
        // Read the file first so the &mut borrow ends before we borrow the diff.
        let lines = self.file_lines(&at.file).map(|l| l.to_vec());
        let fd = file_diff(&self.files, &at.file);
        excerpt(fd, lines.as_deref(), start, end)
    }

    /// The seen addresses of the changed lines the current excerpt shows.
    fn visible_seen_lines(&mut self, at: &Anchor) -> Vec<(String, usize)> {
        let rows = self.rows_for(at);
        let hashes = self.hashes_of(&at.file);
        rows.iter()
            .filter_map(|r| match (r.hunk_idx, r.changed_idx) {
                (Some(h), Some(i)) => Some((hashes.get(h)?.0.clone(), i)),
                _ => None,
            })
            .collect()
    }

    /// The package `a` sends to an agent: everything the slide shows —
    /// claim, anchor, the excerpt as a real diff, notes — plus the
    /// question. Whoever receives it can open the code immediately.
    fn build_ask_prompt(&mut self, question: &str) -> String {
        let step = self.current().clone();
        let mut out = format!(
            "Question about a change, asked from a slidiff slide.\n\nDeck: {} (slide {}/{})\n",
            self.deck.title,
            self.step + 1,
            self.deck.steps.len(),
        );
        if let Some(claim) = match &step {
            Step::Point { claim, .. } | Step::Risk { claim, .. } => Some(claim.clone()),
            Step::BeforeAfter { claim, .. } => claim.clone(),
            Step::Cover { what, .. } => Some(what.clone()),
            Step::Map { .. } => None,
        } {
            out.push_str(&format!("Claim: {claim}\n"));
        }
        if let Some(at) = step.anchor() {
            out.push_str(&format!("At: {at}\n\n```diff\n"));
            for row in self.rows_for(at) {
                let sign = match row.kind {
                    LineKind::Context => ' ',
                    LineKind::Add => '+',
                    LineKind::Del => '-',
                };
                out.push(sign);
                out.push_str(&row.text);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        if !step.notes().is_empty() {
            out.push_str("\nLine notes:\n");
            for note in step.notes() {
                out.push_str(&format!("- {}: {}\n", note.line, note.text));
            }
        }
        if let Some(sn) = step.speaker_notes() {
            out.push_str(&format!("\nSpeaker notes:\n{sn}\n"));
        }
        let question = if question.trim().is_empty() {
            "この変更の意図と動作を説明してください。"
        } else {
            question
        };
        out.push_str(&format!("\nQuestion:\n{question}\n"));
        out
    }

    /// `v` on a slide: mark exactly what the excerpt shows. Completes
    /// first; a fully seen excerpt toggles back to unseen.
    fn slide_toggle_seen(&mut self) {
        let Some(at) = self.current().anchor().cloned() else {
            return;
        };
        let lines = self.visible_seen_lines(&at);
        if !lines.is_empty() {
            self.seen.toggle_lines(&at.file, &lines);
        }
    }

    fn draw_before_after(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        at: &Anchor,
        claim: Option<&str>,
    ) {
        let body = draw_claim(frame, area, claim, None);
        let rows = self.rows_for(at);
        if rows.is_empty() {
            render_note(frame, body, &format!("cannot read {}", at.file));
            return;
        }
        let lang = Lang::from_path(&at.file);
        let before: Vec<ExcerptRow> = rows
            .iter()
            .filter(|r| r.kind != LineKind::Add)
            .cloned()
            .collect();
        let after: Vec<ExcerptRow> = rows
            .iter()
            .filter(|r| r.kind != LineKind::Del)
            .cloned()
            .collect();
        let [l, gap, r] = Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Length(2),
            Constraint::Percentage(50),
        ])
        .areas(body);
        let _ = gap;
        let side = |title: String, rows: &[ExcerptRow], width: u16| {
            let mut lines = vec![Line::from(title).style(Style::new().bold().dim())];
            lines.extend(excerpt_lines(rows, lang, &[], width, &vec![false; rows.len()]));
            Paragraph::new(lines)
        };
        frame.render_widget(side(format!("── before · {}", at.file), &before, l.width), l);
        frame.render_widget(side("── after".to_string(), &after, r.width), r);
    }

    fn draw_map(&mut self, frame: &mut Frame, area: Rect, groups: &[Group]) {
        struct RowData {
            label: String,
            files: usize,
            added: usize,
            deleted: usize,
            rest: bool,
        }
        let mut rows: Vec<RowData> = Vec::new();
        let mut claimed = vec![false; self.files.len()];
        for g in groups {
            let mut files = 0;
            let mut added = 0;
            let mut deleted = 0;
            for (i, fd) in self.files.iter().enumerate() {
                let hit = g.files.iter().any(|entry| {
                    if let Some(dir) = entry.strip_suffix('/') {
                        fd.new_path.starts_with(dir)
                    } else {
                        fd.new_path == *entry
                    }
                });
                if hit {
                    claimed[i] = true;
                    files += 1;
                    added += fd.added();
                    deleted += fd.deleted();
                }
            }
            rows.push(RowData {
                label: g.label.clone(),
                files,
                added,
                deleted,
                rest: false,
            });
        }
        let rest: Vec<&FileDiff> = self
            .files
            .iter()
            .enumerate()
            .filter(|(i, _)| !claimed[*i])
            .map(|(_, f)| f)
            .collect();
        if !rest.is_empty() {
            rows.push(RowData {
                label: "other".to_string(),
                files: rest.len(),
                added: rest.iter().map(|f| f.added()).sum(),
                deleted: rest.iter().map(|f| f.deleted()).sum(),
                rest: true,
            });
        }

        let label_width = rows
            .iter()
            .map(|r| display_width(&r.label))
            .max()
            .unwrap_or(4);
        let max_delta = rows.iter().map(|r| r.added + r.deleted).max().unwrap_or(1).max(1);

        let mut lines = vec![
            Line::from(self.deck.title.clone().bold().fg(ACCENT)),
            Line::from(self.coverage.summary().dim()),
            Line::default(),
        ];
        for row in &rows {
            let pad = " ".repeat(label_width - display_width(&row.label) + 2);
            let bar_len = ((row.added + row.deleted) * 20).div_ceil(max_delta).max(1);
            let label_style = if row.rest {
                Style::new().dim()
            } else {
                Style::new().bold()
            };
            let dim_if_rest = |s: Style| if row.rest { s.add_modifier(Modifier::DIM) } else { s };
            lines.push(Line::from(vec![
                Span::styled(row.label.clone(), label_style),
                Span::raw(pad),
                Span::styled(
                    format!("{:>3} file{} ", row.files, if row.files == 1 { " " } else { "s" }),
                    Style::new().dim(),
                ),
                Span::styled(format!("{:>7} ", format!("+{}", row.added)), dim_if_rest(Style::new().green())),
                Span::styled(format!("{:>6}  ", format!("-{}", row.deleted)), dim_if_rest(Style::new().red())),
                Span::styled("▇".repeat(bar_len), dim_if_rest(Style::new().fg(ACCENT))),
            ]));
        }
        if !self.coverage.untouched_top.is_empty() {
            lines.push(Line::default());
            let mut spans = vec![Span::styled("no step points at: ", Style::new().red().dim())];
            for (i, (name, added)) in self.coverage.untouched_top.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" · ", Style::new().dim()));
                }
                spans.push(Span::styled(
                    format!("{name} +{added}"),
                    Style::new().dim(),
                ));
            }
            spans.push(Span::styled(" …", Style::new().dim()));
            lines.push(Line::from(spans));
        }
        let width = (label_width + 44).min(area.width as usize) as u16;
        let height = (lines.len() as u16).min(area.height);
        frame.render_widget(Paragraph::new(lines), centered(area, width, height));
    }

    /// Which files the dive shows: an explicitly opened file, the current
    /// step's file, or — from a slide with no anchor — everything.
    fn dive_paths(&self) -> Vec<String> {
        if let Some(f) = &self.dive_file {
            return vec![f.clone()];
        }
        match self.current().anchor() {
            Some(at) => vec![at.file.clone()],
            None => self.files.iter().map(|f| f.new_path.clone()).collect(),
        }
    }

    /// Row metadata for the dive, exactly parallel to its display rows.
    fn dive_meta(&self) -> Vec<DiveMeta> {
        let mut meta = Vec::new();
        for path in self.dive_paths() {
            let Some(fd) = file_diff(&self.files, &path) else {
                continue;
            };
            let pairs = self.hashes_of(&fd.new_path);
            for (hunk_idx, hunk) in fd.hunks.iter().enumerate() {
                let (hash, total) = match pairs.get(hunk_idx) {
                    Some((h, c)) => (h.clone(), *c),
                    None => (hunk_hash(hunk), changed_count(hunk)),
                };
                meta.push(DiveMeta::header());
                let mut changed_idx = 0;
                for line in &hunk.lines {
                    let idx = (line.kind != LineKind::Context).then(|| {
                        let i = changed_idx;
                        changed_idx += 1;
                        i
                    });
                    meta.push(DiveMeta {
                        file: Some(fd.new_path.clone()),
                        hunk_hash: Some(hash.clone()),
                        changed_idx: idx,
                        changed_total: total,
                        new_no: line.new_no,
                    });
                }
                meta.push(DiveMeta::header());
            }
        }
        meta
    }

    fn enter_dive(&mut self) {
        let cursor = match self.current().anchor() {
            Some(at) => self
                .dive_meta()
                .iter()
                .position(|m| m.new_no == Some(at.focus()))
                .unwrap_or(0),
            None => 0,
        };
        self.mode = Mode::Dive { scroll: 0, cursor };
    }

    fn dive_toggle_line(&mut self) {
        let Mode::Dive { cursor, .. } = self.mode else {
            return;
        };
        let meta = self.dive_meta();
        let Some(m) = meta.get(cursor) else { return };
        if let (Some(file), Some(hash), Some(idx)) = (&m.file, &m.hunk_hash, m.changed_idx) {
            self.seen.toggle_line(file, hash, idx);
            // Move on to the next changed line — marking flows downward.
            if let Some(next) = meta
                .iter()
                .enumerate()
                .skip(cursor + 1)
                .find(|(_, m)| m.changed_idx.is_some())
                .map(|(i, _)| i)
                && let Mode::Dive { cursor, .. } = &mut self.mode
            {
                *cursor = next;
            }
        }
    }

    fn dive_toggle_hunk(&mut self) {
        let Mode::Dive { cursor, .. } = self.mode else {
            return;
        };
        let meta = self.dive_meta();
        let Some(m) = meta.get(cursor) else { return };
        if let (Some(file), Some(hash)) = (&m.file, &m.hunk_hash) {
            self.seen.toggle_hunk(file, hash, m.changed_total);
        }
    }

    fn hashes_of(&self, file: &str) -> &[(String, usize)] {
        self.hashes.get(file).map(Vec::as_slice).unwrap_or(&[])
    }

    /// (seen, total) changed lines across the whole diff.
    fn seen_overall(&self) -> (usize, usize) {
        self.files
            .iter()
            .map(|fd| self.seen.progress_cached(&fd.new_path, self.hashes_of(&fd.new_path)))
            .fold((0, 0), |(s, t), (a, b)| (s + a, t + b))
    }

    fn draw_dive(&mut self, frame: &mut Frame, area: Rect) {
        let paths = self.dive_paths();
        let shown: Vec<&FileDiff> = paths
            .iter()
            .filter_map(|p| file_diff(&self.files, p))
            .collect();
        if shown.is_empty() {
            let msg = match paths.first() {
                Some(p) if paths.len() == 1 => format!("{p} is not in this diff"),
                _ => "no changes in this diff".to_string(),
            };
            render_note(frame, area, &msg);
            return;
        }

        // Build display rows; must stay parallel to dive_meta().
        let meta = self.dive_meta();
        let Mode::Dive { scroll, cursor } = &mut self.mode else {
            return;
        };
        *cursor = (*cursor).min(meta.len().saturating_sub(1));
        let mut lines: Vec<Line> = Vec::new();
        for fd in shown {
            let lang = Lang::from_path(&fd.new_path);
            let pairs = self.hashes.get(fd.new_path.as_str());
            for (hunk_idx, hunk) in fd.hunks.iter().enumerate() {
                let (hash, h_total) = match pairs.and_then(|p| p.get(hunk_idx)) {
                    Some((h, c)) => (h.clone(), *c),
                    None => (hunk_hash(hunk), changed_count(hunk)),
                };
                let h_seen = (0..h_total)
                    .filter(|&i| self.seen.is_seen(&fd.new_path, &hash, i))
                    .count();
                let mut header = format!("── {}", fd.new_path);
                if !hunk.section.is_empty() {
                    header.push_str(&format!(" · {}", hunk.section));
                }
                let mut spans = vec![Span::styled(header, Style::new().bold().dim())];
                if h_total > 0 {
                    let (text, style) = if h_seen == h_total {
                        (" ✓ seen".to_string(), Style::new().green())
                    } else {
                        (format!(" {h_seen}/{h_total}"), Style::new().dim())
                    };
                    spans.push(Span::styled(text, style));
                }
                lines.push(Line::from(spans));
                let segs = emphasize_hunk(hunk);
                let mut changed_idx = 0usize;
                for (line, seg) in hunk.lines.iter().zip(segs) {
                    let is_changed = line.kind != LineKind::Context;
                    let line_seen = is_changed
                        && self.seen.is_seen(&fd.new_path, &hash, changed_idx);
                    let mark = if line_seen {
                        Span::styled("✓ ", Style::new().green().dim())
                    } else {
                        Span::raw("  ")
                    };
                    let no = format!(
                        "{:>5} ",
                        line.new_no
                            .or(line.old_no)
                            .map(|n| n.to_string())
                            .unwrap_or_default()
                    );
                    let mut spans = vec![mark, Span::styled(no, Style::new().dim())];
                    if line_seen {
                        // A seen change sinks like context: the vivid tint
                        // is reserved for what is still unreviewed.
                        spans.extend(code_spans(lang, &line.text, LineKind::Context, None));
                    } else {
                        spans.extend(code_spans(lang, &line.text, line.kind, Some(&seg)));
                    }
                    let mut out = Line::from(spans);
                    if lines.len() == *cursor {
                        out = out.style(Style::new().bg(Color::DarkGray));
                    }
                    lines.push(out);
                    if is_changed {
                        changed_idx += 1;
                    }
                }
                lines.push(Line::default());
            }
        }

        // Keep the cursor in view.
        let height = usize::from(area.height);
        if *cursor < *scroll {
            *scroll = *cursor;
        } else if *cursor >= *scroll + height {
            *scroll = *cursor + 1 - height;
        }
        *scroll = (*scroll).min(lines.len().saturating_sub(height.max(1)));
        frame.render_widget(
            Paragraph::new(lines).scroll((*scroll as u16, 0)),
            area,
        );
    }
}

/// One dive display row. `changed_idx` is the line's index among its
/// hunk's changed lines — the unit of the seen record.
struct DiveMeta {
    file: Option<String>,
    hunk_hash: Option<String>,
    changed_idx: Option<usize>,
    changed_total: usize,
    new_no: Option<u32>,
}

impl DiveMeta {
    fn header() -> DiveMeta {
        DiveMeta {
            file: None,
            hunk_hash: None,
            changed_idx: None,
            changed_total: 0,
            new_no: None,
        }
    }
}

// ---- shared rendering helpers ---------------------------------------

/// The claim: an accent bar, bold text, no box. Returns the body area.
fn draw_claim(
    frame: &mut Frame,
    area: Rect,
    claim: Option<&str>,
    severity: Option<Severity>,
) -> Rect {
    let Some(claim) = claim else {
        return area;
    };
    let wrap_width = area.width.saturating_sub(4).max(1) as usize;
    let rows = display_width(claim).div_ceil(wrap_width).clamp(1, 3) as u16;
    let [top, _, body] = Layout::vertical([
        Constraint::Length(rows),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    let [bar, text] =
        Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(top);
    let (bar_style, tag) = match severity {
        Some(s) => (
            Style::new().fg(severity_color(s)),
            Some(Span::styled(
                format!(" {s} "),
                Style::new().fg(Color::Black).bg(severity_color(s)).bold(),
            )),
        ),
        None => (Style::new().fg(ACCENT), None),
    };
    frame.render_widget(
        Paragraph::new("▌\n".repeat(rows as usize)).style(bar_style),
        bar,
    );
    let mut spans = Vec::new();
    if let Some(tag) = tag {
        spans.push(tag);
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(claim.to_string(), Style::new().bold()));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
        text,
    );
    body
}

fn severity_color(s: Severity) -> Color {
    match s {
        Severity::Low => Color::Yellow,
        Severity::Medium => Color::LightYellow,
        Severity::High => Color::Red,
    }
}

fn file_header<'a>(path: &str) -> Line<'a> {
    Line::from(format!("── {path}")).style(Style::new().dim())
}

/// Excerpt rows as slide lines: no signs, no numbers — added and deleted
/// lines carry a background tint the full width of the slide instead.
/// Notes hang under their lines.
fn excerpt_lines<'a>(
    rows: &[ExcerptRow],
    lang: Lang,
    notes: &[Note],
    width: u16,
    seen: &[bool],
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let row_seen = seen.get(i).copied().unwrap_or(false);
        let mut spans = if row_seen && row.kind != LineKind::Context {
            // Reviewed change sinks like context; the tint is reserved
            // for what is still unreviewed.
            code_spans(lang, &row.text, LineKind::Context, None)
        } else {
            code_spans(lang, &row.text, row.kind, row.segments.as_deref())
        };
        // Extend the tint across the slide so a changed line reads as a band.
        if row.kind != LineKind::Context && !row_seen {
            let used = display_width(&row.text);
            let target = width as usize;
            if used < target {
                let bg = if row.kind == LineKind::Add { ADD_BG } else { DEL_BG };
                spans.push(Span::styled(
                    " ".repeat(target - used),
                    Style::new().bg(bg),
                ));
            }
        }
        out.push(Line::from(spans));
        if let Some(no) = row.new_no {
            for note in notes.iter().filter(|n| n.line == no) {
                let indent: String = row
                    .text
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                out.push(Line::from(vec![
                    Span::raw(indent),
                    Span::styled("└ ", Style::new().fg(ACCENT)),
                    Span::styled(note.text.clone(), Style::new().fg(ACCENT).bold()),
                ]));
            }
        }
    }
    out
}

/// Syntax-highlighted spans for one code line, adjusted by its diff role:
/// context sinks (DIM), additions sit on a green tint, deletions on a red
/// tint, and changed words in a del/add pair get a stronger tint + bold.
/// A line that is entirely new (no partner) is a plain add — emphasizing
/// all of it would just be noise.
fn code_spans<'a>(
    lang: Lang,
    text: &str,
    kind: LineKind,
    segments: Option<&[Segment]>,
) -> Vec<Span<'a>> {
    let runs = highlight(lang, text);
    match kind {
        LineKind::Context => runs
            .into_iter()
            .map(|run| Span::styled(run.text, run.style.add_modifier(Modifier::DIM)))
            .collect(),
        LineKind::Add | LineKind::Del => {
            let (bg, emph_bg) = if kind == LineKind::Add {
                (ADD_BG, ADD_EMPH_BG)
            } else {
                (DEL_BG, DEL_EMPH_BG)
            };
            let paired = segments
                .filter(|segs| !segs.iter().all(|s| s.emph))
                .filter(|segs| segs.iter().any(|s| s.emph));
            let Some(segs) = paired else {
                return runs
                    .into_iter()
                    .map(|run| {
                        let mut style = run.style.bg(bg);
                        if kind == LineKind::Del {
                            style = style.add_modifier(Modifier::DIM);
                        }
                        Span::styled(run.text, style)
                    })
                    .collect();
            };
            // Overlay word emphasis on the syntax runs: mark each char,
            // then rebuild runs with the stronger tint where emphasized.
            let mut emph_flags = Vec::with_capacity(text.chars().count());
            for seg in segs {
                emph_flags.extend(std::iter::repeat_n(seg.emph, seg.text.chars().count()));
            }
            let mut spans = Vec::new();
            let mut idx = 0;
            for run in runs {
                let mut cur = String::new();
                let mut cur_emph = None;
                for c in run.text.chars() {
                    let e = emph_flags.get(idx).copied().unwrap_or(false);
                    idx += 1;
                    if cur_emph == Some(e) || cur.is_empty() {
                        cur_emph = Some(e);
                        cur.push(c);
                    } else {
                        spans.push(tint_span(cur, run.style, cur_emph.unwrap_or(false), bg, emph_bg));
                        cur = c.to_string();
                        cur_emph = Some(e);
                    }
                }
                if !cur.is_empty() {
                    spans.push(tint_span(cur, run.style, cur_emph.unwrap_or(false), bg, emph_bg));
                }
            }
            spans
        }
    }
}

fn tint_span<'a>(text: String, base: Style, emph: bool, bg: Color, emph_bg: Color) -> Span<'a> {
    if emph {
        Span::styled(text, base.bg(emph_bg).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(text, base.bg(bg))
    }
}

/// Truncate from the left, keeping the tail (the basename side of a path),
/// with a leading ellipsis when cut.
fn fit(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut w = 1;
    for c in s.chars().rev() {
        let cw = if (c as u32) > 0x1100 { 2 } else { 1 };
        if w + cw > max {
            break;
        }
        w += cw;
        tail.push(c);
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("…{tail}")
}

/// Rough display width: wide for CJK, 1 otherwise. Good enough for layout.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) > 0x1100 { 2 } else { 1 })
        .sum()
}

/// The fixed layout of the steps screen, derived from the terminal size
/// alone. Content never changes it — a slide is a fixed canvas, and prose
/// that does not fit belongs in the notes, not on a stretched page.
struct Geometry {
    page_w: u16,
    page_h: u16,
    notes_h: u16,
    strip_h: u16,
}

impl Geometry {
    fn of(area: Rect, band_rows: u16) -> Geometry {
        // Extra strip rows carry the section/subsection bands over the boxes.
        let strip_h = if area.height >= 22 { 3 + band_rows } else { 0 };
        let page_w = area.width.saturating_sub(10).clamp(20, 96);
        // 16:9-ish: terminal cells are ~1:2, so rows ≈ cols × 9⁄32.
        let mut page_h = ((u32::from(page_w) * 9 / 32) as u16).max(10);
        let mut notes_h: u16 = 7;
        let avail = area.height.saturating_sub(strip_h + 1 + 2);
        if page_h + notes_h > avail {
            page_h = page_h.min(avail.saturating_sub(notes_h)).max(8);
        }
        if page_h + notes_h > avail {
            notes_h = avail.saturating_sub(page_h);
        }
        Geometry {
            page_w,
            page_h,
            notes_h,
            strip_h,
        }
    }
}

/// Center a fixed width horizontally in `area`.
fn center_h(area: Rect, width: u16) -> Rect {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(area);
    mid
}

/// The speaker notes directly under the slide: prose the writer kept off
/// the page, markdown-rendered, aligned to the slide. Quiet chrome.
fn draw_speaker_notes(frame: &mut Frame, area: Rect, page_w: u16, text: &str) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(crate::md::render(text))
            .wrap(Wrap { trim: false })
            .block(
                Block::new()
                    .borders(ratatui::widgets::Borders::TOP)
                    .border_style(Style::new().dim())
                    .title(Span::styled(" notes · s to expand ", Style::new().dim())),
            ),
        center_h(area, page_w),
    );
}

/// The expanded notes: a popup floating over the slide, for reading a
/// long note comfortably. `s` again or Esc dismisses it.
fn draw_notes_popup(frame: &mut Frame, area: Rect, text: &str) {
    let width = area.width.saturating_sub(8).clamp(20, 80);
    let lines = crate::md::render(text);
    let wrap_w = usize::from(width.saturating_sub(4).max(1));
    let rows: usize = lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|s| display_width(&s.content)).sum();
            w.div_ceil(wrap_w).max(1)
        })
        .sum();
    let height = (rows as u16 + 2).min(area.height.saturating_mul(3) / 4).max(3);
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let popup = center_h(mid, width);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(ACCENT))
                    .title(Span::styled(" notes · s to hide ", Style::new().fg(ACCENT))),
            ),
        popup,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(2),
    ])
    .areas(area);
    let [_, mid, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(mid);
    mid
}

/// Hand the question to whoever is listening: the command in
/// SLIDIFF_ASK_CMD (prompt on stdin, run through `sh -c`), or failing
/// that the clipboard via OSC 52 — works in any modern terminal, even
/// over ssh.
fn deliver_ask(prompt: &str) -> String {
    if let Ok(cmd) = std::env::var("SLIDIFF_ASK_CMD") {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let spawned = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(prompt.as_bytes());
            }
            if child.wait().is_ok_and(|s| s.success()) {
                return "question sent".to_string();
            }
        }
        return "ask command failed — question copied instead".to_string();
    }
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(prompt.as_bytes()));
    let _ = out.flush();
    "question copied to clipboard".to_string()
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The question input floating over the slide.
fn draw_ask_popup(frame: &mut Frame, area: Rect, buf: &str, context: &str) {
    let width = area.width.saturating_sub(8).clamp(20, 80);
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .areas(area);
    let popup = center_h(mid, width);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(context.to_string()).style(Style::new().dim()),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(ACCENT)),
            Span::raw(buf.to_string()),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ACCENT))
                .title(Span::styled(
                    " ask an agent · enter send · esc cancel ",
                    Style::new().fg(ACCENT),
                )),
        ),
        popup,
    );
}

/// The keyboard cheat sheet (`?`). Any key dismisses it.
fn draw_help(frame: &mut Frame, area: Rect) {
    let rows: &[(&str, &str)] = &[
        ("slides", ""),
        ("n / p, ← →", "next / previous slide"),
        ("↑ / ↓", "walk the file list, incl. not in deck"),
        ("v", "mark the shown change seen"),
        ("a", "ask an agent about this slide"),
        ("s", "speaker notes: panel → popup → hidden"),
        ("f", "toggle the file sidebar"),
        ("h", "hide fully seen files"),
        ("tab", "focus the sidebar (j/k · enter · v)"),
        ("enter", "dive into the full diff"),
        ("file list", ""),
        ("◆ 5/9", "deck-explained lines: seen / shown"),
        ("yellow n", "unexplained lines still unseen"),
        ("✓", "whole file seen"),
        ("dive", ""),
        ("n / p", "move the cursor"),
        ("v / V", "mark line / hunk seen"),
        ("enter / esc", "back to the slides"),
        ("everywhere", ""),
        ("?", "this help"),
        ("q", "quit · mouse: click and wheel work"),
    ];
    let width = 58u16.min(area.width.saturating_sub(4));
    let height = (rows.len() as u16 + 2).min(area.height);
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let popup = center_h(mid, width);
    frame.render_widget(Clear, popup);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, what)| {
            if what.is_empty() {
                Line::from(format!("─ {key} ─")).style(Style::new().dim())
            } else {
                Line::from(vec![
                    Span::styled(format!(" {key:<12}"), Style::new().fg(ACCENT)),
                    Span::raw((*what).to_string()),
                ])
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().dim())
                .title(Span::styled(" keys ", Style::new().dim())),
        ),
        popup,
    );
}

fn render_note(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(text.to_string()).style(Style::new().italic().dim()),
        area,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::diff::parse_unified;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,4 @@ impl Session {
     fn close(&mut self) {
-        self.map.remove(&id);
+        self.map.take(&id);
         self.notify();
 }
";

    fn app_with(steps: Vec<Step>) -> App {
        let files = parse_unified(SAMPLE);
        let hashes = hash_cache(&files);
        let deck = Deck {
            title: "Test deck".into(),
            base: None,
            steps,
        };
        let coverage = Coverage::compute(&deck, &files);
        let outline = outline_of(&deck);
        let covered = covered_map(
            &deck,
            &files,
            &hashes,
            &Repo {
                root: PathBuf::from("/nonexistent"),
            },
        );
        App {
            deck,
            outline,
            files_view: true,
            hide_seen: false,
            sidebar_hits: Vec::new(),
            repo: Repo {
                root: PathBuf::from("/nonexistent"),
            },
            files,
            coverage,
            step: 0,
            mode: Mode::Steps,
            notes_view: NotesView::Panel,
            file_cache: HashMap::new(),
            seen: SeenStore::in_memory(),
            dive_file: None,
            sidebar_area: Rect::default(),
            focus: Focus::Slides,
            ask: None,
            help: false,
            toast: None,
            sidebar_cursor: 0,
            sidebar_items: Vec::new(),
            strip_boxes: Vec::new(),
            git_dir: None,
            deck_key: None,
            hashes,
            covered,
        }
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 36)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn point_shows_claim_excerpt_note_and_chrome() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:10-13".parse().unwrap(),
            claim: "take() returns the value".into(),
            notes: vec![Note {
                line: 11,
                text: "caller now owns it".into(),
            }],
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("▌"), "accent bar missing:\n{s}");
        assert!(s.contains("take() returns the value"), "claim missing:\n{s}");
        assert!(s.contains("self.map.take(&id);"), "add line missing:\n{s}");
        assert!(s.contains("self.map.remove(&id);"), "del line missing:\n{s}");
        assert!(s.contains("└ caller now owns it"), "note missing:\n{s}");
        assert!(s.contains("╭"), "page frame missing:\n{s}");
        assert!(!app.strip_boxes.is_empty(), "filmstrip hit areas missing");
        assert!(s.contains("covers +1/+1"), "coverage missing:\n{s}");
        // No sign column, no line numbers on slides.
        assert!(!s.contains("+ self.map"), "sign column should be gone:\n{s}");
    }

    #[test]
    fn unpaired_add_gets_no_word_emphasis() {
        let text = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,3 @@
 ctx
+let brand_new = 1;
 tail
";
        let hunk = &parse_unified(text)[0].hunks[0];
        let segs = emphasize_hunk(hunk);
        let spans = code_spans(Lang::Rust, "let brand_new = 1;", LineKind::Add, Some(&segs[1]));
        assert!(
            spans.iter().all(|sp| !sp.style.add_modifier.contains(Modifier::BOLD)
                && sp.style.bg == Some(ADD_BG)),
            "whole-line adds must stay plain: {spans:?}"
        );
    }

    #[test]
    fn paired_change_gets_stronger_tint_on_changed_word() {
        let files = parse_unified(SAMPLE);
        let hunk = &files[0].hunks[0];
        let segs = emphasize_hunk(hunk);
        let spans = code_spans(Lang::Rust, &hunk.lines[2].text, LineKind::Add, Some(&segs[2]));
        assert!(
            spans.iter().any(|sp| sp.style.bg == Some(ADD_EMPH_BG)),
            "changed word should get emphasis tint: {spans:?}"
        );
        assert!(
            spans.iter().all(|sp| !sp.style.add_modifier.contains(Modifier::UNDERLINED)),
            "no underline anywhere: {spans:?}"
        );
    }

    #[test]
    fn cover_shows_title_and_bullets() {
        let mut app = app_with(vec![Step::Cover {
            what: "What was done".into(),
            bullets: vec!["first".into(), "second".into()],
            level: 1,
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("Test deck"), "{s}");
        assert!(s.contains("• first"), "{s}");
    }

    #[test]
    fn map_aggregates_groups_and_reports_coverage() {
        let mut app = app_with(vec![Step::Map {
            groups: vec![Group {
                label: "core".into(),
                files: vec!["src/".into()],
            }],
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("core"), "{s}");
        assert!(s.contains("1 file"), "{s}");
        assert!(s.contains("covers +0/+1"), "{s}");
        assert!(s.contains("no step points at: lib.rs +1"), "{s}");
    }

    #[test]
    fn before_after_splits_old_and_new() {
        let mut app = app_with(vec![Step::BeforeAfter {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: None,
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("── before"), "{s}");
        assert!(s.contains("── after"), "{s}");
        assert!(s.contains("remove"), "{s}");
        assert!(s.contains("take"), "{s}");
    }

    #[test]
    fn risk_shows_severity_tag() {
        let mut app = app_with(vec![Step::Risk {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "contract change".into(),
            severity: Severity::High,
            notes: vec![],
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains(" high "), "{s}");
        assert!(s.contains("contract change"), "{s}");
    }

    #[test]
    fn dive_shows_full_hunk_with_line_numbers() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        app.mode = Mode::Dive { scroll: 0, cursor: 0 };
        let s = screen(&mut app);
        assert!(s.contains("self.notify();"), "{s}");
        assert!(s.contains("11"), "dive keeps line numbers:\n{s}");
        assert!(s.contains("enter back"), "{s}");
    }

    #[test]
    fn speaker_notes_show_below_the_slide_and_toggle_off() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "short claim".into(),
            notes: vec![],
            speaker_notes: Some("the **long** prose lands under the slide".into()),
        }]);
        let s = screen(&mut app);
        assert!(s.contains("the long prose lands under the slide"), "{s}");
        assert!(s.contains(" notes · s to expand "), "{s}");
        app.notes_view = NotesView::Popup;
        let s = screen(&mut app);
        assert!(s.contains(" notes · s to hide "), "popup missing:\n{s}");
        assert!(s.contains("the long prose"), "{s}");
        app.notes_view = NotesView::Hidden;
        let s = screen(&mut app);
        assert!(!s.contains("long prose"), "notes should hide:\n{s}");
    }

    #[test]
    fn page_is_slide_shaped_not_full_height() {
        let mut app = app_with(vec![Step::Cover {
            what: "w".into(),
            bullets: vec![],
            level: 1,
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        let first_border = s.lines().position(|l| l.contains("╭─")).unwrap();
        assert!(first_border > 0, "page should not start at row 0:\n{s}");
    }

    #[test]
    fn page_geometry_never_shifts_between_steps() {
        let mut app = app_with(vec![
            Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "with notes".into(),
                notes: vec![],
                speaker_notes: Some("some prose\nover two lines".into()),
            },
            Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "without notes".into(),
                notes: vec![],
                speaker_notes: None,
            },
        ]);
        let page_top = |s: &str| {
            s.lines()
                .position(|l| l.contains("╭──────────"))
                .expect("page border")
        };
        let first = page_top(&screen(&mut app));
        app.step = 1;
        let second = page_top(&screen(&mut app));
        assert_eq!(first, second, "page moved between steps");
        app.notes_view = NotesView::Hidden;
        let third = page_top(&screen(&mut app));
        assert_eq!(first, third, "page moved when notes toggled");
    }

    #[test]
    fn sidebar_lists_section_files_and_click_jumps() {
        let mut app = app_with(vec![
            Step::Cover {
                what: "w".into(),
                bullets: vec![],
                level: 1,
                speaker_notes: None,
            },
            Step::Cover {
                what: "section one".into(),
                bullets: vec![],
                level: 1,
                speaker_notes: None,
            },
            Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "c".into(),
                notes: vec![],
                speaker_notes: None,
            },
        ]);
        let s = screen(&mut app);
        assert!(s.contains("covers +1/+1 · tab"), "sidebar header missing:\n{s}");
        assert!(s.contains("section one"), "section title missing:\n{s}");
        assert!(s.contains("src/lib.rs +1 -1"), "file row missing:\n{s}");
        let (rect, target) = app
            .sidebar_hits
            .iter()
            .find(|(_, t)| *t == SideTarget::Step(2))
            .cloned()
            .expect("file row hit area");
        app.on_click(rect.x + 2, rect.y);
        assert_eq!(SideTarget::Step(app.step), target);

        app.files_view = false;
        let s = screen(&mut app);
        assert!(!s.contains("src/lib.rs +1 -1"), "sidebar should hide:\n{s}");
    }

    #[test]
    fn filmstrip_windows_around_current_for_long_decks() {
        let steps: Vec<Step> = (0..60)
            .map(|_| Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "c".into(),
                notes: vec![],
                speaker_notes: None,
            })
            .collect();
        let mut app = app_with(steps);
        app.step = 30;
        let s = screen(&mut app);
        assert!(s.contains("31"), "current box missing:\n{s}");
        assert!(s.contains("…"), "continuation marks missing:\n{s}");
        assert!(!s.contains("│ 1 │"), "far-away boxes should be off-window");
        // Every visible box is a click target.
        assert!(!app.strip_boxes.is_empty());
        assert!(app.strip_boxes.iter().any(|(_, i)| *i == 30));
    }

    #[test]
    fn section_band_spans_its_slides_and_click_jumps_to_headline() {
        let point = |claim: &str| Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: claim.into(),
            notes: vec![],
            speaker_notes: None,
        };
        let mut app = app_with(vec![
            Step::Cover {
                what: "w".into(),
                bullets: vec![],
                level: 1,
                speaker_notes: None,
            },
            Step::Cover {
                what: "part one".into(),
                bullets: vec![],
                level: 1,
                speaker_notes: None,
            },
            point("a"),
            point("b"),
        ]);
        app.step = 2;
        let s = screen(&mut app);
        assert!(s.contains("╶ part one"), "band missing:\n{s}");
        // The band is a click target for its headline.
        let (rect, target) = *app
            .strip_boxes
            .iter()
            .find(|(_, t)| *t == 1 && matches!(app.deck.steps[1], Step::Cover { .. }))
            .expect("band hit area");
        app.on_click(rect.x + 1, rect.y);
        assert_eq!(app.step, target);
    }

    #[test]
    fn subsections_indent_in_sidebar_and_get_their_own_band_row() {
        let point = |claim: &str| Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: claim.into(),
            notes: vec![],
            speaker_notes: None,
        };
        let headline = |what: &str, level: u8| Step::Cover {
            what: what.into(),
            bullets: vec![],
            level,
            speaker_notes: None,
        };
        let mut app = app_with(vec![
            headline("intro", 1),
            headline("part one", 1),
            headline("sub a", 2),
            point("x"),
            headline("sub b", 2),
            point("y"),
        ]);
        app.step = 3; // inside sub a
        let s = screen(&mut app);
        assert!(s.contains("   sub a"), "indented subsection missing:\n{s}");
        assert!(s.contains("╶ part one"), "level-1 band missing:\n{s}");
        assert!(s.contains("╶ sub a"), "level-2 band missing:\n{s}");
        // The level-2 band row sits below the level-1 band row.
        let row_of = |needle: &str| s.lines().position(|l| l.contains(needle)).unwrap();
        assert!(row_of("╶ part one") < row_of("╶ sub a"), "{s}");
        // sub b has not started yet, so its band is not the lit one; its
        // headline is still clickable via the strip.
        assert!(app.strip_boxes.iter().any(|(_, t)| *t == 4));
    }

    #[test]
    fn down_arrow_steps_into_the_file_list_at_the_current_file() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        screen(&mut app); // populate sidebar_items
        assert!(app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            )))
            .is_ok());
        assert_eq!(app.focus, Focus::Sidebar);
        // Landed relative to the current file's row and stepped once.
        let base = app
            .sidebar_items
            .iter()
            .position(|i| i.file.as_deref() == Some("src/lib.rs"))
            .unwrap();
        assert_eq!(app.sidebar_cursor, (base + 1).min(app.sidebar_items.len() - 1));
        // ← returns to the slides.
        assert!(app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::NONE,
            )))
            .is_ok());
        assert_eq!(app.focus, Focus::Slides);
    }

    #[test]
    fn clicking_a_filmstrip_box_jumps_to_that_step() {
        let mut app = app_with(vec![
            Step::Cover {
                what: "w".into(),
                bullets: vec![],
                level: 1,
                speaker_notes: None,
            },
            Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "c".into(),
                notes: vec![],
                speaker_notes: None,
            },
        ]);
        screen(&mut app); // draw once to populate hit areas
        assert_eq!(app.strip_boxes.len(), 2);
        let (second_box, idx) = app.strip_boxes[1];
        assert_eq!(idx, 1);
        app.on_click(second_box.x + 1, second_box.y + 1);
        assert_eq!(app.step, 1);
        // A click outside every box changes nothing.
        app.on_click(0, 0);
        assert_eq!(app.step, 1);
    }

    #[test]
    fn v_marks_a_changed_line_seen_and_advances() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        app.enter_dive();
        // enter_dive lands the cursor on the anchor line (new_no 11, the add).
        let meta = app.dive_meta();
        let Mode::Dive { cursor: at_anchor, .. } = app.mode else { panic!() };
        assert_eq!(meta[at_anchor].new_no, Some(11));
        assert!(meta[at_anchor].changed_idx.is_some());

        // Start from the first changed line (the deletion) so v can advance.
        let cursor = meta.iter().position(|m| m.changed_idx == Some(0)).unwrap();
        app.mode = Mode::Dive { scroll: 0, cursor };
        app.dive_toggle_line();
        let s = screen(&mut app);
        assert!(s.contains("✓"), "seen mark missing:\n{s}");
        assert!(s.contains("seen 1/2"), "status count missing:\n{s}");
        // The cursor advanced past the toggled line.
        let Mode::Dive { cursor: after, .. } = app.mode else { panic!() };
        assert!(after > cursor);
        // Back on the slides, the sidebar splits the file into deck-explained
        // (with its own progress) and unexplained-unseen parts.
        app.mode = Mode::Steps;
        let s = screen(&mut app);
        assert!(s.contains("◆1/2"), "deck-explained progress missing:\n{s}");

        // V on a row of the same hunk marks the rest, toggling to full.
        app.mode = Mode::Dive { scroll: 0, cursor };
        app.dive_toggle_hunk();
        let s = screen(&mut app);
        assert!(s.contains("seen 2/2"), "{s}");
        assert!(s.contains("✓ seen"), "hunk header check missing:\n{s}");
    }

    #[test]
    fn sidebar_shows_seen_progress_and_not_in_deck_opens_dive() {
        // Deck points at nothing → src/lib.rs lands in "not in deck".
        let mut app = app_with(vec![Step::Cover {
            what: "w".into(),
            bullets: vec![],
            level: 1,
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("not in deck (1)"), "{s}");
        let (rect, _) = app
            .sidebar_hits
            .iter()
            .find(|(_, t)| matches!(t, SideTarget::File(_)))
            .cloned()
            .expect("file target");
        app.on_click(rect.x + 2, rect.y);
        assert!(matches!(app.mode, Mode::Dive { .. }));
        assert_eq!(app.dive_file.as_deref(), Some("src/lib.rs"));
        let s = screen(&mut app);
        assert!(s.contains("self.map.take(&id);"), "dive should show the file:\n{s}");

        // Mark everything seen; the sidebar row gains a checkmark.
        app.mode = Mode::Dive { scroll: 0, cursor: 2 };
        app.dive_toggle_hunk();
        app.mode = Mode::Steps;
        app.dive_file = None;
        let s = screen(&mut app);
        assert!(s.contains("-1 ✓"), "sidebar checkmark missing:\n{s}");
    }

    #[test]
    fn sidebar_focus_navigates_and_v_marks_whole_file() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        screen(&mut app); // populate sidebar_items
        assert!(!app.sidebar_items.is_empty());
        app.focus = Focus::Sidebar;
        // Walk the cursor to the file row of src/lib.rs.
        app.sidebar_cursor = app
            .sidebar_items
            .iter()
            .position(|i| i.file.as_deref() == Some("src/lib.rs"))
            .expect("file item reachable by keyboard");
        let s = screen(&mut app);
        assert!(s.contains("v seen"), "focused hint missing:\n{s}");

        // v marks the whole file, without entering the dive.
        let path = "src/lib.rs".to_string();
        let fd = file_diff(&app.files, &path).cloned().unwrap();
        app.seen.toggle_file(&fd);
        assert_eq!(app.seen.progress_for(&fd), (2, 2));
        let s = screen(&mut app);
        assert!(s.contains("seen 2/2"), "status total missing:\n{s}");
        assert!(s.contains("✓"), "file checkmark missing:\n{s}");
    }

    #[test]
    fn slide_v_marks_only_the_visible_range() {
        // Range 10-11 shows the del (changed 0) and the add (changed 1)
        // but stops before the rest of the hunk.
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:10-11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        app.slide_toggle_seen();
        let fd = file_diff(&app.files, "src/lib.rs").cloned().unwrap();
        assert_eq!(app.seen.progress_for(&fd), (2, 2));
        let s = screen(&mut app);
        assert!(s.contains("✓ seen"), "header check missing:\n{s}");
        // Toggling again clears exactly the same range.
        app.slide_toggle_seen();
        assert_eq!(app.seen.progress_for(&fd), (0, 2));

        // A slide showing only unchanged context marks nothing.
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:13".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        let at: Anchor = "src/lib.rs:13-13".parse().unwrap();
        let lines = app.visible_seen_lines(&at);
        assert!(lines.is_empty());
    }

    #[test]
    fn ask_prompt_carries_slide_context_and_diff() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:10-12".parse().unwrap(),
            claim: "the claim".into(),
            notes: vec![Note {
                line: 11,
                text: "a note".into(),
            }],
            speaker_notes: Some("long prose".into()),
        }]);
        let prompt = app.build_ask_prompt("なぜ remove ではなく take?");
        assert!(prompt.contains("Deck: Test deck (slide 1/1)"), "{prompt}");
        assert!(prompt.contains("Claim: the claim"), "{prompt}");
        assert!(prompt.contains("At: src/lib.rs:10-12"), "{prompt}");
        assert!(prompt.contains("```diff"), "{prompt}");
        assert!(prompt.contains("-        self.map.remove(&id);"), "{prompt}");
        assert!(prompt.contains("+        self.map.take(&id);"), "{prompt}");
        assert!(prompt.contains("- 11: a note"), "{prompt}");
        assert!(prompt.contains("Speaker notes:\nlong prose"), "{prompt}");
        assert!(prompt.contains("なぜ remove ではなく take?"), "{prompt}");
        // Empty question falls back to the default.
        let prompt = app.build_ask_prompt("  ");
        assert!(prompt.contains("この変更の意図と動作"), "{prompt}");
    }

    #[test]
    fn ask_popup_and_help_overlay_render() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        app.ask = Some("why".into());
        let s = screen(&mut app);
        assert!(s.contains(" ask an agent · enter send · esc cancel "), "{s}");
        assert!(s.contains("> why▏"), "{s}");

        app.ask = None;
        app.help = true;
        let s = screen(&mut app);
        assert!(s.contains(" keys "), "{s}");
        assert!(s.contains("ask an agent about this slide"), "{s}");
        assert!(s.contains("mark line / hunk seen"), "{s}");
    }

    #[test]
    fn base64_matches_the_spec() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn h_hides_fully_seen_files_from_the_sidebar() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        let fd = file_diff(&app.files, "src/lib.rs").cloned().unwrap();
        app.seen.toggle_file(&fd);
        let s = screen(&mut app);
        assert!(s.contains("src/lib.rs"), "{s}");

        app.hide_seen = true;
        let s = screen(&mut app);
        assert!(
            !s.contains("▎src/lib.rs") && !s.contains(" src/lib.rs +1"),
            "fully seen file should be hidden:\n{s}"
        );
        assert!(s.contains("hiding 1 ✓"), "hidden count missing:\n{s}");
        assert!(
            !app.sidebar_items.iter().any(|i| i.file.is_some()),
            "hidden file must leave the keyboard list too"
        );
    }

    #[test]
    fn v_in_hide_mode_does_not_advance_past_the_vanishing_row() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        app.hide_seen = true;
        screen(&mut app); // populate items
        app.focus = Focus::Sidebar;
        app.sidebar_cursor = app
            .sidebar_items
            .iter()
            .position(|i| i.file.as_deref() == Some("src/lib.rs"))
            .unwrap();
        let before = app.sidebar_cursor;
        assert!(app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::NONE,
            )))
            .is_ok());
        // Marked fully seen; the row will vanish, so the cursor stays.
        assert_eq!(app.sidebar_cursor, before);
        let fd = file_diff(&app.files, "src/lib.rs").cloned().unwrap();
        assert_eq!(app.seen.progress_for(&fd), (2, 2));

        // With hide off, the same v advances as before.
        app.hide_seen = false;
        screen(&mut app);
        app.focus = Focus::Sidebar;
        app.sidebar_cursor = app
            .sidebar_items
            .iter()
            .position(|i| i.file.as_deref() == Some("src/lib.rs"))
            .unwrap();
        let before = app.sidebar_cursor;
        let _ = app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::NONE,
        )));
        assert_eq!(
            app.sidebar_cursor,
            (before + 1).min(app.sidebar_items.len() - 1)
        );
    }

    #[test]
    fn missing_file_falls_back_to_note_not_panic() {
        let mut app = app_with(vec![Step::Point {
            at: "src/gone.rs:5".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
            speaker_notes: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("cannot read src/gone.rs"), "{s}");
    }
}

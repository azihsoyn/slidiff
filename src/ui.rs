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

pub fn run(deck: Deck, repo: Repo) -> Result<()> {
    let files = load_diff(&repo, deck.base.as_deref())?;
    let coverage = Coverage::compute(&deck, &files);
    let mut app = App {
        deck,
        repo,
        files,
        coverage,
        step: 0,
        mode: Mode::Steps,
        notes_view: NotesView::Panel,
        file_cache: HashMap::new(),
        strip_boxes: Vec::new(),
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
    Dive { scroll: u16 },
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
    file_cache: HashMap<String, Option<Vec<String>>>,
    /// Filmstrip hit areas, refreshed on every draw: (box, step index).
    strip_boxes: Vec<(Rect, usize)>,
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
}

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let key = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => key,
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            self.on_click(mouse.column, mouse.row);
                        }
                        MouseEventKind::ScrollDown => match &mut self.mode {
                            Mode::Steps => {
                                if self.step + 1 < self.deck.steps.len() {
                                    self.step += 1;
                                }
                            }
                            Mode::Dive { scroll } => *scroll = scroll.saturating_add(3),
                        },
                        MouseEventKind::ScrollUp => match &mut self.mode {
                            Mode::Steps => self.step = self.step.saturating_sub(1),
                            Mode::Dive { scroll } => *scroll = scroll.saturating_sub(3),
                        },
                        _ => {}
                    }
                    continue;
                }
                _ => continue,
            };
            let ctrl_c = key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_c || key.code == KeyCode::Char('q') {
                return Ok(());
            }
            match &mut self.mode {
                Mode::Steps => match key.code {
                    KeyCode::Char('n' | 'j' | ' ') | KeyCode::Right | KeyCode::Down => {
                        if self.step + 1 < self.deck.steps.len() {
                            self.step += 1;
                        }
                    }
                    KeyCode::Char('p' | 'k') | KeyCode::Left | KeyCode::Up => {
                        self.step = self.step.saturating_sub(1);
                    }
                    KeyCode::Char('s') => self.notes_view = self.notes_view.next(),
                    KeyCode::Enter => self.mode = Mode::Dive { scroll: 0 },
                    KeyCode::Esc => {
                        if self.notes_view == NotesView::Popup {
                            self.notes_view = NotesView::Panel;
                        } else {
                            return Ok(());
                        }
                    }
                    _ => {}
                },
                Mode::Dive { scroll } => match key.code {
                    KeyCode::Char('n' | 'j') | KeyCode::Down => {
                        *scroll = scroll.saturating_add(3);
                    }
                    KeyCode::Char('p' | 'k') | KeyCode::Up => {
                        *scroll = scroll.saturating_sub(3);
                    }
                    KeyCode::Enter | KeyCode::Esc => self.mode = Mode::Steps,
                    _ => {}
                },
            }
        }
    }

    fn current(&self) -> &Step {
        &self.deck.steps[self.step]
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
                // Fixed geometry, derived from the terminal size alone:
                // the page never moves or resizes between slides.
                let geo = Geometry::of(area);
                let [_, page_row, notes_row, _, strip, status] = Layout::vertical([
                    Constraint::Fill(1),
                    Constraint::Length(geo.page_h),
                    Constraint::Length(geo.notes_h),
                    Constraint::Fill(2),
                    Constraint::Length(geo.strip_h),
                    Constraint::Length(1),
                ])
                .areas(area);
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
            }
            Mode::Dive { scroll } => {
                let [body, status] =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                        .areas(frame.area());
                self.draw_dive(frame, body, scroll);
                let hint = " dive · n/p scroll · enter back · q quit ";
                frame.render_widget(
                    Paragraph::new(hint).style(Style::new().dim()).right_aligned(),
                    status,
                );
            }
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let step = self.current();
        let mut left = format!(" {}/{} · {}", self.step + 1, self.deck.steps.len(), step.type_name());
        if let Some(at) = step.anchor() {
            left.push_str(&format!(" · {at}"));
        }
        left.push_str(&format!(" · {}", self.coverage.summary()));
        let right = "n next · p prev · s notes · enter diff · q quit ";
        let [l, r] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(right.len() as u16),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(left).style(Style::new().dim()), l);
        frame.render_widget(Paragraph::new(right).style(Style::new().dim()), r);
    }

    /// One little numbered box per step, current one lit — the outline the
    /// reader keeps in the corner of their eye. Clicking a box jumps there.
    fn draw_filmstrip(&mut self, frame: &mut Frame, area: Rect) {
        self.strip_boxes.clear();
        let n = self.deck.steps.len() as u16;
        let box_w: u16 = 6;
        let total = n * (box_w + 1) - 1;
        if area.width >= total {
            let x0 = area.x + (area.width - total) / 2;
            for i in 0..self.deck.steps.len() {
                let rect = Rect {
                    x: x0 + i as u16 * (box_w + 1),
                    y: area.y,
                    width: box_w,
                    height: 3,
                };
                self.strip_boxes.push((rect, i));
                let current = i == self.step;
                let style = if current {
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
        } else {
            let labels: Vec<String> =
                (1..=self.deck.steps.len()).map(|i| format!(" {i} ")).collect();
            let total: u16 = labels.iter().map(|l| l.len() as u16).sum();
            let y = area.y + area.height / 2;
            let mut x = area.x + area.width.saturating_sub(total) / 2;
            let mut spans = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                let w = label.len() as u16;
                self.strip_boxes.push((
                    Rect {
                        x,
                        y,
                        width: w,
                        height: 1,
                    },
                    i,
                ));
                x += w;
                if i == self.step {
                    spans.push(Span::styled(
                        label.clone(),
                        Style::new().fg(Color::Black).bg(ACCENT),
                    ));
                } else {
                    spans.push(Span::styled(label.clone(), Style::new().dim()));
                }
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)).centered(),
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
        }
    }

    fn on_click(&mut self, x: u16, y: u16) {
        if !matches!(self.mode, Mode::Steps) {
            return;
        }
        let pos = Position { x, y };
        if let Some(&(_, step)) = self
            .strip_boxes
            .iter()
            .find(|(rect, _)| rect.contains(pos))
        {
            self.step = step;
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
        let lang = Lang::from_path(&at.file);
        let mut lines = vec![file_header(&at.file)];
        lines.extend(excerpt_lines(&rows, lang, notes, body.width));
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn rows_for(&mut self, at: &Anchor) -> Vec<ExcerptRow> {
        let (start, end) = at.range();
        // Read the file first so the &mut borrow ends before we borrow the diff.
        let lines = self.file_lines(&at.file).map(|l| l.to_vec());
        let fd = file_diff(&self.files, &at.file);
        excerpt(fd, lines.as_deref(), start, end)
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
            lines.extend(excerpt_lines(rows, lang, &[], width));
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

    fn draw_dive(&mut self, frame: &mut Frame, area: Rect, scroll: u16) {
        let target = self.current().anchor().cloned();
        let files: Vec<&FileDiff> = match &target {
            Some(at) => self.files.iter().filter(|f| f.new_path == at.file).collect(),
            None => self.files.iter().collect(),
        };
        if files.is_empty() {
            let msg = match &target {
                Some(at) => format!("{} is not in this diff", at.file),
                None => "no changes in this diff".to_string(),
            };
            render_note(frame, area, &msg);
            return;
        }
        let mut lines: Vec<Line> = Vec::new();
        for fd in files {
            let lang = Lang::from_path(&fd.new_path);
            for hunk in &fd.hunks {
                let mut header = format!("── {}", fd.new_path);
                if !hunk.section.is_empty() {
                    header.push_str(&format!(" · {}", hunk.section));
                }
                lines.push(Line::from(header).style(Style::new().bold().dim()));
                let segs = emphasize_hunk(hunk);
                for (line, seg) in hunk.lines.iter().zip(segs) {
                    let no = format!(
                        "{:>5} ",
                        line.new_no
                            .or(line.old_no)
                            .map(|n| n.to_string())
                            .unwrap_or_default()
                    );
                    let mut spans = vec![Span::styled(no, Style::new().dim())];
                    spans.extend(code_spans(lang, &line.text, line.kind, Some(&seg)));
                    let mut out = Line::from(spans);
                    if let Some(t) = &target
                        && line.new_no == Some(t.focus()) {
                            out = out.style(Style::new().bg(Color::DarkGray));
                        }
                    lines.push(out);
                }
                lines.push(Line::default());
            }
        }
        let max = (lines.len() as u16).saturating_sub(area.height);
        frame.render_widget(Paragraph::new(lines).scroll((scroll.min(max), 0)), area);
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
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    for row in rows {
        let mut spans = code_spans(lang, &row.text, row.kind, row.segments.as_deref());
        // Extend the tint across the slide so a changed line reads as a band.
        if row.kind != LineKind::Context {
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
    fn of(area: Rect) -> Geometry {
        let strip_h = if area.height >= 22 { 3 } else { 0 };
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
        let deck = Deck {
            title: "Test deck".into(),
            base: None,
            steps,
        };
        let coverage = Coverage::compute(&deck, &files);
        App {
            deck,
            repo: Repo {
                root: PathBuf::from("/nonexistent"),
            },
            files,
            coverage,
            step: 0,
            mode: Mode::Steps,
            notes_view: NotesView::Panel,
            file_cache: HashMap::new(),
            strip_boxes: Vec::new(),
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
        app.mode = Mode::Dive { scroll: 0 };
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
    fn clicking_a_filmstrip_box_jumps_to_that_step() {
        let mut app = app_with(vec![
            Step::Cover {
                what: "w".into(),
                bullets: vec![],
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

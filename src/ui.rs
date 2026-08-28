//! The viewer. Two modes only: one step per screen, and a dive into the
//! full diff behind the current step. Keys: n/p step, Enter dive/back,
//! q quit.
//!
//! Slide anatomy: a one-line claim with an accent bar, then the excerpt —
//! syntax-highlighted, context dimmed hard, notes hanging off their lines
//! rustc-style. No line numbers on slides; those live in the dive.

use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::deck::{Anchor, Deck, Note, Severity, Step};
use crate::diff::{
    ExcerptRow, FileDiff, LineKind, Repo, Segment, emphasize_hunk, excerpt,
    file_diff, load_diff,
};
use crate::highlight::{Lang, highlight};

pub fn run(deck: Deck, repo: Repo) -> Result<()> {
    let files = load_diff(&repo, deck.base.as_deref())?;
    let mut app = App {
        deck,
        repo,
        files,
        step: 0,
        mode: Mode::Steps,
        file_cache: HashMap::new(),
    };
    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

enum Mode {
    Steps,
    Dive { scroll: u16 },
}

struct App {
    deck: Deck,
    repo: Repo,
    files: Vec<FileDiff>,
    step: usize,
    mode: Mode,
    file_cache: HashMap<String, Option<Vec<String>>>,
}

const ACCENT: Color = Color::Cyan;

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
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
                    KeyCode::Enter => self.mode = Mode::Dive { scroll: 0 },
                    KeyCode::Esc => return Ok(()),
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
        let [body, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        match self.mode {
            Mode::Steps => {
                self.draw_step(frame, body);
                self.draw_status(frame, status);
            }
            Mode::Dive { scroll } => {
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
        let right = "n next · p prev · enter diff · q quit ";
        let [l, r] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(right.len() as u16),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(left).style(Style::new().dim()), l);
        frame.render_widget(Paragraph::new(right).style(Style::new().dim()), r);
    }

    fn draw_step(&mut self, frame: &mut Frame, area: Rect) {
        match self.current().clone() {
            Step::Cover { what, bullets } => self.draw_cover(frame, area, &what, &bullets),
            Step::Point { at, claim, notes } => {
                self.draw_excerpt_slide(frame, area, &at, Some(&claim), &notes, None)
            }
            Step::Risk {
                at,
                claim,
                severity,
                notes,
            } => self.draw_excerpt_slide(frame, area, &at, Some(&claim), &notes, Some(severity)),
            Step::BeforeAfter { at, claim } => {
                self.draw_before_after(frame, area, &at, claim.as_deref())
            }
            Step::Map { groups } => self.draw_map(frame, area, &groups),
        }
    }

    fn draw_cover(&self, frame: &mut Frame, area: Rect, what: &str, bullets: &[String]) {
        let mut lines = vec![
            Line::from(self.deck.title.clone().bold().fg(ACCENT)),
            Line::default(),
            Line::from(what.to_string().bold()),
        ];
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
        let block = centered(area, 76.min(area.width.saturating_sub(4)), height);
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
        lines.extend(excerpt_lines(&rows, lang, notes, at.focus()));
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
        let [l, r] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(body);
        let side = |title: &str, rows: &[ExcerptRow]| {
            let mut lines = vec![Line::from(title.to_string()).style(Style::new().bold().dim())];
            lines.extend(excerpt_lines(rows, lang, &[], 0));
            Paragraph::new(lines)
        };
        frame.render_widget(side(&format!("── before · {}", at.file), &before), l);
        frame.render_widget(side("── after", &after), r);
    }

    fn draw_map(&mut self, frame: &mut Frame, area: Rect, groups: &[crate::deck::Group]) {
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
                label: "その他".to_string(),
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
        let width = (label_width + 42).min(area.width as usize) as u16;
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
                    spans.insert(1, sign_span(line.kind));
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

/// Excerpt rows as slide lines: sign column, syntax colors, context dimmed,
/// notes hanging under their lines.
fn excerpt_lines<'a>(
    rows: &[ExcerptRow],
    lang: Lang,
    notes: &[Note],
    _focus: u32,
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    for row in rows {
        let mut spans = vec![sign_span(row.kind)];
        spans.extend(code_spans(lang, &row.text, row.kind, row.segments.as_deref()));
        out.push(Line::from(spans));
        if let Some(no) = row.new_no {
            for note in notes.iter().filter(|n| n.line == no) {
                let indent: String = row
                    .text
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(indent),
                    Span::styled("└ ", Style::new().fg(ACCENT)),
                    Span::styled(note.text.clone(), Style::new().fg(ACCENT).bold()),
                ]));
            }
        }
    }
    out
}

fn sign_span<'a>(kind: LineKind) -> Span<'a> {
    match kind {
        LineKind::Context => Span::raw("  "),
        LineKind::Add => Span::styled("+ ", Style::new().green().bold()),
        LineKind::Del => Span::styled("- ", Style::new().red().dim()),
    }
}

/// Syntax-highlighted spans for one code line, adjusted by its diff role:
/// context sinks (DIM), deletions go quiet red, additions keep full syntax
/// color with changed words bold-underlined.
fn code_spans<'a>(
    lang: Lang,
    text: &str,
    kind: LineKind,
    segments: Option<&[Segment]>,
) -> Vec<Span<'a>> {
    match kind {
        LineKind::Del => {
            let base = Style::new().fg(Color::Red).add_modifier(Modifier::DIM);
            let emph = Style::new().fg(Color::Red);
            match segments {
                Some(segs) => segs
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), if s.emph { emph } else { base }))
                    .collect(),
                None => vec![Span::styled(text.to_string(), base)],
            }
        }
        LineKind::Context => highlight(lang, text)
            .into_iter()
            .map(|run| Span::styled(run.text, run.style.add_modifier(Modifier::DIM)))
            .collect(),
        LineKind::Add => {
            let runs = highlight(lang, text);
            let Some(segs) = segments else {
                return runs
                    .into_iter()
                    .map(|run| Span::styled(run.text, run.style))
                    .collect();
            };
            // Overlay word emphasis on the syntax runs: mark each char,
            // then rebuild runs with BOLD+UNDERLINED where emphasized.
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
                        spans.push(emph_span(cur, run.style, cur_emph.unwrap_or(false)));
                        cur = c.to_string();
                        cur_emph = Some(e);
                    }
                }
                if !cur.is_empty() {
                    spans.push(emph_span(cur, run.style, cur_emph.unwrap_or(false)));
                }
            }
            spans
        }
    }
}

fn emph_span<'a>(text: String, base: Style, emph: bool) -> Span<'a> {
    if emph {
        Span::styled(
            text,
            base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
    } else {
        Span::styled(text, base)
    }
}

/// Rough display width: wide for CJK, 1 otherwise. Good enough for layout.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) > 0x1100 { 2 } else { 1 })
        .sum()
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
    use crate::deck::Group;
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
        App {
            deck: Deck {
                title: "Test deck".into(),
                base: None,
                steps,
            },
            repo: Repo {
                root: PathBuf::from("/nonexistent"),
            },
            files: parse_unified(SAMPLE),
            step: 0,
            mode: Mode::Steps,
            file_cache: HashMap::new(),
        }
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
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
    fn point_shows_claim_excerpt_and_note() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:10-13".parse().unwrap(),
            claim: "take() returns the value".into(),
            notes: vec![Note {
                line: 11,
                text: "caller now owns it".into(),
            }],
        }]);
        let s = screen(&mut app);
        assert!(s.contains("▌"), "accent bar missing:\n{s}");
        assert!(s.contains("take() returns the value"), "claim missing:\n{s}");
        assert!(s.contains("self.map.take(&id);"), "add line missing:\n{s}");
        assert!(s.contains("self.map.remove(&id);"), "del line missing:\n{s}");
        assert!(s.contains("└ caller now owns it"), "note missing:\n{s}");
        assert!(s.contains("1/1 · point · src/lib.rs:10-13"), "status missing:\n{s}");
        // No line-number gutter on slides.
        assert!(!s.contains(" 10 "), "line numbers should not appear:\n{s}");
    }

    #[test]
    fn cover_shows_title_and_bullets() {
        let mut app = app_with(vec![Step::Cover {
            what: "What was done".into(),
            bullets: vec!["first".into(), "second".into()],
        }]);
        let s = screen(&mut app);
        assert!(s.contains("Test deck"), "{s}");
        assert!(s.contains("• first"), "{s}");
    }

    #[test]
    fn before_after_splits_old_and_new() {
        let mut app = app_with(vec![Step::BeforeAfter {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("── before"), "{s}");
        assert!(s.contains("── after"), "{s}");
        assert!(s.contains("remove"), "{s}");
        assert!(s.contains("take"), "{s}");
    }

    #[test]
    fn map_aggregates_groups_and_rest() {
        let mut app = app_with(vec![Step::Map {
            groups: vec![Group {
                label: "core".into(),
                files: vec!["src/".into()],
            }],
        }]);
        let s = screen(&mut app);
        assert!(s.contains("core"), "{s}");
        assert!(s.contains("1 file"), "{s}");
        assert!(s.contains("+1"), "{s}");
        assert!(s.contains("-1"), "{s}");
        assert!(!s.contains("その他"), "no rest row expected:\n{s}");
    }

    #[test]
    fn risk_shows_severity_tag() {
        let mut app = app_with(vec![Step::Risk {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "contract change".into(),
            severity: Severity::High,
            notes: vec![],
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
        }]);
        app.mode = Mode::Dive { scroll: 0 };
        let s = screen(&mut app);
        assert!(s.contains("self.notify();"), "{s}");
        assert!(s.contains("11"), "dive keeps line numbers:\n{s}");
        assert!(s.contains("enter back"), "{s}");
    }

    #[test]
    fn missing_file_falls_back_to_note_not_panic() {
        let mut app = app_with(vec![Step::Point {
            at: "src/gone.rs:5".parse().unwrap(),
            claim: "c".into(),
            notes: vec![],
        }]);
        let s = screen(&mut app);
        assert!(s.contains("cannot read src/gone.rs"), "{s}");
    }
}

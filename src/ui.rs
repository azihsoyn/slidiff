//! The viewer. Two modes only: one step per screen, and a dive into the
//! full diff behind the current step. Keys: n/p step, Enter dive/back,
//! q quit.

use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::deck::{Deck, Location, Severity, Step};
use crate::diff::{
    FileDiff, FileStatus, Hunk, LineKind, Repo, Segment, emphasize_hunk, file_diff, hunk_at,
    load_diff,
};

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
        if let Some(at) = step.location() {
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
            Step::Point { at, claim } => {
                self.draw_claim_and_hunk(frame, area, &at, Some(&claim), None)
            }
            Step::Risk { at, claim, severity } => {
                self.draw_claim_and_hunk(frame, area, &at, Some(&claim), Some(severity))
            }
            Step::BeforeAfter { at, claim } => {
                self.draw_before_after(frame, area, &at, claim.as_deref())
            }
            Step::Zoom { at, claim } => self.draw_zoom(frame, area, &at, claim.as_deref()),
            Step::Map => self.draw_map(frame, area),
        }
    }

    fn draw_cover(&self, frame: &mut Frame, area: Rect, what: &str, bullets: &[String]) {
        let mut lines = vec![
            Line::from(self.deck.title.clone().bold().cyan()),
            Line::default(),
            Line::from(what.to_string()),
        ];
        if !bullets.is_empty() {
            lines.push(Line::default());
            for b in bullets {
                lines.push(Line::from(format!("• {b}")));
            }
        }
        let height = (lines.len() as u16 + 2).min(area.height);
        let block = centered(area, 72.min(area.width.saturating_sub(4)), height);
        frame.render_widget(Clear, block);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).centered(),
            block,
        );
    }

    /// point and risk: claim on top, the hunk at `at` below.
    fn draw_claim_and_hunk(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        at: &Location,
        claim: Option<&str>,
        severity: Option<Severity>,
    ) {
        let body = draw_claim(frame, area, claim, severity);
        let Some(fd) = file_diff(&self.files, &at.file) else {
            self.draw_zoom_body(frame, body, at, "not in this diff — showing the file");
            return;
        };
        if fd.binary {
            render_note(frame, body, "binary file");
            return;
        }
        let Some((hunk, exact)) = hunk_at(fd, at.line) else {
            render_note(frame, body, "file has no hunks");
            return;
        };
        let mut lines = vec![hunk_header(fd, hunk)];
        if !exact {
            lines.push(
                Line::from(format!("(line {} is outside every hunk — nearest shown)", at.line))
                    .style(Style::new().yellow().italic()),
            );
        }
        lines.extend(hunk_lines(hunk, Some(at.line)));
        let scroll = scroll_to(&lines, body.height, |l| {
            line_is_target(hunk, at.line, l)
        });
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body);
    }

    fn draw_before_after(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        at: &Location,
        claim: Option<&str>,
    ) {
        let body = draw_claim(frame, area, claim, None);
        let Some(fd) = file_diff(&self.files, &at.file) else {
            self.draw_zoom_body(frame, body, at, "not in this diff — showing the file");
            return;
        };
        let Some((hunk, _)) = hunk_at(fd, at.line) else {
            render_note(frame, body, "file has no hunks");
            return;
        };
        let segs = emphasize_hunk(hunk);
        let mut before: Vec<Line> = Vec::new();
        let mut after: Vec<Line> = Vec::new();
        for (line, seg) in hunk.lines.iter().zip(&segs) {
            match line.kind {
                LineKind::Context => {
                    before.push(side_line(line.old_no, seg, Style::new()));
                    after.push(side_line(line.new_no, seg, Style::new()));
                }
                LineKind::Del => before.push(side_line(line.old_no, seg, Style::new().red())),
                LineKind::Add => after.push(side_line(line.new_no, seg, Style::new().green())),
            }
        }
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(body);
        let title_style = Style::new().dim();
        frame.render_widget(
            Paragraph::new(before).block(
                Block::new()
                    .borders(Borders::TOP | Borders::RIGHT)
                    .title(Span::styled(" before ", title_style)),
            ),
            l,
        );
        frame.render_widget(
            Paragraph::new(after).block(
                Block::new()
                    .borders(Borders::TOP)
                    .title(Span::styled(" after ", title_style)),
            ),
            r,
        );
    }

    fn draw_zoom(&mut self, frame: &mut Frame, area: Rect, at: &Location, claim: Option<&str>) {
        let body = draw_claim(frame, area, claim, None);
        self.draw_zoom_body(frame, body, at, "");
    }

    fn draw_zoom_body(&mut self, frame: &mut Frame, area: Rect, at: &Location, note: &str) {
        let added: Vec<u32> = file_diff(&self.files, &at.file)
            .map(|fd| {
                fd.hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == LineKind::Add)
                    .filter_map(|l| l.new_no)
                    .collect()
            })
            .unwrap_or_default();
        let Some(lines) = self.file_lines(&at.file) else {
            render_note(frame, area, &format!("cannot read {}", at.file));
            return;
        };
        let total = lines.len() as u32;
        let height = area.height.saturating_sub(1).max(1) as u32;
        let target = at.line.min(total.max(1));
        let start = target.saturating_sub(height / 2).max(1);
        let mut out = vec![
            Line::from(format!("── {} {}", at.file, note))
                .style(Style::new().bold().dim()),
        ];
        for no in start..(start + height).min(total + 1) {
            let text = &lines[(no - 1) as usize];
            let changed = added.binary_search(&no).is_ok();
            let gutter = Span::styled(
                format!("{no:>5}{} ", if changed { "+" } else { " " }),
                if changed {
                    Style::new().green()
                } else {
                    Style::new().dim()
                },
            );
            let mut line = Line::from(vec![gutter, Span::raw(text.clone())]);
            if no == at.line {
                line = line.style(Style::new().bg(Color::DarkGray));
            }
            out.push(line);
        }
        frame.render_widget(Paragraph::new(out), area);
    }

    fn draw_map(&mut self, frame: &mut Frame, area: Rect) {
        let pointed: Vec<&str> = self
            .deck
            .steps
            .iter()
            .filter_map(|s| s.location())
            .map(|at| at.file.as_str())
            .collect();
        let mut lines = vec![
            Line::from(self.deck.title.clone().bold()),
            Line::from(
                format!(
                    "{} files · +{} -{}",
                    self.files.len(),
                    self.files.iter().map(FileDiff::added).sum::<usize>(),
                    self.files.iter().map(FileDiff::deleted).sum::<usize>(),
                )
                .dim(),
            ),
            Line::default(),
        ];
        if self.files.is_empty() {
            lines.push(Line::from("no changes in this diff".italic()));
        }
        for fd in &self.files {
            let (letter, style) = match fd.status {
                FileStatus::Modified => ("M", Style::new().yellow()),
                FileStatus::Added => ("A", Style::new().green()),
                FileStatus::Deleted => ("D", Style::new().red()),
                FileStatus::Renamed => ("R", Style::new().cyan()),
            };
            let mark = if pointed.contains(&fd.new_path.as_str()) {
                "◆ "
            } else {
                "  "
            };
            lines.push(Line::from(vec![
                Span::raw(mark),
                Span::styled(letter, style),
                Span::raw(" "),
                Span::raw(fd.new_path.clone()),
                Span::styled(format!("  +{}", fd.added()), Style::new().green()),
                Span::styled(format!(" -{}", fd.deleted()), Style::new().red()),
            ]));
        }
        let width = 72.min(area.width);
        let height = (lines.len() as u16).min(area.height);
        frame.render_widget(Paragraph::new(lines), centered(area, width, height));
    }

    fn draw_dive(&mut self, frame: &mut Frame, area: Rect, scroll: u16) {
        let target = self.current().location().cloned();
        let mut lines: Vec<Line> = Vec::new();
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
        for fd in files {
            for hunk in &fd.hunks {
                lines.push(hunk_header(fd, hunk));
                lines.extend(hunk_lines(hunk, target.as_ref().map(|t| t.line)));
                lines.push(Line::default());
            }
        }
        let max = (lines.len() as u16).saturating_sub(area.height);
        frame.render_widget(Paragraph::new(lines).scroll((scroll.min(max), 0)), area);
    }
}

// ---- helpers shared by renderers ------------------------------------

/// Render the claim banner (if any) at the top and return the remaining
/// body area.
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
    let claim_rows = (claim.chars().count().max(1)).div_ceil(wrap_width) as u16;
    let height = (claim_rows + 2).min(area.height / 2).max(3);
    let [top, _, body] = Layout::vertical([
        Constraint::Length(height),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    let mut block = Block::bordered().border_style(Style::new().dim());
    if let Some(sev) = severity {
        let style = match sev {
            Severity::Low => Style::new().black().on_yellow(),
            Severity::Medium => Style::new().black().on_light_yellow(),
            Severity::High => Style::new().white().on_red(),
        };
        block = block.title(Span::styled(format!(" risk: {sev} "), style.bold()));
    }
    frame.render_widget(
        Paragraph::new(claim.to_string().bold())
            .wrap(Wrap { trim: false })
            .block(block),
        top,
    );
    body
}

fn hunk_header<'a>(fd: &FileDiff, hunk: &Hunk) -> Line<'a> {
    let mut text = format!("── {}", fd.new_path);
    if !hunk.section.is_empty() {
        text.push_str(&format!(" · {}", hunk.section));
    }
    Line::from(text).style(Style::new().bold().dim())
}

/// A hunk as displayable lines: dual line-number gutter, sign, word-level
/// emphasis, target line marked.
fn hunk_lines<'a>(hunk: &Hunk, target: Option<u32>) -> Vec<Line<'a>> {
    let segs = emphasize_hunk(hunk);
    hunk.lines
        .iter()
        .zip(segs)
        .map(|(line, seg)| {
            let (sign, base, emph) = match line.kind {
                LineKind::Context => (" ", Style::new(), Style::new()),
                LineKind::Del => (
                    "-",
                    Style::new().red(),
                    Style::new().black().on_red(),
                ),
                LineKind::Add => (
                    "+",
                    Style::new().green(),
                    Style::new().black().on_green(),
                ),
            };
            let gutter = format!(
                "{:>4} {:>4} ",
                line.old_no.map(|n| n.to_string()).unwrap_or_default(),
                line.new_no.map(|n| n.to_string()).unwrap_or_default(),
            );
            let mut spans = vec![
                Span::styled(gutter, Style::new().dim()),
                Span::styled(sign.to_string(), base),
                Span::raw(" "),
            ];
            spans.extend(seg.into_iter().map(|Segment { text, emph: e }| {
                Span::styled(text, if e { emph } else { base })
            }));
            let mut out = Line::from(spans);
            if target.is_some() && line.new_no == target {
                out = out.style(Style::new().bg(Color::DarkGray));
            }
            out
        })
        .collect()
}

fn side_line<'a>(no: Option<u32>, segs: &[Segment], base: Style) -> Line<'a> {
    let emph = match base.fg {
        Some(Color::Red) => Style::new().black().on_red(),
        Some(Color::Green) => Style::new().black().on_green(),
        _ => base,
    };
    let mut spans = vec![Span::styled(
        format!("{:>4} ", no.map(|n| n.to_string()).unwrap_or_default()),
        Style::new().dim(),
    )];
    spans.extend(
        segs.iter()
            .map(|s| Span::styled(s.text.clone(), if s.emph { emph } else { base })),
    );
    Line::from(spans)
}

fn line_is_target(hunk: &Hunk, target: u32, index: usize) -> bool {
    hunk.lines
        .get(index)
        .is_some_and(|l| l.new_no == Some(target))
}

/// Scroll offset that keeps the first matching line in the middle third.
fn scroll_to(lines: &[Line], height: u16, is_target: impl Fn(usize) -> bool) -> u16 {
    if (lines.len() as u16) <= height {
        return 0;
    }
    // Header rows precede hunk rows; find the target among all rows.
    let hit = (0..lines.len()).find(|&i| is_target(i.saturating_sub(1)));
    let Some(hit) = hit else { return 0 };
    let want = (hit as u16).saturating_sub(height / 3);
    want.min((lines.len() as u16).saturating_sub(height))
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
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
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
    fn point_shows_claim_hunk_and_status() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "take() returns the value for the caller".into(),
        }]);
        let s = screen(&mut app);
        assert!(s.contains("take() returns the value"), "claim missing:\n{s}");
        assert!(s.contains("self.map.take(&id);"), "hunk missing:\n{s}");
        assert!(s.contains("impl Session {"), "section missing:\n{s}");
        assert!(s.contains("1/1 · point · src/lib.rs:11"), "status missing:\n{s}");
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
        assert!(s.contains(" before "), "{s}");
        assert!(s.contains(" after "), "{s}");
        assert!(s.contains("remove"), "{s}");
        assert!(s.contains("take"), "{s}");
    }

    #[test]
    fn map_counts_files_and_marks_pointed_ones() {
        let mut app = app_with(vec![
            Step::Point {
                at: "src/lib.rs:11".parse().unwrap(),
                claim: "c".into(),
            },
            Step::Map,
        ]);
        app.step = 1;
        let s = screen(&mut app);
        assert!(s.contains("1 files · +1 -1"), "{s}");
        assert!(s.contains("◆ M src/lib.rs"), "{s}");
    }

    #[test]
    fn risk_shows_severity_badge() {
        let mut app = app_with(vec![Step::Risk {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "take() changes the return type contract".into(),
            severity: Severity::High,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("risk: high"), "{s}");
    }

    #[test]
    fn dive_shows_full_hunk_and_missing_file_note() {
        let mut app = app_with(vec![Step::Point {
            at: "src/lib.rs:11".parse().unwrap(),
            claim: "c".into(),
        }]);
        app.mode = Mode::Dive { scroll: 0 };
        let s = screen(&mut app);
        assert!(s.contains("self.notify();"), "{s}");
        assert!(s.contains("enter back"), "{s}");

        let mut app = app_with(vec![Step::Point {
            at: "src/gone.rs:1".parse().unwrap(),
            claim: "c".into(),
        }]);
        app.mode = Mode::Dive { scroll: 0 };
        let s = screen(&mut app);
        assert!(s.contains("src/gone.rs is not in this diff"), "{s}");
    }

    #[test]
    fn missing_file_falls_back_to_note_not_panic() {
        let mut app = app_with(vec![Step::Zoom {
            at: "src/gone.rs:5".parse().unwrap(),
            claim: None,
        }]);
        let s = screen(&mut app);
        assert!(s.contains("cannot read src/gone.rs"), "{s}");
    }
}

//! Minimal markdown for speaker notes: bold, italic, inline code, bullet
//! lines, headings. Own implementation, line-local, degrades to literal
//! text on anything unclosed or unsupported.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Render markdown-ish text into styled lines. Wrapping is left to the
/// widget that draws them.
pub fn render(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|raw| {
            let indent: String = raw.chars().take_while(|c| *c == ' ').collect();
            let body = &raw[indent.len()..];
            if let Some(rest) = heading(body) {
                let mut spans = vec![Span::raw(indent)];
                spans.extend(inline(rest, Style::new().add_modifier(Modifier::BOLD)));
                return Line::from(spans);
            }
            if let Some(rest) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
                let mut spans = vec![
                    Span::raw(indent),
                    Span::styled("• ", Style::new().fg(Color::Cyan)),
                ];
                spans.extend(inline(rest, Style::new()));
                return Line::from(spans);
            }
            let mut spans = vec![Span::raw(indent)];
            spans.extend(inline(body, Style::new()));
            Line::from(spans)
        })
        .collect()
}

fn heading(s: &str) -> Option<&str> {
    let hashes = s.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    s[hashes..].strip_prefix(' ')
}

/// Inline spans: `**bold**`, `*italic*`, `` `code` ``. A marker without a
/// closing partner stays literal.
fn inline(s: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = s.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;

    let style_of = |bold: bool, italic: bool| {
        let mut style = base;
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    };
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut buf),
                    style_of(bold, italic),
                ));
            }
        };
    }

    let find = |from: usize, marker: &[char]| -> Option<usize> {
        let mut i = from;
        while i + marker.len() <= chars.len() {
            if chars[i..i + marker.len()] == *marker {
                return Some(i);
            }
            i += 1;
        }
        None
    };

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(close) = find(i + 1, &['`']) {
                flush!();
                let code: String = chars[i + 1..close].iter().collect();
                spans.push(Span::styled(code, base.fg(Color::Cyan)));
                i = close + 1;
                continue;
            }
        } else if c == '*' && chars.get(i + 1) == Some(&'*') {
            if bold || find(i + 2, &['*', '*']).is_some() {
                flush!();
                bold = !bold;
                i += 2;
                continue;
            }
        } else if c == '*'
            && (italic || find(i + 1, &['*']).is_some()) {
                flush!();
                italic = !italic;
                i += 1;
                continue;
            }
        buf.push(c);
        i += 1;
    }
    flush!();
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(text: &str) -> String {
        render(text)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.clone().into_owned())
            .collect()
    }

    #[test]
    fn bold_italic_code_are_styled_and_markers_removed() {
        let lines = render("a **b** *c* `d`");
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "b"
            && s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(spans.iter().any(|s| s.content == "c"
            && s.style.add_modifier.contains(Modifier::ITALIC)));
        assert!(spans.iter().any(|s| s.content == "d" && s.style.fg == Some(Color::Cyan)));
        assert_eq!(flat("a **b** *c* `d`"), "a b c d");
    }

    #[test]
    fn unclosed_markers_stay_literal() {
        assert_eq!(flat("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(flat("a ** b"), "a ** b");
        assert_eq!(flat("tick ` alone"), "tick ` alone");
    }

    #[test]
    fn bullets_and_headings() {
        let lines = render("# Title\n- item one\nplain");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content == "Title" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(lines[1].spans.iter().any(|s| s.content == "• "));
        assert!(lines[1].spans.iter().any(|s| s.content == "item one"));
    }

    #[test]
    fn japanese_text_passes_through() {
        assert_eq!(flat("**太字**と`コード`"), "太字とコード");
    }
}

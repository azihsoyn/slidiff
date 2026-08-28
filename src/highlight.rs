//! Minimal own syntax highlighting — enough hierarchy to make an excerpt
//! readable on a slide, nothing more. Line-local: strings, comments,
//! numbers, keywords. No external grammar engines.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Python,
    Go,
    Sql,
    Yaml,
    Shell,
    Markdown,
    Plain,
}

impl Lang {
    pub fn from_path(path: &str) -> Lang {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "rs" => Lang::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "svelte" => Lang::TypeScript,
            "py" => Lang::Python,
            "go" => Lang::Go,
            "sql" => Lang::Sql,
            "yaml" | "yml" | "json" | "jsonnet" | "toml" => Lang::Yaml,
            "sh" | "bash" | "zsh" | "xsh" => Lang::Shell,
            "md" | "markdown" => Lang::Markdown,
            _ => Lang::Plain,
        }
    }

    fn line_comment(&self) -> &'static [&'static str] {
        match self {
            Lang::Rust | Lang::TypeScript | Lang::Go => &["//"],
            Lang::Python | Lang::Yaml | Lang::Shell => &["#"],
            Lang::Sql => &["--"],
            Lang::Markdown | Lang::Plain => &[],
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
                "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "trait", "type", "unsafe", "use", "where", "while",
            ],
            Lang::TypeScript => &[
                "abstract", "as", "async", "await", "break", "case", "catch", "class", "const",
                "continue", "default", "delete", "do", "else", "enum", "export", "extends",
                "finally", "for", "from", "function", "if", "implements", "import", "in",
                "instanceof", "interface", "let", "new", "of", "private", "protected", "public",
                "readonly", "return", "satisfies", "static", "switch", "throw", "try", "type",
                "typeof", "var", "while", "yield",
            ],
            Lang::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "not", "or", "pass", "raise", "return", "try",
                "while", "with", "yield",
            ],
            Lang::Go => &[
                "break", "case", "chan", "const", "continue", "default", "defer", "else",
                "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
                "package", "range", "return", "select", "struct", "switch", "type", "var",
            ],
            Lang::Sql => &[
                "AND", "AS", "ASC", "BY", "CREATE", "DELETE", "DESC", "DROP", "FROM", "GROUP",
                "HAVING", "IN", "INDEX", "INSERT", "INTO", "JOIN", "LEFT", "LIMIT", "NOT",
                "NULL", "ON", "OR", "ORDER", "SELECT", "SET", "TABLE", "UPDATE", "VALUES",
                "WHERE", "and", "as", "from", "insert", "join", "on", "select", "update",
                "where",
            ],
            Lang::Yaml | Lang::Shell | Lang::Markdown | Lang::Plain => &[],
        }
    }

    fn literals(&self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &["true", "false", "None", "Some", "Ok", "Err"],
            Lang::TypeScript => &["true", "false", "null", "undefined", "this", "NaN"],
            Lang::Python => &["True", "False", "None", "self"],
            Lang::Go => &["true", "false", "nil", "iota"],
            Lang::Yaml => &["true", "false", "null"],
            _ => &[],
        }
    }
}

/// Palette. Comments sink, strings and numbers get quiet color, keywords
/// carry the structure. All 16-color-palette safe.
fn style_comment() -> Style {
    Style::new().fg(Color::DarkGray)
}
fn style_string() -> Style {
    Style::new().fg(Color::Yellow)
}
fn style_number() -> Style {
    Style::new().fg(Color::Cyan)
}
fn style_keyword() -> Style {
    Style::new().fg(Color::Magenta)
}
fn style_literal() -> Style {
    Style::new().fg(Color::Cyan)
}

/// One highlighted run of characters.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// Highlight one line. Line-local by design: a multi-line string or block
/// comment degrades to normal text, which on a slide is acceptable and on
/// a parser budget is free.
pub fn highlight(lang: Lang, line: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut push = |text: &str, style: Style| {
        if text.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => runs.push(Run {
                text: text.to_string(),
                style,
            }),
        }
    };

    // Markdown: headings and code fences only.
    if lang == Lang::Markdown {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            push(line, Style::new().add_modifier(Modifier::BOLD));
        } else if trimmed.starts_with("```") || trimmed.starts_with('>') {
            push(line, style_comment());
        } else {
            push(line, Style::new());
        }
        return runs;
    }

    let comment_markers = lang.line_comment();
    let keywords = lang.keywords();
    let literals = lang.literals();
    let bytes = line.char_indices().collect::<Vec<_>>();
    let mut i = 0;
    while i < bytes.len() {
        let (byte_pos, c) = bytes[i];

        // Line comment: rest of the line sinks.
        if let Some(marker) = comment_markers
            .iter()
            .find(|m| line[byte_pos..].starts_with(**m))
        {
            // Shell: `#` only comments at start or after whitespace.
            let at_word_edge = i == 0 || bytes[i - 1].1.is_whitespace();
            if *marker != "#" || at_word_edge {
                push(&line[byte_pos..], style_comment());
                return runs;
            }
        }

        // String literal.
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let mut j = i + 1;
            while j < bytes.len() {
                let (_, cj) = bytes[j];
                if cj == '\\' {
                    j += 2;
                    continue;
                }
                if cj == quote {
                    break;
                }
                j += 1;
            }
            let end_byte = if j < bytes.len() {
                bytes[j].0 + quote.len_utf8()
            } else {
                line.len()
            };
            push(&line[byte_pos..end_byte], style_string());
            i = j + 1;
            continue;
        }

        // Number.
        if c.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].1.is_ascii_alphanumeric() || bytes[j].1 == '.' || bytes[j].1 == '_')
            {
                j += 1;
            }
            let end_byte = bytes.get(j).map_or(line.len(), |&(b, _)| b);
            push(&line[byte_pos..end_byte], style_number());
            i = j;
            continue;
        }

        // Word.
        if c.is_alphanumeric() || c == '_' {
            let mut j = i;
            while j < bytes.len() && (bytes[j].1.is_alphanumeric() || bytes[j].1 == '_') {
                j += 1;
            }
            let end_byte = bytes.get(j).map_or(line.len(), |&(b, _)| b);
            let word = &line[byte_pos..end_byte];
            let style = if keywords.contains(&word) {
                style_keyword()
            } else if literals.contains(&word) {
                style_literal()
            } else {
                Style::new()
            };
            push(word, style);
            i = j;
            continue;
        }

        push(&line[byte_pos..byte_pos + c.len_utf8()], Style::new());
        i += 1;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles_of(lang: Lang, line: &str) -> Vec<(String, Style)> {
        highlight(lang, line)
            .into_iter()
            .map(|r| (r.text, r.style))
            .collect()
    }

    #[test]
    fn rust_keywords_strings_comments() {
        let runs = styles_of(Lang::Rust, "let x = \"hi\"; // done");
        assert!(runs.iter().any(|(t, s)| t == "let" && *s == style_keyword()));
        assert!(runs.iter().any(|(t, s)| t == "\"hi\"" && *s == style_string()));
        assert!(runs.iter().any(|(t, s)| t == "// done" && *s == style_comment()));
    }

    #[test]
    fn typescript_numbers_and_literals() {
        let runs = styles_of(Lang::TypeScript, "const n = 1000 * chunk ?? null;");
        assert!(runs.iter().any(|(t, s)| t == "1000" && *s == style_number()));
        assert!(runs.iter().any(|(t, s)| t == "null" && *s == style_literal()));
        assert!(runs.iter().any(|(t, s)| t == "const" && *s == style_keyword()));
    }

    #[test]
    fn unterminated_string_swallows_rest_of_line_only() {
        let runs = styles_of(Lang::TypeScript, "a(\"unterminated");
        assert_eq!(runs.last().unwrap().1, style_string());
    }

    #[test]
    fn shell_hash_only_comments_at_word_edge() {
        let runs = styles_of(Lang::Shell, "echo a#b # real");
        assert!(runs.iter().any(|(t, s)| t == "# real" && *s == style_comment()));
        assert!(!runs.iter().any(|(t, _)| t.contains("a#b") && false));
        assert!(runs.iter().all(|(t, s)| t != "#b" || *s != style_comment()));
    }

    #[test]
    fn reconstruction_is_lossless() {
        for line in [
            "  const x = { a: \"日本語\", n: 42 }; // コメント",
            "fn main() { println!(\"{}\", 1_000); }",
            "",
            "plain words only",
        ] {
            let joined: String = highlight(Lang::TypeScript, line)
                .into_iter()
                .map(|r| r.text)
                .collect();
            assert_eq!(joined, line);
        }
    }
}

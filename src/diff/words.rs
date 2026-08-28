//! Word-level emphasis inside a hunk: which parts of a changed line
//! actually changed. Own implementation — tokenize, LCS over tokens,
//! mark what is not common.

use super::parse::{Hunk, LineKind};

/// A run of characters in a line, either shared with the other side
/// (`emph == false`) or unique to this side (`emph == true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub emph: bool,
}

/// For each line of `hunk`, the segments to draw. Context lines come back
/// as one unemphasized segment. Del/add lines are paired positionally
/// within each del-run/add-run block; a pair only gets word emphasis when
/// the two lines share enough tokens to make the emphasis meaningful.
pub fn emphasize_hunk(hunk: &Hunk) -> Vec<Vec<Segment>> {
    let mut out: Vec<Vec<Segment>> = hunk
        .lines
        .iter()
        .map(|l| vec![whole(&l.text, false)])
        .collect();

    let mut i = 0;
    while i < hunk.lines.len() {
        if hunk.lines[i].kind != LineKind::Del {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Del {
            i += 1;
        }
        let add_start = i;
        while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Add {
            i += 1;
        }
        let dels = &hunk.lines[del_start..add_start];
        let adds = &hunk.lines[add_start..i];
        for (j, (del, add)) in dels.iter().zip(adds.iter()).enumerate() {
            let (d, a) = emphasize_pair(&del.text, &add.text);
            out[del_start + j] = d;
            out[add_start + j] = a;
        }
    }

    // A deleted or added line with no partner stays fully emphasized: the
    // whole line is the change.
    for (line, segs) in hunk.lines.iter().zip(out.iter_mut()) {
        if line.kind != LineKind::Context && segs.len() == 1 && !segs[0].emph {
            segs[0].emph = true;
        }
    }

    out
}

/// Word-diff one del/add pair. Falls back to whole-line emphasis when the
/// lines share too little.
fn emphasize_pair(del: &str, add: &str) -> (Vec<Segment>, Vec<Segment>) {
    let dt = tokenize(del);
    let at = tokenize(add);
    let common = lcs_flags(&dt, &at);
    let shared: usize = common.0.iter().filter(|&&c| c).count();
    let significant = dt.iter().filter(|t| !t.trim().is_empty()).count();
    // Require a third of the old line's real tokens to survive, or the
    // "emphasis" would just repaint both lines entirely.
    if significant > 0 && shared * 3 < significant {
        return (vec![whole(del, true)], vec![whole(add, true)]);
    }
    (
        segments(&dt, &common.0),
        segments(&at, &common.1),
    )
}

fn whole(text: &str, emph: bool) -> Segment {
    Segment {
        text: text.to_string(),
        emph,
    }
}

/// Split into words ([alnum_]+ including all of Unicode), whitespace runs,
/// and single other characters.
fn tokenize(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let class = char_class(c);
        let mut end = start + c.len_utf8();
        if class != CharClass::Other {
            while let Some(&(i, c2)) = chars.peek() {
                if char_class(c2) != class {
                    break;
                }
                end = i + c2.len_utf8();
                chars.next();
            }
        }
        tokens.push(&s[start..end]);
    }
    tokens
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// Classic LCS over tokens. Returns, for each side, which tokens are part
/// of the common subsequence. Lines are short; O(n·m) is fine.
fn lcs_flags(a: &[&str], b: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[idx(i, j)] = if a[i] == b[j] {
                dp[idx(i + 1, j + 1)] + 1
            } else {
                dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
            };
        }
    }
    let mut fa = vec![false; n];
    let mut fb = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            fa[i] = true;
            fb[j] = true;
            i += 1;
            j += 1;
        } else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (fa, fb)
}

/// Merge tokens back into runs of same emphasis.
fn segments(tokens: &[&str], common: &[bool]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for (tok, &is_common) in tokens.iter().zip(common) {
        let emph = !is_common;
        match out.last_mut() {
            Some(last) if last.emph == emph => last.text.push_str(tok),
            _ => out.push(Segment {
                text: tok.to_string(),
                emph,
            }),
        }
    }
    if out.is_empty() {
        out.push(Segment {
            text: String::new(),
            emph: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_unified;

    fn seg(text: &str, emph: bool) -> Segment {
        Segment {
            text: text.into(),
            emph,
        }
    }

    #[test]
    fn emphasizes_only_the_changed_word() {
        let (d, a) = emphasize_pair("let count = items.len();", "let total = items.len();");
        assert_eq!(
            d,
            vec![seg("let ", false), seg("count", true), seg(" = items.len();", false)]
        );
        assert_eq!(
            a,
            vec![seg("let ", false), seg("total", true), seg(" = items.len();", false)]
        );
    }

    #[test]
    fn unrelated_lines_stay_whole() {
        let (d, a) = emphasize_pair("return None;", "self.registry.write().await.clear()");
        assert_eq!(d, vec![seg("return None;", true)]);
        assert_eq!(a, vec![seg("self.registry.write().await.clear()", true)]);
    }

    #[test]
    fn hunk_pairs_del_and_add_runs_positionally() {
        let text = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,3 +1,3 @@
 ctx
-let x = 1;
+let y = 1;
 tail
";
        let hunk = &parse_unified(text)[0].hunks[0];
        let segs = emphasize_hunk(hunk);
        assert_eq!(segs[0], vec![seg("ctx", false)]);
        assert_eq!(
            segs[1],
            vec![seg("let ", false), seg("x", true), seg(" = 1;", false)]
        );
        assert_eq!(
            segs[2],
            vec![seg("let ", false), seg("y", true), seg(" = 1;", false)]
        );
    }

    #[test]
    fn unpaired_addition_is_fully_emphasized() {
        let text = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,2 +1,3 @@
 ctx
+brand new line
 tail
";
        let hunk = &parse_unified(text)[0].hunks[0];
        let segs = emphasize_hunk(hunk);
        assert_eq!(segs[1], vec![seg("brand new line", true)]);
    }

    #[test]
    fn japanese_words_tokenize_by_run() {
        let (d, a) = emphasize_pair("// 旧い説明", "// 新しい説明");
        assert!(d.iter().any(|s| s.emph));
        assert!(a.iter().any(|s| s.emph));
        assert_eq!(d.last().unwrap().emph, a.last().unwrap().emph);
    }
}

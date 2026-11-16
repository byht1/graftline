//! A small syntax highlighter for diff lines.
//! It colors one line at a time, without the lines around it: a diff shows fragments of a
//! file, so a full parser has nothing to hold on to. Comments, strings, numbers, keywords,
//! types and calls are enough to make code readable. Colors come from the theme.

use super::theme::Theme;
use gpui::Hsla;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tok {
    Com,
    Str,
    Num,
    Kw,
    Const,
    Fn,
    Type,
    Punct,
    Op,
    Id,
}

impl Tok {
    pub fn color(self, t: &Theme) -> Hsla {
        match self {
            Tok::Com => t.t_com,
            Tok::Str => t.t_str,
            Tok::Num => t.t_num,
            Tok::Kw => t.t_kw,
            Tok::Const => t.t_const,
            Tok::Fn => t.t_fn,
            Tok::Type => t.t_type,
            Tok::Punct => t.t_punct,
            Tok::Op => t.t_op,
            Tok::Id => t.t_id,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Rust,
    Js,
    Json,
    Python,
    Go,
    C,
    Css,
    Html,
    Shell,
    Yaml,
    Toml,
    Md,
    Sql,
    Ruby,
    Lua,
}

pub fn lang_of(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => Lang::Js,
        "json" => Lang::Json,
        "py" => Lang::Python,
        "go" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "java" | "kt" | "swift" | "php" => Lang::C,
        "css" | "scss" => Lang::Css,
        "html" | "htm" | "xml" | "svg" | "vue" => Lang::Html,
        "sh" | "bash" | "zsh" | "ps1" => Lang::Shell,
        "yml" | "yaml" => Lang::Yaml,
        "toml" | "ini" => Lang::Toml,
        "md" => Lang::Md,
        "sql" => Lang::Sql,
        "rb" => Lang::Ruby,
        "lua" => Lang::Lua,
        _ => return None,
    })
}

const RUST: &str = "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while";
const JS: &str = "as async await break case catch class const continue debugger default delete do else enum export extends finally for from function get if implements import in instanceof interface let new of private protected public readonly return set static super switch this throw try type typeof var void while with yield declare namespace abstract keyof";
const PYTHON: &str = "and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case self";
const GO: &str = "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var";
const C: &str = "abstract auto break case catch class const constexpr continue default delete do else enum explicit extends extern final finally for friend goto if implements import inline instanceof interface namespace new operator override package private protected public register return sizeof static struct switch template this throw throws try typedef typename union using virtual void volatile while fun val var when internal data object companion sealed open func let guard defer protocol extension echo function use";
const SHELL: &str = "if then else elif fi for while do done case esac function in return export local readonly set unset echo exit source";
const SQL: &str = "select from where and or not in is null like between as on join left right inner outer full cross group by order having limit offset insert into values update set delete create alter drop table index view primary key foreign references default unique constraint union all distinct case when then else end with returning";
const RUBY: &str = "alias and begin break case class def defined do else elsif end ensure for if in module next not or redo rescue retry return self super then undef unless until when while yield require attr_accessor attr_reader";
const LUA: &str = "and break do else elseif end for function goto if in local not or repeat return then until while";
const CONSTANTS: &str = "true false null undefined None True False nil NULL nullptr NaN Infinity";

fn has(list: &str, word: &str) -> bool {
    list.split(' ').any(|w| w == word)
}

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Slash,
    Rust,
    Hash,
    Dash,
}

type Spans = Vec<(Range<usize>, Tok)>;

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// End of a string that starts at `i` with the quote `q`: after the closing quote, or the
/// end of the line when the string is not closed.
fn string_end(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
        } else if b[j] == q {
            return j + 1;
        } else {
            j += 1;
        }
    }
    b.len()
}

fn code(text: &str, family: Family, keywords: &str, caseless: bool) -> Spans {
    let b = text.as_bytes();
    let mut out = Spans::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let rest = &b[i..];
        let line_comment = match family {
            Family::Slash | Family::Rust => rest.starts_with(b"//"),
            Family::Hash => c == b'#',
            Family::Dash => rest.starts_with(b"--"),
        };
        if line_comment {
            out.push((i..b.len(), Tok::Com));
            break;
        }
        if matches!(family, Family::Slash | Family::Rust) && rest.starts_with(b"/*") {
            let end = text[i + 2..].find("*/").map(|p| i + 2 + p + 2).unwrap_or(b.len());
            out.push((i..end, Tok::Com));
            i = end;
            continue;
        }
        if c == b'"' || (c == b'\'' && family != Family::Rust) || (c == b'`' && family == Family::Slash) {
            let end = string_end(b, i, c);
            out.push((i..end, Tok::Str));
            i = end;
            continue;
        }
        if c == b'\'' && family == Family::Rust {
            // 'a is a lifetime, 'x' is a character.
            let end = if b.get(i + 1) == Some(&b'\\') { string_end(b, i, b'\'') } else { i + 3 };
            if end <= b.len() && b.get(end - 1) == Some(&b'\'') && end > i + 2 && text.is_char_boundary(end) {
                out.push((i..end, Tok::Str));
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !is_ident(b[i - 1])) {
            let mut j = i + 1;
            while j < b.len() && (is_ident(b[j]) || b[j] == b'.') {
                j += 1;
            }
            out.push((i..j, Tok::Num));
            i = j;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
            let w = &text[i..j];
            let lower;
            let key = if caseless {
                lower = w.to_lowercase();
                lower.as_str()
            } else {
                w
            };
            let next = b.get(j).copied();
            let tok = if has(keywords, key) {
                Some(Tok::Kw)
            } else if has(CONSTANTS, w) {
                Some(Tok::Const)
            } else if next == Some(b'(') || (next == Some(b'!') && family == Family::Rust) {
                Some(Tok::Fn)
            } else if w.as_bytes()[0].is_ascii_uppercase() && w.bytes().any(|c| c.is_ascii_lowercase()) {
                Some(Tok::Type)
            } else {
                None
            };
            if let Some(tok) = tok {
                out.push((i..j, tok));
            }
            i = j;
            continue;
        }
        if b"{}()[];,.".contains(&c) {
            out.push((i..i + 1, Tok::Punct));
            i += 1;
            continue;
        }
        if b"-+*/%=<>!&|^~?:@".contains(&c) {
            let mut j = i + 1;
            while j < b.len() && b"-+*/%=<>!&|^~?:@".contains(&b[j]) {
                j += 1;
            }
            out.push((i..j, Tok::Op));
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

fn json(text: &str) -> Spans {
    let b = text.as_bytes();
    let mut out = Spans::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let end = string_end(b, i, b'"');
            let key = text[end..].trim_start().starts_with(':');
            out.push((i..end, if key { Tok::Id } else { Tok::Str }));
            i = end;
        } else if c.is_ascii_digit() || (c == b'-' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_digit() || b".eE+-".contains(&b[j])) {
                j += 1;
            }
            out.push((i..j, Tok::Num));
            i = j;
        } else if c.is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            if matches!(&text[i..j], "true" | "false" | "null") {
                out.push((i..j, Tok::Const));
            }
            i = j;
        } else {
            if b"{}[],".contains(&c) {
                out.push((i..i + 1, Tok::Punct));
            }
            i += 1;
        }
    }
    out
}

fn markup(text: &str) -> Spans {
    let b = text.as_bytes();
    let mut out = Spans::new();
    let mut i = 0;
    let name = |c: u8| c.is_ascii_alphanumeric() || b"_:-".contains(&c);
    while i < b.len() {
        let c = b[i];
        if b[i..].starts_with(b"<!--") {
            let end = text[i..].find("-->").map(|p| i + p + 3).unwrap_or(b.len());
            out.push((i..end, Tok::Com));
            i = end;
        } else if c == b'<' {
            let open = if b.get(i + 1) == Some(&b'/') { 2 } else { 1 };
            let mut j = i + open;
            while j < b.len() && name(b[j]) {
                j += 1;
            }
            if j > i + open {
                out.push((i..i + open, Tok::Punct));
                out.push((i + open..j, Tok::Kw));
            }
            i = j.max(i + 1);
        } else if c == b'"' || c == b'\'' {
            let end = b[i + 1..].iter().position(|x| *x == c).map(|p| i + p + 2).unwrap_or(b.len());
            out.push((i..end, Tok::Str));
            i = end;
        } else if name(c) {
            let mut j = i + 1;
            while j < b.len() && name(b[j]) {
                j += 1;
            }
            if b.get(j) == Some(&b'=') {
                out.push((i..j, Tok::Id));
                out.push((j..j + 1, Tok::Op));
                j += 1;
            }
            i = j;
        } else if c == b'>' || b[i..].starts_with(b"/>") {
            let len = if c == b'>' { 1 } else { 2 };
            out.push((i..i + len, Tok::Punct));
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

fn markdown(text: &str) -> Spans {
    let trimmed = text.trim_start();
    let hashes = text.bytes().take_while(|c| *c == b'#').count();
    if (1..=6).contains(&hashes) && text.as_bytes().get(hashes).is_some_and(|c| c.is_ascii_whitespace()) {
        return vec![(0..text.len(), Tok::Kw)];
    }
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        return vec![(0..text.len(), Tok::Com)];
    }
    let b = text.as_bytes();
    let mut out = Spans::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            if let Some(p) = text[i + 1..].find('`').filter(|p| *p > 0) {
                out.push((i..i + p + 2, Tok::Str));
                i += p + 2;
                continue;
            }
        } else if b[i..].starts_with(b"**") {
            if let Some(p) = text[i + 2..].find("**").filter(|p| *p > 0) {
                out.push((i..i + p + 4, Tok::Type));
                i += p + 4;
                continue;
            }
        } else if b[i] == b'[' {
            if let Some(close) = text[i..].find("](") {
                if let Some(end) = text[i + close..].find(')') {
                    out.push((i..i + close + end + 1, Tok::Id));
                    i += close + end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Colored ranges of one line of code. Empty for a very long line.
pub fn highlight(text: &str, lang: Lang) -> Spans {
    if text.len() > 2000 {
        return Spans::new();
    }
    let spans = match lang {
        Lang::Json => json(text),
        Lang::Html => markup(text),
        Lang::Md => markdown(text),
        Lang::Rust => code(text, Family::Rust, RUST, false),
        Lang::Js => code(text, Family::Slash, JS, false),
        Lang::Go => code(text, Family::Slash, GO, false),
        Lang::C => code(text, Family::Slash, C, false),
        Lang::Css => code(text, Family::Slash, "", false),
        Lang::Python => code(text, Family::Hash, PYTHON, false),
        Lang::Shell => code(text, Family::Hash, SHELL, false),
        Lang::Ruby => code(text, Family::Hash, RUBY, false),
        Lang::Yaml | Lang::Toml => code(text, Family::Hash, "", false),
        Lang::Sql => code(text, Family::Dash, SQL, true),
        Lang::Lua => code(text, Family::Dash, LUA, false),
    };
    // Ranges must lie on character boundaries: text runs are built from them.
    spans
        .into_iter()
        .filter(|(r, _)| r.start < r.end && r.end <= text.len() && text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(text: &str, lang: Lang) -> Vec<(&str, Tok)> {
        highlight(text, lang).into_iter().map(|(r, t)| (&text[r], t)).collect()
    }

    #[test]
    fn rust_line() {
        let t = toks("let x: Vec<u8> = foo(1, \"a\"); // done", Lang::Rust);
        assert!(t.contains(&("let", Tok::Kw)));
        assert!(t.contains(&("Vec", Tok::Type)));
        assert!(t.contains(&("foo", Tok::Fn)));
        assert!(t.contains(&("1", Tok::Num)));
        assert!(t.contains(&("\"a\"", Tok::Str)));
        assert!(t.contains(&("// done", Tok::Com)));
    }

    #[test]
    fn rust_lifetime_is_not_a_string() {
        let t = toks("fn f<'a>(x: &'a str) -> char { 'x' }", Lang::Rust);
        assert!(t.contains(&("'x'", Tok::Str)));
        assert!(!t.iter().any(|(s, tok)| *tok == Tok::Str && s.starts_with("'a")));
    }

    #[test]
    fn json_keys_and_values() {
        let t = toks("  \"name\": \"graftline\", \"n\": 12, \"ok\": true", Lang::Json);
        assert!(t.contains(&("\"name\"", Tok::Id)));
        assert!(t.contains(&("\"graftline\"", Tok::Str)));
        assert!(t.contains(&("12", Tok::Num)));
        assert!(t.contains(&("true", Tok::Const)));
    }

    #[test]
    fn sql_keywords_ignore_case() {
        let t = toks("SELECT id FROM users -- all", Lang::Sql);
        assert!(t.contains(&("SELECT", Tok::Kw)));
        assert!(t.contains(&("FROM", Tok::Kw)));
        assert!(t.contains(&("-- all", Tok::Com)));
    }

    #[test]
    fn non_ascii_text_keeps_valid_ranges() {
        let text = "let s = \"привіт\"; // коментар 'є'";
        for (r, _) in highlight(text, Lang::Rust) {
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
        }
    }

    #[test]
    fn language_comes_from_the_extension() {
        assert_eq!(lang_of("src/main.rs"), Some(Lang::Rust));
        assert_eq!(lang_of("a/b/Cargo.TOML"), Some(Lang::Toml));
        assert_eq!(lang_of("LICENSE"), None);
    }
}

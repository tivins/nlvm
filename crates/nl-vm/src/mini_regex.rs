//! Minimal backtracking regex engine, used by `system.io.File.glob`
//! (stdlib.md § system.io.File (glob)) and `system.text.Regex` (stdlib.md §
//! system.text.Regex). The workspace has no regex dependency (same "no
//! external crate" stance as `system.SecureRandom`/`system.Uuid`, which read
//! `/dev/urandom` directly rather than pulling in `getrandom`), so a small
//! hand-rolled matcher is used instead of a real one.
//!
//! Supported syntax: literal characters, `.` (any char), `*`/`+`/`?`
//! (greedy quantifiers on the previous atom), character classes (`[abc]`,
//! `[^abc]`, `[a-z]`), grouping `(...)` (always capturing — there is no
//! `(?:...)` non-capturing syntax), alternation `|`, anchors `^`/`$` (start
//! /end of the whole string only — there is no multi-line mode), and
//! backslash escapes (`\d`/`\w`/`\s`, their uppercase negations, or `\X` for
//! a literal `X` — covers stdlib.md's own glob example pattern `.*\.nl`,
//! written in NL source as the string literal `".*\\.nl"`). Not supported:
//! counted repetition (`{m,n}`), backreferences.
//!
//! Two matching modes, mirroring stdlib.md's two callers:
//! - [`Regex::is_match`] — whole-string match, used by `File.glob` (a glob
//!   pattern must match the *entire* relative path).
//! - [`Regex::find`]/[`Regex::find_all`] — partial match *anywhere* in the
//!   string (stdlib.md § system.text.Regex: "found anywhere in input...
//!   like grep"), used by `system.text.Regex.match`/`matchFirst`/`replace`/
//!   `split`. A caller wanting a full match anchors explicitly (`"^...$"`).

use std::cell::{Cell, RefCell};

pub struct Regex {
    root: Node,
    group_count: usize,
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Start,
    End,
    Class(Vec<(char, char)>, bool),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Star(Box<Node>),
    Plus(Box<Node>),
    Opt(Box<Node>),
    /// Capturing group, 0-based index into `Match::groups`/`Regex::group_count`.
    Group(usize, Box<Node>),
}

/// One match — char (not byte) offsets into the searched string, plus one
/// span per capturing group in the order they open (`None` if the group
/// didn't participate, e.g. the unmatched side of an alternation).
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Regex {
    pub fn compile(pattern: &str) -> Result<Regex, String> {
        let mut parser = Parser { chars: pattern.chars().collect(), pos: 0, group_count: 0 };
        let root = parser.parse_alt()?;
        if parser.pos != parser.chars.len() {
            return Err(format!("unexpected character at position {}", parser.pos));
        }
        Ok(Regex { root, group_count: parser.group_count })
    }

    /// Whole-string match (matching stdlib.md's glob semantics: "Matching is
    /// applied to the relative path"), not a substring search.
    pub fn is_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        let caps = RefCell::new(vec![None; self.group_count]);
        match_node(&self.root, &chars, 0, &caps, &|end| end == chars.len())
    }

    /// Leftmost match anywhere in `s` (partial match, stdlib.md §
    /// system.text.Regex), with capture groups. `None` if the pattern
    /// doesn't occur anywhere.
    pub fn find(&self, s: &str) -> Option<Match> {
        let chars: Vec<char> = s.chars().collect();
        self.find_from(&chars, 0)
    }

    /// All non-overlapping matches, scanning left to right. A zero-width
    /// match advances by one character afterwards so the scan always makes
    /// progress.
    pub fn find_all(&self, s: &str) -> Vec<Match> {
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= chars.len() {
            match self.find_from(&chars, pos) {
                Some(m) => {
                    let next = if m.end > m.start { m.end } else { m.end + 1 };
                    out.push(m);
                    pos = next;
                }
                None => break,
            }
        }
        out
    }

    fn find_from(&self, chars: &[char], from: usize) -> Option<Match> {
        for start in from..=chars.len() {
            let caps = RefCell::new(vec![None; self.group_count]);
            let end_cell = Cell::new(None);
            let matched = match_node(&self.root, chars, start, &caps, &|end| {
                end_cell.set(Some(end));
                true
            });
            if matched {
                return Some(Match { start, end: end_cell.get().expect("set on match"), groups: caps.into_inner() });
            }
        }
        None
    }
}

/// Escapes every regex metacharacter in `s` so the result matches `s`
/// literally when embedded in a pattern (stdlib.md § system.text.Regex:
/// "Always use it when embedding user-controlled input in a pattern").
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '.' | '^' | '$' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    group_count: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn parse_alt(&mut self) -> Result<Node, String> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 { branches.pop().expect("just pushed") } else { Node::Alt(branches) })
    }

    fn parse_concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            items.push(self.parse_postfix()?);
        }
        Ok(Node::Concat(items))
    }

    fn parse_postfix(&mut self) -> Result<Node, String> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => {
                self.bump();
                Ok(Node::Star(Box::new(atom)))
            }
            Some('+') => {
                self.bump();
                Ok(Node::Plus(Box::new(atom)))
            }
            Some('?') => {
                self.bump();
                Ok(Node::Opt(Box::new(atom)))
            }
            _ => Ok(atom),
        }
    }

    fn parse_atom(&mut self) -> Result<Node, String> {
        match self.bump() {
            Some('.') => Ok(Node::Any),
            Some('^') => Ok(Node::Start),
            Some('$') => Ok(Node::End),
            Some('(') => {
                let idx = self.group_count;
                self.group_count += 1;
                let inner = self.parse_alt()?;
                if self.bump() != Some(')') {
                    return Err("expected ')'".to_string());
                }
                Ok(Node::Group(idx, Box::new(inner)))
            }
            Some('[') => self.parse_class(),
            Some('\\') => {
                let esc = self.bump().ok_or("dangling escape at end of pattern")?;
                Ok(escape_node(esc))
            }
            Some(c) => Ok(Node::Char(c)),
            None => Err("unexpected end of pattern".to_string()),
        }
    }

    fn parse_class(&mut self) -> Result<Node, String> {
        let negated = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err("unterminated character class".to_string()),
                Some(']') if !first => {
                    self.bump();
                    break;
                }
                _ => {
                    first = false;
                    let lo = self.bump_class_char()?;
                    if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&c| c != ']') {
                        self.bump();
                        let hi = self.bump_class_char()?;
                        ranges.push((lo, hi));
                    } else {
                        ranges.push((lo, lo));
                    }
                }
            }
        }
        Ok(Node::Class(ranges, negated))
    }

    fn bump_class_char(&mut self) -> Result<char, String> {
        match self.bump() {
            Some('\\') => self.bump().ok_or_else(|| "dangling escape in character class".to_string()),
            Some(c) => Ok(c),
            None => Err("unterminated character class".to_string()),
        }
    }
}

fn escape_node(c: char) -> Node {
    const DIGIT: (char, char) = ('0', '9');
    const WORD: [(char, char); 4] = [('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
    const SPACE: [(char, char); 4] = [(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')];
    match c {
        'd' => Node::Class(vec![DIGIT], false),
        'D' => Node::Class(vec![DIGIT], true),
        'w' => Node::Class(WORD.to_vec(), false),
        'W' => Node::Class(WORD.to_vec(), true),
        's' => Node::Class(SPACE.to_vec(), false),
        'S' => Node::Class(SPACE.to_vec(), true),
        other => Node::Char(other),
    }
}

type Caps = RefCell<Vec<Option<(usize, usize)>>>;

/// Backtracking match with an explicit success continuation `k` — lets
/// concatenation and quantifiers try alternatives without an explicit
/// choice-point stack. Operates on absolute char positions into `chars`
/// (rather than re-slicing a suffix) so `Node::Group`/`Node::Start`/
/// `Node::End` can see where they are in the whole string.
fn match_node(node: &Node, chars: &[char], pos: usize, caps: &Caps, k: &dyn Fn(usize) -> bool) -> bool {
    match node {
        Node::Char(c) => chars.get(pos) == Some(c) && k(pos + 1),
        Node::Any => pos < chars.len() && k(pos + 1),
        Node::Start => pos == 0 && k(pos),
        Node::End => pos == chars.len() && k(pos),
        Node::Class(ranges, negated) => match chars.get(pos) {
            Some(&c) => {
                let inside = ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
                (inside != *negated) && k(pos + 1)
            }
            None => false,
        },
        Node::Concat(items) => match_concat(items, chars, pos, caps, k),
        Node::Alt(branches) => branches.iter().any(|b| match_node(b, chars, pos, caps, k)),
        Node::Star(inner) => match_star(inner, chars, pos, caps, k),
        Node::Plus(inner) => match_node(inner, chars, pos, caps, &|p2| match_star(inner, chars, p2, caps, k)),
        Node::Opt(inner) => match_node(inner, chars, pos, caps, k) || k(pos),
        // Records `[start, end)` for this group right before continuing, and
        // restores whatever was there before on backtrack (failed `k`) — so
        // a group whose capture attempt didn't end up in the successful path
        // doesn't leave a stale span behind.
        Node::Group(idx, inner) => {
            let start = pos;
            match_node(inner, chars, pos, caps, &|end| {
                let old = caps.borrow()[*idx];
                caps.borrow_mut()[*idx] = Some((start, end));
                if k(end) {
                    true
                } else {
                    caps.borrow_mut()[*idx] = old;
                    false
                }
            })
        }
    }
}

fn match_concat(items: &[Node], chars: &[char], pos: usize, caps: &Caps, k: &dyn Fn(usize) -> bool) -> bool {
    match items.split_first() {
        None => k(pos),
        Some((first, rest)) => match_node(first, chars, pos, caps, &|p2| match_concat(rest, chars, p2, caps, k)),
    }
}

/// Greedy `*`: try consuming one more repetition before falling back to the
/// continuation. Guards against infinite recursion on a zero-width
/// repetition (e.g. a pathological `(a?)*`) by requiring every repetition to
/// strictly advance past the previous one.
fn match_star(inner: &Node, chars: &[char], pos: usize, caps: &Caps, k: &dyn Fn(usize) -> bool) -> bool {
    match_node(inner, chars, pos, caps, &|p2| p2 > pos && match_star(inner, chars, p2, caps, k)) || k(pos)
}

#[cfg(test)]
mod tests {
    use super::Regex;

    fn is_match(pattern: &str, s: &str) -> bool {
        Regex::compile(pattern).expect("valid pattern").is_match(s)
    }

    #[test]
    fn literal_and_dot() {
        assert!(is_match("abc", "abc"));
        assert!(!is_match("abc", "abd"));
        assert!(is_match("a.c", "abc"));
    }

    #[test]
    fn stdlib_example_pattern() {
        assert!(is_match(".*\\.nl", "src/main.nl"));
        assert!(!is_match(".*\\.nl", "src/main.txt"));
        assert!(is_match(".*", "anything at all"));
    }

    #[test]
    fn quantifiers_and_classes() {
        assert!(is_match("[a-z]+\\.txt", "readme.txt"));
        assert!(!is_match("[a-z]+\\.txt", "README.txt"));
        assert!(is_match("colou?r", "color"));
        assert!(is_match("colou?r", "colour"));
    }

    #[test]
    fn alternation_and_groups() {
        assert!(is_match("(foo|bar)\\.nl", "foo.nl"));
        assert!(is_match("(foo|bar)\\.nl", "bar.nl"));
        assert!(!is_match("(foo|bar)\\.nl", "baz.nl"));
    }

    #[test]
    fn find_is_partial_anywhere() {
        let re = Regex::compile("\\d+").expect("valid pattern");
        let m = re.find("abc123def").expect("should find a match");
        assert_eq!(m.start, 3);
        assert_eq!(m.end, 6);
        assert!(Regex::compile("^\\d+$").expect("valid pattern").find("abc123").is_none());
        assert!(Regex::compile("^\\d+$").expect("valid pattern").find("123").is_some());
    }

    #[test]
    fn find_captures_groups() {
        let re = Regex::compile("(\\d+)-(\\d+)").expect("valid pattern");
        let m = re.find("id 12-34 end").expect("should find a match");
        let chars: Vec<char> = "id 12-34 end".chars().collect();
        let group = |span: Option<(usize, usize)>| span.map(|(s, e)| chars[s..e].iter().collect::<String>());
        assert_eq!(group(m.groups[0]), Some("12".to_string()));
        assert_eq!(group(m.groups[1]), Some("34".to_string()));
    }

    #[test]
    fn find_all_multiple_matches() {
        let re = Regex::compile("\\d+").expect("valid pattern");
        let matches = re.find_all("a1 b22 c333");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].start, 1);
        assert_eq!(matches[1].start, 4);
        assert_eq!(matches[2].start, 8);
    }

    #[test]
    fn escape_metacharacters() {
        assert_eq!(super::escape("a.b*c"), "a\\.b\\*c");
        assert!(is_match(&super::escape("a.b*c"), "a.b*c"));
    }
}

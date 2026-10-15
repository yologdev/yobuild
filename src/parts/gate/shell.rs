//! Reading a command line as the shell does, enough to see what it runs and
//! what it writes. A port of `confirm-dangerous.ts`'s reader.

/// A word as the shell would see it; `known` is false when it holds an
/// expansion (`$VAR`, `~`, a glob, a substitution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    pub text: String,
    pub known: bool,
}

impl Word {
    fn empty() -> Self {
        Self {
            text: String::new(),
            known: true,
        }
    }
}

/// One simple command: its words, the redirections that truncate a file, the
/// commands its substitutions run (`$( … )`, backquotes, `<( … )`, `>( … )`,
/// also inside double quotes and unquoted heredoc bodies), and the operator
/// after it (`;`, `\n`, `&&`, `||`, `|`, `&`, or `end`).
#[derive(Debug, Clone, Default)]
pub(super) struct Simple {
    pub words: Vec<Word>,
    pub truncates: Vec<Word>,
    /// Files appended to (`>>`, `&>>`).
    pub appends: Vec<Word>,
    pub runs: Vec<String>,
    pub after: String,
}

/// A command, or the start / end of a `( … )` subshell.
#[derive(Debug, Clone)]
pub(super) enum Item {
    Command(Simple),
    Open,
    Close,
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `NAME=…` (an assignment before a command).
pub(super) fn is_assignment(text: &str) -> bool {
    match text.find('=') {
        Some(at) if at > 0 => text[..at].chars().all(is_word_char),
        _ => false,
    }
}

/// The index of the backquote closing the one at `open`.
fn end_of_backquote(s: &[char], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            '\\' => i += 1,
            '`' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index of the `)` closing the `(` at `open`, past quotes and nesting.
fn closing(s: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            '\\' => i += 1,
            '\'' => i = find(s, '\'', i + 1)?,
            '"' => i = end_of_double(s, i)?,
            '`' => i = end_of_backquote(s, i)?,
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index of the `"` closing the one at `open`, past substitutions inside.
fn end_of_double(s: &[char], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            '\\' => i += 1,
            '"' => return Some(i),
            '`' => i = end_of_backquote(s, i)?,
            '$' if s.get(i + 1) == Some(&'(') => i = closing(s, i + 1)?,
            _ => {}
        }
        i += 1;
    }
    None
}

fn find(s: &[char], c: char, from: usize) -> Option<usize> {
    (from..s.len()).find(|&i| s[i] == c)
}

fn text(s: &[char], from: usize, to: usize) -> String {
    s[from.min(to)..to].iter().collect()
}

/// The commands that expanding `body` runs (as in double quotes or an
/// unquoted heredoc body): every `$( … )` and backquoted command, and those
/// inside `$(( … ))`. `None` when one is not closed.
fn substitutions(body: &[char]) -> Option<Vec<String>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            '\\' => i += 1,
            '`' => {
                let end = end_of_backquote(body, i)?;
                runs.push(text(body, i + 1, end).replace("\\`", "`"));
                i = end;
            }
            '$' if body.get(i + 1) == Some(&'(') => {
                let end = closing(body, i + 1)?;
                // Arithmetic only when `$((` is closed by `))` as one pair
                // (`$((rm x); ls)` is a command substitution).
                let arithmetic =
                    body.get(i + 2) == Some(&'(') && closing(body, i + 2) == Some(end - 1);
                if arithmetic {
                    // Arithmetic `$(( … ))`: only what it substitutes runs.
                    runs.extend(substitutions(&body[i + 3..end - 1])?);
                } else {
                    runs.push(text(body, i + 2, end));
                }
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
    Some(runs)
}

/// A heredoc waiting for the end of its line.
struct Heredoc {
    delimiter: String,
    strip: bool,
    expands: bool,
    /// The command it feeds (its id).
    feeds: usize,
}

/// Where a word ends: bash splits words on spaces, tabs and newlines only
/// (other Unicode spaces are part of the word), and at its operators.
fn ends_word(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n') || ";&|<>()".contains(c)
}

/// A heredoc's delimiter right after `<<` / `<<-`: one shell word with its
/// quotes removed (`E'O'F` is `EOF`). Quoted anywhere, the body is not
/// expanded. Returns (delimiter, quoted, length read).
fn delimiter(rest: &[char]) -> Option<(String, bool, usize)> {
    let (mut out, mut quoted, mut i) = (String::new(), false, 0);
    while i < rest.len() && !ends_word(rest[i]) {
        match rest[i] {
            '\'' | '"' => {
                let end = find(rest, rest[i], i + 1)?;
                out.push_str(&text(rest, i + 1, end));
                quoted = true;
                i = end;
            }
            '\\' => {
                out.push(*rest.get(i + 1)?);
                quoted = true;
                i += 1;
            }
            c => out.push(c),
        }
        i += 1;
    }
    (i > 0).then_some((out, quoted, i))
}

#[derive(PartialEq)]
enum Redirect {
    None,
    Truncate,
    Append,
    /// `>&word`: a duplicated descriptor when `word` is a number or `-`,
    /// else a file it truncates.
    Duplicate,
    Skip,
}

/// Split a command line into simple commands and subshell markers. Quotes,
/// escapes, substitutions and redirections are read as the shell reads them;
/// a heredoc has its body skipped, from the next line to the delimiter —
/// unless the delimiter is unquoted, when the shell expands the body and the
/// commands it substitutes run. `None` when the line cannot be read (an
/// unclosed quote or substitution).
// The last `finish_command!` resets state nothing reads again.
#[allow(unused_assignments)]
pub(super) fn parse(line: &str) -> Option<Vec<Item>> {
    let s: Vec<char> = line.chars().collect();
    let mut out: Vec<(Option<usize>, Item)> = Vec::new();
    let mut next_id = 0usize;
    let mut cur = Simple::default();
    let mut cur_id = 0usize;
    let mut word: Option<Word> = None;
    let mut redirect = Redirect::None;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    // Runs found in heredoc bodies, for the command each feeds.
    let mut fed: Vec<(usize, Vec<String>)> = Vec::new();

    macro_rules! finish_word {
        () => {
            if let Some(w) = word.take() {
                match redirect {
                    Redirect::Truncate => cur.truncates.push(w),
                    Redirect::Append => cur.appends.push(w),
                    Redirect::Duplicate
                        if w.known
                            && (w.text == "-"
                                || (!w.text.is_empty()
                                    && w.text.chars().all(|c| c.is_ascii_digit()))) => {}
                    Redirect::Duplicate => cur.truncates.push(w),
                    Redirect::Skip => {}
                    Redirect::None => cur.words.push(w),
                }
                redirect = Redirect::None;
            }
        };
    }
    macro_rules! finish_command {
        ($after:expr) => {
            finish_word!();
            if !cur.words.is_empty()
                || !cur.truncates.is_empty()
                || !cur.appends.is_empty()
                || !cur.runs.is_empty()
            {
                cur.after = $after.to_string();
                out.push((Some(cur_id), Item::Command(std::mem::take(&mut cur))));
            } else {
                cur = Simple::default();
            }
            next_id += 1;
            cur_id = next_id;
        };
    }

    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            '\'' => {
                let end = find(&s, '\'', i + 1)?;
                word.get_or_insert_with(Word::empty)
                    .text
                    .push_str(&text(&s, i + 1, end));
                i = end;
            }
            '"' => {
                let end = end_of_double(&s, i)?;
                let body = &s[i + 1..end];
                cur.runs.extend(substitutions(body)?);
                let w = word.get_or_insert_with(Word::empty);
                // Inside double quotes a backslash-newline joins lines too.
                w.text
                    .push_str(&body.iter().collect::<String>().replace("\\\n", ""));
                if body.iter().any(|c| *c == '$' || *c == '`') {
                    w.known = false;
                }
                i = end;
            }
            // A backslash before a newline joins the lines (`r\⏎m` is `rm`).
            '\\' if s.get(i + 1) == Some(&'\n') => i += 1,
            '\\' => {
                let w = word.get_or_insert_with(Word::empty);
                if let Some(next) = s.get(i + 1) {
                    w.text.push(*next);
                }
                i += 1;
            }
            '`' => {
                let end = end_of_backquote(&s, i)?;
                cur.runs.push(text(&s, i + 1, end).replace("\\`", "`"));
                word.get_or_insert_with(Word::empty).known = false;
                i = end;
            }
            '$' if s.get(i + 1) == Some(&'(') => {
                let end = closing(&s, i + 1)?;
                cur.runs.extend(substitutions(&s[i..=end])?);
                word.get_or_insert_with(Word::empty).known = false;
                i = end;
            }
            '<' | '>' if s.get(i + 1) == Some(&'(') => {
                // Process substitution: the command runs; the word is a path to its pipe.
                let end = closing(&s, i + 1)?;
                cur.runs.push(text(&s, i + 2, end));
                word.get_or_insert_with(Word::empty).known = false;
                i = end;
            }
            '\n' => {
                finish_command!("\n");
                for doc in heredocs.drain(..) {
                    let mut body: Vec<String> = Vec::new();
                    loop {
                        let start = i + 1;
                        let end = find(&s, '\n', start).unwrap_or(s.len());
                        let line = text(&s, start, end);
                        i = end;
                        let compared = if doc.strip {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if compared == doc.delimiter {
                            break;
                        }
                        body.push(line);
                        if end >= s.len() {
                            break;
                        }
                    }
                    if doc.expands {
                        let joined: Vec<char> = body.join("\n").chars().collect();
                        fed.push((doc.feeds, substitutions(&joined)?));
                    }
                }
            }
            ' ' | '\t' => {
                finish_word!();
            }
            ';' => {
                finish_command!(";");
            }
            '(' | ')' => {
                finish_command!(if c == '(' { ";" } else { "end" });
                out.push((None, if c == '(' { Item::Open } else { Item::Close }));
            }
            '|' | '&' => {
                if c == '&' && s.get(i + 1) == Some(&'>') {
                    // `&>file`, `&>>file`
                    finish_word!();
                    let append = s.get(i + 2) == Some(&'>');
                    redirect = if append {
                        Redirect::Append
                    } else {
                        Redirect::Truncate
                    };
                    i += if append { 2 } else { 1 };
                } else {
                    let double = s.get(i + 1) == Some(&c);
                    if double {
                        i += 1;
                    }
                    let op: String = if double {
                        [c, c].iter().collect()
                    } else {
                        c.to_string()
                    };
                    finish_command!(op);
                }
            }
            '<' => {
                finish_word!();
                if s.get(i + 1) == Some(&'<') && s.get(i + 2) == Some(&'<') {
                    // A here-string: its word is data (its substitutions still run).
                    redirect = Redirect::Skip;
                    i += 2;
                } else if s.get(i + 1) == Some(&'<') {
                    // A heredoc: read its delimiter; the body comes after this line.
                    i += 1;
                    let strip = s.get(i + 1) == Some(&'-');
                    if strip {
                        i += 1;
                    }
                    while matches!(s.get(i + 1), Some(' ') | Some('\t')) {
                        i += 1;
                    }
                    let (delimiter, quoted, len) = delimiter(&s[i + 1..])?;
                    heredocs.push(Heredoc {
                        delimiter,
                        strip,
                        expands: !quoted,
                        feeds: cur_id,
                    });
                    i += len;
                } else {
                    redirect = Redirect::Skip;
                }
            }
            '>' => {
                // A file descriptor number right before it belongs to the operator.
                let fd = word.as_ref().is_some_and(|w| {
                    w.known && !w.text.is_empty() && w.text.chars().all(|c| c.is_ascii_digit())
                });
                if fd {
                    word = None;
                } else {
                    finish_word!();
                }
                match s.get(i + 1) {
                    Some('>') => {
                        redirect = Redirect::Append;
                        i += 1;
                    }
                    Some('&') => {
                        redirect = Redirect::Duplicate;
                        i += 1;
                    }
                    Some('|') => {
                        redirect = Redirect::Truncate;
                        i += 1;
                    }
                    _ => redirect = Redirect::Truncate,
                }
            }
            _ => {
                let w = word.get_or_insert_with(Word::empty);
                if matches!(c, '$' | '*' | '?' | '{' | '[') || (c == '~' && w.text.is_empty()) {
                    w.known = false;
                }
                w.text.push(c);
            }
        }
        i += 1;
    }
    finish_command!("end");

    // What heredoc bodies run goes with the command they feed (or on its own
    // when that command had nothing else).
    for (feeds, runs) in fed {
        if runs.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(id, _)| *id == Some(feeds)) {
            Some((_, Item::Command(cmd))) => cmd.runs.extend(runs),
            _ => out.push((
                Some(feeds),
                Item::Command(Simple {
                    runs,
                    after: "end".into(),
                    ..Simple::default()
                }),
            )),
        }
    }
    Some(out.into_iter().map(|(_, item)| item).collect())
}

/// The program a command runs, past wrappers that run another program
/// (`env`, `nohup`, `timeout`, `xargs`, …), and its arguments.
pub(super) struct Program<'a> {
    pub name: String,
    pub args: &'a [Word],
    pub via_xargs: bool,
    /// The program's word is literal text (not `$X`, `$(…)`, a glob).
    pub known: bool,
    /// A `GIT_*` variable is set for it (`GIT_CONFIG_*`, `GIT_DIR`, …).
    pub git_env: bool,
}

fn base(text: &str) -> String {
    basename(text.strip_prefix('\\').unwrap_or(text))
}

/// The last path component, trailing slashes ignored (as Node's `basename`).
pub(super) fn basename(text: &str) -> String {
    let trimmed = text.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned()
}

pub(super) fn unwrap(words: &[Word]) -> Program<'_> {
    let mut i = 0;
    let mut via_xargs = false;
    let mut git_env = false;
    let text = |i: usize| words.get(i).map(|w| w.text.as_str()).unwrap_or("");
    loop {
        while i < words.len() && is_assignment(&words[i].text) {
            git_env |= words[i].text.starts_with("GIT_");
            i += 1;
        }
        let name = base(text(i));
        match name.as_str() {
            "nohup" | "builtin" => i += 1,
            "command" => {
                i += 1;
                while ["-p", "-v", "-V", "--"].contains(&text(i)) {
                    i += 1;
                }
            }
            "exec" => {
                i += 1;
                while text(i).starts_with('-') {
                    if text(i) == "-a" {
                        i += 1;
                    }
                    i += 1;
                }
            }
            "time" => {
                i += 1;
                while text(i).starts_with('-') {
                    i += 1;
                }
            }
            "env" | "stdbuf" | "nice" | "ionice" => {
                i += 1;
                while i < words.len() && (text(i).starts_with('-') || is_assignment(text(i))) {
                    git_env |= text(i).starts_with("GIT_") && is_assignment(text(i));
                    // The options that take a value, per wrapper.
                    let with_value: &[&str] = match name.as_str() {
                        "env" => &["-u", "-C", "-S"],
                        "stdbuf" => &["-i", "-o", "-e"],
                        "nice" => &["-n"],
                        _ => &["-c", "-n"],
                    };
                    if with_value.contains(&text(i)) {
                        i += 1;
                    }
                    i += 1;
                }
            }
            "timeout" => {
                i += 1;
                while text(i).starts_with('-') {
                    if ["-s", "-k", "--signal", "--kill-after"].contains(&text(i)) {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1; // the duration
            }
            "xargs" => {
                via_xargs = true;
                i += 1;
                while text(i).starts_with('-') {
                    let t = text(i);
                    let long_with_value = [
                        "--max-args",
                        "--max-lines",
                        "--max-procs",
                        "--delimiter",
                        "--eof",
                        "--arg-file",
                        "--process-slot-var",
                    ]
                    .contains(&t);
                    if (t.len() == 2 && "IdEsnLPa".contains(&t[1..])) || long_with_value {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {
                return Program {
                    known: words.get(i).is_some_and(|w| w.known) && !name.is_empty(),
                    name,
                    args: words.get(i + 1..).unwrap_or(&[]),
                    via_xargs,
                    git_env,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(line: &str) -> Vec<Simple> {
        parse(line)
            .unwrap()
            .into_iter()
            .filter_map(|i| match i {
                Item::Command(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn words(c: &Simple) -> Vec<&str> {
        c.words.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn quotes_escapes_and_operators() {
        let c = commands("echo 'a b' \"c d\" e\\ f && ls|wc");
        assert_eq!(words(&c[0]), ["echo", "a b", "c d", "e f"]);
        assert_eq!(c[0].after, "&&");
        assert_eq!(words(&c[1]), ["ls"]);
        assert_eq!(c[1].after, "|");
        assert_eq!(c[2].after, "end");
    }

    #[test]
    fn redirections_truncate_or_not() {
        let c = commands("echo x > a 2>b >>c 2>&1 &>d >|e < f");
        let t: Vec<&str> = c[0].truncates.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(t, ["a", "b", "d", "e"]);
        assert_eq!(words(&c[0]), ["echo", "x"]);
    }

    #[test]
    fn substitutions_and_heredocs_are_found() {
        let c = commands("echo $(rm x) \"`ls`\" <(cat y)");
        assert_eq!(c[0].runs, ["rm x", "ls", "cat y"]);
        let c = commands("cat <<EOF > out\n$(date)\nEOF\nls");
        assert_eq!(c[0].runs, ["date"]);
        assert_eq!(words(&c[1]), ["ls"]);
        let c = commands("cat <<'EOF'\n$(date)\nEOF");
        assert!(c[0].runs.is_empty());
    }

    #[test]
    fn unclosed_lines_cannot_be_read() {
        assert!(parse("echo \"open").is_none());
        assert!(parse("echo $(open").is_none());
        assert!(parse("echo 'open").is_none());
    }

    #[test]
    fn wrappers_are_seen_through() {
        let w = |line: &str| commands(line).remove(0).words;
        for (line, name) in [
            ("env -i FOO=1 rm x", "rm"),
            ("timeout -s KILL 5 rm x", "rm"),
            ("nice nohup /bin/rm x", "rm"),
            ("\\rm x", "rm"),
        ] {
            let words = w(line);
            assert_eq!(unwrap(&words).name, name, "{line}");
        }
        let words = w("xargs -I {} rm {}");
        let p = unwrap(&words);
        assert!(p.via_xargs);
        assert_eq!(p.name, "rm");
    }
}

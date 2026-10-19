//! Reading a command line as the shell does: commands, their words (and
//! whether each is literal), redirections, what substitutions run, and
//! subshells. Only reading; what a command may do is judged elsewhere.

/// A word as the shell would see it; `known` is false when it holds an
/// expansion (`$VAR`, `~`, a glob, a substitution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    pub text: String,
    pub known: bool,
    /// It begins with an expansion, so it could begin with anything (`-o…`);
    /// otherwise its first character is the literal one in `text`.
    pub opens_unknown: bool,
    /// Where in `text` the first quoted or escaped part begins (`None`:
    /// nothing is): an assignment's name and `=` are unquoted (`'x=1'` runs
    /// a program).
    quoted_from: Option<usize>,
}

impl Word {
    /// A literal word.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            known: true,
            opens_unknown: false,
            quoted_from: None,
        }
    }

    /// A word made of part of this one (`--file=X`'s `X`): unknown if this
    /// one is, and then it could begin with anything.
    pub fn part(&self, text: &str) -> Self {
        Self {
            text: text.to_owned(),
            known: self.known,
            opens_unknown: !self.known,
            // Quoted anywhere, it is never read as an assignment (where the
            // quoting began is lost).
            quoted_from: self.quoted_from.map(|_| 0),
        }
    }

    /// Some of it was quoted or escaped (`"]]"` is not bash's `]]`).
    pub fn quoted(&self) -> bool {
        self.quoted_from.is_some()
    }

    /// Marks what follows as quoted or escaped.
    fn quote(&mut self) {
        self.quoted_from.get_or_insert(self.text.len());
    }

    /// Whether bash reads it as an assignment (`NAME=value`, the name and
    /// `=` unquoted; the value may be quoted).
    pub fn assigns(&self) -> bool {
        is_assignment(&self.text)
            && self
                .quoted_from
                .is_none_or(|from| self.text.find('=').is_some_and(|at| at < from))
    }

    fn empty() -> Self {
        Self::literal("")
    }

    /// Marks it unknown from here on.
    fn expands(&mut self) {
        self.known = false;
        self.opens_unknown |= self.text.is_empty();
    }
}

/// One simple command: its words, the redirections that truncate a file, the
/// commands its substitutions run (`$( … )`, backquotes, `<( … )`, `>( … )`,
/// also inside double quotes and unquoted heredoc bodies), and the operator
/// after it.
#[derive(Debug, Clone, Default)]
pub(super) struct Simple {
    pub words: Vec<Word>,
    pub truncates: Vec<Word>,
    /// Files appended to (`>>`, `&>>`).
    pub appends: Vec<Word>,
    pub runs: Vec<String>,
    pub after: Op,
}

/// What follows a command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Op {
    /// `;` or a newline (also before a `(`).
    Then,
    /// `&&`
    And,
    /// `||`: what follows runs if it failed.
    Or,
    /// `|`
    Pipe,
    /// `&`: it runs in the background, in a subshell.
    Background,
    /// The end of the line or of a `( … )`.
    #[default]
    End,
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
fn is_assignment(text: &str) -> bool {
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
            '$' if s.get(i + 1) == Some(&'{') => i = brace_end(s, i)?,
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

/// Whether the `${ … }` at `at` expands without evaluating anything as code.
/// bash evaluates array subscripts, `${x:offset}` and the value of a
/// variable used in arithmetic as arithmetic, and arithmetic runs any `$( … )`
/// in a subscript it meets (`x='a[$(rm f)]'; echo ${a[x]}`); `${!x}`
/// expands a name it reads at run time, `${x@P}` a prompt. Only the plain
/// forms are read: `${name}`, `${#name}`, `${name[0]}`, `${name[@]}`, and
/// `${name<op>word}` for the default (`-`, `+`, `?`; not `=`, which
/// assigns), pattern and case operators.
fn plain_parameter(s: &[char], at: usize) -> bool {
    let Some(end) = brace_end(s, at) else {
        return false;
    };
    let body = &s[at + 2..end];
    let mut i = usize::from(body.first() == Some(&'#') && body.len() > 1);
    let length = i == 1;
    let start = i;
    match body.get(i) {
        Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
            while body.get(i).is_some_and(|c| is_word_char(*c)) {
                i += 1;
            }
        }
        Some(c) if c.is_ascii_digit() => {
            while body.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
        }
        Some('@' | '*' | '#' | '?' | '-' | '$') => i += 1,
        _ => return false,
    }
    if i == start {
        return false;
    }
    if body.get(i) == Some(&'[') {
        let Some(close) = body[i..].iter().position(|c| *c == ']') else {
            return false;
        };
        let subscript = &body[i + 1..i + close];
        let literal = matches!(subscript, ['@'] | ['*'])
            || (!subscript.is_empty() && subscript.iter().all(char::is_ascii_digit));
        if !literal {
            return false;
        }
        i += close + 1;
    }
    match (body.get(i), length) {
        (None, _) => true,
        (Some(_), true) => false,
        (Some(':'), false) => match body.get(i + 1) {
            // Not `:=`: it assigns the variable (`${PS4:=…}`), past the
            // rules for assignments.
            Some('-' | '+' | '?') => true,
            // `${x:offset:length}`: only literal numbers.
            _ => body[i + 1..]
                .iter()
                .all(|c| c.is_ascii_digit() || matches!(c, ':' | ' ' | '-')),
        },
        (Some('-' | '+' | '?' | '#' | '%' | '/' | '^' | ','), false) => true,
        _ => false,
    }
}

/// The index of the `}` closing the `${` at `at`: bash reads everything
/// up to it as one word (spaces, `;` and `#` included), past quotes,
/// nested expansions and substitutions.
fn brace_end(s: &[char], at: usize) -> Option<usize> {
    let mut i = at + 2;
    while i < s.len() {
        match s[i] {
            '\\' => i += 1,
            '\'' => i = find(s, '\'', i + 1)?,
            '"' => i = end_of_double(s, i)?,
            '`' => i = end_of_backquote(s, i)?,
            '$' if s.get(i + 1) == Some(&'(') => i = closing(s, i + 1)?,
            '$' if s.get(i + 1) == Some(&'{') => i = brace_end(s, i)?,
            '}' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Arithmetic `$(( … ))` text bash evaluates without running anything:
/// literal numbers and operators only (a variable's value would be
/// evaluated as arithmetic too).
fn plain_arithmetic(body: &[char]) -> bool {
    body.iter()
        .all(|c| c.is_ascii_digit() || c.is_whitespace() || "+-*/%()<>=!&|^~?:,".contains(*c))
}

/// Whether the `$` at `at` starts an expansion the gate can read: not
/// `$[ … ]` (old arithmetic), and a `${ … }` only in its plain forms.
fn plain_dollar(s: &[char], at: usize) -> bool {
    match s.get(at + 1) {
        Some('[') => false,
        Some('{') => plain_parameter(s, at),
        _ => true,
    }
}

/// The commands that expanding `body` runs (as in double quotes or an
/// unquoted heredoc body): every `$( … )` and backquoted command. `None`
/// when one is not closed, or when it holds an expansion bash would
/// evaluate as code (see [`plain_dollar`]).
fn substitutions(body: &[char]) -> Option<Vec<String>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            '\\' => i += 1,
            '$' if !plain_dollar(body, i) => return None,
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
                    if !plain_arithmetic(&body[i + 3..end - 1]) {
                        return None;
                    }
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
    // Inside `[[ … ]]`.
    let mut redirect = Redirect::None;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    // Runs found in heredoc bodies, for the command each feeds.
    let mut fed: Vec<(usize, Vec<String>)> = Vec::new();

    macro_rules! finish_word {
        () => {
            if let Some(mut w) = word.take() {
                // A lone `[`, `[[`, `{` or `}` is never expanded: literal.
                if !w.quoted() && matches!(w.text.as_str(), "[" | "[[" | "{" | "}") {
                    w.known = true;
                    w.opens_unknown = false;
                }
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
            // A command that only feeds a heredoc keeps its place: what
            // the body runs, runs there (`<<EOF; cd sub` runs it before the cd).
            if !cur.words.is_empty()
                || !cur.truncates.is_empty()
                || !cur.appends.is_empty()
                || !cur.runs.is_empty()
                || heredocs.iter().any(|h| h.feeds == cur_id)
            {
                cur.after = $after;
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
                let w = word.get_or_insert_with(Word::empty);
                w.quote();
                w.text.push_str(&text(&s, i + 1, end));
                i = end;
            }
            '"' => {
                let end = end_of_double(&s, i)?;
                let body = &s[i + 1..end];
                cur.runs.extend(substitutions(body)?);
                let w = word.get_or_insert_with(Word::empty);
                w.quote();
                let opens = w.text.is_empty() && matches!(body.first(), Some('$' | '`'));
                // Inside double quotes a backslash-newline joins lines too.
                w.text
                    .push_str(&body.iter().collect::<String>().replace("\\\n", ""));
                if body.iter().any(|c| *c == '$' || *c == '`') {
                    w.known = false;
                    w.opens_unknown |= opens;
                }
                i = end;
            }
            // A backslash before a newline joins the lines (`r\⏎m` is `rm`).
            '\\' if s.get(i + 1) == Some(&'\n') => i += 1,
            '\\' => {
                let w = word.get_or_insert_with(Word::empty);
                w.quote();
                if let Some(next) = s.get(i + 1) {
                    w.text.push(*next);
                }
                i += 1;
            }
            '`' => {
                let end = end_of_backquote(&s, i)?;
                cur.runs.push(text(&s, i + 1, end).replace("\\`", "`"));
                word.get_or_insert_with(Word::empty).expands();
                i = end;
            }
            '$' if s.get(i + 1) == Some(&'(') => {
                let end = closing(&s, i + 1)?;
                cur.runs.extend(substitutions(&s[i..=end])?);
                word.get_or_insert_with(Word::empty).expands();
                i = end;
            }
            // `${ … }`: one word to its `}`, whatever it holds.
            '$' if s.get(i + 1) == Some(&'{') => {
                if !plain_parameter(&s, i) {
                    return None;
                }
                let end = brace_end(&s, i)?;
                cur.runs.extend(substitutions(&s[i + 2..end])?);
                let w = word.get_or_insert_with(Word::empty);
                w.expands();
                w.text.push_str(&text(&s, i, end + 1));
                i = end;
            }
            '<' | '>' if s.get(i + 1) == Some(&'(') => {
                // Process substitution: the command runs; the word is a path to its pipe.
                let end = closing(&s, i + 1)?;
                cur.runs.push(text(&s, i + 2, end));
                word.get_or_insert_with(Word::empty).expands();
                i = end;
            }
            '\n' => {
                finish_command!(Op::Then);
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
            // A comment: to the end of the line (a backslash there does not
            // join the next one).
            '#' if word.is_none() => {
                while i + 1 < s.len() && s[i + 1] != '\n' {
                    i += 1;
                }
            }
            ';' => {
                finish_command!(Op::Then);
            }
            // `(( … ))` and `for (( … ))`: arithmetic on variables' values,
            // which runs any `$( … )` a value's subscript holds.
            '(' if s.get(i + 1) == Some(&'(') => return None,
            '(' | ')' => {
                finish_command!(if c == '(' { Op::Then } else { Op::End });
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
                    finish_command!(match (c, double) {
                        ('|', false) => Op::Pipe,
                        ('|', true) => Op::Or,
                        (_, false) => Op::Background,
                        (_, true) => Op::And,
                    });
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
                // bash expands `~` at a word's start, and after `=` or `:`
                // (`of=~/x`, `PATH=a:~/b`).
                let tilde = c == '~' && (w.text.is_empty() || w.text.ends_with(['=', ':']));
                if c == '$' && !plain_dollar(&s, i) {
                    return None;
                }
                if matches!(c, '$' | '*' | '?' | '{' | '[') || tilde {
                    w.expands();
                }
                w.text.push(c);
            }
        }
        i += 1;
    }
    finish_command!(Op::End);

    // What heredoc bodies run goes with the command they feed, which kept
    // its place in the line.
    for (feeds, runs) in fed {
        match out.iter_mut().find(|(id, _)| *id == Some(feeds)) {
            Some((_, Item::Command(cmd))) => cmd.runs.extend(runs),
            _ if runs.is_empty() => {}
            _ => return None,
        }
    }
    Some(out.into_iter().map(|(_, item)| item).collect())
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
        assert_eq!(c[0].after, Op::And);
        assert_eq!(words(&c[1]), ["ls"]);
        assert_eq!(c[1].after, Op::Pipe);
        assert_eq!(c[2].after, Op::End);
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
    fn a_heredoc_alone_keeps_its_place() {
        let c = commands("<<EOF; cd sub\n$(rm x.txt)\nEOF");
        assert_eq!(c[0].runs, ["rm x.txt"]);
        assert_eq!(words(&c[1]), ["cd", "sub"]);
    }

    #[test]
    fn expansions_bash_evaluates_as_code_cannot_be_read() {
        for line in [
            "echo ${!x}", "echo \"${x@P}\"", "echo ${a[x]}", "echo ${a[$i]}", "echo ${x:y}",
            "echo ${x:$(( n ))}", "echo $[x+1]", "echo $((x))", "echo $(( $x + 1 ))",
            "cat <<EOF\n${!x}\nEOF", "echo ${x",
        ] {
            assert!(parse(line).is_none(), "{line}");
        }
        for line in [
            "echo ${x} ${#x} ${a[0]} ${a[@]} ${#a[@]} ${x:-d} ${x%.rs} ${x//a/b}",
            "echo ${x:2:3} ${1} ${#} ${@} \"${x^^}\"",
            "echo $(( 1 + 2 * 3 ))",
            "echo ${x:-$(date)}",
        ] {
            assert!(parse(line).is_some(), "{line}");
        }
    }

    #[test]
    fn unclosed_lines_cannot_be_read() {
        assert!(parse("echo \"open").is_none());
        assert!(parse("echo $(open").is_none());
        assert!(parse("echo 'open").is_none());
    }
}

//! One option grammar for every program the gate knows: a [`Spec`] lists the
//! options it allows; anything else — an unknown option, an abbreviation, a
//! cluster with an unknown letter — is not read, and the gate asks.

use super::parse::Word;

/// The options a program may be given. Short options are `-x`; a cluster
/// (`-la`) is read letter by letter, and a letter that takes a value takes
/// the rest of the cluster or the next word (`-oFILE`, `-o FILE`). Long
/// options are `--name`, with a value as `--name=V` or `--name V`. An
/// option in both lists takes a value only joined (`--color=V`, `-Skey`).
///
/// A value in the next word must not look like an option (`--format
/// --output=x` is not read). A word that begins with an expansion (`"$X"`)
/// could be any option, so it is not read where an option could be; one
/// that begins with literal text (`src/*.rs`) is an operand.
#[derive(Clone, Copy, Debug)]
pub(super) struct Spec {
    /// Options with no value.
    pub flags: &'static [&'static str],
    /// Options that take a value.
    pub valued: &'static [&'static str],
    /// `-5` style counts (`head -5`).
    pub numbers: bool,
    /// Options end at the first operand (`bash script -x`, `awk PROGRAM`).
    pub posix: bool,
    /// Long options with one dash or two (`-tags`, `--tags`, as Go's flags
    /// do); no clusters (`-vo` is not `-v -o`).
    pub single_dash: bool,
}

impl Spec {
    /// Every name `-x` (one letter) or `--name`; checked when the spec is
    /// built (at compile time for a `const` spec).
    pub const fn new(flags: &'static [&'static str], valued: &'static [&'static str]) -> Self {
        check_names(flags, false);
        check_names(valued, false);
        Self {
            flags,
            valued,
            numbers: false,
            posix: false,
            single_dash: false,
        }
    }

    pub const fn numbers(mut self) -> Self {
        self.numbers = true;
        self
    }

    pub const fn posix(mut self) -> Self {
        self.posix = true;
        self
    }

    /// Long options with one dash, as Go's flags (`-tags`): names `-name`,
    /// checked as for [`new`](Self::new).
    pub const fn single_dash(
        flags: &'static [&'static str],
        valued: &'static [&'static str],
    ) -> Self {
        check_names(flags, true);
        check_names(valued, true);
        Self {
            flags,
            valued,
            numbers: false,
            posix: false,
            single_dash: true,
        }
    }

    fn knows(&self, name: &str) -> bool {
        name == "-NUM" || self.flag(name).is_some() || self.valued(name).is_some()
    }

    fn flag(&self, name: &str) -> Option<&'static str> {
        self.flags.iter().copied().find(|f| *f == name)
    }

    fn valued(&self, name: &str) -> Option<&'static str> {
        self.valued.iter().copied().find(|f| *f == name)
    }
}

/// Panics on a name the parser can never produce (`-tags` in a spec that
/// is not `single_dash` reads as `-t -a -g -s`), so a judge looking it up
/// would never see it.
const fn check_names(names: &[&str], single_dash: bool) {
    let mut i = 0;
    while i < names.len() {
        let name = names[i].as_bytes();
        let fine = match (name, single_dash) {
            ([b'-', b'-', _, ..], false) => true,
            ([b'-', c], false) => *c != b'-',
            ([b'-', c, ..], true) => *c != b'-',
            _ => false,
        };
        if !fine {
            panic!("an option name that could never be given");
        }
        i += 1;
    }
}

/// A program's arguments, read by its [`Spec`].
#[derive(Debug)]
pub(super) struct Parsed {
    spec: Spec,
    /// Options in order, with their values.
    pub options: Vec<(&'static str, Option<Word>)>,
    pub operands: Vec<Word>,
}

impl Parsed {
    /// Whether `name` was given. A name not in the spec is a judge's
    /// mistake (it would never be seen): it panics in a debug build, so
    /// tests catch it (as for [`value`](Self::value) and
    /// [`values`](Self::values)).
    pub fn has(&self, name: &str) -> bool {
        debug_assert!(self.spec.knows(name), "{name} is not in the spec");
        self.options.iter().any(|(o, _)| *o == name)
    }

    pub fn value(&self, name: &str) -> Option<&Word> {
        debug_assert!(self.spec.knows(name), "{name} is not in the spec");
        self.options
            .iter()
            .find(|(o, _)| *o == name)
            .and_then(|(_, v)| v.as_ref())
    }

    pub fn values<'a>(&'a self, names: &'a [&str]) -> impl Iterator<Item = &'a Word> {
        debug_assert!(
            names.iter().all(|n| self.spec.knows(n)),
            "{names:?} are not all in the spec"
        );
        self.options
            .iter()
            .filter(move |(o, _)| names.contains(o))
            .filter_map(|(_, v)| v.as_ref())
    }

    pub fn operand(&self, at: usize) -> Option<&Word> {
        self.operands.get(at)
    }
}

/// Reads all of `args`; `None` when any of them is not an option `spec`
/// allows (or an option is missing its value).
pub(super) fn parse(spec: &Spec, args: &[Word]) -> Option<Parsed> {
    let (parsed, rest) = parse_until(spec, args, usize::MAX)?;
    debug_assert!(rest == args.len());
    Some(parsed)
}

/// Reads options and up to `operands` operands, stopping there (a wrapper's
/// own arguments before the command it runs). Returns where it stopped.
pub(super) fn parse_until(spec: &Spec, args: &[Word], operands: usize) -> Option<(Parsed, usize)> {
    let mut out = Parsed {
        spec: *spec,
        options: Vec::new(),
        operands: Vec::new(),
    };
    let mut i = 0;
    let mut options_end = false;
    while i < args.len() {
        let word = &args[i];
        let text = word.text.as_str();
        if !options_end && word.opens_unknown {
            return None;
        }
        if options_end || text == "-" || !text.starts_with('-') {
            if out.operands.len() == operands {
                return Some((out, i));
            }
            out.operands.push(word.clone());
            options_end |= spec.posix;
            i += 1;
            continue;
        }
        if text == "--" {
            options_end = true;
            i += 1;
            continue;
        }
        if spec.numbers && text[1..].chars().all(|c| c.is_ascii_digit()) && text.len() > 1 {
            out.options.push(("-NUM", None));
            i += 1;
            continue;
        }
        let long = match spec.single_dash {
            true => Some(("-", text.trim_start_matches('-'))),
            false => text.strip_prefix("--").map(|body| ("--", body)),
        };
        if let Some((dashes, body)) = long {
            let (name, joined) = match body.split_once('=') {
                Some((name, value)) => (format!("{dashes}{name}"), Some(value)),
                None => (format!("{dashes}{body}"), None),
            };
            match (joined, spec.flag(&name), spec.valued(&name)) {
                (None, Some(option), _) => out.options.push((option, None)),
                (Some(value), _, Some(option)) => {
                    let value = word.part(value);
                    out.options.push((option, Some(value)));
                }
                (None, None, Some(option)) => {
                    i += 1;
                    out.options.push((option, Some(next_value(args, i)?)));
                }
                _ => return None,
            }
            i += 1;
            continue;
        }
        // A cluster of short options.
        let letters: Vec<char> = text[1..].chars().collect();
        let mut at = 0;
        while at < letters.len() {
            let name = format!("-{}", letters[at]);
            let rest: String = letters[at + 1..].iter().collect();
            // In both lists: a value only when joined (`-S`, `-Skey`).
            let optional = rest.is_empty() && spec.flag(&name).is_some();
            if let Some(option) = spec.valued(&name).filter(|_| !optional) {
                let value = if rest.is_empty() {
                    i += 1;
                    next_value(args, i)?
                } else {
                    word.part(&rest)
                };
                out.options.push((option, Some(value)));
                break;
            }
            out.options.push((spec.flag(&name)?, None));
            at += 1;
        }
        i += 1;
    }
    Some((out, i))
}

/// An option's value from the next word: not one that looks like an option.
fn next_value(args: &[Word], at: usize) -> Option<Word> {
    args.get(at)
        .filter(|w| !(w.text.starts_with('-') && w.text.len() > 1))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<Word> {
        line.split_whitespace().map(Word::literal).collect()
    }

    const CP: Spec = Spec::new(&["-r", "-v", "--recursive"], &["-t", "--target-directory"]);

    #[test]
    fn clusters_joined_values_and_long_forms() {
        let p = parse(&CP, &words("-rv -tdir a b")).unwrap();
        assert!(p.has("-r") && p.has("-v"));
        assert_eq!(p.value("-t").unwrap().text, "dir");
        assert_eq!(p.operands.len(), 2);
        let p = parse(&CP, &words("--target-directory=dir a")).unwrap();
        assert_eq!(p.value("--target-directory").unwrap().text, "dir");
        let p = parse(&CP, &words("--target-directory dir a")).unwrap();
        assert_eq!(p.value("--target-directory").unwrap().text, "dir");
    }

    #[test]
    fn anything_not_in_the_spec_is_not_read() {
        for line in ["-x a", "-rx a", "--recurs a", "--recursive=yes a", "-t"] {
            assert!(parse(&CP, &words(line)).is_none(), "{line}");
        }
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let p = parse(&CP, &words("-- -a -r")).unwrap();
        assert_eq!(p.operands.len(), 2);
        assert!(!p.has("-r"));
    }

    #[test]
    fn an_expansion_where_an_option_could_be_is_not_read() {
        let unknown = |text: &str, opens_unknown| Word {
            text: text.into(),
            known: false,
            opens_unknown,
        };
        assert!(parse(&CP, &[unknown("$X", true)]).is_none());
        assert!(parse(&CP, &[unknown("-r$X", false)]).is_none());
        // After `--`, or when it begins with literal text, it is an operand.
        let p = parse(&CP, &[Word::literal("--"), unknown("$X", true)]).unwrap();
        assert_eq!(p.operands.len(), 1);
        let p = parse(&CP, &[unknown("src/*.rs", false)]).unwrap();
        assert_eq!(p.operands.len(), 1);
        // A joined value keeps what it is.
        let p = parse(&CP, &[unknown("--target-directory=$D", false)]).unwrap();
        assert!(!p.value("--target-directory").unwrap().known);
    }

    #[test]
    fn a_value_does_not_swallow_an_option() {
        assert!(parse(&CP, &words("-t --recursive a")).is_none());
        assert!(parse(&CP, &words("--target-directory -r a")).is_none());
        assert_eq!(
            parse(&CP, &words("-t - a"))
                .unwrap()
                .value("-t")
                .unwrap()
                .text,
            "-"
        );
    }

    #[test]
    fn a_long_option_in_both_lists_takes_a_value_only_joined() {
        const LOG: Spec = Spec::new(&["--pretty"], &["--pretty"]);
        let p = parse(&LOG, &words("--pretty HEAD")).unwrap();
        assert!(p.value("--pretty").is_none());
        assert_eq!(p.operands.len(), 1);
        let p = parse(&LOG, &words("--pretty=oneline")).unwrap();
        assert_eq!(p.value("--pretty").unwrap().text, "oneline");
    }

    #[test]
    fn posix_options_end_at_the_first_operand() {
        const SH: Spec = Spec::new(&["-c", "-x"], &[]).posix();
        let p = parse(&SH, &words("-x script -c")).unwrap();
        assert!(!p.has("-c"));
        assert_eq!(p.operands.len(), 2);
    }

    #[test]
    fn single_dash_long_options() {
        const GO: Spec = Spec::single_dash(&["-v"], &["-o", "-tags"]);
        let p = parse(&GO, &words("-tags=x --o out -v ./...")).unwrap();
        assert_eq!(p.value("-tags").unwrap().text, "x");
        assert_eq!(p.value("-o").unwrap().text, "out");
        assert!(parse(&GO, &words("-exec x")).is_none());
        assert!(parse(&GO, &words("-vo out")).is_none());
    }

    #[test]
    fn a_wrapper_stops_at_its_command() {
        let spec = Spec::new(&["-i"], &["-u"]);
        let args = words("-i -u X ls -la");
        let (p, at) = parse_until(&spec, &args, 0).unwrap();
        assert!(p.has("-i"));
        assert_eq!(args[at].text, "ls");
    }
}

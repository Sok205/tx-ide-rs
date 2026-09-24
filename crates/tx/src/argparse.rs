//! A Python 3.14 `argparse` compatible parser, limited to the features `tx`'s CLI uses.
//!
//! Output (usage lines, error messages, `-h` help and its line wrapping) is byte-compatible with
//! CPython's `argparse.ArgumentParser` + `HelpFormatter` (colour disabled), including the
//! `$COLUMNS`-driven width, long-option prefix abbreviation, short-option clustering and `--`.
//!
//! ```
//! use tx::argparse::{Arg, Parser};
//!
//! let parser = Parser::new("tx tag")
//!     .description("Read or set a session's tags (comma-separated).")
//!     .arg(Arg::positional("name"))
//!     .arg(Arg::positional("tags").optional());
//! let matches = parser.parse(&["w".to_string()]).unwrap();
//! assert_eq!(matches.get_one("name"), Some("w"));
//! assert_eq!(matches.get_one("tags"), None);
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Converts a raw argument string; `Err(msg)` is Python's `ArgumentTypeError(msg)`.
pub type TypeFn = fn(&str) -> Result<String, String>;

/// A parsed value in the namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    None,
    Bool(bool),
    Str(String),
    Int(i64),
    List(Vec<String>),
}

/// What the caller must emit before exiting: `-h` (stdout, 0) or an error (stderr, 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseExit {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl ParseExit {
    /// Writes both streams and returns the exit code.
    pub fn emit(&self) -> i32 {
        use std::io::Write;
        // Python's `_print_message` swallows write errors; so do we.
        let _ = std::io::stdout().write_all(self.stdout.as_bytes());
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().write_all(self.stderr.as_bytes());
        self.code
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Store,
    StoreTrue,
    Append,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nargs {
    One,
    Optional,
    Zero,
}

#[derive(Debug, Clone, Copy)]
enum ValueType {
    Str,
    Int,
    Custom(TypeFn),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Choices {
    Strs(Vec<String>),
    Ints(Vec<i64>),
}

impl Choices {
    fn joined(&self, sep: &str) -> String {
        match self {
            Self::Strs(items) => items.join(sep),
            Self::Ints(items) => items
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(sep),
        }
    }

    /// The `invalid choice` list. The contract (port-tests T-CLI-03, T-ENG-01, T-MODEL-05) pins the
    /// quoted form `'1', '2'` of the CPython build the spec was captured on, for int choices too;
    /// CPython 3.14.4 prints them bare.
    fn quoted(&self, sep: &str) -> String {
        match self {
            Self::Strs(items) => items
                .iter()
                .map(|item| py_repr(item))
                .collect::<Vec<_>>()
                .join(sep),
            Self::Ints(items) => items
                .iter()
                .map(|item| py_repr(&item.to_string()))
                .collect::<Vec<_>>()
                .join(sep),
        }
    }

    fn contains(&self, value: &Converted) -> bool {
        match (self, value) {
            (Self::Strs(items), Converted::Str(s)) => items.contains(s),
            (Self::Ints(items), Converted::Int(n)) => items.contains(n),
            _ => false,
        }
    }
}

/// One `add_argument` call.
#[derive(Debug, Clone)]
pub struct Arg {
    option_strings: Vec<String>,
    dest: String,
    action: Action,
    nargs: Nargs,
    required: bool,
    choices: Option<Choices>,
    value_type: ValueType,
    metavar: Option<String>,
    default: Value,
    constant: Option<String>,
    help: Option<String>,
}

impl Arg {
    /// A positional argument (`add_argument("name")`).
    pub fn positional(name: &str) -> Self {
        Self::base(Vec::new(), name.to_string(), true)
    }

    /// An optional argument with a single option string (`add_argument("--tag")`).
    pub fn option(flag: &str) -> Self {
        Self::options(&[flag])
    }

    /// An optional argument with several option strings (`add_argument("-f", "--filter")`).
    pub fn options(flags: &[&str]) -> Self {
        let option_strings: Vec<String> = flags.iter().map(|flag| flag.to_string()).collect();
        let dest = option_strings
            .iter()
            .find(|flag| flag.starts_with("--"))
            .or(option_strings.first())
            .map(|flag| flag.trim_start_matches('-').replace('-', "_"))
            .unwrap_or_default();
        Self::base(option_strings, dest, false)
    }

    fn base(option_strings: Vec<String>, dest: String, required: bool) -> Self {
        Self {
            option_strings,
            dest,
            action: Action::Store,
            nargs: Nargs::One,
            required,
            choices: None,
            value_type: ValueType::Str,
            metavar: None,
            default: Value::None,
            constant: None,
            help: None,
        }
    }

    fn help_action() -> Self {
        Self {
            action: Action::Help,
            nargs: Nargs::Zero,
            help: Some("show this help message and exit".to_string()),
            ..Self::base(vec!["-h".into(), "--help".into()], "help".into(), false)
        }
    }

    /// `required=True`.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// `action="store_true"`.
    pub fn flag(mut self) -> Self {
        self.action = Action::StoreTrue;
        self.nargs = Nargs::Zero;
        self.default = Value::Bool(false);
        self
    }

    /// `action="append"`.
    pub fn append(mut self) -> Self {
        self.action = Action::Append;
        self
    }

    /// `nargs="?"` (a positional becomes optional).
    pub fn optional(mut self) -> Self {
        self.nargs = Nargs::Optional;
        if self.option_strings.is_empty() {
            self.required = false;
        }
        self
    }

    /// `choices=[...]` of strings.
    pub fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = Some(Choices::Strs(
            choices.iter().map(|choice| choice.to_string()).collect(),
        ));
        self
    }

    /// `type=int, choices=range(...)`.
    pub fn int_choices(mut self, choices: impl IntoIterator<Item = i64>) -> Self {
        self.value_type = ValueType::Int;
        self.choices = Some(Choices::Ints(choices.into_iter().collect()));
        self
    }

    /// `type=int`.
    pub fn int(mut self) -> Self {
        self.value_type = ValueType::Int;
        self
    }

    /// `type=<callback>`; the callback's `Err` is an `ArgumentTypeError`.
    pub fn value_parser(mut self, parse: TypeFn) -> Self {
        self.value_type = ValueType::Custom(parse);
        self
    }

    pub fn metavar(mut self, metavar: &str) -> Self {
        self.metavar = Some(metavar.to_string());
        self
    }

    pub fn dest(mut self, dest: &str) -> Self {
        self.dest = dest.to_string();
        self
    }

    /// `default="..."` (converted through the type at the end of parsing, like argparse).
    pub fn default(mut self, default: &str) -> Self {
        self.default = Value::Str(default.to_string());
        self
    }

    /// `const="..."` for `nargs="?"` options.
    pub fn constant(mut self, constant: &str) -> Self {
        self.constant = Some(constant.to_string());
        self
    }

    pub fn help(mut self, help: &str) -> Self {
        self.help = Some(help.to_string());
        self
    }

    fn is_positional(&self) -> bool {
        self.option_strings.is_empty()
    }

    /// `_get_action_name`.
    fn name(&self) -> String {
        if !self.option_strings.is_empty() {
            self.option_strings.join("/")
        } else if let Some(metavar) = &self.metavar {
            metavar.clone()
        } else {
            self.dest.clone()
        }
    }

    /// `_metavar_formatter` result for a single slot.
    fn metavar_text(&self) -> String {
        if let Some(metavar) = &self.metavar {
            metavar.clone()
        } else if let Some(choices) = &self.choices {
            format!("{{{}}}", choices.joined(","))
        } else if self.is_positional() {
            self.dest.clone()
        } else {
            self.dest.to_uppercase()
        }
    }

    /// `_format_args`.
    fn format_args(&self) -> String {
        let metavar = self.metavar_text();
        match self.nargs {
            Nargs::One => metavar,
            Nargs::Optional => format!("[{metavar}]"),
            Nargs::Zero => String::new(),
        }
    }

    /// `_format_action_invocation`.
    fn invocation(&self) -> String {
        if self.is_positional() {
            self.metavar_text()
        } else if self.nargs == Nargs::Zero {
            self.option_strings.join(", ")
        } else {
            format!("{} {}", self.option_strings.join(", "), self.format_args())
        }
    }

    /// One part of the usage line (`_get_actions_usage_parts`).
    fn usage_part(&self) -> String {
        if self.is_positional() {
            return self.format_args();
        }
        let first = &self.option_strings[0];
        let part = if self.nargs == Nargs::Zero {
            first.clone()
        } else {
            format!("{first} {}", self.format_args())
        };
        if self.required {
            part
        } else {
            format!("[{part}]")
        }
    }
}

/// The parsed namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matches {
    values: Vec<(String, Value)>,
}

impl Matches {
    pub fn value(&self, dest: &str) -> Option<&Value> {
        self.values
            .iter()
            .find(|(name, _)| name == dest)
            .map(|(_, value)| value)
    }

    /// A string value, `None` when unset.
    pub fn get_one(&self, dest: &str) -> Option<&str> {
        match self.value(dest) {
            Some(Value::Str(value)) => Some(value),
            _ => None,
        }
    }

    pub fn get_int(&self, dest: &str) -> Option<i64> {
        match self.value(dest) {
            Some(Value::Int(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn get_flag(&self, dest: &str) -> bool {
        matches!(self.value(dest), Some(Value::Bool(true)))
    }

    /// An `append` list, `None` when the option never appeared.
    pub fn get_many(&self, dest: &str) -> Option<&[String]> {
        match self.value(dest) {
            Some(Value::List(values)) => Some(values),
            _ => None,
        }
    }

    /// All `(dest, value)` pairs in `add_argument` order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    fn set(&mut self, dest: &str, value: Value) {
        match self.values.iter_mut().find(|(name, _)| name == dest) {
            Some(slot) => slot.1 = value,
            None => self.values.push((dest.to_string(), value)),
        }
    }
}

/// `ArgumentParser(prog=..., description=...)`.
#[derive(Debug, Clone)]
pub struct Parser {
    prog: String,
    description: Option<String>,
    args: Vec<Arg>,
    columns: Option<usize>,
}

enum Converted {
    Str(String),
    Int(i64),
}

impl fmt::Display for Converted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(value) => f.write_str(value),
            Self::Int(value) => write!(f, "{value}"),
        }
    }
}

impl From<Converted> for Value {
    fn from(value: Converted) -> Self {
        match value {
            Converted::Str(value) => Self::Str(value),
            Converted::Int(value) => Self::Int(value),
        }
    }
}

enum Stop {
    Error(String),
    Help,
}

#[derive(Debug, Clone)]
struct OptionTuple {
    action: Option<usize>,
    option_string: String,
    sep: Option<String>,
    explicit_arg: Option<String>,
}

const OPT: u8 = b'O';
const ARG: u8 = b'A';
const DASH: u8 = b'-';

impl Parser {
    pub fn new(prog: &str) -> Self {
        Self {
            prog: prog.to_string(),
            description: None,
            args: vec![Arg::help_action()],
            columns: None,
        }
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn arg(mut self, arg: Arg) -> Self {
        self.args.push(arg);
        self
    }

    /// Fixes the terminal width instead of reading `$COLUMNS` / the tty at format time.
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = Some(columns);
        self
    }

    pub fn prog(&self) -> &str {
        &self.prog
    }

    /// `parser.parse_args(argv)`.
    pub fn parse(&self, argv: &[String]) -> Result<Matches, ParseExit> {
        match self.parse_inner(argv) {
            Ok(matches) => Ok(matches),
            Err(Stop::Help) => Err(ParseExit {
                stdout: self.format_help(),
                stderr: String::new(),
                code: 0,
            }),
            Err(Stop::Error(message)) => Err(self.error(&message)),
        }
    }

    /// `parser.error(msg)`: usage + `prog: error: msg` on stderr, exit 2.
    pub fn error(&self, message: &str) -> ParseExit {
        ParseExit {
            stdout: String::new(),
            stderr: format!("{}{}: error: {message}\n", self.format_usage(), self.prog),
            code: 2,
        }
    }

    /// `parser.format_usage()`.
    pub fn format_usage(&self) -> String {
        let width = self.width();
        let mut usage = self.usage_text(width);
        usage.push('\n');
        usage
    }

    /// `parser.format_help()`.
    pub fn format_help(&self) -> String {
        let width = self.width();
        let max_help_position = 24.min((width - 20).max(4));
        let action_max_length = self
            .args
            .iter()
            .map(|arg| as_isize(char_len(&arg.invocation())) + 2)
            .max()
            .unwrap_or(0);
        let help_position = (action_max_length + 2).min(max_help_position);

        let mut out = format!("{}\n\n", self.usage_text(width));
        if let Some(description) = &self.description {
            let lines = wrap(&normalize_whitespace(description), width.max(11));
            if !lines.is_empty() {
                out.push_str(&lines.join("\n"));
                out.push_str("\n\n");
            }
        }
        let positionals: Vec<&Arg> = self.args.iter().filter(|a| a.is_positional()).collect();
        let optionals: Vec<&Arg> = self.args.iter().filter(|a| !a.is_positional()).collect();
        for (heading, section) in [
            ("positional arguments", positionals),
            ("options", optionals),
        ] {
            if section.is_empty() {
                continue;
            }
            out.push('\n');
            out.push_str(heading);
            out.push_str(":\n");
            for arg in section {
                out.push_str(&format_action(arg, width, help_position));
            }
            out.push('\n');
        }
        collapse_newlines(&out)
    }

    fn width(&self) -> isize {
        as_isize(self.columns.unwrap_or_else(terminal_columns)) - 2
    }

    /// `HelpFormatter._format_usage` without the trailing blank line.
    fn usage_text(&self, text_width: isize) -> String {
        let prefix = "usage: ";
        let prog = self.prog.as_str();
        let parts_of = |positional: bool| -> Vec<String> {
            self.args
                .iter()
                .filter(|arg| arg.is_positional() == positional)
                .map(Arg::usage_part)
                .collect()
        };
        let opt_parts = parts_of(false);
        let pos_parts = parts_of(true);

        let usage = std::iter::once(prog)
            .chain(opt_parts.iter().map(String::as_str))
            .chain(pos_parts.iter().map(String::as_str))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if as_isize(char_len(prefix) + char_len(&usage)) <= text_width {
            return format!("{prefix}{usage}");
        }

        let prog_len = char_len(prog);
        let with_prog = |parts: &[String]| -> Vec<String> {
            std::iter::once(prog.to_string())
                .chain(parts.iter().cloned())
                .collect()
        };
        let lines = if (char_len(prefix) + prog_len) as f64 <= 0.75 * text_width as f64 {
            let indent = " ".repeat(char_len(prefix) + prog_len + 1);
            if !opt_parts.is_empty() {
                let mut lines =
                    usage_lines(&with_prog(&opt_parts), &indent, Some(prefix), text_width);
                lines.extend(usage_lines(&pos_parts, &indent, None, text_width));
                lines
            } else if !pos_parts.is_empty() {
                usage_lines(&with_prog(&pos_parts), &indent, Some(prefix), text_width)
            } else {
                vec![prog.to_string()]
            }
        } else {
            let indent = " ".repeat(char_len(prefix));
            let all: Vec<String> = opt_parts.iter().chain(&pos_parts).cloned().collect();
            let mut lines = usage_lines(&all, &indent, None, text_width);
            if lines.len() > 1 {
                lines = usage_lines(&opt_parts, &indent, None, text_width);
                lines.extend(usage_lines(&pos_parts, &indent, None, text_width));
            }
            let mut with = vec![prog.to_string()];
            with.extend(lines);
            with
        };
        format!("{prefix}{}", lines.join("\n"))
    }

    fn option_string_map(&self) -> impl Iterator<Item = (&str, usize)> {
        self.args.iter().enumerate().flat_map(|(index, arg)| {
            arg.option_strings
                .iter()
                .map(move |flag| (flag.as_str(), index))
        })
    }

    fn lookup(&self, option_string: &str) -> Option<usize> {
        self.option_string_map()
            .find(|(flag, _)| *flag == option_string)
            .map(|(_, index)| index)
    }

    /// `_parse_optional`.
    fn parse_optional(&self, arg_string: &str) -> Option<Vec<OptionTuple>> {
        if !arg_string.starts_with('-') {
            return None;
        }
        if let Some(action) = self.lookup(arg_string) {
            return Some(vec![OptionTuple {
                action: Some(action),
                option_string: arg_string.to_string(),
                sep: None,
                explicit_arg: None,
            }]);
        }
        if char_len(arg_string) == 1 {
            return None;
        }
        if let Some((option_string, explicit_arg)) = arg_string.split_once('=')
            && let Some(action) = self.lookup(option_string)
        {
            return Some(vec![OptionTuple {
                action: Some(action),
                option_string: option_string.to_string(),
                sep: Some("=".to_string()),
                explicit_arg: Some(explicit_arg.to_string()),
            }]);
        }
        let tuples = self.option_tuples(arg_string);
        if !tuples.is_empty() {
            return Some(tuples);
        }
        if looks_negative(arg_string) || arg_string.contains(' ') {
            return None;
        }
        Some(vec![OptionTuple {
            action: None,
            option_string: arg_string.to_string(),
            sep: None,
            explicit_arg: None,
        }])
    }

    /// `_get_option_tuples` (allow_abbrev=True).
    fn option_tuples(&self, arg_string: &str) -> Vec<OptionTuple> {
        let (option_prefix, sep, explicit_arg) = match arg_string.split_once('=') {
            Some((prefix, explicit)) => (prefix, Some("=".to_string()), Some(explicit.to_string())),
            None => (arg_string, None, None),
        };
        let abbreviation = |flag: &str, action: usize| OptionTuple {
            action: Some(action),
            option_string: flag.to_string(),
            sep: sep.clone(),
            explicit_arg: explicit_arg.clone(),
        };
        if arg_string.starts_with("--") {
            return self
                .option_string_map()
                .filter(|(flag, _)| flag.starts_with(option_prefix))
                .map(|(flag, action)| abbreviation(flag, action))
                .collect();
        }
        let split = arg_string
            .char_indices()
            .nth(2)
            .map_or(arg_string.len(), |(index, _)| index);
        let (short_prefix, short_explicit) = arg_string.split_at(split);
        self.option_string_map()
            .filter_map(|(flag, action)| {
                if flag == short_prefix {
                    Some(OptionTuple {
                        action: Some(action),
                        option_string: flag.to_string(),
                        sep: Some(String::new()),
                        explicit_arg: Some(short_explicit.to_string()),
                    })
                } else if flag.starts_with(option_prefix) {
                    Some(abbreviation(flag, action))
                } else {
                    None
                }
            })
            .collect()
    }

    fn parse_inner(&self, arg_strings: &[String]) -> Result<Matches, Stop> {
        let mut state = ParseState {
            parser: self,
            arg_strings,
            pattern: Vec::with_capacity(arg_strings.len()),
            option_indices: HashMap::new(),
            namespace: Matches::default(),
            seen: HashSet::new(),
            extras: Vec::new(),
            positionals: (0..self.args.len())
                .filter(|&index| self.args[index].is_positional())
                .collect(),
        };
        for arg in &self.args {
            if arg.action != Action::Help {
                state.namespace.set(&arg.dest, arg.default.clone());
            }
        }

        let mut after_dashes = false;
        for (index, arg_string) in arg_strings.iter().enumerate() {
            if after_dashes {
                state.pattern.push(ARG);
            } else if arg_string == "--" {
                after_dashes = true;
                state.pattern.push(DASH);
            } else if let Some(tuples) = self.parse_optional(arg_string) {
                state.option_indices.insert(index, tuples);
                state.pattern.push(OPT);
            } else {
                state.pattern.push(ARG);
            }
        }

        let mut start_index = 0;
        if let Some(max_option_index) = state.option_indices.keys().max().copied() {
            while start_index <= max_option_index {
                let mut next_option_index = start_index;
                while next_option_index <= max_option_index
                    && !state.option_indices.contains_key(&next_option_index)
                {
                    next_option_index += 1;
                }
                if start_index != next_option_index {
                    let positionals_end = state.consume_positionals(start_index)?;
                    if positionals_end > start_index {
                        start_index = positionals_end;
                        continue;
                    }
                    start_index = positionals_end;
                }
                if !state.option_indices.contains_key(&start_index) {
                    state
                        .extras
                        .extend_from_slice(&arg_strings[start_index..next_option_index]);
                    start_index = next_option_index;
                }
                start_index = state.consume_optional(start_index)?;
            }
        }
        let stop_index = state.consume_positionals(start_index)?;
        state.extras.extend_from_slice(&arg_strings[stop_index..]);

        let mut required = Vec::new();
        for (index, arg) in self.args.iter().enumerate() {
            if state.seen.contains(&index) {
                continue;
            }
            if arg.required {
                required.push(arg.name());
            } else if let Value::Str(default) = &arg.default
                && state.namespace.value(&arg.dest) == Some(&arg.default)
            {
                let converted = convert(arg, default)?;
                state.namespace.set(&arg.dest, converted.into());
            }
        }
        if !required.is_empty() {
            return Err(Stop::Error(format!(
                "the following arguments are required: {}",
                required.join(", ")
            )));
        }
        if !state.extras.is_empty() {
            return Err(Stop::Error(format!(
                "unrecognized arguments: {}",
                state.extras.join(" ")
            )));
        }
        Ok(state.namespace)
    }
}

struct ParseState<'a> {
    parser: &'a Parser,
    arg_strings: &'a [String],
    pattern: Vec<u8>,
    option_indices: HashMap<usize, Vec<OptionTuple>>,
    namespace: Matches,
    seen: HashSet<usize>,
    extras: Vec<String>,
    positionals: Vec<usize>,
}

impl ParseState<'_> {
    fn take_action(&mut self, index: usize, arg_strings: &[String]) -> Result<(), Stop> {
        let arg = &self.parser.args[index];
        self.seen.insert(index);
        let value = values(arg, arg_strings)?;
        match arg.action {
            Action::Help => return Err(Stop::Help),
            Action::Store => self.namespace.set(&arg.dest, value),
            Action::StoreTrue => self.namespace.set(&arg.dest, Value::Bool(true)),
            Action::Append => {
                let mut items = match self.namespace.value(&arg.dest) {
                    Some(Value::List(items)) => items.clone(),
                    _ => Vec::new(),
                };
                items.push(match value {
                    Value::Str(item) => item,
                    Value::Int(item) => item.to_string(),
                    _ => String::new(),
                });
                self.namespace.set(&arg.dest, Value::List(items));
            }
        }
        Ok(())
    }

    fn consume_optional(&mut self, start_index: usize) -> Result<usize, Stop> {
        let tuples = self.option_indices.remove(&start_index).unwrap_or_default();
        if tuples.len() > 1 {
            let options: Vec<&str> = tuples.iter().map(|t| t.option_string.as_str()).collect();
            return Err(Stop::Error(format!(
                "ambiguous option: {} could match {}",
                self.arg_strings[start_index],
                options.join(", ")
            )));
        }
        let Some(OptionTuple {
            mut action,
            mut option_string,
            mut sep,
            mut explicit_arg,
        }) = tuples.into_iter().next()
        else {
            return Ok(start_index + 1);
        };

        let mut action_tuples: Vec<(usize, Vec<String>)> = Vec::new();
        let stop = loop {
            let Some(index) = action else {
                self.extras.push(self.arg_strings[start_index].clone());
                return Ok(start_index + 1);
            };
            let arg = &self.parser.args[index];
            let Some(explicit) = explicit_arg.take() else {
                let start = start_index + 1;
                let stop = start + match_option(arg, &self.pattern[start..])?;
                action_tuples.push((index, self.arg_strings[start..stop].to_vec()));
                break stop;
            };
            let arg_count = match_option(arg, &[ARG])?;
            let single_dash = option_string.chars().nth(1).is_some_and(|c| c != '-');
            if arg_count == 1 {
                action_tuples.push((index, vec![explicit]));
                break start_index + 1;
            }
            if !(arg_count == 0 && single_dash && !explicit.is_empty()) {
                return Err(Stop::Error(ignored_explicit(arg, &explicit)));
            }
            if sep.as_deref().is_some_and(|s| !s.is_empty()) || explicit.starts_with('-') {
                return Err(Stop::Error(ignored_explicit(arg, &explicit)));
            }
            action_tuples.push((index, Vec::new()));
            let mut rest = explicit.chars();
            let first_char = rest.next().unwrap_or_default();
            option_string = format!("-{first_char}");
            let Some(next) = self.parser.lookup(&option_string) else {
                self.extras.push(format!("-{explicit}"));
                break start_index + 1;
            };
            action = Some(next);
            let rest = rest.as_str();
            (sep, explicit_arg) = if rest.is_empty() {
                (None, None)
            } else if let Some(stripped) = rest.strip_prefix('=') {
                (Some("=".to_string()), Some(stripped.to_string()))
            } else {
                (Some(String::new()), Some(rest.to_string()))
            };
        };
        for (index, args) in action_tuples {
            self.take_action(index, &args)?;
        }
        Ok(stop)
    }

    fn consume_positionals(&mut self, mut start_index: usize) -> Result<usize, Stop> {
        let nargs: Vec<Nargs> = self
            .positionals
            .iter()
            .map(|&index| self.parser.args[index].nargs)
            .collect();
        let arg_counts = match_partial(&nargs, &self.pattern[start_index..]);
        let taken: Vec<usize> = self.positionals.drain(..arg_counts.len()).collect();
        for (index, arg_count) in taken.into_iter().zip(arg_counts) {
            let end = start_index + arg_count;
            let mut args = self.arg_strings[start_index..end].to_vec();
            if self.pattern[start_index..end].contains(&DASH)
                && let Some(position) = args.iter().position(|a| a == "--")
            {
                args.remove(position);
            }
            start_index = end;
            self.take_action(index, &args)?;
        }
        Ok(start_index)
    }
}

fn ignored_explicit(arg: &Arg, explicit: &str) -> String {
    format!(
        "argument {}: ignored explicit argument {}",
        arg.name(),
        py_repr(explicit)
    )
}

/// `_match_argument` for an optional: `([A])`, `(A?)` or `()` against the head of the pattern.
fn match_option(arg: &Arg, pattern: &[u8]) -> Result<usize, Stop> {
    let next_is_arg = pattern.first() == Some(&ARG);
    match arg.nargs {
        Nargs::Zero => Ok(0),
        Nargs::Optional => Ok(usize::from(next_is_arg)),
        Nargs::One if next_is_arg => Ok(1),
        Nargs::One => Err(Stop::Error(format!(
            "argument {}: expected one argument",
            arg.name()
        ))),
    }
}

/// One `x*`, `x?` or `x` atom of a positional's nargs regex.
#[derive(Clone, Copy)]
struct Atom {
    symbol: u8,
    min: usize,
    max: usize,
    group: usize,
}

/// `_match_arguments_partial`: the longest prefix of positionals whose concatenated nargs regexes
/// (`(-*A-*)` / `(-*A?-*)`) match the head of `pattern`, with regex backtracking semantics.
fn match_partial(nargs: &[Nargs], pattern: &[u8]) -> Vec<usize> {
    for count in (1..=nargs.len()).rev() {
        let mut atoms = Vec::with_capacity(count * 3);
        for (group, &n) in nargs[..count].iter().enumerate() {
            let dashes = Atom {
                symbol: DASH,
                min: 0,
                max: usize::MAX,
                group,
            };
            let argument = Atom {
                symbol: ARG,
                min: usize::from(n == Nargs::One),
                max: usize::from(n != Nargs::Zero),
                group,
            };
            atoms.extend([dashes, argument, dashes]);
        }
        let mut taken = vec![0; atoms.len()];
        if let Some(end) = match_atoms(&atoms, 0, pattern, 0, &mut taken) {
            let mut result = vec![0; count];
            for (atom, n) in atoms.iter().zip(&taken) {
                result[atom.group] += n;
            }
            if pattern.get(end) == Some(&OPT) {
                while result.last() == Some(&0) {
                    result.pop();
                }
            }
            return result;
        }
    }
    Vec::new()
}

fn match_atoms(
    atoms: &[Atom],
    atom_index: usize,
    pattern: &[u8],
    position: usize,
    taken: &mut [usize],
) -> Option<usize> {
    let Some(atom) = atoms.get(atom_index) else {
        return Some(position);
    };
    let available = pattern[position..]
        .iter()
        .take_while(|&&symbol| symbol == atom.symbol)
        .count()
        .min(atom.max);
    if available < atom.min {
        return None;
    }
    for n in (atom.min..=available).rev() {
        taken[atom_index] = n;
        if let Some(end) = match_atoms(atoms, atom_index + 1, pattern, position + n, taken) {
            return Some(end);
        }
    }
    None
}

/// `_get_values` for the nargs this module supports.
fn values(arg: &Arg, arg_strings: &[String]) -> Result<Value, Stop> {
    match (arg_strings, arg.nargs) {
        ([], Nargs::Optional) => {
            let raw = if arg.is_positional() {
                match &arg.default {
                    Value::Str(default) => Some(default.as_str()),
                    _ => None,
                }
            } else {
                arg.constant.as_deref()
            };
            match raw {
                Some(raw) => Ok(convert(arg, raw)?.into()),
                None if arg.is_positional() => Ok(arg.default.clone()),
                None => Ok(Value::None),
            }
        }
        ([single], Nargs::One | Nargs::Optional) => {
            let value = convert(arg, single)?;
            if let Some(choices) = &arg.choices
                && !choices.contains(&value)
            {
                return Err(Stop::Error(format!(
                    "argument {}: invalid choice: {} (choose from {})",
                    arg.name(),
                    py_repr(&value.to_string()),
                    choices.quoted(", ")
                )));
            }
            Ok(value.into())
        }
        _ => Ok(Value::List(Vec::new())),
    }
}

/// `_get_value`: run the `type=` conversion.
fn convert(arg: &Arg, raw: &str) -> Result<Converted, Stop> {
    match arg.value_type {
        ValueType::Str => Ok(Converted::Str(raw.to_string())),
        ValueType::Int => py_int(raw).map(Converted::Int).ok_or_else(|| {
            Stop::Error(format!(
                "argument {}: invalid int value: {}",
                arg.name(),
                py_repr(raw)
            ))
        }),
        ValueType::Custom(parse) => parse(raw)
            .map(Converted::Str)
            .map_err(|message| Stop::Error(format!("argument {}: {message}", arg.name()))),
    }
}

/// `HelpFormatter._format_action` at section indent 2.
fn format_action(arg: &Arg, width: isize, help_position: isize) -> String {
    const INDENT: &str = "  ";
    let help_width = (width - help_position).max(11);
    let action_width = help_position - as_isize(INDENT.len()) - 2;
    let header = arg.invocation();

    let Some(help) = arg.help.as_deref().filter(|help| !help.is_empty()) else {
        return format!("{INDENT}{header}\n");
    };
    let (mut out, indent_first) = if as_isize(char_len(&header)) <= action_width {
        let pad = usize::try_from(action_width).unwrap_or(0);
        (format!("{INDENT}{header:<pad$}  "), 0)
    } else {
        (format!("{INDENT}{header}\n"), help_position)
    };
    let lines = wrap(&normalize_whitespace(help), help_width);
    if lines.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return out;
    }
    for (number, line) in lines.iter().enumerate() {
        let pad = if number == 0 {
            indent_first
        } else {
            help_position
        };
        out.push_str(&" ".repeat(usize::try_from(pad).unwrap_or(0)));
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `get_lines` inside `_format_usage`.
fn usage_lines(
    parts: &[String],
    indent: &str,
    prefix: Option<&str>,
    text_width: isize,
) -> Vec<String> {
    let indent_length = char_len(indent);
    let mut lines = Vec::new();
    let mut line: Vec<&str> = Vec::new();
    let mut line_len = as_isize(prefix.map_or(indent_length, char_len)) - 1;
    for part in parts {
        let part_len = as_isize(char_len(part));
        if line_len + 1 + part_len > text_width && !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
            line.clear();
            line_len = as_isize(indent_length) - 1;
        }
        line.push(part);
        line_len += part_len + 1;
    }
    if !line.is_empty() {
        lines.push(format!("{indent}{}", line.join(" ")));
    }
    if prefix.is_some()
        && let Some(first) = lines.first_mut()
    {
        *first = first.chars().skip(indent_length).collect();
    }
    lines
}

/// `re.sub(r'\n\n\n+', '\n\n', help).strip('\n') + '\n'`.
fn collapse_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            continue;
        }
        out.push_str(&"\n\n"[..run.min(2)]);
        run = 0;
        out.push(c);
    }
    let mut out = out.trim_matches('\n').to_string();
    out.push('\n');
    out
}

/// `re.sub(r'\s+', ' ', text, flags=re.ASCII).strip()`.
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c') {
            in_space = true;
        } else {
            if in_space {
                out.push(' ');
                in_space = false;
            }
            out.push(c);
        }
    }
    out.trim_matches(py_isspace).to_string()
}

/// `textwrap.wrap(text, width)` with default options (text is already whitespace-normalised).
fn wrap(text: &str, width: isize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = split_chunks(&chars);
    chunks.reverse();
    let is_blank = |chunk: &[char]| chunk.iter().all(|&c| py_isspace(c));
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur_line: Vec<Vec<char>> = Vec::new();
        let mut cur_len: isize = 0;
        if !lines.is_empty() && chunks.last().is_some_and(|c| is_blank(c)) {
            chunks.pop();
        }
        while let Some(chunk) = chunks.last() {
            let len = as_isize(chunk.len());
            if cur_len + len > width {
                break;
            }
            cur_len += len;
            cur_line.extend(chunks.pop());
        }
        if chunks.last().is_some_and(|c| as_isize(c.len()) > width) {
            handle_long_word(&mut chunks, &mut cur_line, cur_len, width);
        }
        if cur_line.last().is_some_and(|c| is_blank(c)) {
            cur_line.pop();
        }
        if !cur_line.is_empty() {
            lines.push(cur_line.iter().flatten().collect());
        }
    }
    lines
}

/// `TextWrapper._handle_long_word`.
fn handle_long_word(
    chunks: &mut [Vec<char>],
    cur_line: &mut Vec<Vec<char>>,
    cur_len: isize,
    width: isize,
) {
    let space_left = if width < 1 { 1 } else { width - cur_len };
    let Some(chunk) = chunks.last_mut() else {
        return;
    };
    if space_left > 0 {
        let space_left = usize::try_from(space_left).unwrap_or(0);
        let mut end = space_left;
        if chunk.len() > space_left
            && let Some(hyphen) = chunk[..space_left].iter().rposition(|&c| c == '-')
            && hyphen > 0
            && chunk[..hyphen].iter().any(|&c| c != '-')
        {
            end = hyphen + 1;
        }
        let rest = chunk.split_off(end.min(chunk.len()));
        cur_line.push(std::mem::replace(chunk, rest));
    } else if cur_line.is_empty() {
        cur_line.push(std::mem::take(chunk));
    }
}

/// `TextWrapper.wordsep_re.split` (break_on_hyphens=True), empty chunks dropped.
fn split_chunks(chars: &[char]) -> Vec<Vec<char>> {
    let n = chars.len();
    let at = |i: usize| chars.get(i).copied();
    let is_ws = |c: char| matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ');
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let is_letter = |c: char| is_word(c) && !c.is_numeric();
    let is_wp = |c: char| is_word(c) || matches!(c, '!' | '"' | '\'' | '&' | '.' | ',' | '?');
    let dash_run = |i: usize| chars[i.min(n)..].iter().take_while(|&&c| c == '-').count();
    let dashes_then_word = |i: usize| {
        let run = dash_run(i);
        run >= 2 && at(i + run).is_some_and(is_word)
    };
    let letter = |k: usize| at(k).is_some_and(is_letter);

    let mut chunks = Vec::new();
    let mut i = 0;
    while i < n {
        let end = if is_ws(chars[i]) {
            i + chars[i..].iter().take_while(|&&c| is_ws(c)).count()
        } else if chars[i] == '-' && i > 0 && is_wp(chars[i - 1]) && dashes_then_word(i) {
            i + dash_run(i)
        } else {
            let mut j = i + 1;
            loop {
                let hyphen_break = at(j) == Some('-')
                    && ((j >= 2 && letter(j - 2) && letter(j - 1))
                        || (j >= 3 && letter(j - 3) && at(j - 2) == Some('-') && letter(j - 1)))
                    && letter(j + 1)
                    && (letter(j + 2) || (at(j + 2) == Some('-') && letter(j + 3)));
                if hyphen_break {
                    break j + 1;
                }
                if j == n || is_ws(chars[j]) {
                    break j;
                }
                if is_wp(chars[j - 1]) && dashes_then_word(j) {
                    break j;
                }
                j += 1;
            }
        };
        chunks.push(chars[i..end].to_vec());
        i = end;
    }
    chunks
}

/// `_negative_number_matcher`: `-\.?\d`.
fn looks_negative(arg: &str) -> bool {
    let mut chars = arg.chars().skip(1);
    match chars.next() {
        Some('.') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        Some(c) => c.is_ascii_digit(),
        None => false,
    }
}

/// Python `int(str)` in base 10 (ASCII digits, `_` separators, surrounding whitespace).
fn py_int(raw: &str) -> Option<i64> {
    let text = raw.trim_matches(py_isspace);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    let magnitude: i128 = cleaned.parse().ok()?;
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// Python `str.isspace` (Unicode White_Space plus the ASCII information separators).
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python `repr(str)`.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_isprintable(c) => {
                let code = u32::from(c);
                let escaped = if code < 0x100 {
                    format!("\\x{code:02x}")
                } else if code < 0x10000 {
                    format!("\\u{code:04x}")
                } else {
                    format!("\\U{code:08x}")
                };
                out.push_str(&escaped);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Approximates Python's `str.isprintable` (std has no Unicode general-category tables).
fn py_isprintable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let code = u32::from(c);
    !(c.is_control()
        || py_isspace(c)
        || code == 0xad
        || (0x200b..=0x200f).contains(&code)
        || (0x202a..=0x202e).contains(&code)
        || (0x2060..=0x206f).contains(&code)
        || code == 0xfeff
        || (0xfff9..=0xfffb).contains(&code)
        || (0xe000..=0xf8ff).contains(&code)
        || code >= 0xf0000)
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn as_isize(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

/// `shutil.get_terminal_size().columns`: `$COLUMNS` if a positive int, else stdout's tty width,
/// else 80.
pub fn terminal_columns() -> usize {
    columns_from(std::env::var("COLUMNS").ok().as_deref(), stdout_tty_columns)
}

/// The `shutil.get_terminal_size` decision, with the tty query injected.
pub fn columns_from(env_columns: Option<&str>, tty: impl FnOnce() -> Option<usize>) -> usize {
    match env_columns.and_then(py_int) {
        Some(columns) if columns > 0 => usize::try_from(columns).unwrap_or(usize::MAX),
        _ => tty().filter(|&columns| columns > 0).unwrap_or(80),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stdout_tty_columns() -> Option<usize> {
    use std::ffi::{c_int, c_ulong};

    #[repr(C)]
    #[derive(Default)]
    struct Winsize {
        rows: u16,
        cols: u16,
        x_pixels: u16,
        y_pixels: u16,
    }
    unsafe extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }
    #[cfg(target_os = "macos")]
    const TIOCGWINSZ: c_ulong = 0x4008_7468;
    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: c_ulong = 0x5413;

    let mut size = Winsize::default();
    // SAFETY: TIOCGWINSZ writes one `struct winsize` through the pointer, which points to a live
    // `#[repr(C)]` value of exactly that layout; a non-tty fd 1 just makes the call fail.
    let status = unsafe { ioctl(1, TIOCGWINSZ, &raw mut size) };
    (status == 0).then_some(usize::from(size.cols))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn stdout_tty_columns() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_env_wins_when_positive() {
        assert_eq!(columns_from(Some("120"), || Some(50)), 120);
        assert_eq!(columns_from(Some(" 1_00 "), || None), 100);
    }

    #[test]
    fn columns_fall_back_to_tty_then_80() {
        assert_eq!(columns_from(Some("0"), || Some(50)), 50);
        assert_eq!(columns_from(Some("-3"), || Some(50)), 50);
        assert_eq!(columns_from(Some("abc"), || Some(50)), 50);
        assert_eq!(columns_from(None, || None), 80);
        assert_eq!(columns_from(None, || Some(0)), 80);
    }
}

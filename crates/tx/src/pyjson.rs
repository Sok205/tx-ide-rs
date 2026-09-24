//! Python `json` compatible encoding.
//!
//! Byte-for-byte equal to CPython's `json.dumps` with `ensure_ascii=True` for
//! `serde_json::Value` (object key order is insertion order via `preserve_order`).

use serde_json::{Map, Number, Value};

/// `json.dumps(v)`: separators `", "` and `": "`.
pub fn dumps(v: &Value) -> String {
    encode(v, None, ", ", ": ")
}

/// `json.dumps(v, indent=2)`.
pub fn dumps_pretty(v: &Value) -> String {
    encode(v, Some(2), ",", ": ")
}

/// `json.dumps(v, separators=(",", ":"))`.
pub fn dumps_compact(v: &Value) -> String {
    encode(v, None, ",", ":")
}

fn encode(v: &Value, indent: Option<usize>, item_sep: &str, key_sep: &str) -> String {
    let mut enc = Encoder {
        out: String::new(),
        indent,
        item_sep,
        key_sep,
    };
    enc.value(v, 0);
    enc.out
}

struct Encoder<'a> {
    out: String,
    indent: Option<usize>,
    item_sep: &'a str,
    key_sep: &'a str,
}

impl Encoder<'_> {
    fn value(&mut self, v: &Value, depth: usize) {
        match v {
            Value::Null => self.out.push_str("null"),
            Value::Bool(true) => self.out.push_str("true"),
            Value::Bool(false) => self.out.push_str("false"),
            Value::Number(n) => self.out.push_str(&number(n)),
            Value::String(s) => write_str(&mut self.out, s),
            Value::Array(items) => self.array(items, depth),
            Value::Object(map) => self.object(map, depth),
        }
    }

    fn newline(&mut self, depth: usize) {
        if let Some(n) = self.indent {
            self.out.push('\n');
            self.out.extend(std::iter::repeat_n(' ', n * depth));
        }
    }

    fn array(&mut self, items: &[Value], depth: usize) {
        if items.is_empty() {
            self.out.push_str("[]");
            return;
        }
        self.out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(self.item_sep);
            }
            self.newline(depth + 1);
            self.value(item, depth + 1);
        }
        self.newline(depth);
        self.out.push(']');
    }

    fn object(&mut self, map: &Map<String, Value>, depth: usize) {
        if map.is_empty() {
            self.out.push_str("{}");
            return;
        }
        self.out.push('{');
        for (i, (k, v)) in map.iter().enumerate() {
            if i > 0 {
                self.out.push_str(self.item_sep);
            }
            self.newline(depth + 1);
            write_str(&mut self.out, k);
            self.out.push_str(self.key_sep);
            self.value(v, depth + 1);
        }
        self.newline(depth);
        self.out.push('}');
    }
}

fn number(n: &Number) -> String {
    if let Some(u) = n.as_u64() {
        u.to_string()
    } else if let Some(i) = n.as_i64() {
        i.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// Python `float.__repr__`, with JSON's spellings of the non-finite values
/// (`NaN`, `Infinity`, `-Infinity`).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // `{:e}` yields the shortest round-trip digit count, but on a tie between two
    // shortest candidates it may pick the upper one; CPython picks the correctly
    // rounded (half-even) one, which `{:.*e}` produces.
    let shortest = format!("{f:e}");
    let ndigits = shortest
        .split_once('e')
        .map_or(0, |(m, _)| m.bytes().filter(u8::is_ascii_digit).count());
    let sci = format!("{:.*e}", ndigits.saturating_sub(1), f);
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let (neg, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        d => d,
    };
    let mut out = String::with_capacity(digits.len() + 8);
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-exp - 1) as usize));
            out.push_str(digits);
        } else {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                out.push_str(digits);
                out.extend(std::iter::repeat_n('0', int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.unsigned_abs()));
    }
    out
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

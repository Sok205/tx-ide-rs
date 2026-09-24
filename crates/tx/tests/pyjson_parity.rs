//! Differential tests: `tx::pyjson` against the real CPython 3.14 `json` module.

use std::io::Write;
use std::process::{Command, Stdio};

use proptest::prelude::*;
use serde_json::Value;
use tx::pyjson::{dumps, dumps_compact, dumps_pretty, float_repr};

const PYTHON: &str = "python3.14";

/// Runs `script` under Python with `input` on stdin and parses its stdout as JSON.
fn python<T: serde::de::DeserializeOwned>(script: &str, input: &str) -> T {
    let mut child = Command::new(PYTHON)
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{PYTHON} is required for parity tests: {e}"));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "python failed");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Each input line is a JSON document; Python answers with
/// `[dumps(v), dumps(v, indent=2), dumps(v, separators=(",", ":"))]` per line.
const DUMP_SCRIPT: &str = r#"
import json, sys
out = []
for line in sys.stdin.read().split("\n"):
    v = json.loads(line)
    out.append([json.dumps(v), json.dumps(v, indent=2), json.dumps(v, separators=(",", ":"))])
print(json.dumps(out))
"#;

fn assert_parity(docs: &[String]) {
    let expected: Vec<[String; 3]> = python(DUMP_SCRIPT, &docs.join("\n"));
    assert_eq!(expected.len(), docs.len());
    for (doc, [plain, pretty, compact]) in docs.iter().zip(expected) {
        let v: Value = serde_json::from_str(doc).unwrap();
        assert_eq!(dumps(&v), plain, "dumps of {doc}");
        assert_eq!(dumps_pretty(&v), pretty, "dumps_pretty of {doc}");
        assert_eq!(dumps_compact(&v), compact, "dumps_compact of {doc}");
    }
}

const CORPUS: &[&str] = &[
    // scalars
    "null",
    "true",
    "false",
    "0",
    "-1",
    "18446744073709551615",
    "-9223372036854775808",
    "9223372036854775807",
    // floats around the fixed/scientific thresholds
    "0.0",
    "-0.0",
    "1.0",
    "3.0",
    "0.1",
    "-0.1",
    "0.5",
    "1e15",
    "1e16",
    "9999999999999998.0",
    "1234567890123456.7",
    "123456789012345680.0",
    "1e-5",
    "1.5e-5",
    "0.0001",
    "0.00012345",
    "-2.5e20",
    "5e-324",
    "2.2250738585072014e-308",
    "1.7976931348623157e308",
    "1727181234.567891",
    "1727181234.0",
    "3.141592653589793",
    "100.0",
    "1e22",
    "1e23",
    "12345.678e10",
    // exact ties between two shortest candidates (CPython rounds half-even)
    "988336231328981.25",
    "0.3000000000000000444089209850062616169452667236328125",
    // strings
    r#""""#,
    r#""plain ascii ~ ""#,
    r#""quote \" backslash \\ slash /""#,
    r#""\n\r\t\b\f""#,
    r#""\u0000\u0001\u001f\u007f""#,
    r#""zażółć gęślą jaźń""#,
    r#""\u00e9\u0080\u00ff""#,
    r#""日本語""#,
    r#""emoji 😀 🚀 👨‍👩‍👧""#,
    r#""\ud834\udd1e""#,
    r#""\u2028\u2029\ufeff\uffff""#,
    "\"\u{2028}\u{2029}\u{feff}\u{ffff}\u{10ffff}\"",
    // containers
    "[]",
    "{}",
    "[[]]",
    "[{}]",
    r#"{"a": {}}"#,
    r#"{"a": []}"#,
    r#"[[], {}, [[]], {"x": [{}]}]"#,
    r#"[1, 2.0, "3", null, true]"#,
    r#"{"z": 1, "a": 2, "m": 3, "b": {"y": 1, "c": 2}}"#,
    r#"{"ключ": "значение", "😀": ["\t", 1e16]}"#,
    r#"{"schema_version": 6, "name": "s", "created": 1727181234.567891, "tags": [], "meta": {"k": [1, [2, [3, {}]]]}}"#,
    r#"[[[[[[["deep"]]]]]]]"#,
    r#"{"": ""}"#,
];

#[test]
fn corpus_matches_python() {
    let docs: Vec<String> = CORPUS.iter().map(|s| s.to_string()).collect();
    assert_parity(&docs);
}

#[test]
fn integer_and_float_stay_distinct() {
    // serde_json parses the literal `-0` as the float -0.0 (Python: int 0), so it is
    // deliberately not in the corpus.
    let v: Value = serde_json::from_str("[1, 1.0, 0, -0.0, -1]").unwrap();
    assert_eq!(dumps_compact(&v), "[1,1.0,0,-0.0,-1]");
}

/// Python `json.dumps(float)` for each f64 given as raw bits (one per line).
const FLOAT_SCRIPT: &str = r#"
import json, struct, sys
print(json.dumps([json.dumps(struct.unpack("<d", struct.pack("<Q", int(b)))[0])
                  for b in sys.stdin.read().split()]))
"#;

fn check_floats(fs: &[f64]) -> Result<(), TestCaseError> {
    let input: Vec<String> = fs.iter().map(|f| f.to_bits().to_string()).collect();
    let expected: Vec<String> = python(FLOAT_SCRIPT, &input.join("\n"));
    for (f, want) in fs.iter().zip(expected) {
        prop_assert_eq!(float_repr(*f), want.clone(), "float_repr({:e})", f);
        if let Some(n) = serde_json::Number::from_f64(*f) {
            prop_assert_eq!(dumps(&Value::Number(n)), want, "dumps({:e})", f);
        }
    }
    Ok(())
}

#[test]
fn non_finite_floats_match_python() {
    check_floats(&[f64::NAN, f64::INFINITY, f64::NEG_INFINITY]).unwrap();
}

fn interesting_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>(),
        // decimal-looking values across the exponent range
        (-999_999i64..999_999, -30i32..30).prop_map(|(m, e)| m as f64 * 10f64.powi(e)),
        // integral floats near the 1e16 threshold
        (0u64..(1 << 55)).prop_map(|n| n as f64),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// One python process per batch of 2000 floats.
    #[test]
    fn random_floats_match_python(fs in prop::collection::vec(interesting_f64(), 2000)) {
        check_floats(&fs)?;
    }
}

//! POSIX-mode `shlex.split` / `shlex.quote` / `shlex.join` (Python stdlib semantics).

/// `shlex.split` failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShlexError {
    #[error("No closing quotation")]
    NoClosingQuotation,
    #[error("No escaped character")]
    NoEscapedCharacter,
}

/// `shlex.split(s)` (POSIX mode, no comments): whitespace-separated words, `'…'` literal,
/// `"…"` with `\` escaping only `"` and `\`, a bare `\` escaping any character.
pub fn shlex_split(s: &str) -> Result<Vec<String>, ShlexError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\\' => {
                word.push(chars.next().ok_or(ShlexError::NoEscapedCharacter)?);
                in_word = true;
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next().ok_or(ShlexError::NoClosingQuotation)? {
                        '\'' => break,
                        other => word.push(other),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next().ok_or(ShlexError::NoClosingQuotation)? {
                        '"' => break,
                        '\\' => match chars.next().ok_or(ShlexError::NoClosingQuotation)? {
                            escaped @ ('"' | '\\') => word.push(escaped),
                            other => {
                                word.push('\\');
                                word.push(other);
                            }
                        },
                        other => word.push(other),
                    }
                }
            }
            other => {
                word.push(other);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `shlex.quote(s)`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// `shlex.join(words)`.
pub fn shlex_join<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|word| shlex_quote(word.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_matches_python() {
        let split_cases: [(&str, &[&str]); 9] = [
            ("a b", &["a", "b"]),
            ("'a b' c", &["a b", "c"]),
            ("a\\ b", &["a b"]),
            ("\"a\\\"b\" c", &["a\"b", "c"]),
            ("''", &[""]),
            ("x\"y z\"w", &["xy zw"]),
            ("a\\\\b", &["a\\b"]),
            ("\"a\\nb\"", &["a\\nb"]),
            ("  \t ", &[]),
        ];
        for (input, expected) in split_cases {
            assert_eq!(shlex_split(input).unwrap(), expected, "{input:?}");
        }
        assert_eq!(shlex_split("\"ab"), Err(ShlexError::NoClosingQuotation));
        assert_eq!(shlex_split("'x"), Err(ShlexError::NoClosingQuotation));
        assert_eq!(shlex_split("ab\\"), Err(ShlexError::NoEscapedCharacter));
        for (word, quoted) in [
            ("", "''"),
            ("abc", "abc"),
            ("a b", "'a b'"),
            ("it's", "'it'\"'\"'s'"),
            ("é", "'é'"),
            ("a=b,c:d@e%f+g/h.i-j_k", "a=b,c:d@e%f+g/h.i-j_k"),
        ] {
            assert_eq!(shlex_quote(word), quoted, "{word:?}");
        }
    }
}

//! The tx effort scale: `1`–`5`, the same for every engine. Each engine still owns its words and
//! checks a value against them; a level on the scale is read as the word it stands for.

use std::fmt;

use serde::Deserializer;
use serde::de::{self, Visitor};

/// The words levels `1`–`5` stand for, in order.
pub const EFFORT_SCALE: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The word a level on the scale (`"1"`–`"5"`) stands for; `None` for anything else.
pub fn effort_word(level: &str) -> Option<&'static str> {
    match level {
        "1" => Some(EFFORT_SCALE[0]),
        "2" => Some(EFFORT_SCALE[1]),
        "3" => Some(EFFORT_SCALE[2]),
        "4" => Some(EFFORT_SCALE[3]),
        "5" => Some(EFFORT_SCALE[4]),
        _ => None,
    }
}

/// `deserialize_with` for an effort in a config file: a number (`effort: 3`) or a string
/// (`effort: high`), kept as text; the engine checks it.
pub fn deserialize_effort<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    deserializer.deserialize_any(EffortText)
}

/// [`deserialize_effort`] for an optional effort: `null` stays `None`.
pub fn deserialize_optional_effort<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    deserializer.deserialize_option(OptionalEffortText)
}

struct EffortText;

impl Visitor<'_> for EffortText {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an effort: a number 1-5 or the engine's word")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<String, E> {
        Ok(value.to_string())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<String, E> {
        Ok(value.to_string())
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        Ok(value.to_owned())
    }
}

struct OptionalEffortText;

impl<'de> Visitor<'de> for OptionalEffortText {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        EffortText.expecting(formatter)
    }

    fn visit_none<E: de::Error>(self) -> Result<Option<String>, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Option<String>, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Option<String>, D::Error> {
        deserialize_effort(deserializer).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_one_to_five_are_the_words_in_order() {
        for (index, word) in EFFORT_SCALE.into_iter().enumerate() {
            assert_eq!(effort_word(&(index + 1).to_string()), Some(word));
        }
        for text in ["0", "6", "03", "+3", "high", ""] {
            assert_eq!(effort_word(text), None, "{text:?}");
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct Settings {
        #[serde(deserialize_with = "deserialize_effort")]
        effort: String,
        #[serde(default, deserialize_with = "deserialize_optional_effort")]
        optional: Option<String>,
    }

    #[test]
    fn a_number_or_a_string_is_read_as_text() {
        use serde_json::json;
        let read = |value| serde_json::from_value::<Settings>(value);
        let settings = read(json!({"effort": 3, "optional": "high"})).unwrap();
        assert_eq!(settings.effort, "3");
        assert_eq!(settings.optional.as_deref(), Some("high"));
        let settings = read(json!({"effort": "max", "optional": 4})).unwrap();
        assert_eq!(settings.effort, "max");
        assert_eq!(settings.optional.as_deref(), Some("4"));
        assert_eq!(
            read(json!({"effort": 1, "optional": null}))
                .unwrap()
                .optional,
            None
        );
        assert_eq!(read(json!({"effort": 1})).unwrap().optional, None);
        let error = read(json!({"effort": true})).unwrap_err().to_string();
        assert!(
            error.contains("a number 1-5 or the engine's word"),
            "{error}"
        );
    }
}

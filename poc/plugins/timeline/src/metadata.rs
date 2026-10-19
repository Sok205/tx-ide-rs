//! Latest model, effort and context usage from engine transcripts, with the record's launch options as fallbacks.

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

impl Metadata {
    /// Transcript entries are external engine data; missing fields leave the last report intact.
    pub fn update(&mut self, entry: &Value) {
        match entry["type"].as_str() {
            Some("turn_context") => {
                let payload = &entry["payload"];
                if let Some(model) = payload["model"].as_str() {
                    self.model = Some(model.to_owned());
                }
                if let Some(effort) = payload["effort"].as_str() {
                    self.effort = Some(effort.to_owned());
                }
            }
            Some("event_msg") if entry["payload"]["type"] == "token_count" => {
                let information = &entry["payload"]["info"];
                if let Some(tokens) = information["last_token_usage"]["input_tokens"].as_u64() {
                    self.context_tokens = Some(tokens);
                }
                if let Some(window) = information["model_context_window"].as_u64() {
                    self.context_window = (window > 0).then_some(window);
                }
            }
            Some("token_usage_record") => {
                if let Some(tokens) = entry["payload"]["usage"]["input_tokens"].as_u64() {
                    self.context_tokens = Some(tokens);
                }
            }
            Some("assistant") => {
                let message = &entry["message"];
                // Claude also writes synthetic assistant errors; those are not model reports.
                if let Some(model) = message["model"]
                    .as_str()
                    .filter(|model| *model != "<synthetic>")
                {
                    self.model = Some(model.to_owned());
                    let usage = &message["usage"];
                    if let Some(input) = usage["input_tokens"].as_u64() {
                        self.context_tokens = Some(
                            input
                                + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                                + usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                        );
                    }
                }
            }
            Some("system") if entry["subtype"] == "compact_boundary" => {
                // A pre-compaction request no longer represents the context in use.
                self.context_tokens = entry["compactMetadata"]["postTokens"].as_u64();
            }
            _ => {}
        }
    }

    /// The record's own launch options fill what no transcript reported.
    pub fn with_launch_fallback(mut self, model: Option<&str>, effort: Option<&str>) -> Self {
        self.model = self.model.or_else(|| model.map(str::to_owned));
        self.effort = self.effort.or_else(|| effort.map(str::to_owned));
        self
    }

    pub fn summary(&self) -> String {
        let context = match (self.context_tokens, self.context_window) {
            (Some(tokens), Some(window)) => format!(
                "{}/{} ({:.0}%)",
                format_tokens(tokens),
                format_tokens(window),
                tokens as f64 / window as f64 * 100.0,
            ),
            (Some(tokens), None) => format_tokens(tokens),
            (None, _) => "—".to_owned(),
        };
        format!(
            "{} · effort {} · ctx {context}",
            self.model.as_deref().unwrap_or("model —"),
            self.effort.as_deref().unwrap_or("—"),
        )
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn transcript_reports_win_over_the_launch() {
        let mut metadata = Metadata::default();
        metadata.update(&json!({"type": "assistant", "message": {
            "model": "claude-opus-4", "usage": {"input_tokens": 10, "cache_read_input_tokens": 5}}}));
        let metadata = metadata.with_launch_fallback(Some("opus"), Some("high"));
        assert_eq!(metadata.model.as_deref(), Some("claude-opus-4"));
        assert_eq!(metadata.effort.as_deref(), Some("high"));
        assert_eq!(metadata.summary(), "claude-opus-4 · effort high · ctx 15");
    }

    #[test]
    fn codex_context_window() {
        let mut metadata = Metadata::default();
        metadata.update(&json!({"type": "event_msg", "payload": {"type": "token_count",
            "info": {"last_token_usage": {"input_tokens": 20000}, "model_context_window": 200000}}}));
        assert_eq!(
            metadata.summary(),
            "model — · effort — · ctx 20.0K/200.0K (10%)"
        );
    }
}

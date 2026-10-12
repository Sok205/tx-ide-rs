//! Latest model, effort and context usage from engine transcripts, with launch-option fallbacks.

use serde_json::Value;

use crate::shlex::shlex_split;

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

    pub fn with_launch_fallback(mut self, command: &str) -> Self {
        let Ok(arguments) = shlex_split(command) else {
            return self;
        };
        let mut model = None;
        let mut effort = None;
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            let (option, value) = match argument.split_once('=') {
                Some((option, value)) => (option, Some(value)),
                None => match argument.as_str() {
                    "--" => break,
                    "--model" | "-m" | "--effort" | "-c" | "--config" => {
                        (argument.as_str(), arguments.next().map(String::as_str))
                    }
                    _ => continue,
                },
            };
            match (option, value) {
                ("--model" | "-m", Some(value)) => model = Some(value.to_owned()),
                ("--effort", Some(value)) => effort = Some(value.to_owned()),
                ("-c" | "--config", Some(value)) => {
                    if let Some((key, value)) = value.split_once('=') {
                        let value = value.trim().trim_matches(['\'', '"']).to_owned();
                        match key.trim() {
                            "model_reasoning_effort" => effort = Some(value),
                            "model" => model = Some(value),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        self.model = self.model.or(model);
        self.effort = self.effort.or(effort);
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

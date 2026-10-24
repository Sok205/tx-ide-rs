//! fzf's query editor as a one-line prompt.

use std::process::{Command, Stdio};

/// fzf's query editor prefilled with `default`; `None` when cancelled.
pub fn prompt_with_default(header: &str, default: &str) -> std::io::Result<Option<String>> {
    let output = Command::new("fzf")
        .args([
            "--disabled",
            "--query",
            default,
            "--ansi",
            "--prompt",
            "  ❯ ",
            "--header",
            header,
            "--header-first",
            "--reverse",
            "--no-info",
            "--no-separator",
            "--bind",
            "enter:accept-or-print-query",
            "--color",
            "fg:#a9b1d6,header:#a9b1d6,prompt:#a9b1d6,query:#c0caf5",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(Some(text.strip_suffix('\n').unwrap_or(&text).to_owned()))
}

//! Switches `plugins.<id>.enabled` in the text of `config.yaml`, line by line, so the user's
//! comments and layout survive. Both entry forms are handled: flow (`id: { enabled: true }`)
//! and block (`id:` with `enabled:` on a line below). A missing entry is appended to the
//! `plugins:` block, a missing block to the file. The result is parsed back by the caller.

/// The text with `id` switched to `enabled`.
pub fn set_enabled(text: &str, id: &str, enabled: bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let Some(block) = lines
        .iter()
        .position(|line| without_comment(line) == "plugins:")
    else {
        lines.push("plugins:".to_owned());
        lines.push(format!("  {id}: {{ enabled: {enabled} }}"));
        return joined(lines);
    };
    // The block: every line after `plugins:` up to the next one at column 0.
    let end = (block + 1..lines.len())
        .find(|&index| {
            let line = &lines[index];
            !line.starts_with([' ', '\t']) && !without_comment(line).is_empty()
        })
        .unwrap_or(lines.len());
    let indent = (block + 1..end)
        .map(|index| &lines[index])
        .find(|line| !without_comment(line).is_empty())
        .map_or(2, |line| line.len() - line.trim_start().len());
    let key = format!("{id}:");
    let entry = (block + 1..end).find(|&index| {
        let line = &lines[index];
        line.len() - line.trim_start().len() == indent && line.trim_start().starts_with(&key)
    });
    let Some(entry) = entry else {
        let last = (block..end)
            .rev()
            .find(|&index| !without_comment(&lines[index]).is_empty())
            .unwrap_or(block);
        lines.insert(
            last + 1,
            format!("{}{id}: {{ enabled: {enabled} }}", " ".repeat(indent)),
        );
        return joined(lines);
    };
    let value = without_comment(&lines[entry])[key.len()..]
        .trim()
        .to_owned();
    if value.starts_with('{') {
        lines[entry] = match replaced(&lines[entry], enabled) {
            Some(line) => line,
            None if value == "{}" => {
                lines[entry].replacen("{}", &format!("{{ enabled: {enabled} }}"), 1)
            }
            None => lines[entry].replacen('{', &format!("{{ enabled: {enabled},"), 1),
        };
        return joined(lines);
    }
    // Block form: `enabled:` among the lines indented under the entry.
    let children = (entry + 1..end).take_while(|&index| {
        let line = &lines[index];
        without_comment(line).is_empty() || line.len() - line.trim_start().len() > indent
    });
    for index in children {
        if lines[index].trim_start().starts_with("enabled:") {
            lines[index] = replaced(&lines[index], enabled).expect("an enabled: line");
            return joined(lines);
        }
    }
    lines.insert(
        entry + 1,
        format!("{}enabled: {enabled}", " ".repeat(indent + 2)),
    );
    joined(lines)
}

/// `line` with the word after its `enabled:` replaced, `None` without one.
fn replaced(line: &str, enabled: bool) -> Option<String> {
    let start = line.find("enabled:")? + "enabled:".len();
    let rest = &line[start..];
    let word_start = start + rest.len() - rest.trim_start().len();
    let word_end = line[word_start..]
        .find(|character: char| !character.is_ascii_alphanumeric())
        .map_or(line.len(), |offset| word_start + offset);
    Some(format!("{} {enabled}{}", &line[..start], &line[word_end..]))
}

fn without_comment(line: &str) -> &str {
    let line = line.trim();
    if line.starts_with('#') {
        return "";
    }
    line.find(" #")
        .map_or(line, |index| line[..index].trim_end())
}

fn joined(lines: Vec<String>) -> String {
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::set_enabled;

    #[test]
    fn every_form_of_entry_is_switched_and_the_rest_is_kept() {
        let text = "# mine\nagent:\n  engine: claude\nplugins:\n  board: { enabled: true }   # b\n  \
                    home: {}\n  tour: { settings: { a: 1 } }\n  assistant:\n    enabled: true\n    \
                    settings: { model: opus }\n  mockup:\n    settings: {}\n";
        let text = set_enabled(text, "board", false);
        let text = set_enabled(&text, "home", false);
        let text = set_enabled(&text, "tour", false);
        let text = set_enabled(&text, "assistant", false);
        let text = set_enabled(&text, "mockup", false);
        let text = set_enabled(&text, "graph", false);
        assert_eq!(
            text,
            "# mine\nagent:\n  engine: claude\nplugins:\n  board: { enabled: false }   # b\n  \
             home: { enabled: false }\n  tour: { enabled: false, settings: { a: 1 } }\n  assistant:\n    \
             enabled: false\n    settings: { model: opus }\n  mockup:\n    enabled: false\n    \
             settings: {}\n  graph: { enabled: false }\n"
        );
        assert_eq!(
            set_enabled("agent: {}\n", "board", true),
            "agent: {}\nplugins:\n  board: { enabled: true }\n"
        );
    }
}

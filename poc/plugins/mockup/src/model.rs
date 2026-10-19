//! The mockup description format: JSON text → [`Screen`].
//!
//! Keys follow the spec's sketch: `title` `sub` `fps` `options[{id,title,note,body}]` `body`;
//! a node is `size` plus exactly one of `row` `col` `block`(+`body`) `text` `list`(+`sel`)
//! `table`(+`head`) `frames`(+`fps`). A screen has `options` or `body`, never both or neither.

use serde::Deserialize;

pub const DEFAULT_FPS: u32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub title: Option<String>,
    pub sub: Option<String>,
    pub fps: u32,
    pub options: Vec<Choice>,
    pub body: Option<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    pub body: Node,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub size: Size,
    pub kind: Kind,
}

/// `7` | `"30%"` | absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Len(u16),
    Pct(u16),
    Fill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Row(Vec<Node>),
    Col(Vec<Node>),
    Block {
        title: String,
        body: Box<Node>,
    },
    Text(String),
    List {
        items: Vec<String>,
        sel: Option<usize>,
    },
    Table {
        head: Option<Vec<String>>,
        rows: Vec<Vec<String>>,
    },
    Frames {
        frames: Vec<String>,
        fps: Option<u32>,
    },
}

/// One line, e.g. `line 3 col 9: expected ...`.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("line {line} col {column}: {message}")]
    Json {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("{0}")]
    Invalid(String),
}

impl From<serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        ParseError::Json {
            line: error.line(),
            column: error.column(),
            message: error.to_string(),
        }
    }
}

pub fn parse(text: &str) -> Result<Screen, ParseError> {
    let raw: RawScreen = serde_json::from_str(text)?;
    raw.convert()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScreen {
    title: Option<String>,
    sub: Option<String>,
    fps: Option<u32>,
    options: Option<Vec<RawChoice>>,
    body: Option<RawNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChoice {
    id: String,
    title: String,
    note: Option<String>,
    body: RawNode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    size: Option<RawSize>,
    row: Option<Vec<RawNode>>,
    col: Option<Vec<RawNode>>,
    block: Option<String>,
    body: Option<Box<RawNode>>,
    text: Option<String>,
    list: Option<Vec<String>>,
    sel: Option<usize>,
    table: Option<Vec<Vec<String>>>,
    head: Option<Vec<String>>,
    frames: Option<Vec<String>>,
    fps: Option<u32>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSize {
    Len(u16),
    Text(String),
}

const KIND_KEYS: &str = "row, col, block, text, list, table, frames";

impl RawScreen {
    fn convert(self) -> Result<Screen, ParseError> {
        let (options, body) = match (self.options, self.body) {
            (Some(options), None) => (
                options
                    .into_iter()
                    .map(RawChoice::convert)
                    .collect::<Result<Vec<_>, _>>()?,
                None,
            ),
            (None, Some(body)) => (Vec::new(), Some(body.convert()?)),
            (Some(_), Some(_)) => {
                return Err(ParseError::Invalid(
                    "screen has both options and body; want one".into(),
                ));
            }
            (None, None) => {
                return Err(ParseError::Invalid(
                    "screen has neither options nor body; want one".into(),
                ));
            }
        };
        Ok(Screen {
            title: self.title,
            sub: self.sub,
            fps: self.fps.unwrap_or(DEFAULT_FPS),
            options,
            body,
        })
    }
}

impl RawChoice {
    fn convert(self) -> Result<Choice, ParseError> {
        Ok(Choice {
            id: self.id,
            title: self.title,
            note: self.note,
            body: self.body.convert()?,
        })
    }
}

impl RawNode {
    fn convert(self) -> Result<Node, ParseError> {
        let size = match self.size {
            None => Size::Fill,
            Some(RawSize::Len(n)) => Size::Len(n),
            Some(RawSize::Text(text)) => parse_pct(&text)?,
        };
        let present: Vec<&str> = [
            ("row", self.row.is_some()),
            ("col", self.col.is_some()),
            ("block", self.block.is_some()),
            ("text", self.text.is_some()),
            ("list", self.list.is_some()),
            ("table", self.table.is_some()),
            ("frames", self.frames.is_some()),
        ]
        .into_iter()
        .filter(|(_, set)| *set)
        .map(|(key, _)| key)
        .collect();
        if present.len() != 1 {
            let found = if present.is_empty() {
                "none".to_string()
            } else {
                present.join(" and ")
            };
            return Err(ParseError::Invalid(format!(
                "node has {found}; want exactly one of {KIND_KEYS}"
            )));
        }
        let kind = match present[0] {
            "row" => Kind::Row(convert_all(self.row.unwrap())?),
            "col" => Kind::Col(convert_all(self.col.unwrap())?),
            "block" => {
                let body = self
                    .body
                    .ok_or_else(|| ParseError::Invalid("block needs a body".into()))?;
                Kind::Block {
                    title: self.block.unwrap(),
                    body: Box::new(body.convert()?),
                }
            }
            "text" => Kind::Text(self.text.unwrap()),
            "list" => Kind::List {
                items: self.list.unwrap(),
                sel: self.sel,
            },
            "table" => Kind::Table {
                head: self.head,
                rows: self.table.unwrap(),
            },
            "frames" => Kind::Frames {
                frames: self.frames.unwrap(),
                fps: self.fps,
            },
            _ => unreachable!("kind key comes from KIND_KEYS"),
        };
        Ok(Node { size, kind })
    }
}

fn convert_all(nodes: Vec<RawNode>) -> Result<Vec<Node>, ParseError> {
    nodes.into_iter().map(RawNode::convert).collect()
}

fn parse_pct(text: &str) -> Result<Size, ParseError> {
    let invalid = || {
        ParseError::Invalid(format!(
            "size {text:?}: want an integer or \"N%\" with N ≤ 100"
        ))
    };
    let digits = text.strip_suffix('%').ok_or_else(invalid)?;
    let pct: u16 = digits.parse().map_err(|_| invalid())?;
    if pct > 100 {
        return Err(invalid());
    }
    Ok(Size::Pct(pct))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"{"title":"Start menu — which layout?","sub":"pick the closest","fps":8,
 "options":[
  {"id":"a","title":"Centered splash","note":"game title screen",
   "body":{"col":[
     {"size":7,"frames":["[green]▀█▀ ▀▄▀[/]","[dim]▀█▀ ▀▄▀[/]"]},
     {"list":["New tx session","Resume","Timeline","Quit"],"sel":0},
     {"block":"recent","body":{"text":"[yellow]wrangler-p1[/] [dim]2h ago[/]"}}]}},
  {"id":"b","title":"Dashboard panes","body":{"row":[
     {"size":"30%","block":"actions","body":{"list":["New tx","Resume"],"sel":0}},
     {"block":"history","body":{"table":[["wrangler-p1","tx-ide-rs","2h"]]}}]}}]}"#;

    #[test]
    fn parses_spec_example() {
        let screen = parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(screen.title.as_deref(), Some("Start menu — which layout?"));
        assert_eq!(screen.sub.as_deref(), Some("pick the closest"));
        assert_eq!(screen.fps, 8);
        assert!(screen.body.is_none());
        assert_eq!(screen.options.len(), 2);

        let a = &screen.options[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.title, "Centered splash");
        assert_eq!(a.note.as_deref(), Some("game title screen"));
        let Kind::Col(children) = &a.body.kind else {
            panic!("expected col, got {:?}", a.body.kind);
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].size, Size::Len(7));
        let Kind::Frames { frames, fps } = &children[0].kind else {
            panic!("expected frames, got {:?}", children[0].kind);
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(*fps, None);
        let Kind::List { items, sel } = &children[1].kind else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 4);
        assert_eq!(*sel, Some(0));
        let Kind::Block { title, body } = &children[2].kind else {
            panic!("expected block");
        };
        assert_eq!(title, "recent");
        assert!(matches!(body.kind, Kind::Text(_)));

        let b = &screen.options[1];
        assert_eq!(b.id, "b");
        assert_eq!(b.note, None);
        let Kind::Row(children) = &b.body.kind else {
            panic!("expected row");
        };
        assert_eq!(children[0].size, Size::Pct(30));
        assert_eq!(children[1].size, Size::Fill);
        let Kind::Block { body, .. } = &children[1].kind else {
            panic!("expected block");
        };
        let Kind::Table { head, rows } = &body.kind else {
            panic!("expected table");
        };
        assert_eq!(*head, None);
        assert_eq!(rows, &vec![vec!["wrangler-p1", "tx-ide-rs", "2h"]]);
    }

    #[test]
    fn default_fps_is_8() {
        let screen = parse(r#"{"body":{"text":"hi"}}"#).unwrap();
        assert_eq!(screen.fps, 8);
    }

    #[test]
    fn single_screen_body() {
        let screen = parse(r#"{"body":{"text":"hi"}}"#).unwrap();
        assert!(screen.options.is_empty());
        let body = screen.body.expect("body");
        assert_eq!(body.size, Size::Fill);
        assert_eq!(body.kind, Kind::Text("hi".into()));
    }

    #[test]
    fn node_with_two_kinds_is_error() {
        let error = parse(r#"{"body":{"text":"a","list":[]}}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("text"), "{error}");
        assert!(error.contains("list"), "{error}");
    }

    #[test]
    fn node_with_no_kind_is_error() {
        let error = parse(r#"{"body":{"size":3}}"#).unwrap_err().to_string();
        assert!(error.contains("one of"), "{error}");
    }

    #[test]
    fn options_and_body_is_error() {
        let error = parse(r#"{"options":[],"body":{"text":"a"}}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("options"), "{error}");
        assert!(error.contains("body"), "{error}");
    }

    #[test]
    fn neither_options_nor_body_is_error() {
        let error = parse(r#"{"title":"t"}"#).unwrap_err().to_string();
        assert!(error.contains("options"), "{error}");
        assert!(error.contains("body"), "{error}");
    }

    #[test]
    fn bad_size_is_error() {
        let error = parse(r#"{"body":{"size":"abc","text":"a"}}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("size"), "{error}");
        let error = parse(r#"{"body":{"size":"130%","text":"a"}}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("size"), "{error}");
    }

    #[test]
    fn truncated_json_is_error_with_position() {
        let error = parse(r#"{"title":"t","body":{"text":"a"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("line"), "{error}");
        assert!(error.contains("col"), "{error}");
    }

    #[test]
    fn frames_fps_overrides() {
        let screen = parse(r#"{"body":{"frames":["a","b"],"fps":2}}"#).unwrap();
        let Kind::Frames { fps, .. } = screen.body.unwrap().kind else {
            panic!("expected frames");
        };
        assert_eq!(fps, Some(2));
    }

    #[test]
    fn table_with_head() {
        let screen = parse(r#"{"body":{"table":[["1"]],"head":["n"]}}"#).unwrap();
        let Kind::Table { head, .. } = screen.body.unwrap().kind else {
            panic!("expected table");
        };
        assert_eq!(head, Some(vec!["n".to_string()]));
    }
}

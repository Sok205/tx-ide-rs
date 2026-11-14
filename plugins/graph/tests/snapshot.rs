mod world;

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tx_graph::app::{App, Source};
use tx_graph::model::{self, Filter};
use tx_graph::run;
use world::HOUR;

fn app(filter: Filter) -> App {
    let now = world::now();
    let agents = world::agents();
    let messages = model::read_messages(&world::dir().join("log.jsonl"), now - HOUR, now).unwrap();
    let contexts: HashMap<_, _> = agents
        .iter()
        .filter_map(|agent| Some((agent.id.clone(), world::context(agent)?)))
        .collect();
    App::new(
        Source {
            agents,
            messages,
            contexts,
        },
        filter,
        now,
        HOUR,
    )
}

fn expected(name: &str) -> String {
    let path = world::dir().join("expected").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn press(app: &mut App, keys: &str) {
    for key in keys.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
    }
}

/// `GRAPH_BLESS=1` rewrites the expected frames.
fn check(name: &str, text: &str) {
    if std::env::var("GRAPH_BLESS").is_ok_and(|value| value == "1") {
        let directory = world::dir().join("expected");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), text).unwrap();
        return;
    }
    assert_eq!(text, expected(name), "{name}");
}

#[test]
fn full_boxes_at_140_columns() {
    let mut app = app(Filter::default());
    check("graph-140x40.txt", &run::snapshot(&mut app, 140, 40));
}

#[test]
fn compact_boxes_at_80_columns() {
    let mut app = app(Filter::default());
    check("graph-80x24.txt", &run::snapshot(&mut app, 80, 24));
}

#[test]
fn moving_selects_and_scrolls() {
    let mut app = app(Filter::default());
    run::snapshot(&mut app, 140, 20);
    press(&mut app, "jjjlll");
    assert_eq!(app.graph.nodes[app.selected].name, "qa-pr4");
    check("graph-140x20-jjjlll.txt", &run::snapshot(&mut app, 140, 20));
}

#[test]
fn g_cycles_the_groups() {
    let mut app = app(Filter::default());
    run::snapshot(&mut app, 140, 40);
    let mut seen = Vec::new();
    for _ in 0..app.graph.groups.len() + 1 {
        press(&mut app, "g");
        seen.push(app.group().unwrap_or("all").to_owned());
    }
    assert_eq!(
        seen,
        [
            "finished",
            "legacy",
            "openbot-research",
            "persistent-views",
            "stray",
            "tx-system",
            "all"
        ]
    );
    press(&mut app, "gggg");
    check(
        "graph-140x30-persistent-views.txt",
        &run::snapshot(&mut app, 140, 30),
    );
}

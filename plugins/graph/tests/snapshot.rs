mod world;

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tx_graph::app::{App, Source};
use tx_graph::model::{self, Filter};
use tx_graph::run;
use world::HOUR;

fn app(filter: Filter) -> App {
    world::set_utc();
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
fn the_sidebar_when_there_is_room() {
    let mut app = app(Filter::default());
    let text = run::snapshot(&mut app, 180, 40);
    assert!(text.contains("messages / min"), "{text}");
    check("graph-180x40.txt", &text);
}

#[test]
fn the_header_names_the_filters_and_the_detail_a_hidden_parent() {
    let mut app = app(Filter {
        tag: None,
        group: Some("persistent-views".into()),
    });
    app.selected = world::index(&app.graph, "pv-orchestrator");
    let text = run::snapshot(&mut app, 120, 30);
    assert!(
        text.lines().next().unwrap().contains("persistent-views"),
        "{text}"
    );
    assert!(text.contains("parent tx-assistant (hidden)"), "{text}");
    let text = run::snapshot(&mut app, 170, 30);
    assert!(
        text.contains("┃ parent      tx-assistant (hidden)"),
        "the sidebar: {text}"
    );
    let mut app = app_with_tag("legacy");
    let text = run::snapshot(&mut app, 100, 30);
    assert!(
        text.lines().next().unwrap().contains("tag legacy"),
        "{text}"
    );
}

fn app_with_tag(tag: &str) -> App {
    app(Filter {
        tag: Some(tag.into()),
        group: None,
    })
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

/// An orchestrator with twenty workers, each sent a different number of messages: one bus, and
/// every count on its worker's own row, never stacked.
#[test]
fn a_wide_fan_shares_a_bus_and_keeps_each_badge_on_its_row() {
    use tx_core::{Agent, AgentId};
    use tx_graph::model::Message;
    let now = 1_000_000.0;
    let id = |number: usize| format!("{number:08x}-0000-4000-8000-000000000000");
    let record = |number: usize, name: String, parent: Option<String>| {
        let parent = parent.map_or("null".to_owned(), |parent| format!("\"{parent}\""));
        Agent::from_json(&format!(
            r#"{{"version":2,"id":"{}","name":"{name}","engine":"claude","working_directory":"/tmp","roles":[],"state":"working","created_at":{},"last_modified_at":{now},"tags":[],"parent_id":{parent}}}"#,
            id(number),
            now - 1000.0 + number as f64
        ))
        .unwrap()
    };
    let mut agents = vec![record(0, "lead".into(), None)];
    agents.extend((1..=20).map(|number| record(number, format!("worker-{number}"), Some(id(0)))));
    let lead: AgentId = id(0).parse().unwrap();
    let mut messages = Vec::new();
    for number in 1..=20 {
        let worker: AgentId = id(number).parse().unwrap();
        for _ in 0..number {
            messages.push(Message {
                at: now - 60.0,
                from: Some(lead.clone()),
                to: worker.clone(),
            });
        }
    }
    for (columns, compact) in [(120, false), (60, true)] {
        let mut app = App::new(
            Source {
                agents: agents.clone(),
                messages: messages.clone(),
                ..Source::default()
            },
            Filter::default(),
            now,
            HOUR,
        );
        let text = run::snapshot(&mut app, columns, 120);
        assert_eq!(app.placement.compact, compact);
        for number in 1..=20 {
            let name = format!("worker-{number}");
            let row = text
                .lines()
                .position(|line| line.contains(&format!(" {name} ")) || line.ends_with(&name))
                .unwrap_or_else(|| panic!("{name} not drawn:\n{text}"));
            // A full box's name is on its top border; the edge arrives one row below.
            let arrival = if compact { row } else { row + 1 };
            let line = text.lines().nth(arrival).unwrap();
            assert!(
                line.contains(&format!(" {number} ▶")),
                "{columns} columns, {name}: {line:?}\n{text}"
            );
        }
    }
}

#[test]
fn the_header_counts_in_the_singular_and_wears_the_brand() {
    use tx_core::Agent;
    use tx_graph::model::Message;
    let now = 1_000_000.0;
    let id = "00000001-0000-4000-8000-000000000000";
    let agent = Agent::from_json(&format!(
        r#"{{"version":2,"id":"{id}","name":"solo","engine":"claude","working_directory":"/tmp","roles":[],"state":"working","created_at":{},"last_modified_at":{now},"tags":[],"parent_id":null}}"#,
        now - 100.0
    ))
    .unwrap();
    let mut app = App::new(
        Source {
            agents: vec![agent],
            messages: vec![Message {
                at: now - 10.0,
                from: None,
                to: id.parse().unwrap(),
            }],
            ..Source::default()
        },
        Filter::default(),
        now,
        HOUR,
    );
    app.brand = "tx-qa".to_owned();
    let text = run::snapshot(&mut app, 120, 20);
    let header = text.lines().next().unwrap();
    assert!(header.starts_with(" tx-qa  graph"), "{header}");
    assert!(header.contains(" 1 agent · "), "{header}");
    assert!(header.contains(" 1 message / 1h"), "{header}");
}

#[test]
fn j_and_k_past_a_columns_end_stay_in_the_column() {
    let mut app = app(Filter::default());
    run::snapshot(&mut app, 140, 20);
    app.selected = world::index(&app.graph, "pv-pr6");
    press(&mut app, "j");
    for _ in 0..10 {
        press(&mut app, "j");
        assert_eq!(app.graph.nodes[app.selected].name, "pv-pr6");
    }
    let rect = app.placement.boxes[app.selected];
    assert!(rect.y >= app.scroll && rect.bottom() <= app.scroll + app.canvas.height);
    app.selected = world::index(&app.graph, "you");
    for _ in 0..10 {
        press(&mut app, "k");
        assert_eq!(app.graph.nodes[app.selected].name, "you");
    }
    assert_eq!(app.scroll, 0);
}

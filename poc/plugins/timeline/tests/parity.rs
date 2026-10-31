//! Phase 6 gate: the payload and the `TestBackend` frames match main's timeline over the same
//! world, with the row source swapped to tx-core records + `tx-chat`'s message parser.

mod world;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tx_core::Session;
use tx_timeline::app::App;
use tx_timeline::model::{Builder, Payload};
use tx_timeline::{run, ui};

struct Built {
    payload: Payload,
    now: f64,
    _dir: tempfile::TempDir,
}

fn build() -> Built {
    world::set_utc();
    let fixtures = world::fixture_dir();
    let world = world::world(&fixtures);
    let sessions: Vec<Session> = world::records(&fixtures)
        .into_iter()
        .map(|record| Session::from_json(&record.to_string()).unwrap())
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut builder = Builder::new(dir.path().join("timeline"));
    let now = world["now"].as_f64().unwrap();
    let transcripts = world::transcripts(&sessions);
    let payload = builder.build(
        &sessions,
        &transcripts,
        world["days"].as_f64().unwrap(),
        now,
    );
    Built {
        payload,
        now,
        _dir: dir,
    }
}

fn expected(name: &str) -> String {
    world::expected(&world::fixture_dir(), name)
}

#[test]
fn timeline_json_matches_mains_fixture() {
    let built = build();
    let main: Value = serde_json::from_str(&expected("timeline.json")).unwrap();
    let ours = built.payload.to_json();
    for (index, (left, right)) in ours["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .zip(main["sessions"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(left, right, "lane {index}");
    }
    assert_eq!(ours, main);
}

#[test]
fn snapshot_4h_matches_main() {
    let built = build();
    let text = run::snapshot(built.payload, built.now, 240.0, 120, 40);
    assert_eq!(text, expected("snapshot-120x40-4h.txt"));
}

#[test]
fn snapshot_1d_matches_main() {
    let built = build();
    let text = run::snapshot(built.payload, built.now, 1440.0, 100, 30);
    assert_eq!(text, expected("snapshot-100x30-1d.txt"));
}

#[test]
fn snapshot_after_keys_matches_main() {
    let built = build();
    let mut app = App::new(built.payload, built.now, 240.0);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| ui::render(&mut app, frame)).unwrap();
    app.fit_initial();
    for key in ['s', 'j', 'j'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
    }
    terminal.draw(|frame| ui::render(&mut app, frame)).unwrap();
    assert_eq!(
        ui::buffer_text(terminal.backend().buffer()),
        expected("snapshot-120x40-4h-sys-jj.txt")
    );
}

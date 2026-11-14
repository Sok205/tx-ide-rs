mod world;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tx_graph::actions;
use tx_graph::app::{App, Mode, Purpose, Request, Source};
use tx_graph::model::Filter;
use tx_graph::run;
use world::HOUR;

fn app() -> App {
    world::set_utc();
    let now = world::now();
    let messages =
        tx_graph::model::read_messages(&world::dir().join("log.jsonl"), now - HOUR, now).unwrap();
    let mut app = App::new(
        Source {
            agents: world::agents(),
            messages,
            ..Source::default()
        },
        Filter::default(),
        now,
        HOUR,
    );
    run::snapshot(&mut app, 140, 40);
    app
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        key(app, KeyCode::Char(character));
    }
}

fn select(app: &mut App, name: &str) {
    app.selected = world::index(&app.graph, name);
}

#[test]
fn send_prompts_then_sends_the_line_as_typed() {
    let mut app = app();
    select(&mut app, "pv-pr4");
    key(&mut app, KeyCode::Char('s'));
    type_text(&mut app, "hello qx");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.mode,
        Mode::Prompt {
            purpose: Purpose::Send,
            text: "hello q".into()
        },
        "q is typed, not quit"
    );
    assert!(!app.quit);
    let frame = run::snapshot(&mut app, 140, 40);
    assert!(
        frame
            .lines()
            .last()
            .unwrap()
            .contains("send to pv-pr4 ▸ hello q▏"),
        "{frame}"
    );
    key(&mut app, KeyCode::Enter);
    let request = app.request.take().unwrap();
    assert!(
        matches!(&request, Request::Send { name, text, .. } if name == "pv-pr4" && text == "hello q")
    );
    let words = actions::words(&request).unwrap();
    assert_eq!(words[0], "send");
    assert_eq!(words[2..], ["--".to_owned(), "hello q".to_owned()]);
    assert_eq!(app.mode, Mode::Browse);
}

#[test]
fn an_empty_send_and_escape_send_nothing() {
    let mut app = app();
    select(&mut app, "pv-pr4");
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.request, None);
    key(&mut app, KeyCode::Char('s'));
    type_text(&mut app, "never");
    key(&mut app, KeyCode::Esc);
    assert_eq!(
        (app.request.clone(), app.mode.clone(), app.quit),
        (None, Mode::Browse, false)
    );
}

#[test]
fn fork_takes_a_name_or_the_default() {
    let mut app = app();
    select(&mut app, "pv-pr5");
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Enter);
    let request = app.request.take().unwrap();
    assert!(matches!(
        &request,
        Request::Fork {
            fork_name: None,
            ..
        }
    ));
    let words = actions::words(&request).unwrap();
    assert_eq!(words[0], "fork");
    assert_eq!(
        words[2..],
        [
            "--prompt".to_owned(),
            "You are a fork of pv-pr5. Introduce yourself.".to_owned()
        ]
    );
    key(&mut app, KeyCode::Char('f'));
    type_text(&mut app, "pr5-b");
    key(&mut app, KeyCode::Enter);
    let words = actions::words(&app.request.take().unwrap()).unwrap();
    assert_eq!(words[4..], ["--name".to_owned(), "pr5-b".to_owned()]);
}

#[test]
fn stop_waits_for_y() {
    let mut app = app();
    select(&mut app, "pv-pr6");
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.mode, Mode::ConfirmStop);
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(
        (app.request.clone(), app.mode.clone()),
        (None, Mode::Browse)
    );
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Char('y'));
    let request = app.request.take().unwrap();
    assert_eq!(actions::words(&request).unwrap()[0], "stop");
}

#[test]
fn refusals_say_why() {
    let mut app = app();
    select(&mut app, "you");
    for code in ['s', 'f', 'a', 'x', 'r'] {
        key(&mut app, KeyCode::Char(code));
        assert_eq!(app.request, None);
        assert!(
            app.notice.as_deref().unwrap().contains("the operator"),
            "{code}"
        );
    }
    key(&mut app, KeyCode::Enter);
    assert!(app.notice.as_deref().unwrap().contains("the operator"));
    select(&mut app, "finished");
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.notice.as_deref(),
        Some("finished has ended · r respawns it")
    );
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.mode, Mode::Browse);
    key(&mut app, KeyCode::Char('r'));
    let request = app.request.take().unwrap();
    assert_eq!(actions::words(&request).unwrap()[0], "respawn");
    select(&mut app, "pv-pr4");
    key(&mut app, KeyCode::Char('r'));
    assert_eq!(app.notice.as_deref(), Some("pv-pr4 is running"));
}

#[test]
fn focus_and_artifacts_need_the_terminal() {
    let mut app = app();
    select(&mut app, "pv-pr4");
    key(&mut app, KeyCode::Enter);
    let focus = app.request.take().unwrap();
    assert_eq!(
        focus,
        Request::Focus {
            name: "pv-pr4".into()
        }
    );
    assert_eq!(actions::words(&focus), None);
    key(&mut app, KeyCode::Char('a'));
    let artifacts = app.request.take().unwrap();
    assert!(matches!(artifacts, Request::Artifacts { .. }));
    assert_eq!(actions::words(&artifacts), None);
}

#[test]
fn question_mark_shows_the_keys() {
    let mut app = app();
    key(&mut app, KeyCode::Char('?'));
    let frame = run::snapshot(&mut app, 100, 24);
    assert!(frame.contains("respawn an ended agent"), "{frame}");
    assert!(frame.ends_with("any key closes"), "{frame}");
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quit, "the key that closes the page does nothing else");
    assert_eq!(app.mode, Mode::Browse);
}

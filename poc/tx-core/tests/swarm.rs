//! Spec verification 1: a swarm survives the core that launched it; a second process sees the
//! same sessions; execution targets reach the right pane while the others keep running.

mod common;

use std::process::Command;
use std::time::Duration;

use common::{Harness, agent, cwd_of, typed, wait_for};
use tx_core::{CoreConfig, Directories, ListFilter, SendOptions, TxCore};

#[test]
fn child_lists_running_sessions() {
    let Ok(home) = std::env::var("TX_CORE_SWARM_CHILD") else {
        return;
    };
    let core = TxCore::open(CoreConfig::new(Directories::under(home))).unwrap();
    let mut names: Vec<String> = core
        .list_agents(&ListFilter::new().running())
        .unwrap()
        .iter()
        .map(|id| core.get_agent(id).unwrap().name)
        .collect();
    names.sort();
    println!("\nTXC-SWARM {}", names.join(","));
}

#[test]
fn swarm_outlives_its_core_and_targets_reach_the_right_pane() {
    require_tmux!();
    let h = Harness::new();
    let (alice, bob, tool, alice_dir, bob_dir, tool_dir) = {
        let core = h.open_with_engine();
        let alice = core
            .create_agent(agent("alice", &h.repository("alice")))
            .unwrap();
        let bob = core
            .create_agent(agent("bob", &h.repository("bob")))
            .unwrap();
        let tool = core
            .create_agent(agent("tool", &h.repository("tool")))
            .unwrap();
        let (alice_dir, bob_dir, tool_dir) = (
            cwd_of(&core, &alice),
            cwd_of(&core, &bob),
            cwd_of(&core, &tool),
        );
        for dir in [&alice_dir, &bob_dir, &tool_dir] {
            assert!(wait_for(Duration::from_secs(5), || dir
                .join("env.txt")
                .exists()));
        }
        core.send_message(&bob, "ping", SendOptions::from_agent(alice.clone()))
            .unwrap();
        core.send_message(&alice, "pong", SendOptions::from_agent(bob.clone()))
            .unwrap();
        core.send_message(&tool, "run", SendOptions::default())
            .unwrap();
        (alice, bob, tool, alice_dir, bob_dir, tool_dir)
    };
    // The launching core is gone; the sessions are not.
    assert_eq!(h.live_names().len(), 3);
    assert!(wait_for(Duration::from_secs(5), || typed(&bob_dir).contains("ping")));
    assert_eq!(
        typed(&bob_dir),
        "<from-agent session=\"alice\">ping</from-agent>\n"
    );
    assert_eq!(
        typed(&alice_dir),
        "<from-agent session=\"bob\">pong</from-agent>\n"
    );
    assert_eq!(typed(&tool_dir), "<from-user>run</from-user>\n");

    // A new process opens the same state and sees the same running sessions.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_lists_running_sessions",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("TX_CORE_SWARM_CHILD", h.home())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let seen = stdout
        .lines()
        .find_map(|l| l.strip_prefix("TXC-SWARM "))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(seen, "alice,bob,tool");

    // Execution targets resolve to the right session and pane while the others keep running.
    let core = h.open_with_engine();
    for (id, name, dir) in [
        (&alice, "alice", &alice_dir),
        (&bob, "bob", &bob_dir),
        (&tool, "tool", &tool_dir),
    ] {
        let at = core.get_execution_location(id).unwrap();
        assert_eq!(at.socket, h.socket());
        let resolved = h.tmux_ok(&[
            "display-message",
            "-p",
            "-t",
            &at.session_target,
            "#{session_name}",
        ]);
        assert_eq!(resolved.trim(), name);
        let pane_session = h.tmux_ok(&[
            "display-message",
            "-p",
            "-t",
            &at.pane_target,
            "#{session_name}",
        ]);
        assert_eq!(pane_session.trim(), name);
        core.send_message(id, "again", SendOptions::default())
            .unwrap();
        assert!(wait_for(Duration::from_secs(5), || typed(dir).contains("again")));
    }
    assert_eq!(h.live_names().len(), 3);
    assert!(core.refresh_all().unwrap().is_empty());
}

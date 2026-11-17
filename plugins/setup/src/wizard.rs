//! The checklist a person chooses the plugins from. [`Wizard`] is the state and the frame, and
//! needs no terminal; [`run`] is the key loop. It draws on stderr: stdout carries the result.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::PermissionsExt;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::presets::Bundles;

/// The header when the ticks are no preset's exact set.
const CUSTOM: &str = "custom";
const LINK_DOTFILES: &str = "link ~/.config/nvim and ~/.tmux.conf to this checkout";
const KEYS: &str = "j/k move   space toggle   1 headless   2 ide   3 full   enter accept   q leave";

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    Accept,
    Leave,
}

pub struct Wizard<'a> {
    bundles: &'a Bundles,
    /// Every preset that resolves against the bundles, in listing order, with what it selects.
    presets: &'a [(String, BTreeSet<String>)],
    /// Per plugin, the programs it needs that are not on `PATH`.
    missing: BTreeMap<String, Vec<String>>,
    pub ticked: BTreeSet<String>,
    /// The preset last ticked as a whole: the one named while the ticks still equal its set.
    picked: String,
    pub link_dotfiles: bool,
    /// The row the cursor is on: a plugin in id order, or the dotfiles row, the last.
    cursor: usize,
    /// What the last toggle added or could not add.
    notes: Vec<String>,
}

impl<'a> Wizard<'a> {
    /// The checklist with the preset `picked` ticked, which is one of `presets`. `on_path`
    /// says whether a program can be run.
    pub fn new(
        bundles: &'a Bundles,
        presets: &'a [(String, BTreeSet<String>)],
        picked: &str,
        on_path: impl Fn(&str) -> bool,
    ) -> Self {
        let missing = bundles
            .manifests
            .iter()
            .map(|(id, manifest)| {
                let missing = manifest
                    .needs
                    .iter()
                    .filter(|program| !on_path(program))
                    .cloned()
                    .collect();
                (id.clone(), missing)
            })
            .collect();
        let mut wizard = Self {
            bundles,
            presets,
            missing,
            ticked: BTreeSet::new(),
            picked: String::new(),
            link_dotfiles: false,
            cursor: 0,
            notes: Vec::new(),
        };
        wizard.tick_preset(picked);
        wizard
    }

    /// The preset the ticks equal; none when they are no preset's exact set. Of two presets
    /// that hold the same set it is the one last picked, else the first in listing order: the
    /// name is recorded, and it is the tiers an upgrade follows.
    pub fn preset(&self) -> Option<&str> {
        let equal = |name: &str| {
            self.presets
                .iter()
                .any(|(known, plugins)| known == name && *plugins == self.ticked)
        };
        if equal(&self.picked) {
            return Some(&self.picked);
        }
        self.presets
            .iter()
            .find(|(_, plugins)| *plugins == self.ticked)
            .map(|(name, _)| name.as_str())
    }

    fn last_row(&self) -> usize {
        self.bundles.manifests.len()
    }

    /// Ticks exactly what the preset `name` selects.
    fn tick_preset(&mut self, name: &str) {
        if let Some((_, plugins)) = self.presets.iter().find(|(known, _)| known == name) {
            self.ticked = plugins.clone();
            self.picked = name.to_owned();
            self.notes.clear();
        }
    }

    /// Toggles the row under the cursor. A plugin is ticked with what it requires, and unticked
    /// with what requires it.
    fn toggle(&mut self) {
        self.notes.clear();
        let Some(id) = self.bundles.manifests.keys().nth(self.cursor) else {
            self.link_dotfiles = !self.link_dotfiles;
            return;
        };
        if self.ticked.remove(id) {
            for dependant in self.bundles.dependants(id, &self.ticked) {
                self.ticked.remove(&dependant);
                self.notes
                    .push(format!("{dependant} requires {id}: removed"));
            }
        } else {
            self.ticked.insert(id.clone());
            self.notes = self.bundles.require(id, &mut self.ticked);
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Step {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Step::Leave,
                _ => Step::Continue,
            };
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(self.last_row())
            }
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.cursor = self.last_row(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('1') => self.tick_preset(crate::presets::SHIPPED[0].0),
            KeyCode::Char('2') => self.tick_preset(crate::presets::SHIPPED[1].0),
            KeyCode::Char('3') => self.tick_preset(crate::presets::SHIPPED[2].0),
            KeyCode::Enter => return Step::Accept,
            KeyCode::Char('q') | KeyCode::Esc => return Step::Leave,
            _ => {}
        }
        Step::Continue
    }

    fn header(&self) -> String {
        format!(
            "tx setup: {}   ({} of {} plugins)",
            self.preset().unwrap_or(CUSTOM),
            self.ticked.len(),
            self.bundles.manifests.len()
        )
    }

    /// One line per row: the plugins in id order, then the dotfiles row.
    fn rows(&self) -> Vec<String> {
        let width = self
            .bundles
            .manifests
            .keys()
            .map(String::len)
            .max()
            .unwrap_or(0);
        let tick = |ticked: bool| if ticked { "[x]" } else { "[ ]" };
        let mut rows: Vec<String> = self
            .bundles
            .manifests
            .iter()
            .map(|(id, manifest)| {
                let mut row = format!(
                    "{} {id:width$}  {:5}  {}",
                    tick(self.ticked.contains(id)),
                    manifest.tier.as_str(),
                    manifest.description
                );
                for program in &self.missing[id] {
                    row.push_str(&format!("  needs {program} (not found)"));
                }
                row
            })
            .collect();
        rows.push(format!("{} {LINK_DOTFILES}", tick(self.link_dotfiles)));
        rows
    }

    pub fn draw(&self, area: Rect, buffer: &mut Buffer) {
        let notes = u16::try_from(self.notes.len()).unwrap_or(u16::MAX);
        let [header, rows, notes] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(notes),
        ])
        .areas(area);
        Paragraph::new(vec![
            Line::styled(self.header(), Style::new().add_modifier(Modifier::BOLD)),
            Line::styled(KEYS, Style::new().add_modifier(Modifier::DIM)),
        ])
        .render(header, buffer);
        // The cursor's row stays in view.
        let first = (self.cursor + 1).saturating_sub(usize::from(rows.height));
        let lines: Vec<Line> = self
            .rows()
            .into_iter()
            .enumerate()
            .skip(first)
            .map(|(row, text)| {
                if row == self.cursor {
                    Line::styled(
                        format!("> {text}"),
                        Style::new().add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Line::raw(format!("  {text}"))
                }
            })
            .collect();
        Paragraph::new(lines).render(rows, buffer);
        let lines: Vec<Line> = self
            .notes
            .iter()
            .map(|note| Line::raw(note.as_str()))
            .collect();
        Paragraph::new(lines).render(notes, buffer);
    }
}

/// Whether `program` is an executable file in a directory of `PATH`.
pub fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        std::fs::metadata(directory.join(program))
            .is_ok_and(|found| found.is_file() && found.permissions().mode() & 0o111 != 0)
    })
}

/// Shows `wizard` on the terminal of stderr until a key accepts or leaves. Whether it was
/// accepted.
pub fn run(wizard: &mut Wizard<'_>) -> io::Result<bool> {
    enable_raw_mode()?;
    execute!(io::stderr(), EnterAlternateScreen)?;
    let result = keys(wizard);
    execute!(io::stderr(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}

fn keys(wizard: &mut Wizard<'_>) -> io::Result<bool> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    loop {
        terminal.draw(|frame| wizard.draw(frame.area(), frame.buffer_mut()))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match wizard.key(key) {
                Step::Continue => {}
                Step::Accept => return Ok(true),
                Step::Leave => return Ok(false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Presets;
    use crate::presets::testing::{all, set, source};

    fn resolved(bundles: &Bundles) -> Vec<(String, BTreeSet<String>)> {
        let (presets, _) = Presets::read(std::path::Path::new("/nonexistent/presets")).unwrap();
        let resolve = |(name, preset): &(String, crate::presets::Preset)| {
            (name.clone(), preset.resolve(bundles).unwrap())
        };
        presets.0.iter().map(resolve).collect()
    }

    fn press(wizard: &mut Wizard<'_>, keys: &str) -> Step {
        let mut step = Step::Continue;
        for character in keys.chars() {
            let code = match character {
                '\n' => KeyCode::Enter,
                character => KeyCode::Char(character),
            };
            step = wizard.key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        step
    }

    fn screen(wizard: &Wizard<'_>) -> Vec<String> {
        let area = Rect::new(0, 0, 100, 12);
        let mut buffer = Buffer::empty(area);
        wizard.draw(area, &mut buffer);
        (0..area.height)
            .map(|row| {
                let line: String = (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect();
                line.trim_end().to_owned()
            })
            .collect()
    }

    #[test]
    fn it_starts_as_given_and_draws_a_row_per_plugin_and_the_dotfiles_row() {
        let (_directory, bundles) = source();
        let presets = resolved(&bundles);
        let wizard = Wizard::new(&bundles, &presets, "full", |_| false);
        assert_eq!(
            screen(&wizard)[..9],
            [
                "tx setup: full   (5 of 5 plugins)",
                KEYS,
                "",
                "> [x] engine  core   The engine plugin.",
                "  [x] home    ide    The home plugin.",
                "  [x] ide     ide    The ide plugin.",
                "  [x] labels  core   The labels plugin.  needs tx-setup-no-such-program (not found)",
                "  [x] themes  extra  The themes plugin.",
                "  [ ] link ~/.config/nvim and ~/.tmux.conf to this checkout",
            ]
        );
    }

    #[test]
    fn a_program_on_path_is_not_reported() {
        let (_directory, bundles) = source();
        let presets = resolved(&bundles);
        let wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        assert!(!screen(&wizard).join("\n").contains("not found"));
        assert!(on_path("sh"));
        assert!(!on_path("tx-setup-no-such-program"));
    }

    #[test]
    fn the_digits_tick_exactly_a_shipped_preset_and_a_toggle_makes_it_custom() {
        let (_directory, bundles) = source();
        let presets = resolved(&bundles);
        let mut wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        press(&mut wizard, "1");
        assert_eq!(wizard.ticked, set(&["engine", "labels"]));
        assert_eq!(wizard.preset(), Some("headless"));
        press(&mut wizard, "2");
        assert_eq!(wizard.ticked, set(&["engine", "home", "ide", "labels"]));
        assert_eq!(wizard.preset(), Some("ide"));
        press(&mut wizard, " ");
        assert_eq!(wizard.preset(), None);
        assert!(screen(&wizard)[0].starts_with("tx setup: custom "));
        press(&mut wizard, "3");
        assert_eq!(wizard.preset(), Some("full"));
    }

    #[test]
    fn of_two_presets_with_the_same_set_the_one_last_picked_is_named() {
        let (directory, _) = source();
        for id in ["ide", "home", "themes"] {
            std::fs::remove_dir_all(directory.path().join(id)).unwrap();
        }
        let (bundles, _) = Bundles::read(directory.path()).unwrap();
        let presets = resolved(&bundles);
        let mut wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        assert_eq!(
            wizard.preset(),
            Some("full"),
            "all three hold the two core plugins"
        );
        press(&mut wizard, "2");
        assert_eq!(wizard.preset(), Some("ide"));
        assert!(screen(&wizard)[0].starts_with("tx setup: ide "));
        press(&mut wizard, " ");
        assert_eq!(wizard.preset(), None);
        press(&mut wizard, " ");
        assert_eq!(
            wizard.preset(),
            Some("ide"),
            "the ticks equal its set again"
        );
        press(&mut wizard, "1");
        assert_eq!(wizard.preset(), Some("headless"));
    }

    #[test]
    fn a_tick_brings_what_the_plugin_requires_and_an_untick_takes_what_requires_it() {
        let (directory, bundles) = source();
        let presets = resolved(&bundles);
        let mut wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        wizard.ticked.clear();
        press(&mut wizard, "jjjj ");
        assert_eq!(wizard.ticked, set(&["home", "ide", "themes"]));
        let screen_text = screen(&wizard).join("\n");
        assert!(
            screen_text.contains("themes requires home: added"),
            "{screen_text}"
        );
        assert!(
            screen_text.contains("home requires ide: added"),
            "{screen_text}"
        );
        let missing = format!(
            "themes requires gone, which has no bundle in {}",
            directory.path().display()
        );
        assert!(screen_text.contains(&missing[..60]), "{screen_text}");
        press(&mut wizard, "kk ");
        assert_eq!(
            wizard.ticked,
            set(&[]),
            "ide went, and home and themes with it"
        );
    }

    #[test]
    fn the_last_row_is_the_dotfiles_and_enter_accepts_while_q_and_escape_leave() {
        let (_directory, bundles) = source();
        let presets = resolved(&bundles);
        let mut wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        assert_eq!(press(&mut wizard, "jjjjjjjj "), Step::Continue);
        assert!(wizard.link_dotfiles);
        assert_eq!(wizard.ticked, all(&bundles), "the row is no plugin");
        press(&mut wizard, "g G ");
        assert!(!wizard.link_dotfiles, "G is the last row again");
        assert!(!wizard.ticked.contains("engine"), "g was the first");
        press(&mut wizard, " ");
        assert_eq!(press(&mut wizard, "\n"), Step::Accept);
        assert_eq!(press(&mut wizard, "q"), Step::Leave);
        assert_eq!(
            wizard.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Step::Leave
        );
        assert_eq!(
            wizard.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Step::Leave
        );
    }

    #[test]
    fn the_cursor_row_stays_in_view_on_a_short_screen() {
        let (_directory, bundles) = source();
        let presets = resolved(&bundles);
        let mut wizard = Wizard::new(&bundles, &presets, "full", |_| true);
        press(&mut wizard, "G");
        let area = Rect::new(0, 0, 80, 6);
        let mut buffer = Buffer::empty(area);
        wizard.draw(area, &mut buffer);
        let last: String = (0..area.width)
            .map(|column| buffer[(column, 5)].symbol())
            .collect();
        assert!(last.starts_with("> [ ] link "), "{last}");
    }
}

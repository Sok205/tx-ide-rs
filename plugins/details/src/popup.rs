//! The prefix+i popup: the details rows, scrollable, closed by `q` or `Esc` like the other tx
//! popups.

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::rows::{Row, label_width};

const ACCENT: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(169, 177, 214);
const FOREGROUND: Color = Color::Rgb(192, 202, 245);

pub fn lines(rows: &[Row]) -> Vec<Line<'static>> {
    let accent = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(DIM);
    let foreground = Style::new().fg(FOREGROUND);
    let width = label_width(rows);
    rows.iter()
        .map(|row| match row {
            Row::Title(name, id) => Line::from(vec![
                Span::styled(name.clone(), foreground.add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {id}"), dim),
            ]),
            Row::Heading(text) => Line::styled(text.clone(), accent),
            Row::Field(label, value) => Line::from(vec![
                Span::styled(format!("  {label:<width$}  "), dim),
                Span::styled(value.clone(), foreground),
            ]),
            Row::Item(text) => Line::styled(format!("  {text}"), foreground),
            Row::Note(text) => Line::styled(text.clone(), dim.add_modifier(Modifier::ITALIC)),
            Row::Blank => Line::default(),
            Row::Text(text) => Line::styled(format!("  {text}"), foreground),
        })
        .collect()
}

pub fn run(rows: &[Row]) -> io::Result<()> {
    let lines = lines(rows);
    let total = lines.len() as u16;
    let mut terminal = ratatui::try_init()?;
    let mut offset: u16 = 0;
    let result = loop {
        let mut height = 0;
        if let Err(error) = terminal.draw(|frame| {
            height = frame.area().height;
            frame.render_widget(
                Paragraph::new(lines.clone()).scroll((offset, 0)),
                frame.area(),
            );
        }) {
            break Err(error);
        }
        let last = total.saturating_sub(height);
        let event = match event::read() {
            Ok(event) => event,
            Err(error) => break Err(error),
        };
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let page = height.saturating_sub(1).max(1);
        offset = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Char('j') | KeyCode::Down => offset.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => offset.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => offset.saturating_add(page),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_add(page / 2)
            }
            KeyCode::PageUp => offset.saturating_sub(page),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_sub(page / 2)
            }
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => last,
            _ => offset,
        }
        .min(last);
    };
    ratatui::restore();
    result
}

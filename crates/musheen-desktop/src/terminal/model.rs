use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode, test::TermSize as AlacrittySize};
use alacritty_terminal::vte::ansi;

use super::TerminalSize;

pub const MAX_SCROLLBACK_LINES: usize = 10_000;
pub const MAX_SCROLLBACK_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteDisposition {
    Safe,
    ConfirmationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalCell {
    character: char,
    width: u8,
    row: i32,
    column: usize,
}

impl TerminalCell {
    #[must_use]
    pub const fn character(&self) -> char {
        self.character
    }

    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    #[must_use]
    pub const fn row(&self) -> i32 {
        self.row
    }

    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Title(_) | Event::ResetTitle) {
            self.0
                .lock()
                .expect("terminal event lock is not poisoned")
                .push(event);
        }
    }
}

pub struct TerminalModel {
    size: TerminalSize,
    terminal: Term<Listener>,
    parser: ansi::Processor,
    listener: Listener,
    title: Option<String>,
    transcript: BoundedTranscript,
}

impl TerminalModel {
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        let listener = Listener::default();
        let dimensions = AlacrittySize::new(size.columns().into(), size.rows().into());
        let config = Config {
            scrolling_history: MAX_SCROLLBACK_LINES,
            ..Config::default()
        };
        Self {
            size,
            terminal: Term::new(config, &dimensions, listener.clone()),
            parser: ansi::Processor::new(),
            listener,
            title: None,
            transcript: BoundedTranscript::default(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.terminal, bytes);
        self.transcript.feed(bytes);
        for event in self
            .listener
            .0
            .lock()
            .expect("terminal event lock is not poisoned")
            .drain(..)
        {
            match event {
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                _ => {}
            }
        }
    }

    pub fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.terminal.resize(AlacrittySize::new(
            size.columns().into(),
            size.rows().into(),
        ));
    }

    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn cells(&self) -> Vec<TerminalCell> {
        self.terminal
            .renderable_content()
            .display_iter
            .filter(|indexed| !indexed.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|indexed| TerminalCell {
                character: indexed.c,
                width: if indexed.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                },
                row: indexed.point.line.0,
                column: indexed.point.column.0,
            })
            .collect()
    }

    #[must_use]
    pub fn visible_text(&self) -> String {
        let mut cells = self.cells();
        cells.sort_by_key(|cell| (cell.row, cell.column));
        let mut text = String::new();
        let mut row = cells.first().map_or(0, |cell| cell.row);
        for cell in cells {
            while cell.row > row {
                while text.ends_with(' ') {
                    text.pop();
                }
                text.push('\n');
                row += 1;
            }
            text.push(cell.character);
        }
        text.trim_end().to_owned()
    }

    #[must_use]
    pub fn classify_paste(&self, value: &str) -> PasteDisposition {
        if value.contains(['\n', '\r'])
            || value
                .chars()
                .any(|character| character.is_control() && character != '\t')
        {
            PasteDisposition::ConfirmationRequired
        } else {
            PasteDisposition::Safe
        }
    }

    #[must_use]
    pub fn encode_paste(&self, value: &str, bracketed: bool) -> Vec<u8> {
        let sanitized: String = value
            .chars()
            .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
            .collect();
        if bracketed {
            format!("\u{1b}[200~{sanitized}\u{1b}[201~").into_bytes()
        } else {
            sanitized.into_bytes()
        }
    }

    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.terminal.mode().contains(TermMode::BRACKETED_PASTE)
    }

    #[must_use]
    pub fn scrollback_line_count(&self) -> usize {
        self.transcript.lines.len()
    }

    #[must_use]
    pub const fn scrollback_bytes(&self) -> usize {
        self.transcript.bytes + self.transcript.current.len()
    }

    #[must_use]
    pub fn scrollback_text(&self) -> String {
        self.transcript
            .lines
            .iter()
            .map(|line| String::from_utf8_lossy(line))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EscapeState {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
    DeviceControl,
    DeviceControlEscape,
}

#[derive(Default)]
struct BoundedTranscript {
    lines: VecDeque<Vec<u8>>,
    current: Vec<u8>,
    bytes: usize,
    escape: EscapeState,
    discarding_oversized_line: bool,
}

impl BoundedTranscript {
    fn feed(&mut self, input: &[u8]) {
        for &byte in input {
            match self.escape {
                EscapeState::Text => match byte {
                    0x1b => self.escape = EscapeState::Escape,
                    b'\n' => self.finish_line(),
                    // A carriage return moves the terminal cursor; it does not
                    // complete or erase the logical scrollback line.
                    b'\r' => {}
                    b'\t' => self.push_text(byte),
                    0x00..=0x1f | 0x7f => {}
                    _ => self.push_text(byte),
                },
                EscapeState::Escape => {
                    self.escape = match byte {
                        b'[' => EscapeState::Csi,
                        b']' => EscapeState::Osc,
                        b'P' | b'X' | b'^' | b'_' => EscapeState::DeviceControl,
                        _ => EscapeState::Text,
                    };
                }
                EscapeState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = EscapeState::Text;
                    }
                }
                EscapeState::Osc => match byte {
                    0x07 => self.escape = EscapeState::Text,
                    0x1b => self.escape = EscapeState::OscEscape,
                    _ => {}
                },
                EscapeState::OscEscape => {
                    self.escape = if byte == b'\\' {
                        EscapeState::Text
                    } else {
                        EscapeState::Osc
                    };
                }
                EscapeState::DeviceControl => {
                    if byte == 0x1b {
                        self.escape = EscapeState::DeviceControlEscape;
                    }
                }
                EscapeState::DeviceControlEscape => {
                    self.escape = if byte == b'\\' {
                        EscapeState::Text
                    } else {
                        EscapeState::DeviceControl
                    };
                }
            }
        }
    }

    fn finish_line(&mut self) {
        if self.discarding_oversized_line {
            self.discarding_oversized_line = false;
            self.current.clear();
            return;
        }
        self.current.push(b'\n');
        self.bytes = self.bytes.saturating_add(self.current.len());
        self.lines.push_back(std::mem::take(&mut self.current));
        while self.lines.len() > MAX_SCROLLBACK_LINES || self.bytes > MAX_SCROLLBACK_BYTES {
            let Some(removed) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.len());
        }
    }

    fn push_text(&mut self, byte: u8) {
        if self.discarding_oversized_line {
            return;
        }
        self.current.push(byte);
        while self.bytes + self.current.len() > MAX_SCROLLBACK_BYTES {
            if let Some(removed) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                // A single unterminated line cannot be split while honoring
                // complete-line truncation, so discard that line as a unit.
                self.current.clear();
                self.discarding_oversized_line = true;
                break;
            }
        }
    }
}

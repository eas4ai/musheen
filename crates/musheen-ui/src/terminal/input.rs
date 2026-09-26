#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TerminalKey {
    F4,
    Enter,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    CtrlC,
    CtrlD,
    CtrlL,
    CtrlU,
    CtrlW,
    CtrlZ,
    CtrlShiftC,
    CtrlShiftV,
    ShiftInsert,
}

#[derive(Clone, Debug)]
pub struct TerminalInput {
    bindings: &'static [(TerminalKey, &'static str, &'static [u8])],
}

impl TerminalInput {
    #[must_use]
    pub const fn default_bindings() -> Self {
        Self {
            bindings: DEFAULT_BINDINGS,
        }
    }

    #[must_use]
    pub fn action_for(&self, key: TerminalKey) -> Option<&'static str> {
        self.bindings
            .iter()
            .find_map(|(candidate, action, _)| (*candidate == key).then_some(*action))
    }

    #[must_use]
    pub fn bytes_for(&self, key: TerminalKey) -> Option<&'static [u8]> {
        self.bindings
            .iter()
            .find_map(|(candidate, _, bytes)| (*candidate == key).then_some(*bytes))
    }
}

impl Default for TerminalInput {
    fn default() -> Self {
        Self::default_bindings()
    }
}

const DEFAULT_BINDINGS: &[(TerminalKey, &str, &[u8])] = &[
    (TerminalKey::F4, "terminal.toggle", b""),
    (TerminalKey::Enter, "terminal.enter", b"\r"),
    (TerminalKey::Backspace, "terminal.backspace", b"\x7f"),
    (TerminalKey::Tab, "terminal.tab", b"\t"),
    (TerminalKey::Escape, "terminal.escape", b"\x1b"),
    (TerminalKey::ArrowUp, "terminal.up", b"\x1b[A"),
    (TerminalKey::ArrowDown, "terminal.down", b"\x1b[B"),
    (TerminalKey::ArrowRight, "terminal.right", b"\x1b[C"),
    (TerminalKey::ArrowLeft, "terminal.left", b"\x1b[D"),
    (TerminalKey::Home, "terminal.home", b"\x1b[H"),
    (TerminalKey::End, "terminal.end", b"\x1b[F"),
    (TerminalKey::PageUp, "terminal.page-up", b"\x1b[5~"),
    (TerminalKey::PageDown, "terminal.page-down", b"\x1b[6~"),
    (TerminalKey::Delete, "terminal.delete", b"\x1b[3~"),
    (TerminalKey::Insert, "terminal.insert", b"\x1b[2~"),
    (TerminalKey::CtrlC, "terminal.interrupt", b"\x03"),
    (TerminalKey::CtrlD, "terminal.eof", b"\x04"),
    (TerminalKey::CtrlL, "terminal.clear", b"\x0c"),
    (TerminalKey::CtrlU, "terminal.erase-line", b"\x15"),
    (TerminalKey::CtrlW, "terminal.erase-word", b"\x17"),
    (TerminalKey::CtrlZ, "terminal.suspend", b"\x1a"),
    (TerminalKey::CtrlShiftC, "terminal.copy", b""),
    (TerminalKey::CtrlShiftV, "terminal.paste", b""),
    (TerminalKey::ShiftInsert, "terminal.paste", b""),
];

//! Versioned presentation preferences. Command behavior always comes from the registry.
use crate::{CommandId, CommandRegistry, ShortcutScope};
use std::{collections::HashSet, error::Error, fmt};

const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_ENTRIES: usize = 256;
const NAVIGATION_ESCAPE: &str = "navigation.location";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CustomizationError {
    InvalidDocument,
    UnknownCommand,
    DuplicateCommand,
    RequiredNavigation,
    InvalidPosition,
    InvalidChord,
    ReservedChord,
    Conflict(CommandId),
}
impl fmt::Display for CustomizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "customization rejected: {self:?}")
    }
}
impl Error for CustomizationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolbarLayout {
    ids: Vec<CommandId>,
}
impl Default for ToolbarLayout {
    fn default() -> Self {
        Self {
            ids: [
                "navigation.location",
                "clipboard.cut",
                "clipboard.copy",
                "clipboard.paste_into",
                "file.rename",
                "view.info",
                "app.settings",
            ]
            .into_iter()
            .map(|id| CommandId::new(id).expect("built-in id"))
            .collect(),
        }
    }
}
impl ToolbarLayout {
    pub fn ids(&self) -> &[CommandId] {
        &self.ids
    }
    pub fn add(&mut self, id: &str, registry: &CommandRegistry) -> Result<(), CustomizationError> {
        let command = registry.get(id).ok_or(CustomizationError::UnknownCommand)?;
        if self.ids.contains(command.id()) {
            return Err(CustomizationError::DuplicateCommand);
        }
        if self.ids.len() >= MAX_ENTRIES || self.export().len() + id.len() + 1 > MAX_DOCUMENT {
            return Err(CustomizationError::InvalidDocument);
        }
        self.ids.push(command.id().clone());
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> Result<(), CustomizationError> {
        if id == NAVIGATION_ESCAPE {
            return Err(CustomizationError::RequiredNavigation);
        }
        let index = self
            .ids
            .iter()
            .position(|value| value.as_str() == id)
            .ok_or(CustomizationError::UnknownCommand)?;
        self.ids.remove(index);
        Ok(())
    }
    /// Used by pointer drops and keyboard Move Up/Down buttons alike.
    pub fn move_to(&mut self, id: &str, index: usize) -> Result<(), CustomizationError> {
        if index >= self.ids.len() {
            return Err(CustomizationError::InvalidPosition);
        }
        let from = self
            .ids
            .iter()
            .position(|value| value.as_str() == id)
            .ok_or(CustomizationError::UnknownCommand)?;
        let id = self.ids.remove(from);
        self.ids.insert(index, id);
        Ok(())
    }
    pub fn export(&self) -> String {
        format!(
            "v1;{}",
            self.ids
                .iter()
                .map(CommandId::as_str)
                .collect::<Vec<_>>()
                .join(";")
        )
    }
    pub fn import(value: &str) -> Result<Self, CustomizationError> {
        if value == "default" {
            return Ok(Self::default());
        }
        let entries = document_entries(value)?;
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for value in entries {
            let id = CommandId::new(value).map_err(|_| CustomizationError::InvalidDocument)?;
            if !seen.insert(id.clone()) {
                return Err(CustomizationError::DuplicateCommand);
            }
            ids.push(id);
        }
        if !ids.iter().any(|id| id.as_str() == NAVIGATION_ESCAPE) {
            return Err(CustomizationError::RequiredNavigation);
        }
        Ok(Self { ids })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutBinding {
    pub command: CommandId,
    pub scope: ShortcutScope,
    pub chord: String,
}
/// Overrides are per (command, scope); an empty chord disables that default.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortcutMap {
    overrides: Vec<ShortcutBinding>,
}
impl ShortcutMap {
    pub fn overrides(&self) -> &[ShortcutBinding] {
        &self.overrides
    }
    pub fn bindings(&self, registry: &CommandRegistry) -> Vec<ShortcutBinding> {
        let mut result: Vec<_> = registry
            .commands()
            .iter()
            .flat_map(|command| {
                command.shortcuts().iter().map(|shortcut| ShortcutBinding {
                    command: command.id().clone(),
                    scope: shortcut.scope(),
                    chord: canonical_chord(shortcut.chord()).expect("registry shortcut is valid"),
                })
            })
            .filter(|binding| {
                !self
                    .overrides
                    .iter()
                    .any(|entry| entry.command == binding.command && entry.scope == binding.scope)
            })
            .collect();
        result.extend(
            self.overrides
                .iter()
                .filter(|entry| !entry.chord.is_empty())
                .cloned(),
        );
        result
    }
    pub fn assign(
        &mut self,
        id: &str,
        scope: ShortcutScope,
        chord: &str,
        registry: &CommandRegistry,
    ) -> Result<(), CustomizationError> {
        let command = registry
            .get(id)
            .ok_or(CustomizationError::UnknownCommand)?
            .id()
            .clone();
        let chord = canonical_chord(chord)?;
        if reserved_chord(&chord) {
            return Err(CustomizationError::ReservedChord);
        }
        let mut draft = self.clone();
        draft
            .overrides
            .retain(|entry| entry.command != command || entry.scope != scope);
        draft.overrides.push(ShortcutBinding {
            command,
            scope,
            chord,
        });
        draft.validate(registry)?;
        *self = draft;
        Ok(())
    }
    pub fn clear(
        &mut self,
        id: &str,
        scope: ShortcutScope,
        registry: &CommandRegistry,
    ) -> Result<(), CustomizationError> {
        let command = registry
            .get(id)
            .ok_or(CustomizationError::UnknownCommand)?
            .id()
            .clone();
        let mut draft = self.clone();
        draft
            .overrides
            .retain(|entry| entry.command != command || entry.scope != scope);
        draft.overrides.push(ShortcutBinding {
            command,
            scope,
            chord: String::new(),
        });
        draft.validate(registry)?;
        *self = draft;
        Ok(())
    }
    pub fn validate(&self, registry: &CommandRegistry) -> Result<(), CustomizationError> {
        if self.overrides.len() > MAX_ENTRIES || self.export().len() > MAX_DOCUMENT {
            return Err(CustomizationError::InvalidDocument);
        }
        let bindings = self.bindings(registry);
        for (index, binding) in bindings.iter().enumerate() {
            if bindings[..index].iter().any(|other| {
                other.chord == binding.chord
                    && (other.scope == binding.scope
                        || other.scope == ShortcutScope::Global
                        || binding.scope == ShortcutScope::Global)
            }) {
                return Err(CustomizationError::Conflict(binding.command.clone()));
            }
        }
        Ok(())
    }
    pub fn resolve(
        &self,
        chord: &str,
        scope: ShortcutScope,
        registry: &CommandRegistry,
    ) -> Option<CommandId> {
        let chord = canonical_chord(chord).ok()?;
        self.bindings(registry)
            .into_iter()
            .find(|entry| {
                entry.chord == chord
                    && (entry.scope == scope
                        || (scope != ShortcutScope::Dialog && entry.scope == ShortcutScope::Global))
                    && registry.get(entry.command.as_str()).is_some()
            })
            .map(|entry| entry.command)
    }
    pub fn export(&self) -> String {
        let mut result = String::from("v1");
        for entry in &self.overrides {
            result.push_str(&format!(
                ";{}:{}:{}",
                scope_name(entry.scope),
                entry.command.as_str(),
                entry.chord
            ));
        }
        result
    }
    pub fn import(value: &str) -> Result<Self, CustomizationError> {
        if value == "default" {
            return Ok(Self::default());
        }
        let mut overrides = Vec::new();
        let mut seen = HashSet::new();
        for entry in document_entries(value)? {
            let parts: Vec<_> = entry.split(':').collect();
            if parts.len() != 3 {
                return Err(CustomizationError::InvalidDocument);
            }
            let scope = match parts[0] {
                "global" => ShortcutScope::Global,
                "browser" => ShortcutScope::Browser,
                "dialog" => ShortcutScope::Dialog,
                _ => return Err(CustomizationError::InvalidDocument),
            };
            let command =
                CommandId::new(parts[1]).map_err(|_| CustomizationError::InvalidDocument)?;
            if !seen.insert((command.clone(), scope)) {
                return Err(CustomizationError::DuplicateCommand);
            }
            let chord = if parts[2].is_empty() {
                String::new()
            } else {
                canonical_chord(parts[2])?
            };
            if reserved_chord(&chord) {
                return Err(CustomizationError::ReservedChord);
            }
            overrides.push(ShortcutBinding {
                command,
                scope,
                chord,
            });
        }
        let result = Self { overrides };
        result.validate(&CommandRegistry::built_in())?;
        Ok(result)
    }
}

pub fn scope_name(scope: ShortcutScope) -> &'static str {
    match scope {
        ShortcutScope::Global => "global",
        ShortcutScope::Browser => "browser",
        ShortcutScope::Dialog => "dialog",
    }
}

fn document_entries(value: &str) -> Result<Vec<&str>, CustomizationError> {
    if value.len() > MAX_DOCUMENT || value.contains(['\n', '\r']) {
        return Err(CustomizationError::InvalidDocument);
    }
    let mut parts = value.split(';');
    if parts.next() != Some("v1") {
        return Err(CustomizationError::InvalidDocument);
    }
    let entries: Vec<_> = parts.collect();
    if entries.len() > MAX_ENTRIES {
        return Err(CustomizationError::InvalidDocument);
    }
    Ok(entries)
}

pub fn canonical_chord(value: &str) -> Result<String, CustomizationError> {
    let lower = value.trim().to_ascii_lowercase();
    let mut parts: Vec<_> = lower.split(['+', '-']).collect();
    let key = parts.pop().ok_or(CustomizationError::InvalidChord)?;
    // The document uses semicolons as record separators.
    let key = if key == ";" { "semicolon" } else { key };
    if !(key.len() == 1 && (key.as_bytes()[0].is_ascii_alphanumeric() || ",./[]".contains(key))
        || matches!(
            key,
            "left"
                | "right"
                | "up"
                | "down"
                | "enter"
                | "delete"
                | "backspace"
                | "space"
                | "tab"
                | "escape"
                | "menu"
                | "semicolon"
        )
        || key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n)))
    {
        return Err(CustomizationError::InvalidChord);
    }
    let mut unique = HashSet::new();
    if parts
        .iter()
        .any(|part| !matches!(*part, "ctrl" | "alt" | "shift" | "super") || !unique.insert(*part))
    {
        return Err(CustomizationError::InvalidChord);
    }
    let mut result: Vec<_> = ["ctrl", "alt", "shift", "super"]
        .into_iter()
        .filter(|modifier| parts.contains(modifier))
        .collect();
    result.push(key);
    Ok(result.join("-"))
}

fn reserved_chord(chord: &str) -> bool {
    chord.split('-').any(|part| part == "super")
        || matches!(
            chord,
            "alt-f4"
                | "alt-tab"
                | "alt-shift-tab"
                | "ctrl-alt-delete"
                | "escape"
                | "tab"
                | "shift-tab"
        )
        || chord.starts_with("ctrl-alt-f")
}

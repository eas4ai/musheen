use super::NavigationError;
use musheen_core::StorePath;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NavigationHistory {
    entries: Vec<StorePath>,
    cursor: usize,
}

impl NavigationHistory {
    #[must_use]
    pub fn new(initial: StorePath) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
        }
    }

    #[must_use]
    pub fn current(&self) -> &StorePath {
        &self.entries[self.cursor]
    }

    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn navigate(&mut self, location: StorePath) {
        if self.current() == &location {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(location);
        self.cursor = self.entries.len() - 1;
    }

    pub fn back(&mut self) -> Option<&StorePath> {
        self.can_go_back().then(|| {
            self.cursor -= 1;
            self.current()
        })
    }

    pub fn forward(&mut self) -> Option<&StorePath> {
        self.can_go_forward().then(|| {
            self.cursor += 1;
            self.current()
        })
    }

    pub(super) fn validate(&self) -> Result<(), NavigationError> {
        if self.entries.is_empty() || self.cursor >= self.entries.len() {
            return Err(NavigationError::InvalidDocument(
                "history cursor does not name an entry".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn recover_missing(
        &mut self,
        exists: &impl Fn(&StorePath) -> bool,
        fallback: &StorePath,
    ) {
        let current = self.current().clone();
        self.entries.retain(|entry| exists(entry));
        if self.entries.is_empty() {
            self.entries.push(fallback.clone());
            self.cursor = 0;
        } else if let Some(cursor) = self.entries.iter().position(|entry| entry == &current) {
            self.cursor = cursor;
        } else {
            self.entries.push(fallback.clone());
            self.cursor = self.entries.len() - 1;
        }
    }
}

use musheen_core::ItemId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionMode {
    Replace,
    Add,
    Toggle,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SelectionModel {
    selected: Vec<ItemId>,
    anchor: Option<ItemId>,
}

impl SelectionModel {
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    pub fn apply(&mut self, ids: Vec<ItemId>, mode: SelectionMode) {
        if mode == SelectionMode::Replace {
            self.selected.clear();
        }
        for id in ids {
            match mode {
                SelectionMode::Replace | SelectionMode::Add => {
                    if !self.selected.contains(&id) {
                        self.selected.push(id.clone());
                    }
                }
                SelectionMode::Toggle => {
                    if let Some(index) = self.selected.iter().position(|selected| selected == &id) {
                        self.selected.remove(index);
                    } else {
                        self.selected.push(id.clone());
                    }
                }
            }
            self.anchor = Some(id);
        }
    }

    pub fn remove(&mut self, id: &ItemId) {
        self.selected.retain(|selected| selected != id);
        if self.anchor.as_ref() == Some(id) {
            self.anchor = self.selected.last().cloned();
        }
    }

    pub fn retain(&mut self, mut exists: impl FnMut(&ItemId) -> bool) {
        self.selected.retain(&mut exists);
        if self.anchor.as_ref().is_some_and(|id| !exists(id)) {
            self.anchor = self.selected.last().cloned();
        }
    }

    pub fn ids(&self) -> &[ItemId] {
        &self.selected
    }

    pub fn contains(&self, id: &ItemId) -> bool {
        self.selected.contains(id)
    }
}

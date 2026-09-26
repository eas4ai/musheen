use musheen_core::{
    SearchBatch, SearchCompletion, SearchQuery, SearchResult, SearchScopeError, StorePath,
};
use std::collections::VecDeque;

const MAX_RETAINED_SCOPE_ERRORS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchGeneration(u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchState {
    #[default]
    Idle,
    Running,
    Complete,
    Partial,
    RefineRequired,
    Cancelled,
    Error,
}

#[derive(Clone, Debug)]
pub struct SearchResultModel {
    scope: StorePath,
    query: SearchQuery,
    retention_limit: usize,
    result_limit: usize,
    generation: u64,
    retained: VecDeque<SearchResult>,
    errors: Vec<SearchScopeError>,
    dropped_errors: usize,
    total: usize,
    state: SearchState,
}

impl SearchResultModel {
    #[must_use]
    pub fn new(
        scope: StorePath,
        query: SearchQuery,
        retention_limit: usize,
        result_limit: usize,
    ) -> Self {
        Self {
            scope,
            query,
            retention_limit: retention_limit.max(1),
            result_limit: result_limit.max(1),
            generation: 0,
            retained: VecDeque::new(),
            errors: Vec::new(),
            dropped_errors: 0,
            total: 0,
            state: SearchState::Idle,
        }
    }

    pub fn begin(&mut self) -> SearchGeneration {
        self.generation = self.generation.wrapping_add(1);
        self.retained.clear();
        self.errors.clear();
        self.dropped_errors = 0;
        self.total = 0;
        self.state = SearchState::Running;
        SearchGeneration(self.generation)
    }

    pub fn apply(&mut self, generation: SearchGeneration, batch: SearchBatch) -> bool {
        if generation != SearchGeneration(self.generation) || self.state != SearchState::Running {
            return false;
        }
        let remaining = self.result_limit.saturating_sub(self.total);
        for result in batch.results().iter().take(remaining).cloned() {
            self.retained.push_back(result);
            self.total += 1;
        }
        while self.retained.len() > self.retention_limit {
            self.retained.pop_front();
        }
        for error in batch.errors() {
            if self.errors.len() < MAX_RETAINED_SCOPE_ERRORS {
                self.errors.push(error.clone());
            } else {
                self.dropped_errors = self.dropped_errors.saturating_add(1);
            }
        }
        self.state = if self.total >= self.result_limit
            || batch.completion() == SearchCompletion::RefineRequired
        {
            SearchState::RefineRequired
        } else {
            match batch.completion() {
                SearchCompletion::Running => SearchState::Running,
                SearchCompletion::Complete if self.errors.is_empty() => SearchState::Complete,
                SearchCompletion::Complete => SearchState::Partial,
                SearchCompletion::RefineRequired => SearchState::RefineRequired,
            }
        };
        true
    }

    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.state = SearchState::Cancelled;
    }

    pub fn fail(&mut self, generation: SearchGeneration) -> bool {
        if generation != SearchGeneration(self.generation) {
            return false;
        }
        self.state = SearchState::Error;
        true
    }

    #[must_use]
    pub const fn scope(&self) -> &StorePath {
        &self.scope
    }

    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }

    #[must_use]
    pub fn retained_results(&self) -> &VecDeque<SearchResult> {
        &self.retained
    }

    #[must_use]
    pub fn errors(&self) -> &[SearchScopeError] {
        &self.errors
    }

    #[must_use]
    pub const fn dropped_error_count(&self) -> usize {
        self.dropped_errors
    }

    #[must_use]
    pub const fn total_results(&self) -> usize {
        self.total
    }

    #[must_use]
    pub const fn state(&self) -> SearchState {
        self.state
    }
}

use musheen_core::{SearchQuery, StorePath};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    scope: StorePath,
    query: SearchQuery,
}

impl SearchRequest {
    #[must_use]
    pub const fn new(scope: StorePath, query: SearchQuery) -> Self {
        Self { scope, query }
    }

    #[must_use]
    pub const fn scope(&self) -> &StorePath {
        &self.scope
    }

    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }
}

use crate::{BoxFuture, CancellationToken, ItemKind, StoreError, StoreItem, StorePath};
use std::error::Error;
use std::fmt;
use std::str::FromStr;

pub const SEARCH_BATCH_RESULTS: usize = 256;
pub const SEARCH_BATCH_ERRORS: usize = 64;
pub const SEARCH_CHANNEL_RESULTS: usize = 2_048;
pub const SEARCH_QUERY_BYTES: usize = 16 * 1_024;
pub const SEARCH_QUERY_TERMS: usize = 64;
pub const SEARCH_RETAINED_RESULTS: usize = 4_096;
pub const SEARCH_RESULT_LIMIT: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCompletion {
    Running,
    Complete,
    RefineRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRange<T> {
    minimum: Option<(T, bool)>,
    maximum: Option<(T, bool)>,
}

impl<T: Ord> SearchRange<T> {
    #[must_use]
    pub fn matches(&self, value: &T) -> bool {
        let above_minimum = self.minimum.as_ref().is_none_or(|(minimum, inclusive)| {
            if *inclusive {
                value >= minimum
            } else {
                value > minimum
            }
        });
        let below_maximum = self.maximum.as_ref().is_none_or(|(maximum, inclusive)| {
            if *inclusive {
                value <= maximum
            } else {
                value < maximum
            }
        });
        above_minimum && below_maximum
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    name_terms: Vec<Box<str>>,
    content_terms: Vec<Box<str>>,
    glob: Option<Box<str>>,
    mime: Option<Box<str>>,
    size: Option<SearchRange<u64>>,
    modified: Option<SearchRange<i64>>,
    kind: Option<ItemKind>,
    include_hidden: bool,
    hidden_explicit: bool,
    follow_links: bool,
    follow_links_explicit: bool,
}

impl SearchQuery {
    pub fn parse(expression: &str) -> Result<Self, SearchQueryError> {
        if expression.len() > SEARCH_QUERY_BYTES {
            return Err(SearchQueryError::LimitExceeded {
                resource: "query bytes",
                value: expression.len(),
                maximum: SEARCH_QUERY_BYTES,
            });
        }
        let tokens = tokenize(expression)?;
        if tokens.is_empty() {
            return Err(SearchQueryError::Empty);
        }
        if tokens.len() > SEARCH_QUERY_TERMS {
            return Err(SearchQueryError::LimitExceeded {
                resource: "query terms",
                value: tokens.len(),
                maximum: SEARCH_QUERY_TERMS,
            });
        }
        let mut query = Self {
            name_terms: Vec::new(),
            content_terms: Vec::new(),
            glob: None,
            mime: None,
            size: None,
            modified: None,
            kind: None,
            include_hidden: false,
            hidden_explicit: false,
            follow_links: false,
            follow_links_explicit: false,
        };
        for token in tokens {
            let Some((key, value)) = token.split_once(':') else {
                query.name_terms.push(non_empty("name", &token)?);
                continue;
            };
            match key {
                "name" => query.name_terms.push(non_empty(key, value)?),
                "content" => query.content_terms.push(non_empty(key, value)?),
                "glob" => set_once(&mut query.glob, key, value)?,
                "mime" | "type" => set_once(&mut query.mime, "mime", value)?,
                "size" => set_range_once(&mut query.size, key, value)?,
                "modified" => set_range_once(&mut query.modified, key, value)?,
                "kind" => {
                    if query.kind.is_some() {
                        return Err(SearchQueryError::DuplicateCriterion(key.into()));
                    }
                    query.kind = Some(parse_kind(value)?);
                }
                "hidden" => {
                    reject_duplicate_flag(&mut query.hidden_explicit, key)?;
                    query.include_hidden = parse_bool(key, value)?;
                }
                "follow-links" => {
                    reject_duplicate_flag(&mut query.follow_links_explicit, key)?;
                    query.follow_links = parse_bool(key, value)?;
                }
                _ => return Err(SearchQueryError::UnknownCriterion(key.into())),
            }
        }
        Ok(query)
    }

    #[must_use]
    pub fn name_terms(&self) -> &[Box<str>] {
        &self.name_terms
    }

    #[must_use]
    pub fn content_terms(&self) -> &[Box<str>] {
        &self.content_terms
    }

    #[must_use]
    pub fn glob(&self) -> Option<&str> {
        self.glob.as_deref()
    }

    #[must_use]
    pub fn mime(&self) -> Option<&str> {
        self.mime.as_deref()
    }

    #[must_use]
    pub fn size(&self) -> Option<&SearchRange<u64>> {
        self.size.as_ref()
    }

    #[must_use]
    pub fn modified(&self) -> Option<&SearchRange<i64>> {
        self.modified.as_ref()
    }

    #[must_use]
    pub const fn kind(&self) -> Option<ItemKind> {
        self.kind
    }

    #[must_use]
    pub const fn include_hidden(&self) -> bool {
        self.include_hidden
    }

    #[must_use]
    pub const fn has_hidden_policy(&self) -> bool {
        self.hidden_explicit
    }

    #[must_use]
    pub fn with_default_hidden_policy(mut self, include_hidden: bool) -> Self {
        if !self.hidden_explicit {
            self.include_hidden = include_hidden;
            self.hidden_explicit = true;
        }
        self
    }

    #[must_use]
    pub const fn follow_links(&self) -> bool {
        self.follow_links
    }

    #[must_use]
    pub const fn has_follow_links_policy(&self) -> bool {
        self.follow_links_explicit
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchQueryError {
    Empty,
    UnterminatedQuote,
    UnknownCriterion(Box<str>),
    DuplicateCriterion(Box<str>),
    LimitExceeded {
        resource: &'static str,
        value: usize,
        maximum: usize,
    },
    InvalidValue {
        criterion: Box<str>,
        value: Box<str>,
    },
    UnsupportedCriterion(Box<str>),
}

impl fmt::Display for SearchQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("search query is empty"),
            Self::UnterminatedQuote => {
                formatter.write_str("search query has an unterminated quote")
            }
            Self::UnknownCriterion(criterion) => {
                write!(formatter, "unknown search criterion {criterion}")
            }
            Self::DuplicateCriterion(criterion) => write!(
                formatter,
                "search criterion {criterion} appears more than once"
            ),
            Self::LimitExceeded {
                resource,
                value,
                maximum,
            } => write!(
                formatter,
                "search {resource} limit exceeded: {value} is greater than {maximum}"
            ),
            Self::InvalidValue { criterion, value } => write!(
                formatter,
                "invalid value {value:?} for search criterion {criterion}"
            ),
            Self::UnsupportedCriterion(criterion) => {
                write!(formatter, "the active provider cannot evaluate {criterion}")
            }
        }
    }
}

impl Error for SearchQueryError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchCapabilities {
    pub name: bool,
    pub content: bool,
    pub glob: bool,
    pub mime: bool,
    pub size: bool,
    pub modified: bool,
    pub kind: bool,
    pub hidden: bool,
    pub follow_links: bool,
}

impl SearchCapabilities {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            name: false,
            content: false,
            glob: false,
            mime: false,
            size: false,
            modified: false,
            kind: false,
            hidden: false,
            follow_links: false,
        }
    }

    #[must_use]
    pub const fn names_only() -> Self {
        Self {
            name: true,
            ..Self::none()
        }
    }

    #[must_use]
    pub const fn all() -> Self {
        Self {
            name: true,
            content: true,
            glob: true,
            mime: true,
            size: true,
            modified: true,
            kind: true,
            hidden: true,
            follow_links: true,
        }
    }

    pub fn validate(self, query: &SearchQuery) -> Result<(), SearchQueryError> {
        for (required, supported, label) in [
            (!query.name_terms.is_empty(), self.name, "name"),
            (!query.content_terms.is_empty(), self.content, "content"),
            (query.glob.is_some(), self.glob, "glob"),
            (query.mime.is_some(), self.mime, "mime"),
            (query.size.is_some(), self.size, "size"),
            (query.modified.is_some(), self.modified, "modified"),
            (query.kind.is_some(), self.kind, "kind"),
            (query.has_hidden_policy(), self.hidden, "hidden"),
            (
                query.has_follow_links_policy(),
                self.follow_links,
                "follow-links",
            ),
        ] {
            if required && !supported {
                return Err(SearchQueryError::UnsupportedCriterion(label.into()));
            }
        }
        Ok(())
    }
}

impl Default for SearchCapabilities {
    fn default() -> Self {
        Self::none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    item: StoreItem,
    mime: Option<Box<str>>,
}

impl SearchResult {
    #[must_use]
    pub fn new(item: StoreItem, mime: Option<impl Into<Box<str>>>) -> Self {
        Self {
            item,
            mime: mime.map(Into::into),
        }
    }

    #[must_use]
    pub const fn item(&self) -> &StoreItem {
        &self.item
    }

    #[must_use]
    pub fn mime(&self) -> Option<&str> {
        self.mime.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchScopeError {
    path: StorePath,
    message: Box<str>,
    retryable: bool,
}

impl SearchScopeError {
    #[must_use]
    pub fn new(path: StorePath, message: impl Into<Box<str>>, retryable: bool) -> Self {
        Self {
            path,
            message: message.into(),
            retryable,
        }
    }

    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchBatch {
    results: Vec<SearchResult>,
    errors: Vec<SearchScopeError>,
    completion: SearchCompletion,
}

impl SearchBatch {
    pub fn new(
        results: Vec<SearchResult>,
        errors: Vec<SearchScopeError>,
        completion: SearchCompletion,
    ) -> Result<Self, StoreError> {
        if results.len() > SEARCH_BATCH_RESULTS {
            return Err(StoreError::ResourceLimit {
                resource: "search batch results",
                value: results.len(),
                maximum: SEARCH_BATCH_RESULTS,
            });
        }
        if errors.len() > SEARCH_BATCH_ERRORS {
            return Err(StoreError::ResourceLimit {
                resource: "search batch errors",
                value: errors.len(),
                maximum: SEARCH_BATCH_ERRORS,
            });
        }
        Ok(Self {
            results,
            errors,
            completion,
        })
    }

    pub fn running(
        results: Vec<SearchResult>,
        errors: Vec<SearchScopeError>,
    ) -> Result<Self, StoreError> {
        Self::new(results, errors, SearchCompletion::Running)
    }

    #[must_use]
    pub fn results(&self) -> &[SearchResult] {
        &self.results
    }

    #[must_use]
    pub fn errors(&self) -> &[SearchScopeError] {
        &self.errors
    }

    #[must_use]
    pub const fn completion(&self) -> SearchCompletion {
        self.completion
    }
}

pub trait SearchStream: Send {
    fn next_batch<'a>(
        &'a mut self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Option<SearchBatch>, StoreError>>;
}

fn tokenize(expression: &str) -> Result<Vec<String>, SearchQueryError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in expression.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if quoted {
        return Err(SearchQueryError::UnterminatedQuote);
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn non_empty(criterion: &str, value: &str) -> Result<Box<str>, SearchQueryError> {
    if value.is_empty() {
        Err(invalid_value(criterion, value))
    } else {
        Ok(value.into())
    }
}

fn set_once(
    target: &mut Option<Box<str>>,
    criterion: &str,
    value: &str,
) -> Result<(), SearchQueryError> {
    if target.is_some() {
        return Err(SearchQueryError::DuplicateCriterion(criterion.into()));
    }
    *target = Some(non_empty(criterion, value)?);
    Ok(())
}

fn set_range_once<T>(
    target: &mut Option<SearchRange<T>>,
    criterion: &str,
    value: &str,
) -> Result<(), SearchQueryError>
where
    T: FromStr + Ord,
{
    if target.is_some() {
        return Err(SearchQueryError::DuplicateCriterion(criterion.into()));
    }
    *target = Some(parse_range(criterion, value)?);
    Ok(())
}

fn parse_range<T: FromStr + Ord>(
    criterion: &str,
    value: &str,
) -> Result<SearchRange<T>, SearchQueryError> {
    let parse = |raw: &str| {
        raw.parse::<T>()
            .map_err(|_| invalid_value(criterion, value))
    };
    if let Some(raw) = value.strip_prefix(">=") {
        return Ok(SearchRange {
            minimum: Some((parse(raw)?, true)),
            maximum: None,
        });
    }
    if let Some(raw) = value.strip_prefix('>') {
        return Ok(SearchRange {
            minimum: Some((parse(raw)?, false)),
            maximum: None,
        });
    }
    if let Some(raw) = value.strip_prefix("<=") {
        return Ok(SearchRange {
            minimum: None,
            maximum: Some((parse(raw)?, true)),
        });
    }
    if let Some(raw) = value.strip_prefix('<') {
        return Ok(SearchRange {
            minimum: None,
            maximum: Some((parse(raw)?, false)),
        });
    }
    if let Some((minimum, maximum)) = value.split_once("..") {
        if minimum.is_empty() && maximum.is_empty() {
            return Err(invalid_value(criterion, value));
        }
        let minimum = (!minimum.is_empty()).then(|| parse(minimum)).transpose()?;
        let maximum = (!maximum.is_empty()).then(|| parse(maximum)).transpose()?;
        if minimum
            .as_ref()
            .zip(maximum.as_ref())
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(invalid_value(criterion, value));
        }
        return Ok(SearchRange {
            minimum: minimum.map(|value| (value, true)),
            maximum: maximum.map(|value| (value, true)),
        });
    }
    let minimum = parse(value)?;
    let maximum = parse(value)?;
    Ok(SearchRange {
        minimum: Some((minimum, true)),
        maximum: Some((maximum, true)),
    })
}

fn reject_duplicate_flag(seen: &mut bool, criterion: &str) -> Result<(), SearchQueryError> {
    if *seen {
        return Err(SearchQueryError::DuplicateCriterion(criterion.into()));
    }
    *seen = true;
    Ok(())
}

fn parse_bool(criterion: &str, value: &str) -> Result<bool, SearchQueryError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid_value(criterion, value)),
    }
}

fn parse_kind(value: &str) -> Result<ItemKind, SearchQueryError> {
    match value {
        "file" => Ok(ItemKind::RegularFile),
        "directory" | "folder" => Ok(ItemKind::Directory),
        "link" | "symlink" => Ok(ItemKind::SymbolicLink),
        "other" => Ok(ItemKind::Other),
        _ => Err(invalid_value("kind", value)),
    }
}

fn invalid_value(criterion: &str, value: &str) -> SearchQueryError {
    SearchQueryError::InvalidValue {
        criterion: criterion.into(),
        value: value.into(),
    }
}

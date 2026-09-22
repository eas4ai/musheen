use super::ArchiveError;

/// A normalized byte path owned by one archive provider.
///
/// Both slash styles are separators. This keeps a name that is harmless on
/// Linux from becoming an absolute or parent path on another platform.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchivePath(Box<[u8]>);

impl ArchivePath {
    pub const MAX_BYTES: usize = 4_096;

    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, ArchiveError> {
        Self::with_limit(value.as_ref(), Self::MAX_BYTES)
    }

    pub(crate) fn with_limit(value: &[u8], maximum: usize) -> Result<Self, ArchiveError> {
        if value.len() > maximum {
            return Err(ArchiveError::LimitExceeded {
                resource: "path bytes",
                value: value.len(),
                maximum,
            });
        }
        if value.contains(&0) {
            return Err(ArchiveError::UnsafePath("archive paths cannot contain NUL"));
        }
        if value
            .first()
            .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
            || value
                .get(0..2)
                .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':')
        {
            return Err(ArchiveError::UnsafePath(
                "archive paths must be relative to the archive root",
            ));
        }

        let mut normalized = Vec::with_capacity(value.len().min(maximum));
        for component in value.split(|byte| matches!(byte, b'/' | b'\\')) {
            if component.is_empty() || component == b"." {
                continue;
            }
            if component == b".." {
                return Err(ArchiveError::UnsafePath(
                    "archive paths cannot contain a parent component",
                ));
            }
            if normalized.is_empty()
                && component.len() >= 2
                && component[0].is_ascii_alphabetic()
                && component[1] == b':'
            {
                return Err(ArchiveError::UnsafePath(
                    "archive paths must not contain a Windows drive prefix",
                ));
            }
            let separator = usize::from(!normalized.is_empty());
            let next_length = normalized
                .len()
                .checked_add(separator)
                .and_then(|length| length.checked_add(component.len()))
                .ok_or(ArchiveError::LimitExceeded {
                    resource: "path bytes",
                    value: usize::MAX,
                    maximum,
                })?;
            if next_length > maximum {
                return Err(ArchiveError::LimitExceeded {
                    resource: "path bytes",
                    value: next_length,
                    maximum,
                });
            }
            if separator == 1 {
                normalized.push(b'/');
            }
            normalized.extend_from_slice(component);
        }
        Ok(Self(normalized.into_boxed_slice()))
    }

    pub(crate) fn root() -> Self {
        Self(Box::default())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn parent(&self) -> Option<Self> {
        let split = self.0.iter().rposition(|byte| *byte == b'/');
        match split {
            Some(index) => Some(Self(self.0[..index].to_vec().into_boxed_slice())),
            None if !self.is_root() => Some(Self::root()),
            None => None,
        }
    }

    pub(crate) fn file_name(&self) -> &[u8] {
        self.0
            .iter()
            .rposition(|byte| *byte == b'/')
            .map_or(&self.0, |index| &self.0[index + 1..])
    }

    pub(crate) fn ancestors(&self) -> impl Iterator<Item = Self> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'/')
            .map(|(index, _)| Self(self.0[..index].to_vec().into_boxed_slice()))
    }
}

use std::error::Error;
use std::fmt;

/// An invalid portable-domain value rejected at a trust boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreError {
    InvalidProviderId,
    InvalidItemId,
    InvalidProviderKey,
    InvalidCapabilityReason,
    InvalidResourceLimit {
        field: &'static str,
        value: usize,
        maximum: usize,
    },
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderId => formatter.write_str(
                "provider IDs must be 1-64 lowercase ASCII letters, digits, dots, hyphens, or underscores",
            ),
            Self::InvalidItemId => {
                formatter.write_str("item IDs must contain between 1 and 4,096 bytes")
            }
            Self::InvalidProviderKey => {
                formatter.write_str("provider path keys must contain between 1 and 4,096 bytes")
            }
            Self::InvalidCapabilityReason => {
                formatter.write_str("capability reasons must contain visible text")
            }
            Self::InvalidResourceLimit {
                field,
                value,
                maximum,
            } => write!(
                formatter,
                "resource limit {field} must be between 1 and {maximum}, got {value}"
            ),
        }
    }
}

impl Error for CoreError {}

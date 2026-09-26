use crate::ByteWriter;
use zeroize::Zeroizing;

/// A password used for password protected, encrypted files.
///
/// Use [`Password::empty()`] to create an empty password when no
/// password is used.
///
/// You can convert strings easily into password using the Into/From traits:
///
/// ```rust
/// use sevenz_rust2::Password;
///
/// let password: Password = "a password string".into();
/// ```
#[derive(Default, Clone, PartialEq)]
pub struct Password(Zeroizing<Vec<u8>>);

impl std::fmt::Debug for Password {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Password([REDACTED])")
    }
}

impl Password {
    /// Creates a new [`Password`] from the given password string.
    ///
    /// Internally a password string is encoded as UTF-16.
    pub fn new(password: &str) -> Self {
        Self::from(password)
    }

    /// Creates a new [`Password`] from the given raw bytes.
    pub fn from_raw(bytes: &[u8]) -> Self {
        Self(Zeroizing::new(bytes.to_vec()))
    }

    /// Creates an empty password.
    pub fn empty() -> Self {
        Self(Default::default())
    }

    /// Returns the byte representation of the password.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Returns `true` if the password is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for Password {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<&str> for Password {
    fn from(s: &str) -> Self {
        let mut result = Vec::with_capacity(s.len() * 2);
        let utf16 = s.encode_utf16();
        for u in utf16 {
            let _ = result.write_u16(u);
        }
        Self(Zeroizing::new(result))
    }
}

#[cfg(test)]
mod tests {
    use super::Password;

    #[test]
    fn debug_output_redacts_password_bytes() {
        let password = Password::new("do not log me");
        assert_eq!(format!("{password:?}"), "Password([REDACTED])");
    }
}

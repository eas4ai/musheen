use serde::{Deserialize, Serialize};

/// A bounded, versioned palette of semantic GPUI Kit overrides. Icon identity,
/// motion and desktop accessibility preferences are never part of a palette.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDocument {
    pub version: u32,
    pub tokens: ThemeTokens,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeTokens {
    pub background: String,
    pub foreground: String,
    pub primary: String,
    pub primary_foreground: String,
    pub secondary: String,
    pub secondary_foreground: String,
    pub muted_foreground: String,
    pub border: String,
    pub ring: String,
    pub danger: String,
    pub danger_foreground: String,
    pub warning: String,
    pub warning_foreground: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeError {
    InvalidDocument,
    UnsupportedVersion,
    InvalidColor,
    InsufficientContrast,
}

impl ThemeError {
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::InvalidDocument => "theme-error-document",
            Self::UnsupportedVersion => "theme-error-version",
            Self::InvalidColor => "theme-error-color",
            Self::InsufficientContrast => "theme-error-contrast",
        }
    }
}

impl ThemeDocument {
    pub fn import(text: &str) -> Result<Self, ThemeError> {
        if text.len() > 8192 {
            return Err(ThemeError::InvalidDocument);
        }
        let document: Self = serde_json::from_str(text).map_err(|_| ThemeError::InvalidDocument)?;
        document.validate()?;
        Ok(document)
    }

    pub fn export(&self) -> String {
        serde_json::to_string(self).expect("theme contains only strings and an integer")
    }

    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.version != 1 {
            return Err(ThemeError::UnsupportedVersion);
        }
        let t = &self.tokens;
        for value in [
            &t.background,
            &t.foreground,
            &t.primary,
            &t.primary_foreground,
            &t.secondary,
            &t.secondary_foreground,
            &t.muted_foreground,
            &t.border,
            &t.ring,
            &t.danger,
            &t.danger_foreground,
            &t.warning,
            &t.warning_foreground,
        ] {
            parse_color(value)?;
        }
        for (foreground, background, minimum) in [
            (&t.foreground, &t.background, 4.5),
            (&t.primary_foreground, &t.primary, 4.5),
            (&t.primary, &t.background, 3.0),
            (&t.secondary_foreground, &t.secondary, 4.5),
            (&t.muted_foreground, &t.background, 4.5),
            (&t.foreground, &t.secondary, 4.5),
            (&t.muted_foreground, &t.secondary, 4.5),
            (&t.danger_foreground, &t.danger, 4.5),
            (&t.warning_foreground, &t.warning, 4.5),
            (&t.danger, &t.background, 4.5),
            (&t.warning, &t.background, 4.5),
            (&t.ring, &t.background, 3.0),
            (&t.ring, &t.secondary, 3.0),
            (&t.ring, &t.primary, 3.0),
            (&t.ring, &t.danger, 3.0),
            (&t.ring, &t.warning, 3.0),
            (&t.border, &t.background, 3.0),
            (&t.border, &t.secondary, 3.0),
        ] {
            if contrast_ratio(foreground, background)? < minimum {
                return Err(ThemeError::InsufficientContrast);
            }
        }
        Ok(())
    }

    /// Accessible editable starting point; the current native palette remains
    /// untouched until the user explicitly previews it.
    pub fn starter() -> Self {
        Self {
            version: 1,
            tokens: ThemeTokens {
                background: "#ffffff".into(),
                foreground: "#111111".into(),
                primary: "#222222".into(),
                primary_foreground: "#ffffff".into(),
                secondary: "#eeeeee".into(),
                secondary_foreground: "#111111".into(),
                muted_foreground: "#444444".into(),
                border: "#555555".into(),
                ring: "#777777".into(),
                danger: "#330000".into(),
                danger_foreground: "#ffffff".into(),
                warning: "#332200".into(),
                warning_foreground: "#ffffff".into(),
            },
        }
    }
}

pub fn validate_setting(value: &str) -> Result<(), ThemeError> {
    if value == "native" {
        Ok(())
    } else {
        ThemeDocument::import(value).map(|_| ())
    }
}

pub fn parse_color(value: &str) -> Result<u32, ThemeError> {
    let hex = value.strip_prefix('#').ok_or(ThemeError::InvalidColor)?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ThemeError::InvalidColor);
    }
    u32::from_str_radix(hex, 16).map_err(|_| ThemeError::InvalidColor)
}

pub fn contrast_ratio(first: &str, second: &str) -> Result<f64, ThemeError> {
    fn luminance(rgb: u32) -> f64 {
        let linear = |shift: u32| {
            let channel = f64::from((rgb >> shift) & 255_u32) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
    }
    let first = luminance(parse_color(first)?);
    let second = luminance(parse_color(second)?);
    Ok((first.max(second) + 0.05) / (first.min(second) + 0.05))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppearanceMode {
    Light,
    Dark,
    HighContrast,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionPolicy {
    Standard,
    Reduced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemeProfile {
    mode: AppearanceMode,
    motion: MotionPolicy,
}

impl ThemeProfile {
    #[must_use]
    pub const fn new(mode: AppearanceMode, reduce_motion: bool) -> Self {
        Self {
            mode,
            motion: if reduce_motion {
                MotionPolicy::Reduced
            } else {
                MotionPolicy::Standard
            },
        }
    }

    /// Captures the active native theme and accessibility preferences at render
    /// time. High contrast intentionally takes precedence over color scheme.
    #[must_use]
    pub const fn from_active_native(
        is_dark: bool,
        high_contrast: bool,
        reduce_motion: bool,
    ) -> Self {
        Self::new(
            if high_contrast {
                AppearanceMode::HighContrast
            } else if is_dark {
                AppearanceMode::Dark
            } else {
                AppearanceMode::Light
            },
            reduce_motion,
        )
    }

    #[must_use]
    pub const fn surface(self) -> &'static str {
        match self.mode {
            AppearanceMode::Light => "system-surface-light",
            AppearanceMode::Dark => "system-surface-dark",
            AppearanceMode::HighContrast => "system-surface-high-contrast",
        }
    }

    #[must_use]
    pub const fn has_strong_boundaries(self) -> bool {
        matches!(self.mode, AppearanceMode::HighContrast)
    }

    #[must_use]
    pub const fn motion(self) -> MotionPolicy {
        self.motion
    }
}

#[cfg(test)]
mod tests {
    use super::{AppearanceMode, MotionPolicy, ThemeProfile};

    #[test]
    fn active_native_profile_prioritizes_high_contrast_and_motion_preferences() {
        let profile = ThemeProfile::from_active_native(true, true, true);

        assert_eq!(profile.surface(), "system-surface-high-contrast");
        assert!(profile.has_strong_boundaries());
        assert_eq!(profile.motion(), MotionPolicy::Reduced);
    }

    #[test]
    fn active_native_profile_uses_color_scheme_without_high_contrast() {
        let profile = ThemeProfile::from_active_native(true, false, false);

        assert_eq!(profile, ThemeProfile::new(AppearanceMode::Dark, false));
    }
}

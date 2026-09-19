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

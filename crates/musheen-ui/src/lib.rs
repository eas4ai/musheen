//! Native-themed GPUI application shell.

mod app;
mod directory;
mod icons;
pub mod navigation;
pub mod search;
mod shell;
pub mod sidebar;
mod status_bar;
mod theme;
mod toolbar;
pub mod views;

pub use app::run;
pub use directory::{
    ApplyPageResult, DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory,
};
pub use icons::{ContentIdentity, LucideIcon, freedesktop_icon_name, lucide_icon};
pub use shell::{FocusTarget, SemanticRegion, ShellModel};
pub use theme::{AppearanceMode, MotionPolicy, ThemeProfile};

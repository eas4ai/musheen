//! Native-themed GPUI application shell.

mod app;
pub mod dialogs;
mod directory;
mod icons;
mod info_pane;
pub mod navigation;
pub mod search;
mod shell;
pub mod sidebar;
mod status_bar;
mod theme;
mod toolbar;
pub mod views;

pub use app::run;
pub use dialogs::*;
pub use directory::{
    ApplyPageResult, DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory,
};
pub use icons::{ContentIdentity, LucideIcon, freedesktop_icon_name, lucide_icon};
pub use info_pane::*;
pub use shell::{FocusTarget, SemanticRegion, ShellModel};
pub use theme::{AppearanceMode, MotionPolicy, ThemeProfile};

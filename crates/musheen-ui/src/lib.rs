//! Native-themed GPUI application shell.

mod app;
pub mod dialogs;
mod directory;
mod i18n;
mod icons;
mod info_pane;
pub mod menus;
pub mod navigation;
mod operations;
mod providers;
pub mod search;
pub mod settings;
mod shell;
pub mod sidebar;
mod status_bar;
mod status_center;
pub mod theme;
mod toolbar;
pub mod views;

pub use app::run;
pub use dialogs::*;
pub use directory::{
    ApplyPageResult, DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory,
};
pub use i18n::{Catalog, CatalogError, Locale};
pub use icons::{
    ApplicationIdentity, ContentIdentity, LucideIcon, freedesktop_icon_name, lucide_icon,
    lucide_icon_or_fallback,
};
pub use info_pane::*;
pub use menus::*;
pub use operations::*;
pub use shell::{FocusTarget, SemanticRegion, ShellModel};
pub use status_center::{
    ConfirmationDefault, DestructiveConfirmation, EmptyTrashChallenge, EmptyTrashConfirmation,
    OperationFailure, OperationStatus, OperationStatusEntry, RecoveryAction, StatusCenterError,
    StatusCenterModel, TrashItem, TrashSurfaceModel,
};
pub use theme::{AppearanceMode, MotionPolicy, ThemeProfile};

//! Native-themed GPUI application shell.

mod app;
mod date_time;
pub mod dialogs;
mod directory;
mod elevated_browser;
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
pub mod terminal;
pub mod theme;
pub mod toolbar;
pub mod views;

pub use app::{installed_static_command_actions, installed_static_shortcut_bindings, run};
pub use dialogs::*;
pub use directory::{
    ApplyPageResult, DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory,
};
pub use elevated_browser::{
    ElevatedBrowser, ElevatedChrome, PrivilegeBackend, RootedFilesystemStore,
    SystemPrivilegeBackend,
};
pub use i18n::{Catalog, CatalogError, Locale};
pub use icons::{
    ApplicationIdentity, ContentIdentity, LucideIcon, freedesktop_icon_name, lucide_icon,
    lucide_icon_or_fallback,
};
pub use info_pane::*;
pub use menus::*;
pub use navigation::OmnibarMode;
pub use operations::*;
pub use shell::{FocusTarget, SemanticRegion, ShellModel};
pub use status_center::{
    ConfirmationDefault, DestructiveConfirmation, EmptyTrashChallenge, EmptyTrashConfirmation,
    OperationFailure, OperationStatus, OperationStatusEntry, RecoveryAction, StatusCenterError,
    StatusCenterModel, TrashItem, TrashSurfaceModel,
};
pub use terminal::*;
pub use theme::{AppearanceMode, MotionPolicy, ThemeProfile};

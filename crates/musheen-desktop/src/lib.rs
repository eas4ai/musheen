//! Linux desktop-service adapters, including XDG MIME-application integration.

mod accounts;
mod apps;
pub mod archive;
mod catalog;
mod checksum;
mod clipboard;
mod conflict_journal;
mod custom_action;
mod file_manager1;
mod maintenance;
mod mime;
mod notifications;
mod operation_journal;
mod permissions;
mod portals;
mod preview;
pub mod privilege;
mod properties;
pub mod remote;
mod secrets;
mod session;
mod settings;
mod status;
mod terminal;
mod thumbnail;
mod updates;
mod volumes;

pub use accounts::{
    all_groups, all_users, current_user_groups, effective_user, group_name, user_name,
};
pub use apps::*;
pub use archive::*;
pub use catalog::*;
pub use checksum::*;
pub use clipboard::*;
pub use conflict_journal::*;
pub use custom_action::*;
pub use file_manager1::*;
pub use maintenance::*;
pub use mime::*;
pub use notifications::*;
pub use operation_journal::*;
pub use permissions::*;
pub use portals::*;
pub use preview::*;
pub use privilege::*;
pub use properties::*;
pub use remote::*;
pub use secrets::*;
pub use session::*;
pub use settings::*;
pub use status::*;
pub use terminal::*;
pub use thumbnail::*;
pub use updates::*;
pub use volumes::*;

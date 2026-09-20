//! Linux desktop-service adapters.

mod catalog;
mod checksum;
mod clipboard;
mod conflict_journal;
mod custom_action;
mod mime;
mod operation_journal;
mod permissions;
mod preview;
mod properties;
mod session;
mod settings;
mod status;
mod thumbnail;

pub use catalog::*;
pub use checksum::*;
pub use clipboard::*;
pub use conflict_journal::*;
pub use custom_action::*;
pub use mime::*;
pub use operation_journal::*;
pub use permissions::*;
pub use preview::*;
pub use properties::*;
pub use session::*;
pub use settings::*;
pub use status::*;
pub use thumbnail::*;

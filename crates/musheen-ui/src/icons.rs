#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LucideIcon {
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    RefreshCw,
    Search,
    TextCursorInput,
    List,
    ListChecks,
    Grid2x2,
    Settings,
    X,
    Home,
    Folder,
    HardDrive,
    Network,
    Trash,
    Info,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentIdentity {
    Directory,
    Mime(Box<str>),
    Application(Box<str>),
    SymbolicLink,
    GenericFile,
}

impl ContentIdentity {
    #[must_use]
    pub const fn directory() -> Self {
        Self::Directory
    }

    #[must_use]
    pub fn mime(value: impl Into<Box<str>>) -> Self {
        Self::Mime(value.into().replace('/', "-").into())
    }

    #[must_use]
    pub fn application(value: impl Into<Box<str>>) -> Self {
        Self::Application(value.into())
    }

    #[must_use]
    pub const fn symbolic_link() -> Self {
        Self::SymbolicLink
    }
}

#[must_use]
pub fn lucide_icon(icon_key: &str) -> Option<LucideIcon> {
    Some(match icon_key {
        "arrow-left" => LucideIcon::ArrowLeft,
        "arrow-right" => LucideIcon::ArrowRight,
        "arrow-up" => LucideIcon::ArrowUp,
        "refresh-cw" => LucideIcon::RefreshCw,
        "search" => LucideIcon::Search,
        "text-cursor-input" => LucideIcon::TextCursorInput,
        "list" => LucideIcon::List,
        "list-checks" => LucideIcon::ListChecks,
        "grid-2x2" => LucideIcon::Grid2x2,
        "settings" => LucideIcon::Settings,
        "x" => LucideIcon::X,
        "home" => LucideIcon::Home,
        "folder" => LucideIcon::Folder,
        "hard-drive" => LucideIcon::HardDrive,
        "network" => LucideIcon::Network,
        "trash" => LucideIcon::Trash,
        "info" => LucideIcon::Info,
        _ => return None,
    })
}

#[must_use]
pub fn freedesktop_icon_name(identity: &ContentIdentity) -> &str {
    match identity {
        ContentIdentity::Directory => "folder",
        ContentIdentity::Mime(mime) => mime,
        ContentIdentity::Application(icon) => icon,
        ContentIdentity::SymbolicLink => "emblem-symbolic-link",
        ContentIdentity::GenericFile => "text-x-generic",
    }
}

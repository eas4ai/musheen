#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidebarPlace {
    pub label: &'static str,
    pub icon_key: &'static str,
}

pub const PLACES: [SidebarPlace; 5] = [
    SidebarPlace {
        label: "Home",
        icon_key: "home",
    },
    SidebarPlace {
        label: "Desktop",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Documents",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Downloads",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Trash",
        icon_key: "trash",
    },
];

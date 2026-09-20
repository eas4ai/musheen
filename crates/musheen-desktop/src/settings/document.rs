use super::{SettingsDocument, SettingsError};
use musheen_core::ResourceLimitConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum SettingsPage {
    General,
    Appearance,
    Layout,
    Files,
    Search,
    Operations,
    Integrations,
    Shortcuts,
    Advanced,
}

impl SettingsPage {
    pub const ALL: [Self; 9] = [
        Self::General,
        Self::Appearance,
        Self::Layout,
        Self::Files,
        Self::Search,
        Self::Operations,
        Self::Integrations,
        Self::Shortcuts,
        Self::Advanced,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "settings-page-general",
            Self::Appearance => "settings-page-appearance",
            Self::Layout => "settings-page-layout",
            Self::Files => "settings-page-files",
            Self::Search => "settings-page-search",
            Self::Operations => "settings-page-operations",
            Self::Integrations => "settings-page-integrations",
            Self::Shortcuts => "settings-page-shortcuts",
            Self::Advanced => "settings-page-advanced",
        }
    }
}

/// An owner must enable its controls only after installing the corresponding backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsFeature {
    None,
    Terminal,
    Remote,
    Privilege,
    Catalog,
    Customization,
    Execution,
    Search,
    Operations,
    Archive,
    Desktop,
    Diagnostics,
    Updates,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingKind {
    Boolean,
    Choice(&'static [&'static str]),
    Integer { maximum: usize, units: &'static str },
    CredentialReference,
    Toolbar,
    Shortcuts,
    Theme,
    CustomActions,
}

#[derive(Clone, Copy, Debug)]
pub struct SettingSpec {
    pub key: &'static str,
    pub page: SettingsPage,
    pub label: &'static str,
    pub group: &'static str,
    pub aliases: &'static str,
    pub default: &'static str,
    pub kind: SettingKind,
    pub restart_required: bool,
    pub feature: SettingsFeature,
}

impl SettingSpec {
    pub const fn maximum(&self) -> Option<usize> {
        match self.kind {
            SettingKind::Integer { maximum, .. } => Some(maximum),
            _ => None,
        }
    }
    pub const fn units(&self) -> Option<&'static str> {
        match self.kind {
            SettingKind::Integer { units, .. } => Some(units),
            _ => None,
        }
    }
}

pub fn settings_schema() -> &'static [SettingSpec] {
    SETTINGS
}

const SETTINGS: &[SettingSpec] = &[
    SettingSpec {
        key: "general.startup",
        page: SettingsPage::General,
        label: "setting-general-startup",
        group: "settings-group-startup",
        aliases: "start home session",
        default: "last-session",
        kind: SettingKind::Choice(&["home", "last-session"]),
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "general.click",
        page: SettingsPage::General,
        label: "setting-general-click",
        group: "settings-group-startup",
        aliases: "click activation",
        default: "double",
        kind: SettingKind::Choice(&["single", "double"]),
        restart_required: true,
        feature: SettingsFeature::Execution,
    },
    SettingSpec {
        key: "general.restore_session",
        page: SettingsPage::General,
        label: "setting-general-restore-session",
        group: "settings-group-startup",
        aliases: "tabs restore",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "general.record_history",
        page: SettingsPage::General,
        label: "setting-general-record-history",
        group: "settings-group-privacy",
        aliases: "privacy recent history",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Catalog,
    },
    SettingSpec {
        key: "appearance.mode",
        page: SettingsPage::Appearance,
        label: "setting-appearance-mode",
        group: "settings-group-theme",
        aliases: "native theme contrast",
        default: "system",
        kind: SettingKind::Choice(&["system", "light", "dark", "high-contrast"]),
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "appearance.reduce_motion",
        page: SettingsPage::Appearance,
        label: "setting-appearance-reduce-motion",
        group: "settings-group-theme",
        aliases: "animation accessibility",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "appearance.theme",
        page: SettingsPage::Appearance,
        label: "setting-appearance-theme",
        group: "settings-group-theme",
        aliases: "theme tokens import colors",
        default: "native",
        kind: SettingKind::Theme,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "appearance.density",
        page: SettingsPage::Appearance,
        label: "setting-appearance-density",
        group: "settings-group-theme",
        aliases: "spacing",
        default: "comfortable",
        kind: SettingKind::Choice(&["comfortable", "compact"]),
        restart_required: true,
        feature: SettingsFeature::Customization,
    },
    SettingSpec {
        key: "appearance.icons",
        page: SettingsPage::Appearance,
        label: "setting-appearance-icons",
        group: "settings-group-theme",
        aliases: "icons",
        default: "normal",
        kind: SettingKind::Choice(&["normal", "large"]),
        restart_required: true,
        feature: SettingsFeature::Customization,
    },
    SettingSpec {
        key: "layout.view",
        page: SettingsPage::Layout,
        label: "setting-layout-view",
        group: "settings-group-layout",
        aliases: "view layout",
        default: "details",
        kind: SettingKind::Choice(&["details", "list", "cards", "grid", "columns", "adaptive"]),
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "layout.sidebar",
        page: SettingsPage::Layout,
        label: "setting-layout-sidebar",
        group: "settings-group-layout",
        aliases: "places",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "layout.info_pane",
        page: SettingsPage::Layout,
        label: "setting-layout-info-pane",
        group: "settings-group-layout",
        aliases: "preview details",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "layout.panes",
        page: SettingsPage::Layout,
        label: "setting-layout-panes",
        group: "settings-group-layout",
        aliases: "split",
        default: "one",
        kind: SettingKind::Choice(&["one", "two"]),
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "layout.terminal",
        page: SettingsPage::Layout,
        label: "setting-layout-terminal",
        group: "settings-group-layout",
        aliases: "terminal",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Terminal,
    },
    SettingSpec {
        key: "layout.toolbar",
        page: SettingsPage::Layout,
        label: "setting-layout-toolbar",
        group: "settings-group-layout",
        aliases: "toolbar commands",
        default: "default",
        kind: SettingKind::Toolbar,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "files.hidden",
        page: SettingsPage::Files,
        label: "setting-files-hidden",
        group: "settings-group-files",
        aliases: "dotfiles hidden",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "files.directories_first",
        page: SettingsPage::Files,
        label: "setting-files-directories-first",
        group: "settings-group-files",
        aliases: "folders sorting",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "files.executable",
        page: SettingsPage::Files,
        label: "setting-files-executable",
        group: "settings-group-files",
        aliases: "executable",
        default: "ask",
        kind: SettingKind::Choice(&["ask", "open", "run"]),
        restart_required: true,
        feature: SettingsFeature::Execution,
    },
    SettingSpec {
        key: "files.folder_preferences",
        page: SettingsPage::Files,
        label: "setting-files-folder-preferences",
        group: "settings-group-files",
        aliases: "directory reset",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Catalog,
    },
    SettingSpec {
        key: "search.scope",
        page: SettingsPage::Search,
        label: "setting-search-scope",
        group: "settings-group-search",
        aliases: "recursive scope",
        default: "recursive",
        kind: SettingKind::Choice(&["recursive", "current-directory"]),
        restart_required: true,
        feature: SettingsFeature::Search,
    },
    SettingSpec {
        key: "search.symlinks",
        page: SettingsPage::Search,
        label: "setting-search-symlinks",
        group: "settings-group-search",
        aliases: "symlinks",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Search,
    },
    SettingSpec {
        key: "search.previews",
        page: SettingsPage::Search,
        label: "setting-search-previews",
        group: "settings-group-preview",
        aliases: "preview",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Search,
    },
    SettingSpec {
        key: "search.thumbnails",
        page: SettingsPage::Search,
        label: "setting-search-thumbnails",
        group: "settings-group-preview",
        aliases: "images",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Search,
    },
    SettingSpec {
        key: "operations.conflict",
        page: SettingsPage::Operations,
        label: "setting-operations-conflict",
        group: "settings-group-operations",
        aliases: "replace conflict",
        default: "ask",
        kind: SettingKind::Choice(&["ask", "skip", "keep-both"]),
        restart_required: true,
        feature: SettingsFeature::Operations,
    },
    SettingSpec {
        key: "operations.confirm_delete",
        page: SettingsPage::Operations,
        label: "setting-operations-confirm-delete",
        group: "settings-group-operations",
        aliases: "delete confirmation",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Operations,
    },
    SettingSpec {
        key: "operations.archive_limit",
        page: SettingsPage::Operations,
        label: "setting-operations-archive-limit",
        group: "settings-group-operations",
        aliases: "archive bomb",
        default: "4096",
        kind: SettingKind::Integer {
            maximum: 65536,
            units: "settings-unit-mib",
        },
        restart_required: true,
        feature: SettingsFeature::Archive,
    },
    SettingSpec {
        key: "terminal.program",
        page: SettingsPage::Integrations,
        label: "setting-terminal-program",
        group: "settings-group-terminal",
        aliases: "shell console",
        default: "system",
        kind: SettingKind::Choice(&["system", "embedded"]),
        restart_required: true,
        feature: SettingsFeature::Terminal,
    },
    SettingSpec {
        key: "remote.credential",
        page: SettingsPage::Integrations,
        label: "setting-remote-credential",
        group: "settings-group-remote",
        aliases: "remote password secret",
        default: "",
        kind: SettingKind::CredentialReference,
        restart_required: true,
        feature: SettingsFeature::Remote,
    },
    SettingSpec {
        key: "integrations.privilege",
        page: SettingsPage::Integrations,
        label: "setting-integrations-privilege",
        group: "settings-group-remote",
        aliases: "administrator",
        default: "polkit",
        kind: SettingKind::Choice(&["polkit", "sudo"]),
        restart_required: true,
        feature: SettingsFeature::Privilege,
    },
    SettingSpec {
        key: "integrations.portal",
        page: SettingsPage::Integrations,
        label: "setting-integrations-portal",
        group: "settings-group-desktop",
        aliases: "portal",
        default: "system",
        kind: SettingKind::Choice(&["system"]),
        restart_required: true,
        feature: SettingsFeature::Desktop,
    },
    SettingSpec {
        key: "integrations.notifications",
        page: SettingsPage::Integrations,
        label: "setting-integrations-notifications",
        group: "settings-group-desktop",
        aliases: "notifications",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Desktop,
    },
    SettingSpec {
        key: "integrations.mounts",
        page: SettingsPage::Integrations,
        label: "setting-integrations-mounts",
        group: "settings-group-desktop",
        aliases: "mounts disks",
        default: "true",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Desktop,
    },
    SettingSpec {
        key: "shortcuts.bindings",
        page: SettingsPage::Shortcuts,
        label: "setting-shortcuts-bindings",
        group: "settings-group-shortcuts",
        aliases: "keyboard shortcuts",
        default: "default",
        kind: SettingKind::Shortcuts,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "advanced.logging",
        page: SettingsPage::Advanced,
        label: "setting-advanced-logging",
        group: "settings-group-advanced",
        aliases: "logs diagnostic",
        default: "warning",
        kind: SettingKind::Choice(&["error", "warning", "info"]),
        restart_required: true,
        feature: SettingsFeature::Diagnostics,
    },
    SettingSpec {
        key: "advanced.updates",
        page: SettingsPage::Advanced,
        label: "setting-advanced-updates",
        group: "settings-group-advanced",
        aliases: "updates",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: true,
        feature: SettingsFeature::Updates,
    },
    SettingSpec {
        key: "advanced.custom_actions",
        page: SettingsPage::Advanced,
        label: "setting-advanced-custom-actions",
        group: "settings-group-advanced",
        aliases: "scripts actions",
        default: "{\"version\":1,\"actions\":[]}",
        kind: SettingKind::CustomActions,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "advanced.script_directory",
        page: SettingsPage::Advanced,
        label: "setting-advanced-script-directory",
        group: "settings-group-advanced",
        aliases: "scripts actions directory manifest",
        default: "false",
        kind: SettingKind::Boolean,
        restart_required: false,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "search.cache_mib",
        page: SettingsPage::Search,
        label: "setting-search-cache-mib",
        group: "settings-group-preview",
        aliases: "cache limits",
        default: "512",
        kind: SettingKind::Integer {
            maximum: 4096,
            units: "settings-unit-mib",
        },
        restart_required: true,
        feature: SettingsFeature::Search,
    },
    SettingSpec {
        key: "directory_page_items",
        page: SettingsPage::Advanced,
        label: "setting-directory-page-items",
        group: "settings-group-resources",
        aliases: "paging memory",
        default: "512",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_DIRECTORY_PAGE_ITEMS,
            units: "settings-unit-items",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "directory_prefetch_pages",
        page: SettingsPage::Advanced,
        label: "setting-directory-prefetch-pages",
        group: "settings-group-resources",
        aliases: "paging",
        default: "2",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_DIRECTORY_PREFETCH_PAGES,
            units: "settings-unit-pages",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "directory_retained_items",
        page: SettingsPage::Advanced,
        label: "setting-directory-retained-items",
        group: "settings-group-resources",
        aliases: "memory",
        default: "4096",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_DIRECTORY_RETAINED_ITEMS,
            units: "settings-unit-items",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "directory_rendered_viewports",
        page: SettingsPage::Advanced,
        label: "setting-directory-rendered-viewports",
        group: "settings-group-resources",
        aliases: "render",
        default: "3",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_DIRECTORY_RENDERED_VIEWPORTS,
            units: "settings-unit-viewports",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "operation_data_mutations",
        page: SettingsPage::Operations,
        label: "setting-operation-data-mutations",
        group: "settings-group-resources",
        aliases: "concurrency parallel copy",
        default: "2",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_OPERATION_DATA_MUTATIONS,
            units: "settings-unit-jobs",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "operation_metadata_jobs",
        page: SettingsPage::Operations,
        label: "setting-operation-metadata-jobs",
        group: "settings-group-resources",
        aliases: "concurrency",
        default: "4",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_OPERATION_METADATA_JOBS,
            units: "settings-unit-jobs",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
    SettingSpec {
        key: "operation_hash_preview_jobs",
        page: SettingsPage::Operations,
        label: "setting-operation-hash-preview-jobs",
        group: "settings-group-resources",
        aliases: "concurrency",
        default: "4",
        kind: SettingKind::Integer {
            maximum: ResourceLimitConfig::MAX_OPERATION_HASH_PREVIEW_JOBS,
            units: "settings-unit-jobs",
        },
        restart_required: true,
        feature: SettingsFeature::None,
    },
];

impl SettingsDocument {
    pub fn value(&self, key: &str) -> Option<String> {
        let limits = &self.resource_limits;
        let limit = match key {
            "directory_page_items" => Some(limits.directory_page_items),
            "directory_prefetch_pages" => Some(limits.directory_prefetch_pages),
            "directory_retained_items" => Some(limits.directory_retained_items),
            "directory_rendered_viewports" => Some(limits.directory_rendered_viewports),
            "operation_data_mutations" => Some(limits.operation_data_mutations),
            "operation_metadata_jobs" => Some(limits.operation_metadata_jobs),
            "operation_hash_preview_jobs" => Some(limits.operation_hash_preview_jobs),
            _ => None,
        };
        limit
            .map(|value| value.to_string())
            .or_else(|| self.values.get(key).map(ToString::to_string))
    }

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<(), SettingsError> {
        let spec = SETTINGS
            .iter()
            .find(|spec| spec.key == key)
            .ok_or_else(|| SettingsError::InvalidValue { key: key.into() })?;
        super::validate::validate_value(spec, value)?;
        let canonical = match spec.kind {
            SettingKind::Theme if value != "native" => Some(
                super::theme::ThemeDocument::import(value)
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
                    .export(),
            ),
            SettingKind::CustomActions => Some(
                crate::CustomActionDocument::import(value)
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
                    .export(),
            ),
            _ => None,
        };
        let value = canonical.as_deref().unwrap_or(value);
        match key {
            "directory_page_items" => {
                self.resource_limits.directory_page_items = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "directory_prefetch_pages" => {
                self.resource_limits.directory_prefetch_pages = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "directory_retained_items" => {
                self.resource_limits.directory_retained_items = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "directory_rendered_viewports" => {
                self.resource_limits.directory_rendered_viewports = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "operation_data_mutations" => {
                self.resource_limits.operation_data_mutations = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "operation_metadata_jobs" => {
                self.resource_limits.operation_metadata_jobs = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            "operation_hash_preview_jobs" => {
                self.resource_limits.operation_hash_preview_jobs = value
                    .parse()
                    .map_err(|_| SettingsError::InvalidValue { key: key.into() })?
            }
            _ => {
                self.values.insert(key.into(), value.into());
            }
        }
        Ok(())
    }

    pub fn reset_page(&mut self, page: SettingsPage) {
        for spec in SETTINGS.iter().filter(|spec| spec.page == page) {
            self.set_value(spec.key, spec.default)
                .expect("schema defaults are valid");
        }
    }
}

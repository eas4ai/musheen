use musheen_core::{CancellationToken, ItemKind};
use musheen_desktop::PreviewDocument;
use musheen_ui::search::SearchState;
use musheen_ui::views::Layout;
use musheen_ui::{
    AppearanceMode, Catalog, DirectoryState, InfoPaneDetails, InfoPaneState, Locale, MotionPolicy,
    PreviewPresentation, PropertiesPage, PropertiesState, ThemeProfile,
};
use std::path::PathBuf;

fn directory_state_id(state: &DirectoryState) -> &'static str {
    match state {
        DirectoryState::Loading => "loading",
        DirectoryState::Empty => "empty",
        DirectoryState::Ready => "ready",
        DirectoryState::Error(_) => "error",
    }
}

fn search_state_id(state: SearchState) -> &'static str {
    match state {
        SearchState::Idle => "idle",
        SearchState::Running => "running",
        SearchState::Complete => "complete",
        SearchState::Partial => "partial",
        SearchState::RefineRequired => "refine-required",
        SearchState::Cancelled => "cancelled",
        SearchState::Error => "error",
    }
}

fn preview_id(preview: &PreviewPresentation) -> &'static str {
    match preview {
        PreviewPresentation::Text(_) => "text",
        PreviewPresentation::Binary { .. } => "binary",
        PreviewPresentation::Thumbnail(_) => "thumbnail",
        PreviewPresentation::DetailsOnly => "details-only",
    }
}

fn info_state_id(state: &InfoPaneState) -> &'static str {
    match state {
        InfoPaneState::Empty => "empty",
        InfoPaneState::Multiple { .. } => "multiple",
        InfoPaneState::Loading { .. } => "loading",
        InfoPaneState::Ready { .. } => "ready",
        InfoPaneState::Error { .. } => "error",
    }
}

fn properties_page_id(page: PropertiesPage) -> &'static str {
    match page {
        PropertiesPage::General => "general",
        PropertiesPage::Permissions => "permissions",
        PropertiesPage::OpenWith => "open-with",
        PropertiesPage::Tags => "tags",
        PropertiesPage::Checksums => "checksums",
    }
}

fn properties_state_id(state: PropertiesState) -> &'static str {
    match state {
        PropertiesState::Ready => "ready",
        PropertiesState::Replaced => "replaced",
        PropertiesState::Missing => "missing",
    }
}

#[test]
fn view_and_theme_baseline_is_stable_in_both_locales() {
    let english = Catalog::load(Locale::EnUs).expect("the English catalog is valid");
    let pseudo = Catalog::load(Locale::EnXa).expect("the pseudo catalog is valid");
    let layouts = Layout::ALL
        .into_iter()
        .map(|layout| {
            let id = match layout {
                Layout::Details => "command.view-details",
                Layout::List => "command.view-list",
                Layout::Cards => "command.view-cards",
                Layout::Grid => "command.view-grid",
                Layout::Columns => "command.view-columns",
                Layout::Adaptive => "command.view-adaptive",
            };
            format!(
                "{layout:?}:{}:{}",
                english.message(id).expect("English layout label"),
                pseudo.message(id).expect("pseudo layout label")
            )
        })
        .collect::<Vec<_>>();
    let themes = [
        AppearanceMode::Light,
        AppearanceMode::Dark,
        AppearanceMode::HighContrast,
    ]
    .map(|mode| {
        let profile = ThemeProfile::new(mode, false);
        format!(
            "{mode:?}:{}:{}:{:?}",
            profile.surface(),
            profile.has_strong_boundaries(),
            profile.motion()
        )
    });

    assert_eq!(
        layouts.join("\n"),
        "Details:Details:⟦Đḗŧȧīŀş··⟧\n\
         List:List:⟦Ŀīşŧ··⟧\n\
         Cards:Cards:⟦Ƈȧřḓş··⟧\n\
         Grid:Grid:⟦Ɠřīḓ··⟧\n\
         Columns:Columns:⟦Ƈǿŀŭḿƞş··⟧\n\
         Adaptive:Adaptive:⟦Ȧḓȧƥŧīṽḗ··⟧"
    );
    assert_eq!(
        themes.join("\n"),
        "Light:system-surface-light:false:Standard\n\
         Dark:system-surface-dark:false:Standard\n\
         HighContrast:system-surface-high-contrast:true:Standard"
    );
    assert_eq!(
        ThemeProfile::new(AppearanceMode::Light, true).motion(),
        MotionPolicy::Reduced
    );
}

#[test]
fn state_gallery_inventory_has_a_deterministic_baseline() {
    let temporary = tempfile::tempdir().expect("temporary preview directory");
    let text_path = temporary.path().join("preview.txt");
    std::fs::write(&text_path, "preview text").expect("preview fixture writes");
    let text = PreviewDocument::open(&text_path, CancellationToken::new())
        .expect("text preview fixture loads");
    let previews = [
        PreviewPresentation::Text(text),
        PreviewPresentation::Binary { bytes_read: 4_096 },
        PreviewPresentation::Thumbnail(PathBuf::from("/preview.png")),
        PreviewPresentation::DetailsOnly,
    ];
    let details = InfoPaneDetails::new("example.txt", ItemKind::RegularFile, Some(12), Some(42));
    let path = PathBuf::from("/example.txt");
    let info_states = [
        InfoPaneState::Empty,
        InfoPaneState::Multiple { count: 2 },
        InfoPaneState::Loading {
            details: details.clone(),
            path: path.clone(),
        },
        InfoPaneState::Ready {
            details: details.clone(),
            path: path.clone(),
            mime_type: "text/plain".into(),
            preview: PreviewPresentation::DetailsOnly,
        },
        InfoPaneState::Error {
            details,
            path,
            message: "permission denied".into(),
        },
    ];
    let directory_states = [
        DirectoryState::Loading,
        DirectoryState::Empty,
        DirectoryState::Ready,
        DirectoryState::Error("permission denied".into()),
    ];
    let search_states = [
        SearchState::Idle,
        SearchState::Running,
        SearchState::Complete,
        SearchState::Partial,
        SearchState::RefineRequired,
        SearchState::Cancelled,
        SearchState::Error,
    ];
    let property_pages = [
        PropertiesPage::General,
        PropertiesPage::Permissions,
        PropertiesPage::OpenWith,
        PropertiesPage::Tags,
        PropertiesPage::Checksums,
    ];
    let property_states = [
        PropertiesState::Ready,
        PropertiesState::Replaced,
        PropertiesState::Missing,
    ];
    let baseline = [
        format!(
            "directory:{}",
            directory_states
                .iter()
                .map(directory_state_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "search:{}",
            search_states
                .into_iter()
                .map(search_state_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "preview:{}",
            previews
                .iter()
                .map(preview_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "info:{}",
            info_states
                .iter()
                .map(info_state_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "properties-pages:{}",
            property_pages
                .into_iter()
                .map(properties_page_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "properties-states:{}",
            property_states
                .into_iter()
                .map(properties_state_id)
                .collect::<Vec<_>>()
                .join(",")
        ),
    ]
    .join("\n");

    assert_eq!(
        baseline,
        "directory:loading,empty,ready,error\n\
         search:idle,running,complete,partial,refine-required,cancelled,error\n\
         preview:text,binary,thumbnail,details-only\n\
         info:empty,multiple,loading,ready,error\n\
         properties-pages:general,permissions,open-with,tags,checksums\n\
         properties-states:ready,replaced,missing"
    );
}

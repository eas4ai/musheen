use super::document::{ThemeDocument, ThemeTokens};
use super::validate::parse_color;
use gpui_kit::component::{ActiveTheme, Theme, ThemeColor};
use gpui_kit::{App, Hsla};
use musheen_desktop::SettingsDocument;

/// Capture all bridge state, not just GPUI colors: cancel must also restore
/// native resolved colors and accessibility preferences.
#[derive(Clone)]
pub struct AppearanceSnapshot {
    theme: Theme,
    light: Option<native_theme::theme::ResolvedTheme>,
    dark: Option<native_theme::theme::ResolvedTheme>,
    pub(crate) preferences: native_theme::AccessibilityPreferences,
}

impl AppearanceSnapshot {
    pub fn capture(cx: &App) -> Self {
        let native = cx.try_global::<native_theme_gpui::NativeTheme>();
        Self {
            theme: cx.theme().clone(),
            light: native
                .and_then(|theme| theme.resolved_variant(false))
                .cloned(),
            dark: native
                .and_then(|theme| theme.resolved_variant(true))
                .cloned(),
            preferences: native
                .map(|theme| theme.accessibility().clone())
                .unwrap_or_default(),
        }
    }

    pub fn restore(&self, cx: &mut App) {
        self.install(
            self.theme.mode.is_dark(),
            Some(self.theme.clone()),
            &self.preferences,
            cx,
        );
    }

    pub(crate) fn restore_mode(
        &self,
        is_dark: bool,
        preferences: &native_theme::AccessibilityPreferences,
        cx: &mut App,
    ) {
        self.install(is_dark, None, preferences, cx);
    }

    pub(crate) fn is_dark(&self) -> bool {
        self.theme.mode.is_dark()
    }

    fn install(
        &self,
        is_dark: bool,
        exact_theme: Option<Theme>,
        preferences: &native_theme::AccessibilityPreferences,
        cx: &mut App,
    ) {
        let name = self.theme.theme_name().to_string();
        let active = if is_dark { &self.dark } else { &self.light };
        let inactive = if is_dark { &self.light } else { &self.dark };

        // Install the inactive variant first. The active apply then keeps it in
        // NativeTheme and rebuilds both GPUI configs before painting.
        if let Some(resolved) = inactive {
            let theme = native_theme_gpui::to_theme(resolved, &name, !is_dark, preferences);
            native_theme_gpui::apply(theme, resolved, preferences, cx);
        }
        if let Some(resolved) = active {
            let theme = exact_theme.unwrap_or_else(|| {
                native_theme_gpui::to_theme(resolved, &name, is_dark, preferences)
            });
            native_theme_gpui::apply(theme, resolved, preferences, cx);
        } else {
            *Theme::global_mut(cx) = exact_theme.unwrap_or_else(|| self.theme.clone());
            Theme::sync_base(cx);
            native_theme_gpui::apply_accessibility(preferences, cx);
        }
    }
}

pub(crate) fn apply_tokens(document: &SettingsDocument, cx: &mut App) {
    if native_high_contrast(cx) {
        apply_high_contrast(cx);
        return;
    }

    let Some(palette) = configured_palette(document) else {
        return;
    };
    apply_custom_palette(&palette, cx);
}

fn native_high_contrast(cx: &App) -> bool {
    cx.try_global::<native_theme_gpui::NativeTheme>()
        .is_some_and(|theme| theme.accessibility().high_contrast)
}

fn configured_palette(document: &SettingsDocument) -> Option<ThemeDocument> {
    document
        .value("appearance.theme")
        .and_then(|text| ThemeDocument::import(&text).ok())
}

/// Native high contrast always wins. Repair native aliases without replacing
/// the bridge's geometry, fonts, icon family, or accessibility preferences.
fn apply_high_contrast(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.focus_ring = true;
    normalize_high_contrast_text(&mut theme.colors);
    normalize_high_contrast_boundaries(&mut theme.colors);
    theme.tokens = theme.colors.into();
    publish_palette(None, cx);
}

fn normalize_high_contrast_text(colors: &mut ThemeColor) {
    colors.foreground = readable_foreground(colors.foreground, &[colors.background]);
    colors.muted_foreground = readable_foreground(
        colors.muted_foreground,
        &[colors.background, colors.secondary],
    );
    colors.primary_foreground = readable_foreground(
        colors.primary_foreground,
        &[
            colors.primary,
            colors.primary_hover,
            colors.primary_active,
            colors.button_primary,
            colors.button_primary_hover,
            colors.button_primary_active,
        ],
    );
    colors.button_primary_foreground = colors.primary_foreground;
    colors.secondary_foreground = readable_foreground(
        colors.secondary_foreground,
        &[
            colors.secondary,
            colors.secondary_hover,
            colors.secondary_active,
            colors.button_secondary,
            colors.button_secondary_hover,
            colors.button_secondary_active,
        ],
    );
    colors.button_secondary_foreground = colors.secondary_foreground;
    colors.button_foreground = readable_foreground(
        colors.button_foreground,
        &[colors.button, colors.button_hover, colors.button_active],
    );
    colors.danger_foreground = readable_foreground(
        colors.danger_foreground,
        &[
            colors.danger,
            colors.danger_hover,
            colors.danger_active,
            colors.button_danger,
            colors.button_danger_hover,
            colors.button_danger_active,
        ],
    );
    colors.button_danger_foreground = colors.danger_foreground;
    colors.warning_foreground = readable_foreground(
        colors.warning_foreground,
        &[
            colors.warning,
            colors.warning_hover,
            colors.warning_active,
            colors.button_warning,
            colors.button_warning_hover,
            colors.button_warning_active,
        ],
    );
    colors.button_warning_foreground = colors.warning_foreground;
}

fn normalize_high_contrast_boundaries(colors: &mut ThemeColor) {
    let readable = colors.foreground;
    for target in [
        &mut colors.border,
        &mut colors.input,
        &mut colors.ring,
        &mut colors.drag_border,
        &mut colors.sidebar_border,
        &mut colors.title_bar_border,
        &mut colors.status_bar_border,
        &mut colors.window_border,
        &mut colors.table_row_border,
        &mut colors.list_active_border,
        &mut colors.table_active_border,
        &mut colors.scrollbar_thumb,
        &mut colors.scrollbar_thumb_hover,
    ] {
        *target = readable;
    }
}

fn apply_custom_palette(palette: &ThemeDocument, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.focus_ring = true;
    project_palette(&palette.tokens, &mut theme.colors);
    theme.tokens = theme.colors.into();
    publish_palette(Some(palette), cx);
}

/// Legacy GPUI components store resolved aliases in addition to semantic
/// tokens. Project every role so old native colors cannot leak into widgets.
fn project_palette(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    project_surfaces(tokens, colors);
    project_primary(tokens, colors);
    project_secondary(tokens, colors);
    project_boundaries(tokens, colors);
    project_feedback(tokens, colors);
}

fn project_surfaces(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    for target in [
        &mut colors.background,
        &mut colors.popover,
        &mut colors.list,
        &mut colors.table,
        &mut colors.sidebar,
        &mut colors.title_bar,
        &mut colors.status_bar,
        &mut colors.tab,
        &mut colors.tab_active,
        &mut colors.group_box,
        &mut colors.accordion,
    ] {
        *target = palette_color(&tokens.background);
    }
    for target in [
        &mut colors.foreground,
        &mut colors.popover_foreground,
        &mut colors.sidebar_foreground,
        &mut colors.tab_foreground,
        &mut colors.tab_active_foreground,
        &mut colors.group_box_foreground,
        &mut colors.caret,
        &mut colors.link,
        &mut colors.link_hover,
        &mut colors.link_active,
    ] {
        *target = palette_color(&tokens.foreground);
    }
    colors.muted_foreground = palette_color(&tokens.muted_foreground);
}

fn project_primary(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    for target in [
        &mut colors.primary,
        &mut colors.primary_hover,
        &mut colors.primary_active,
        &mut colors.button_primary,
        &mut colors.button_primary_hover,
        &mut colors.button_primary_active,
        &mut colors.sidebar_primary,
        &mut colors.progress_bar,
        &mut colors.slider_bar,
    ] {
        *target = palette_color(&tokens.primary);
    }
    for target in [
        &mut colors.primary_foreground,
        &mut colors.button_primary_foreground,
        &mut colors.sidebar_primary_foreground,
    ] {
        *target = palette_color(&tokens.primary_foreground);
    }
}

fn project_secondary(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    for target in [
        &mut colors.secondary,
        &mut colors.secondary_hover,
        &mut colors.secondary_active,
        &mut colors.button,
        &mut colors.button_hover,
        &mut colors.button_active,
        &mut colors.button_secondary,
        &mut colors.button_secondary_hover,
        &mut colors.button_secondary_active,
        &mut colors.accent,
        &mut colors.muted,
        &mut colors.list_hover,
        &mut colors.list_active,
        &mut colors.list_even,
        &mut colors.list_head,
        &mut colors.table_hover,
        &mut colors.table_active,
        &mut colors.table_even,
        &mut colors.table_head,
        &mut colors.table_foot,
        &mut colors.selection,
        &mut colors.sidebar_accent,
        &mut colors.tab_bar,
        &mut colors.tab_bar_segmented,
        &mut colors.scrollbar,
        &mut colors.description_list_label,
        &mut colors.skeleton,
        &mut colors.drop_target,
        &mut colors.switch,
    ] {
        *target = palette_color(&tokens.secondary);
    }
    for target in [
        &mut colors.secondary_foreground,
        &mut colors.button_foreground,
        &mut colors.button_secondary_foreground,
        &mut colors.accent_foreground,
        &mut colors.sidebar_accent_foreground,
        &mut colors.table_head_foreground,
        &mut colors.table_foot_foreground,
        &mut colors.description_list_label_foreground,
        &mut colors.switch_thumb,
        &mut colors.slider_thumb,
    ] {
        *target = palette_color(&tokens.secondary_foreground);
    }
}

fn project_boundaries(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    for target in [
        &mut colors.border,
        &mut colors.input,
        &mut colors.sidebar_border,
        &mut colors.title_bar_border,
        &mut colors.status_bar_border,
        &mut colors.window_border,
        &mut colors.table_row_border,
        &mut colors.scrollbar_thumb,
        &mut colors.scrollbar_thumb_hover,
    ] {
        *target = palette_color(&tokens.border);
    }
    for target in [
        &mut colors.ring,
        &mut colors.drag_border,
        &mut colors.list_active_border,
        &mut colors.table_active_border,
    ] {
        *target = palette_color(&tokens.ring);
    }
}

fn project_feedback(tokens: &ThemeTokens, colors: &mut ThemeColor) {
    for target in [
        &mut colors.danger,
        &mut colors.danger_hover,
        &mut colors.danger_active,
        &mut colors.button_danger,
        &mut colors.button_danger_hover,
        &mut colors.button_danger_active,
    ] {
        *target = palette_color(&tokens.danger);
    }
    for target in [
        &mut colors.danger_foreground,
        &mut colors.button_danger_foreground,
    ] {
        *target = palette_color(&tokens.danger_foreground);
    }
    for target in [
        &mut colors.warning,
        &mut colors.warning_hover,
        &mut colors.warning_active,
        &mut colors.button_warning,
        &mut colors.button_warning_hover,
        &mut colors.button_warning_active,
    ] {
        *target = palette_color(&tokens.warning);
    }
    for target in [
        &mut colors.warning_foreground,
        &mut colors.button_warning_foreground,
    ] {
        *target = palette_color(&tokens.warning_foreground);
    }
    // Information/success retain their semantic labels/icons but use the
    // validated primary pair; themes cannot make those messages unreadable.
    for target in [
        &mut colors.info,
        &mut colors.info_hover,
        &mut colors.info_active,
        &mut colors.button_info,
        &mut colors.button_info_hover,
        &mut colors.button_info_active,
        &mut colors.success,
        &mut colors.success_hover,
        &mut colors.success_active,
        &mut colors.button_success,
        &mut colors.button_success_hover,
        &mut colors.button_success_active,
    ] {
        *target = palette_color(&tokens.primary);
    }
    for target in [
        &mut colors.info_foreground,
        &mut colors.button_info_foreground,
        &mut colors.success_foreground,
        &mut colors.button_success_foreground,
    ] {
        *target = palette_color(&tokens.primary_foreground);
    }
}

fn palette_color(value: &str) -> Hsla {
    gpui_kit::rgb(parse_color(value).expect("validated color")).into()
}

fn readable_foreground(
    foreground: gpui_kit::Hsla,
    backgrounds: &[gpui_kit::Hsla],
) -> gpui_kit::Hsla {
    let hex = |color: gpui_kit::Hsla| {
        let color: gpui_kit::Rgba = color.into();
        format!(
            "#{:02x}{:02x}{:02x}",
            (color.r * 255.).round() as u8,
            (color.g * 255.).round() as u8,
            (color.b * 255.).round() as u8
        )
    };
    let minimum_contrast = |foreground| {
        backgrounds
            .iter()
            .map(|background| {
                super::validate::contrast_ratio(&hex(foreground), &hex(*background))
                    .expect("RGB colors")
            })
            .fold(f64::INFINITY, f64::min)
    };
    if minimum_contrast(foreground) >= 4.5 {
        return foreground;
    }
    let black = gpui_kit::rgb(0x000000).into();
    let white = gpui_kit::rgb(0xffffff).into();
    if minimum_contrast(black) >= minimum_contrast(white) {
        black
    } else {
        white
    }
}

fn publish_palette(palette: Option<&ThemeDocument>, cx: &mut App) {
    let theme = cx.theme().clone();
    let bridge = cx.try_global::<native_theme_gpui::NativeTheme>();
    if let Some((mut resolved, preferences)) = bridge.and_then(|bridge| {
        bridge
            .resolved(cx)
            .map(|resolved| (resolved.clone(), bridge.accessibility().clone()))
    }) {
        // The native bridge restores scrollbar/resize colors after every base
        // rebuild. Update its resolved color source as well, retaining native
        // geometry, fonts and accessibility. Cancel restores the full snapshot.
        let (border, focus) = if let Some(palette) = palette {
            resolved.scrollbar.track_color =
                palette.tokens.secondary.parse().expect("validated color");
            (
                palette.tokens.border.parse().expect("validated color"),
                palette.tokens.ring.parse().expect("validated color"),
            )
        } else {
            (resolved.defaults.text_color, resolved.defaults.text_color)
        };
        resolved.defaults.border.color = border;
        resolved.defaults.focus_ring_color = focus;
        resolved.defaults.focus_ring_width = resolved.defaults.focus_ring_width.max(1.0);
        resolved.scrollbar.thumb_color = border;
        resolved.scrollbar.thumb_hover_color = border;
        resolved.scrollbar.thumb_active_color = Some(border);
        resolved.splitter.divider_color = border;
        resolved.splitter.hover_color = focus;
        native_theme_gpui::apply(theme, &resolved, &preferences, cx);
    } else {
        Theme::sync_base(cx);
        cx.refresh_windows();
    }
}

/// Preview a validated settings draft through the same path used at startup
/// and by the Appearance page. The caller owns the cancel snapshot.
pub fn preview(document: &SettingsDocument, cx: &mut App) {
    crate::settings::apply_appearance(document, cx);
}

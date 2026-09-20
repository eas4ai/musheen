//! Native-popup rendering for registry-backed context menus.
//!
//! This module writes semantics and directional chrome directly onto the live
//! native popup rows; it intentionally has no parallel menu-tree model.

use super::{
    ContextMenu, MenuAccessibleRole, MenuDirection, MenuEntry, MenuEntryKind, MenuThemeTokens,
};
use crate::icons::{LucideIcon, lucide_icon_or_fallback};
use gpui_kit::accesskit::Toggled;
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuDirection, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Role, SharedString, TestSupportExt, Window, div, px};

/// App-owned bridge from a [`ContextMenu`] projection into GPUI Kit's live
/// [`PopupMenu`]. The activation callback receives the real window so callers
/// can capture focus before opening a confirmation or chooser dialog.
pub struct ContextMenuRenderer;

impl ContextMenuRenderer {
    /// Populates the same native popup type the app stores for keyboard menus.
    /// Every nested popup inherits the root locale direction and theme tokens.
    pub fn populate<F>(
        popup: PopupMenu,
        menu: ContextMenu,
        path: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
        on_activate: F,
    ) -> PopupMenu
    where
        F: Fn(MenuEntry, &mut Window, &mut App) + Clone + 'static,
    {
        Self::populate_with_direction(
            popup.direction(popup_direction(menu.locale_direction())),
            menu,
            path.into(),
            window,
            cx,
            on_activate,
        )
    }

    fn populate_with_direction<F>(
        mut popup: PopupMenu,
        menu: ContextMenu,
        path: String,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
        on_activate: F,
    ) -> PopupMenu
    where
        F: Fn(MenuEntry, &mut Window, &mut App) + Clone + 'static,
    {
        let direction = menu.locale_direction();
        let theme_tokens = menu.theme_tokens();
        let viewport_height = window.viewport_size().height;
        let available_height = if viewport_height > px(16.) {
            viewport_height - px(16.)
        } else {
            viewport_height
        };
        popup = popup
            .direction(popup_direction(direction))
            .scrollable(true)
            .max_h(available_height);
        for (index, entry) in menu.entries().iter().cloned().enumerate() {
            let row_path = format!("{path}-{index}");
            popup = match entry.kind() {
                MenuEntryKind::Separator => popup.separator(),
                MenuEntryKind::Submenu => {
                    let Some(submenu) = entry.submenu().cloned() else {
                        continue;
                    };
                    let child_path = row_path.clone();
                    let activate = on_activate.clone();
                    let submenu = PopupMenu::build(window, cx, move |popup, window, cx| {
                        Self::populate_with_direction(
                            popup, submenu, child_path, window, cx, activate,
                        )
                    });
                    let item = PopupMenuItem::submenu(entry.label(), submenu)
                        .disabled(!entry.state().is_enabled());
                    popup.item(match entry.accessible_disabled_reason() {
                        Some(reason) => item.accessibility_description(reason),
                        None => item,
                    })
                }
                MenuEntryKind::Command | MenuEntryKind::Overflow => popup.item(Self::popup_item(
                    entry,
                    row_path,
                    direction,
                    theme_tokens,
                    on_activate.clone(),
                )),
            };
        }
        popup
    }

    fn popup_item<F>(
        entry: MenuEntry,
        id: String,
        direction: MenuDirection,
        theme_tokens: MenuThemeTokens,
        on_activate: F,
    ) -> PopupMenuItem
    where
        F: Fn(MenuEntry, &mut Window, &mut App) + Clone + 'static,
    {
        let label = entry.label().to_owned();
        let icon_key = entry.icon_key().map(str::to_owned);
        let icon = icon_key.as_deref().map(menu_icon);
        let shortcut = entry.shortcut().map(str::to_owned);
        let disabled_reason = entry.accessible_disabled_reason().map(str::to_owned);
        let enabled = entry.state().is_enabled();
        let checked = entry.state().is_checked();
        let destructive = entry.danger_level() != musheen_core::DangerLevel::None;
        let role = menu_role(&entry);
        let row_label = label.clone();
        let item = PopupMenuItem::element(move |_, cx| {
            let colors = cx.theme().colors;
            div()
                .id(SharedString::from(format!("context-menu-row-{id}")))
                .test_support()
                .w_full()
                .flex()
                .justify_between()
                .gap_4()
                .when(direction == MenuDirection::RightToLeft, |row| {
                    row.flex_row_reverse()
                })
                .when(theme_tokens.strong_boundaries(), |row| {
                    row.border_b_1().border_color(colors.border)
                })
                .when(destructive, |row| row.text_color(colors.danger))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(direction == MenuDirection::RightToLeft, |row| {
                            row.flex_row_reverse()
                        })
                        .when_some(icon, |row, icon| {
                            row.child(
                                div()
                                    .id(SharedString::from(format!("context-menu-icon-{id}")))
                                    .test_support()
                                    .role(Role::Image)
                                    .aria_label(
                                        icon_key
                                            .clone()
                                            .unwrap_or_else(|| "extension action".to_owned()),
                                    )
                                    .child(Icon::new(icon).small()),
                            )
                        })
                        .child(if checked {
                            format!("✓ {row_label}")
                        } else {
                            row_label.clone()
                        }),
                )
                .when_some(shortcut.clone(), |row, shortcut| {
                    row.child(div().text_xs().child(shortcut))
                })
        })
        .accessibility(role, label.clone())
        .accessibility_toggled(if checked {
            Toggled::True
        } else {
            Toggled::False
        });
        let item = if let Some(reason) = disabled_reason {
            item.accessibility_description(reason)
        } else {
            item
        };
        item.checked(checked)
            .disabled(!enabled)
            .on_click(move |_, window, cx| on_activate(entry.clone(), window, cx))
    }
}

const fn popup_direction(direction: MenuDirection) -> PopupMenuDirection {
    match direction {
        MenuDirection::LeftToRight => PopupMenuDirection::LeftToRight,
        MenuDirection::RightToLeft => PopupMenuDirection::RightToLeft,
    }
}

fn menu_role(entry: &MenuEntry) -> Role {
    match entry.accessible_role() {
        MenuAccessibleRole::Checkbox => Role::MenuItemCheckBox,
        MenuAccessibleRole::Radio => Role::MenuItemRadio,
        MenuAccessibleRole::MenuItem | MenuAccessibleRole::Submenu => Role::MenuItem,
    }
}

fn menu_icon(icon_key: &str) -> IconName {
    match lucide_icon_or_fallback(icon_key) {
        LucideIcon::ArrowLeft => IconName::ArrowLeft,
        LucideIcon::ArrowRight => IconName::ArrowRight,
        LucideIcon::ArrowUp => IconName::ArrowUp,
        LucideIcon::RefreshCw => IconName::RefreshCw,
        LucideIcon::Search => IconName::Search,
        LucideIcon::ListFilter | LucideIcon::TextCursorInput => IconName::TextCursorInput,
        LucideIcon::List => IconName::List,
        LucideIcon::ListChecks => IconName::ListChecks,
        LucideIcon::Grid2x2 => IconName::Grid2x2,
        LucideIcon::Settings => IconName::Settings,
        LucideIcon::X => IconName::X,
        LucideIcon::Home => IconName::House,
        LucideIcon::Folder => IconName::Folder,
        LucideIcon::HardDrive => IconName::HardDrive,
        LucideIcon::Network => IconName::Network,
        LucideIcon::Trash => IconName::Trash,
        LucideIcon::Info => IconName::Info,
        LucideIcon::Plus => IconName::Plus,
        LucideIcon::Copy => IconName::Copy,
        LucideIcon::RotateCcw => IconName::RotateCcw,
        LucideIcon::File => IconName::File,
        LucideIcon::Puzzle => IconName::Puzzle,
        LucideIcon::Columns2 => IconName::Columns2,
        LucideIcon::PanelRight => IconName::PanelRight,
        LucideIcon::Scissors => IconName::Scissors,
        LucideIcon::Terminal => IconName::Terminal,
        LucideIcon::Eye => IconName::Eye,
        LucideIcon::EyeOff => IconName::EyeOff,
        LucideIcon::FileSymlink => IconName::FileSymlink,
        LucideIcon::Link2 => IconName::Link2,
        LucideIcon::Tag => IconName::Tag,
        LucideIcon::Shield => IconName::Shield,
        LucideIcon::ShieldCheck => IconName::ShieldCheck,
        LucideIcon::Pin => IconName::Pin,
        LucideIcon::PinOff => IconName::PinOff,
        LucideIcon::MapPin => IconName::MapPin,
        LucideIcon::Star => IconName::Star,
    }
}

#[cfg(test)]
mod tests {
    use super::ContextMenuRenderer;
    use crate::{
        AppearanceMode, ContextMenuRequest, ContextMenuSurface, Locale, MenuTarget,
        OpenWithApplication, ThemeProfile,
    };
    use gpui_kit::accesskit::Toggled;
    use gpui_kit::component::{
        Root,
        menu::{PopupMenu, PopupMenuItem},
    };
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Focusable, ParentElement as _, Role, TestAppContext, div, px, size,
    };
    use musheen_core::{
        CapabilityMatrix, CapabilityState, CommandContext, CommandRegistry, CommandTarget,
        CommandTargetRef, ItemId, ProviderActionMatrix, ProviderId, StorePath,
    };

    fn file_request() -> ContextMenuRequest {
        let target = CommandTargetRef::new(
            ItemId::new(
                ProviderId::new("local").expect("provider id"),
                b"rtl-file".to_vec(),
            )
            .expect("item id"),
            StorePath::from_unix_path("/work/alpha/beta.txt"),
        )
        .expect("target reference");
        let context = CommandContext {
            target: CommandTarget::File,
            selection_count: 1,
            item_count: 1,
            location_is_writable: true,
            mutation_is_supported: true,
            is_local: true,
            capabilities: CapabilityMatrix::new(|_| CapabilityState::Supported),
            provider_actions: ProviderActionMatrix::from_states(
                CapabilityState::Supported,
                CapabilityState::Supported,
                CapabilityState::Supported,
                CapabilityState::Supported,
            ),
            ..CommandContext::default()
        };
        ContextMenuRequest::new(
            context,
            MenuTarget::Item,
            StorePath::from_unix_path("/work"),
            vec![target],
        )
        .with_open_with(&[OpenWithApplication::compatible(
            "Editor",
            "org.example.Editor",
        )])
    }

    #[gpui_kit::test]
    fn rtl_popup_opens_nested_menu_with_live_accesskit_semantics(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let menu = ContextMenuSurface::new(CommandRegistry::built_in())
            .with_locale(Locale::Ar)
            .with_theme_profile(ThemeProfile::new(AppearanceMode::HighContrast, true))
            .compose(file_request());
        let handle = cx.open_window(size(px(640.), px(480.)), move |window, cx| {
            let root_popup = PopupMenu::build(window, cx, move |popup, window, popup_cx| {
                ContextMenuRenderer::populate(
                    popup,
                    menu,
                    "renderer-rtl",
                    window,
                    popup_cx,
                    |_, _, _| {},
                )
            });
            root_popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
            Root::new(root_popup, window, cx)
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.activate_accessibility_for_test();
            window.render_frame(cx);
            // At the left viewport edge RTL falls back to opening right.
            window.press("down", cx);
            window.press("down", cx);
            window.render_frame(cx);
            window.press("right", cx);
            window.render_frame(cx);

            let tree = window
                .debug_a11y_tree_json()
                .expect("debug builds expose the live AccessKit tree");
            let tree: serde_json::Value = serde_json::from_str(&tree).expect("valid tree JSON");
            let nodes = tree["nodes"]
                .as_object()
                .expect("rendered accessibility nodes");
            let submenu = nodes
                .values()
                .find(|node| node["aria"]["label"] == "Open with")
                .expect("Open with submenu is rendered");
            assert_eq!(submenu["aria"]["role"], "MenuItem");
            assert_eq!(submenu["aria"]["has_popup"], "Menu");
            assert_eq!(submenu["aria"]["expanded"], true);
            let editor = nodes
                .values()
                .find(|node| node["aria"]["label"] == "Editor")
                .expect("nested Editor item is rendered");
            assert_eq!(editor["aria"]["role"], "MenuItem");
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    fn submenu_layout_and_keyboard_follow_direction_and_available_space(cx: &mut TestAppContext) {
        use gpui_kit::Styled;
        use gpui_kit::component::menu::PopupMenuDirection;
        struct PositionedPopup {
            popup: gpui_kit::Entity<PopupMenu>,
            x: f32,
        }
        impl gpui_kit::Render for PositionedPopup {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                div()
                    .absolute()
                    .left(px(self.x))
                    .top(px(40.))
                    .child(self.popup.clone())
            }
        }
        cx.update(gpui_kit::init);
        for (direction, x, opens_left) in [
            (PopupMenuDirection::LeftToRight, 20., false),
            (PopupMenuDirection::LeftToRight, 580., true),
            (PopupMenuDirection::RightToLeft, 580., true),
            (PopupMenuDirection::RightToLeft, 20., false),
        ] {
            let mut root = None;
            let mut child = None;
            let handle = cx.open_window(size(px(800.), px(480.)), |window, cx| {
                let submenu = PopupMenu::build(window, cx, |popup, _, _| {
                    popup
                        .direction(direction)
                        .min_w(px(180.))
                        .max_w(px(180.))
                        .item(PopupMenuItem::new("Child"))
                });
                child = Some(submenu.clone());
                let popup = PopupMenu::build(window, cx, |popup, _, _| {
                    popup
                        .direction(direction)
                        .min_w(px(180.))
                        .max_w(px(180.))
                        .item(PopupMenuItem::submenu("Parent", submenu))
                });
                popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
                root = Some(popup.clone());
                Root::new(cx.new(|_| PositionedPopup { popup, x }), window, cx)
            });
            let root = root.unwrap();
            let child = child.unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.press("down", cx);
                window.render_frame(cx);
                let label = window.find(("submenu-label", 0usize)).bounds();
                let chevron = window.find(("submenu-chevron", 0usize)).bounds();
                assert_eq!(
                    chevron.origin.x < label.origin.x,
                    direction == PopupMenuDirection::RightToLeft
                );
                let submenu = window.find("submenu").bounds();
                assert!(
                    submenu.origin.x >= px(0.) && submenu.right() <= window.viewport_size().width
                );
                assert_eq!(submenu.origin.x < label.origin.x, opens_left);
                window.press(if opens_left { "left" } else { "right" }, cx);
                window.render_frame(cx);
                assert!(child.read(cx).focus_handle(cx).is_focused(window));
                window.press(if opens_left { "right" } else { "left" }, cx);
                window.render_frame(cx);
                assert!(root.read(cx).focus_handle(cx).is_focused(window));
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn popup_row_forwards_disabled_accesskit_state_without_losing_toggle_semantics(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(640.), px(480.)), move |window, cx| {
            let popup = PopupMenu::build(window, cx, |popup, _, _| {
                popup.item(
                    PopupMenuItem::element(|_, _| div().child("Unavailable layout"))
                        .accessibility(Role::MenuItemRadio, "Unavailable layout")
                        .accessibility_description("This layout is unavailable")
                        .accessibility_toggled(Toggled::True)
                        .checked(true)
                        .disabled(true),
                )
            });
            popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
            Root::new(popup, window, cx)
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.activate_accessibility_for_test();
            window.render_frame(cx);
            let tree = window
                .debug_a11y_tree_json()
                .expect("debug builds expose the live AccessKit tree");
            let tree: serde_json::Value = serde_json::from_str(&tree).expect("valid tree JSON");
            let node = tree["nodes"]
                .as_object()
                .expect("rendered accessibility nodes")
                .values()
                .find(|node| node["aria"]["label"] == "Unavailable layout")
                .expect("disabled menu row is rendered");
            assert_eq!(node["aria"]["role"], "MenuItemRadio");
            assert_eq!(node["aria"]["description"], "This layout is unavailable");
            assert_eq!(node["aria"]["toggled"], "True");
            assert_eq!(node["aria"]["disabled"], true);
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    fn popup_keyboard_skips_disabled_rows_on_entry_wrap_and_submenu(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        cx.update(gpui_kit::init);
        for (keys, expected) in [
            (vec!["down", "enter"], 1),
            (vec!["up", "up", "down", "down", "enter"], 1),
            (vec!["down", "up", "up", "enter"], 2),
            (vec!["up", "right", "enter"], 3),
        ] {
            let invoked = Rc::new(Cell::new(0));
            let handle = cx.open_window(size(px(800.), px(480.)), |window, cx| {
                let item = |label, value, disabled| {
                    let invoked = invoked.clone();
                    PopupMenuItem::new(label)
                        .disabled(disabled)
                        .on_click(move |_, _, _| invoked.set(value))
                };
                let child = PopupMenu::build(window, cx, |popup, _, _| {
                    popup
                        .item(item("Disabled child", 99, true))
                        .item(PopupMenuItem::label("Child label"))
                        .item(item("Child", 3, false))
                });
                let popup = PopupMenu::build(window, cx, |popup, _, _| {
                    let disabled_invoked = invoked.clone();
                    popup
                        .item(item("Disabled first", 99, true))
                        .item(PopupMenuItem::label("Section"))
                        .item(PopupMenuItem::separator())
                        .item(item("First", 1, false))
                        .item(
                            PopupMenuItem::element(|_, _| div().child("Disabled custom"))
                                .disabled(true)
                                .on_click(move |_, _, _| disabled_invoked.set(99)),
                        )
                        .item(item("Second", 2, false))
                        .item(
                            PopupMenuItem::submenu("Disabled submenu", child.clone())
                                .disabled(true),
                        )
                        .item(PopupMenuItem::submenu("More", child))
                        .item(item("Disabled last", 99, true))
                });
                popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
                Root::new(popup, window, cx)
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                for key in keys {
                    window.press(key, cx);
                    window.render_frame(cx);
                }
                assert_eq!(invoked.get(), expected);
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn popup_with_only_disabled_rows_does_not_invoke_or_dismiss(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        cx.update(gpui_kit::init);
        let invoked = Rc::new(Cell::new(false));
        let dismissed = Rc::new(Cell::new(false));
        let mut subscription = None;
        let handle = cx.open_window(size(px(640.), px(480.)), |window, cx| {
            let invoked = invoked.clone();
            let popup = PopupMenu::build(window, cx, |popup, _, _| {
                popup.item(
                    PopupMenuItem::new("Disabled")
                        .disabled(true)
                        .on_click(move |_, _, _| invoked.set(true)),
                )
            });
            let observed = popup.clone();
            let dismissed = dismissed.clone();
            popup.update(cx, |popup, cx| {
                subscription = Some(cx.subscribe(
                    &observed,
                    move |_, _, _: &gpui_kit::DismissEvent, _| {
                        dismissed.set(true);
                    },
                ));
                popup.focus_handle(cx).focus(window, cx);
            });
            Root::new(popup, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            for key in ["down", "enter", "up", "enter"] {
                window.press(key, cx);
                window.render_frame(cx);
            }
            assert!(!invoked.get());
            assert!(!dismissed.get());
        })
        .unwrap();
        drop(subscription);
    }

    #[gpui_kit::test]
    fn viewport_bound_popup_scrolls_keyboard_selection_to_the_last_command(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(640.), px(480.)), move |window, cx| {
            let popup = PopupMenu::build(window, cx, |popup, _, _| {
                (0..24).fold(popup.scrollable(true).max_h(px(464.)), |popup, index| {
                    popup.item(PopupMenuItem::new(format!("Command {index}")))
                })
            });
            popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
            Root::new(popup, window, cx)
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.activate_accessibility_for_test();
            window.render_frame(cx);
            for _ in 0..24 {
                window.press("down", cx);
                window.render_frame(cx);
            }
            let tree = window
                .debug_a11y_tree_json()
                .expect("debug builds expose the live AccessKit tree");
            let tree: serde_json::Value = serde_json::from_str(&tree).expect("valid tree JSON");
            let node = tree["nodes"]
                .as_object()
                .expect("rendered accessibility nodes")
                .values()
                .find(|node| node["aria"]["label"] == "Command 23")
                .expect("last command remains reachable");
            assert_eq!(node["aria"]["selected"], true);
        })
        .expect("test window remains open");
    }
}

use crate::{ActiveTheme, Disableable, StyledExt, h_flex};
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window, prelude::FluentBuilder as _,
};
use gpui::accesskit::{HasPopup, Toggled};
use gpui_base::TestSupportExt as _;
use smallvec::SmallVec;

#[derive(IntoElement)]
pub(crate) struct MenuItemElement {
    id: ElementId,
    group_name: SharedString,
    role: Role,
    aria_label: Option<SharedString>,
    aria_description: Option<SharedString>,
    aria_expanded: Option<bool>,
    aria_has_popup: Option<HasPopup>,
    aria_toggled: Option<Toggled>,
    style: StyleRefinement,
    disabled: bool,
    selected: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_hover: Option<Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
    children: SmallVec<[AnyElement; 2]>,
}

impl MenuItemElement {
    /// Create a new MenuItem with the given ID and group name.
    pub(crate) fn new(id: impl Into<ElementId>, group_name: impl Into<SharedString>) -> Self {
        let id: ElementId = id.into();
        Self {
            id: id.clone(),
            group_name: group_name.into(),
            role: Role::MenuItem,
            aria_label: None,
            aria_description: None,
            aria_expanded: None,
            aria_has_popup: None,
            aria_toggled: None,
            style: StyleRefinement::default(),
            disabled: false,
            selected: false,
            on_click: None,
            on_hover: None,
            children: SmallVec::new(),
        }
    }

    /// Set ListItem as the selected item style.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Set the accessible label for the menu item.
    pub(crate) fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    pub(crate) fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    pub(crate) fn aria_description(mut self, description: impl Into<SharedString>) -> Self {
        self.aria_description = Some(description.into());
        self
    }

    /// Set whether this menu item controls a popup submenu.
    pub(crate) fn aria_has_popup(mut self, popup: HasPopup) -> Self {
        self.aria_has_popup = Some(popup);
        self
    }

    /// Set whether this menu item's submenu is currently expanded.
    pub(crate) fn aria_expanded(mut self, expanded: bool) -> Self {
        self.aria_expanded = Some(expanded);
        self
    }

    pub(crate) fn aria_toggled(mut self, toggled: Toggled) -> Self {
        self.aria_toggled = Some(toggled);
        self
    }

    /// Set the disabled state of the MenuItem.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set a handler for when the MenuItem is clicked.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// Set a handler for when the mouse enters the MenuItem.
    #[allow(unused)]
    pub fn on_hover(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }
}

impl Disableable for MenuItemElement {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Styled for MenuItemElement {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for MenuItemElement {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for MenuItemElement {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .id(self.id)
            .test_support()
            .role(self.role)
            .when_some(self.aria_label, |this, label| this.aria_label(label))
            .when_some(self.aria_description, |this, description| this.aria_description(description))
            .when_some(self.aria_has_popup, |this, popup| this.aria_has_popup(popup))
            .when_some(self.aria_expanded, |this, expanded| this.aria_expanded(expanded))
            .when_some(self.aria_toggled, |this, toggled| this.aria_toggled(toggled))
            .aria_selected(self.selected)
            .group(&self.group_name)
            .gap_x_1()
            .py_1()
            .px_2()
            .text_base()
            .text_color(cx.theme().foreground)
            .relative()
            .items_center()
            .justify_between()
            .refine_style(&self.style)
            .when_some(self.on_hover, |this, on_hover| {
                this.on_hover(move |hovered, window, cx| (on_hover)(hovered, window, cx))
            })
            .when(!self.disabled, |this| {
                this.group_hover(self.group_name, |this| {
                    this.bg(cx.theme().tokens.accent)
                        .text_color(cx.theme().accent_foreground)
                })
                .when(self.selected, |this| {
                    this.bg(cx.theme().tokens.accent)
                        .text_color(cx.theme().accent_foreground)
                })
                .when_some(self.on_click, |this, on_click| {
                    this.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(on_click)
                })
            })
            .when(self.disabled, |this| {
                this.text_color(cx.theme().muted_foreground)
            })
            .children(self.children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn aria_label_sets_accessible_name(_cx: &mut gpui::TestAppContext) {
        let item = MenuItemElement::new("open", "menu").aria_label("Open");

        assert_eq!(item.aria_label, Some("Open".into()));
    }

}

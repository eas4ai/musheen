//! Capability-aware context menu composition and invocation.
//!
//! This surface deliberately projects `CommandRegistry` entries. It does not
//! decide whether an operation is allowed and never dispatches display paths.

mod builder;
mod context;
mod open_with;
mod send_to;

pub use builder::{
    ContextMenu, MAX_VARIABLE_CONTRIBUTIONS, MenuAccessibilityNode, MenuAccessibleRole, MenuChrome,
    MenuDirection, MenuEntry, MenuEntryKind, MenuFocus, MenuKeyRoute, MenuPresentation,
    MenuThemeTokens,
};
pub use context::{ContextMenuRequest, ContextMenuSource, MenuTarget, PreparedContextTarget};
pub use open_with::OpenWithApplication;
pub use send_to::{SendToDestination, SendToDestinationKind};

use builder::InvocationData;
use musheen_core::{
    CommandDispatchError, CommandDispatcher, CommandParameterContract, CommandParameters,
    CommandRegistry, DangerLevel, StorePath,
};
use std::error::Error;
use std::fmt;

/// A bounded provider or extension contribution for Tags or Actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuContribution {
    label: Box<str>,
    command_id: Box<str>,
}

impl MenuContribution {
    #[must_use]
    pub fn new(label: impl Into<Box<str>>, command_id: impl Into<Box<str>>) -> Self {
        Self {
            label: label.into(),
            command_id: command_id.into(),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }
}

/// The UI-facing composition and invocation seam for context menus.
#[derive(Clone, Debug)]
pub struct ContextMenuSurface {
    registry: CommandRegistry,
    locale: crate::Locale,
    theme: crate::ThemeProfile,
}

impl ContextMenuSurface {
    #[must_use]
    pub const fn new(registry: CommandRegistry) -> Self {
        Self {
            registry,
            locale: crate::Locale::EnUs,
            theme: crate::ThemeProfile::new(crate::AppearanceMode::Light, false),
        }
    }

    #[must_use]
    pub const fn with_locale(mut self, locale: crate::Locale) -> Self {
        self.locale = locale;
        self
    }

    #[must_use]
    pub const fn with_theme_profile(mut self, theme: crate::ThemeProfile) -> Self {
        self.theme = theme;
        self
    }

    #[must_use]
    pub const fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    #[must_use]
    pub fn compose(&self, request: ContextMenuRequest) -> ContextMenu {
        builder::compose(&self.registry, self.locale, self.theme, request)
    }

    #[must_use]
    pub fn prepare_pointer_target(
        &self,
        selection: &[musheen_core::CommandTargetRef],
        clicked: &musheen_core::CommandTargetRef,
    ) -> PreparedContextTarget {
        context::pointer_target(selection, clicked)
    }

    #[must_use]
    pub fn prepare_keyboard_target(
        &self,
        focused: Option<musheen_core::CommandTargetRef>,
    ) -> PreparedContextTarget {
        context::keyboard_target(focused)
    }

    /// Invokes only through the registry handler. Review and destructive rows
    /// return a typed confirmation request; chooser workflows cannot mutate
    /// until a destination has been resolved.
    #[must_use]
    pub fn invoke(
        &self,
        entry: &MenuEntry,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> MenuInvocation {
        let Some(data) = entry.invocation.as_ref() else {
            return MenuInvocation::Rejected(MenuInvocationError::NotInvokable);
        };
        if !entry.state().is_enabled() {
            return MenuInvocation::Rejected(MenuInvocationError::Disabled(
                entry
                    .accessible_disabled_reason()
                    .unwrap_or("the command is unavailable")
                    .into(),
            ));
        }
        if matches!(
            data.action.parameter_contract(),
            CommandParameterContract::DestinationWorkflow(_)
        ) {
            return MenuInvocation::NeedsDestinationChooser(PendingInvocation::from(data));
        }
        let pending = match pending_with_parameters(data.clone()) {
            Ok(pending) => pending,
            Err(error) => return MenuInvocation::Rejected(error),
        };
        self.dispatch_or_confirm(entry.danger_level(), pending, dispatcher)
    }

    /// Cancelling a chooser retains no resolved destination and never dispatches.
    #[must_use]
    pub const fn cancel_destination(&self) -> MenuInvocation {
        MenuInvocation::Cancelled
    }

    /// Resolves a chooser destination through the registry predicate before
    /// routing to dispatch or review.
    #[must_use]
    pub fn resolve_destination(
        &self,
        pending: PendingInvocation,
        destination: StorePath,
        writable: bool,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> MenuInvocation {
        let Some(command) = self.registry.get(pending.id.as_str()) else {
            return MenuInvocation::Rejected(MenuInvocationError::MissingCommand(
                pending.id.as_str().into(),
            ));
        };
        let mut context = pending.context.clone();
        context.resolved_destination = Some(if writable {
            musheen_core::ResolvedDestination::writable(destination.clone())
        } else {
            musheen_core::ResolvedDestination::read_only(
                destination.clone(),
                "the destination is read-only",
            )
        });
        let state = command.state(&context);
        if !state.is_enabled() {
            return MenuInvocation::Rejected(MenuInvocationError::Disabled(
                state
                    .disabled_reason()
                    .unwrap_or("the destination is unavailable")
                    .into(),
            ));
        }
        let pending = PendingInvocation {
            context,
            parameters: CommandParameters::destination(pending.selection.clone(), destination),
            ..pending
        };
        self.dispatch_or_confirm(command.danger_level(), pending, dispatcher)
    }

    pub fn confirm(
        &self,
        invocation: MenuInvocation,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> Result<(), MenuInvocationError> {
        let MenuInvocation::NeedsConfirmation(pending) = invocation else {
            return Err(MenuInvocationError::ConfirmationRequired);
        };
        self.dispatch(pending, dispatcher)
    }

    fn dispatch_or_confirm(
        &self,
        danger: DangerLevel,
        pending: PendingInvocation,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> MenuInvocation {
        if danger != DangerLevel::None {
            return MenuInvocation::NeedsConfirmation(pending);
        }
        match self.dispatch(pending, dispatcher) {
            Ok(()) => MenuInvocation::Dispatched,
            Err(error) => MenuInvocation::Rejected(error),
        }
    }

    fn dispatch(
        &self,
        pending: PendingInvocation,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> Result<(), MenuInvocationError> {
        let command = self
            .registry
            .get(pending.id.as_str())
            .ok_or_else(|| MenuInvocationError::MissingCommand(pending.id.as_str().into()))?;
        command
            .handler()
            .invoke(dispatcher, pending.parameters)
            .map_err(MenuInvocationError::Dispatch)
    }
}

#[derive(Clone, Debug)]
pub struct PendingInvocation {
    id: musheen_core::CommandId,
    context: musheen_core::CommandContext,
    selection: Vec<musheen_core::CommandTargetRef>,
    parameters: CommandParameters,
}

impl From<&InvocationData> for PendingInvocation {
    fn from(value: &InvocationData) -> Self {
        Self {
            id: value.id.clone(),
            context: value.context.clone(),
            selection: value.selection.clone(),
            parameters: CommandParameters::None,
        }
    }
}

fn pending_with_parameters(data: InvocationData) -> Result<PendingInvocation, MenuInvocationError> {
    let parameters = match data.action.parameter_contract() {
        CommandParameterContract::None => CommandParameters::None,
        CommandParameterContract::Location => CommandParameters::Location(data.location.clone()),
        CommandParameterContract::Targets(_) => CommandParameters::targets(data.selection.clone()),
        CommandParameterContract::Destination(_) => CommandParameters::destination(
            data.selection.clone(),
            data.destination
                .clone()
                .ok_or(MenuInvocationError::DestinationRequired)?,
        ),
        CommandParameterContract::DestinationWorkflow(_) => {
            return Err(MenuInvocationError::DestinationRequired);
        }
        CommandParameterContract::CustomAction(_) => CommandParameters::CustomAction {
            targets: data.selection.clone(),
            supports_provider_uris: data.context.supports_provider_uris,
        },
    };
    Ok(PendingInvocation {
        id: data.id,
        context: data.context,
        selection: data.selection,
        parameters,
    })
}

#[derive(Clone, Debug)]
pub enum MenuInvocation {
    Dispatched,
    Cancelled,
    NeedsDestinationChooser(PendingInvocation),
    NeedsConfirmation(PendingInvocation),
    Rejected(MenuInvocationError),
}

impl MenuInvocation {
    #[must_use]
    pub const fn needs_destination_chooser(&self) -> bool {
        matches!(self, Self::NeedsDestinationChooser(_))
    }
    #[must_use]
    pub const fn needs_confirmation(&self) -> bool {
        matches!(self, Self::NeedsConfirmation(_))
    }
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
    #[must_use]
    pub const fn is_dispatched(&self) -> bool {
        matches!(self, Self::Dispatched)
    }
    #[must_use]
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuInvocationError {
    NotInvokable,
    Disabled(Box<str>),
    DestinationRequired,
    ConfirmationRequired,
    MissingCommand(Box<str>),
    Dispatch(CommandDispatchError),
}

impl fmt::Display for MenuInvocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInvokable => formatter.write_str("this menu row is not invokable"),
            Self::Disabled(reason) => formatter.write_str(reason),
            Self::DestinationRequired => formatter.write_str("choose a destination first"),
            Self::ConfirmationRequired => formatter.write_str("the command requires confirmation"),
            Self::MissingCommand(id) => write!(formatter, "missing command {id}"),
            Self::Dispatch(error) => error.fmt(formatter),
        }
    }
}

impl Error for MenuInvocationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dispatch(error) => Some(error),
            _ => None,
        }
    }
}

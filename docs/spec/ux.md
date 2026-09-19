Status: Draft
Prefix: UXF

# Interaction and recovery

This specification owns behavior that crosses the browser, operations,
search, settings, and desktop integration domains. Domain specs continue
to own the underlying action and data.

Review (2026-09-19): traced keyboard, pointer, drag, long-running work,
errors, focus, and restart flows. The requirements avoid hidden mode
changes and preserve a recovery path for destructive or failed actions.

## Focus and selection

[UXF-001]
Status: Draft
The app keeps one visible keyboard-focus target and renders that target
distinctly in every theme.
Falsifier: keyboard input can act on a surface with no visible focus indicator.
Mechanism: keyboard traversal tests plus theme-state screenshot checks.

[UXF-002]
Status: Draft
Selection belongs to one pane and never transfers to another pane merely
because focus changes.
Falsifier: focusing the other pane moves or duplicates the prior selection.
Mechanism: dual-pane selection and focus interaction test.

[UXF-003]
Status: Draft
Pointer, keyboard, and assistive-technology activation invoke the same
registered command for an equivalent action.
Falsifier: one input path bypasses command state or confirmation used by another.
Mechanism: command-binding equivalence tests for browser and operation actions.

## Keyboard model

The default map uses `Ctrl+T`/`Ctrl+W`/`Ctrl+Shift+T` for tab create,
close, and restore; `Alt+Left`/`Alt+Right`/`Alt+Up` for navigation; `F5`
for refresh; `Ctrl+L` for path entry; `Ctrl+F` for search; `F2` for rename;
`Ctrl+C`/`Ctrl+X`/`Ctrl+V` for clipboard operations; `Delete` for trash;
`Shift+Delete` for permanent delete; `Alt+Enter` for Properties; `Ctrl+,`
for Settings; `Shift+F10` or the Menu key for the context menu; and `F4`
for the terminal drawer.

[UXF-004]
Status: Draft
The default key map implements the tab bindings listed above and Ctrl+1
through Ctrl+9 tab selection commands.
Falsifier: a default binding is missing or selects a tab other than the visible ordinal.
Mechanism: key-map schema test plus nine-tab interaction fixture.

[UXF-005]
Status: Draft
The default key map implements the listed navigation, editing, clipboard,
delete, Properties, Settings, context-menu, and terminal commands.
Falsifier: a listed core action lacks a default keyboard route.
Mechanism: key-map completeness test against the registered core command set.

[UXF-006]
Status: Draft
Dialog-local shortcuts take precedence over browser shortcuts while a
modal dialog is active.
Falsifier: a browser mutation runs from a key intended for the active dialog.
Mechanism: modal key-routing tests over rename, conflict, properties, and delete dialogs.

## Feedback and long-running work

[UXF-007]
Status: Draft
Every accepted user action produces a visible state change, progress entry,
or actionable error within 100 ms on the reference test machine; work that
cannot finish in that budget continues asynchronously.
Falsifier: an accepted action provides no visible response within 100 ms.
Mechanism: interaction latency assertions for navigation, mutation, search,
settings, properties, and context-menu actions.

[UXF-008]
Status: Draft
Long-running operations remain available in the status center after their
originating tab or window closes.
Falsifier: closing the initiating surface hides or cancels work without a user choice.
Mechanism: multi-window operation test with origin closure.

[UXF-009]
Status: Draft
Errors state the failed action, affected item or location, known cause,
and available recovery action without exposing secrets.
Falsifier: an operation failure is shown only as a code or generic failure label.
Mechanism: error-content tests over permission, missing, conflict, offline, and timeout failures.

[UXF-010]
Status: Draft
Retry repeats only the failed unit of work unless the user explicitly
chooses to repeat a larger batch.
Falsifier: retry silently repeats items that already completed.
Mechanism: partial-batch failure test with recorded mutation calls.

## Direct manipulation

[UXF-011]
Status: Draft
Drag feedback distinguishes copy, move, link, pin, tag, and rejected drops
before release.
Falsifier: two materially different drop results use the same unexplained feedback.
Mechanism: drag-state tests over content, sidebar, tab, and tag targets.

[UXF-012]
Status: Draft
Context menus show commands for the current target and preserve disabled
commands when their unavailable state explains a capability limit.
Falsifier: a menu offers an inapplicable command or hides a relevant capability limit.
Mechanism: context-menu schema tests across files, directories, backgrounds, mounts, and tags.

[UXF-013]
Status: Draft
Inline rename, omnibar entry, and settings search each expose a consistent
cancel action that restores the state present before entry.
Falsifier: cancelling an edit commits text or loses prior navigation state.
Mechanism: cancellation interaction tests for each inline editing mode.

## Accessibility and continuity

[UXF-014]
Status: Draft
Every interactive control exposes an accessible name, role, state, and
keyboard route through GPUI's accessibility surface.
Falsifier: the accessibility tree contains an unnamed or unreachable interactive control.
Mechanism: accessibility-tree audit for every top-level surface and dialog.

[UXF-015]
Status: Draft
Text and essential controls remain usable at 200 percent interface scale
without clipping required actions.
Falsifier: a required control becomes unreachable or loses its label at 200 percent scale.
Mechanism: layout tests and screenshots at 100, 150, and 200 percent scale.

[UXF-016]
Status: Draft
The app preserves recoverable browsing and queued-operation state after a
controlled restart and reports any state it cannot restore.
Falsifier: restart silently drops restorable work or presents stale work as active.
Mechanism: restart tests during navigation, queued work, running work, and completed work.

## Terminal drawer

[UXF-017]
Status: Draft
F4 toggles the focused window's terminal drawer, moves focus into the terminal
when it opens, and restores the prior browser focus when it closes.
Falsifier: an F4 toggle loses the prior focus target or acts on another window.
Mechanism: keyboard interaction test across two windows and both drawer states.

[UXF-018]
Status: Draft
Right-clicking an unselected item selects only that item before opening its
menu; right-clicking an item already in a multi-selection preserves that
selection. A keyboard-opened menu targets the focused item or the view
background when no item is focused.
Falsifier: opening a menu silently changes an existing multi-selection or
targets an item other than the one visually indicated.
Mechanism: pointer and keyboard menu tests over empty, single, and multi-selection states.

[UXF-019]
Status: Draft
Properties and Settings are non-modal windows that remain usable beside
browser windows. Conflict, authorization, and destructive confirmation
dialogs trap focus until their decision is resolved, then restore prior focus.
Falsifier: an information window blocks browsing or a modal dialog permits
focus to reach the action it guards.
Mechanism: focus traversal tests across all listed surface kinds.

[UXF-020]
Status: Draft
All labels, menus, sort and group names, dates, sizes, and numbers are
localizable. Right-to-left locales mirror directional layout and icons but
do not reverse path component order, terminal content, or file names.
Falsifier: a translated string is clipped at 200 percent scale or RTL
changes the identity or order of path data.
Mechanism: pseudo-localization, Arabic RTL, long-label, and mixed-path tests.

[UXF-021]
Status: Draft
Confirmations for permanent delete, empty trash, replace-tree, privilege,
and destructive remote actions name the command, scope, reversibility, and
affected location before the destructive button receives default focus.
Falsifier: a listed confirmation omits scope or makes its destructive action
the initial keyboard default.
Mechanism: confirmation-content and initial-focus tests.

[UXF-022]
Status: Draft
When an external change invalidates an open dialog or menu target, Musheen
disables submission, explains the change, and offers refresh or close rather
than applying the command to a replacement item at the same display path.
Falsifier: a stale Properties or context-menu command mutates a different item.
Mechanism: stable-identity replacement tests while each surface is open.

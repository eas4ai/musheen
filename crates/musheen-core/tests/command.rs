mod command {
    use musheen_core::{CommandContext, CommandPresentation, CommandRegistry};
    use std::collections::HashSet;

    #[test]
    fn built_ins_have_one_complete_stable_definition() {
        let registry = CommandRegistry::built_in();
        let expected = [
            "navigation.back",
            "navigation.forward",
            "navigation.parent",
            "navigation.refresh",
            "navigation.location",
            "view.search",
            "view.filter",
            "view.command",
            "view.details",
            "view.list",
            "view.cards",
            "view.grid",
            "view.columns",
            "view.adaptive",
            "view.sort",
            "view.group",
            "view.directories_first",
            "view.hidden",
            "view.sidebar",
            "view.info",
            "tab.new",
            "tab.close",
            "tab.duplicate",
            "tab.reopen_closed",
            "tab.move_other_pane",
            "tab.move_left",
            "tab.move_right",
            "tab.tear_out",
            "pane.split",
            "pane.focus_next",
            "selection.select_all",
            "selection.clear",
            "item.properties",
            "app.settings",
        ];
        let actual = registry
            .commands()
            .iter()
            .map(|entry| entry.id().as_str())
            .collect::<HashSet<_>>();

        assert_eq!(actual, expected.into_iter().collect());
        for entry in registry.commands() {
            assert!(!entry.label_key().is_empty());
            assert!(!entry.icon_key().is_empty());
            assert_eq!(entry.handler().action(), entry.action());
            for presentation in CommandPresentation::ALL {
                let projected = registry
                    .project(entry.id(), presentation)
                    .expect("every built-in command can be projected");
                assert!(std::ptr::eq(projected.command(), entry));
            }
        }
    }

    #[test]
    fn predicates_and_shortcuts_are_shared_by_every_presentation() {
        let registry = CommandRegistry::built_in();
        let unavailable = CommandContext::default();
        let available = CommandContext {
            can_go_back: true,
            can_go_forward: true,
            has_parent: true,
            item_count: 4,
            selection_count: 1,
        };

        for (id, shortcut) in [
            ("navigation.back", "Alt+Left"),
            ("navigation.forward", "Alt+Right"),
            ("navigation.parent", "Alt+Up"),
            ("navigation.refresh", "F5"),
            ("navigation.location", "Ctrl+L"),
            ("view.search", "Ctrl+F"),
            ("view.filter", "Ctrl+Shift+F"),
            ("view.command", "Ctrl+Shift+P"),
            ("view.hidden", "Ctrl+H"),
            ("view.details", "Ctrl+1"),
            ("view.list", "Ctrl+2"),
            ("view.cards", "Ctrl+3"),
            ("view.grid", "Ctrl+4"),
            ("view.columns", "Ctrl+5"),
            ("view.adaptive", "Ctrl+6"),
            ("view.sidebar", "Ctrl+B"),
            ("selection.select_all", "Ctrl+A"),
            ("selection.clear", "Escape"),
            ("item.properties", "Alt+Enter"),
            ("app.settings", "Ctrl+,"),
        ] {
            let entry = registry.get(id).expect("the command is registered");
            assert!(
                entry
                    .shortcuts()
                    .iter()
                    .any(|bound| bound.chord() == shortcut)
            );
        }

        assert!(!is_enabled(&registry, "navigation.back", &unavailable));
        assert!(is_enabled(&registry, "navigation.back", &available));
        assert!(!is_enabled(&registry, "selection.select_all", &unavailable));
        assert!(is_enabled(&registry, "selection.select_all", &available));
        assert!(!is_enabled(&registry, "selection.clear", &unavailable));
        assert!(is_enabled(&registry, "selection.clear", &available));
        assert!(is_enabled(&registry, "app.settings", &unavailable));
    }

    #[test]
    fn mutation_entries_are_absent_during_the_read_only_foundation() {
        let registry = CommandRegistry::built_in();
        for id in [
            "edit.cut",
            "edit.paste",
            "file.rename",
            "file.trash",
            "file.delete_permanently",
        ] {
            assert!(registry.get(id).is_none(), "{id} must remain absent");
        }
    }

    fn is_enabled(registry: &CommandRegistry, id: &str, context: &CommandContext) -> bool {
        registry
            .get(id)
            .expect("the command is registered")
            .state(context)
            .is_enabled()
    }
}

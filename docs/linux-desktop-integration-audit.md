# Linux Desktop Integration Audit

Audit date: 2026-09-22

## Service Failure Matrix

Each external service has deterministic absence, delay, disconnect, and restart
coverage. A local `FileManager1` request completes while delayed fakes remain
blocked.

| Service | Evidence |
| --- | --- |
| UDisks2 | `volumes.rs`: `absent_slow_disconnected_and_restarted_services_keep_mounts_usable`; `udisks_zbus.rs`: owner, signal, disconnect, and restart tests |
| Desktop portal | `portals.rs`: `portal_absence_slowness_disconnect_and_restart_do_not_block_file_management`; private-bus cancellation and backend introspection tests |
| Notifications | `notifications.rs`: `notification_absence_slowness_disconnect_and_restart_leave_file_management_usable` |
| Update metadata | `updates.rs`: `update_absence_slowness_disconnect_and_restart_leave_file_management_usable`; signed, expired, tampered, disabled, and delayed fixtures |
| Secret Service | `secrets.rs`: `secret_service_absence_slowness_disconnect_and_restart_leave_file_management_usable`; `secrets_zbus.rs`: owner replacement and restart tests |
| Polkit | `polkit_zbus.rs`: typed absence, pending-prompt cancellation, disconnect, and restart tests |
| FileManager1 | `file_manager1.rs`: bounded acknowledgement, missing path, private-bus introspection, and restart tests |

## Trust-Boundary Review

| Boundary | Required invariant | Evidence |
| --- | --- | --- |
| Process launch | Programs and arguments remain separate OS strings. No user value enters a shell command. Working directories remain process metadata. | `launch.rs` malicious field-code fixtures; `terminal.rs` hostile argument and working-directory tests |
| URI conversion | Only local `file://` URIs cross FileManager1 and portal boundaries. Limits are checked before decoding. Unix names retain exact bytes. | `file_manager1.rs`: malformed, oversized, missing, and non-UTF-8 URI tests; portal document-grant fixtures |
| D-Bus identity | Actions bind to the current service owner, object identity, and bounded request acknowledgement. Replacement or stale objects fail closed. | FileManager1, UDisks2, Secret Service, and Polkit private-bus suites |
| Portal routing | The client rejects its own destination, binds the returned request path, closes cancelled requests, and ignores late confirmation. | `portals.rs` client/backend production fixtures |
| Credentials | Settings and journals contain references only. Plaintext buffers redact `Debug` output and wipe app-owned bytes. Owner changes and ambiguous items fail closed. | `secrets.rs` and `secrets_zbus.rs`; repository credential scan |
| Privilege | The broker accepts a closed operation schema, binds request subject and digest, reopens targets after authorization, confines rooted browsing, and scrubs the environment. | `privilege.rs`, `polkit_zbus.rs`, and `elevated_browser.rs` |

Ripwire structural triage found no unsafe C calls, C-style casts, or weak
cryptography in the workspace. Accessibility coverage verifies localized names
for the terminal, elevated browser, volume dialogs, notification action, and
authorization UI in English, pseudo-English, and Arabic. The visual suite
checks the state gallery and light, dark, high-contrast, narrow, and scaled
baselines.

## Release Gates

The closeout gate runs formatting, strict all-target/all-feature Clippy, the
full serial workspace suite, a locked release build, `cargo deny check`, D-Bus
introspection fixtures, accessibility tests, and visual baselines. All Cargo
commands use the repository's locked dependency graph.

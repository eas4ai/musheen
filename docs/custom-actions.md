# Custom actions

Settings → Advanced owns action definitions. Set a stable ID, label, absolute
executable, argument array, applicable MIME types, optional folder prefix, working
directory, environment names, timeout, and confirmation policy. Add or update
edits the Settings draft; Apply commits atomically and Cancel discards the draft.
Actions appear under the registry's Actions submenu after a background eligibility
check. The open submenu updates when the check completes.

Arguments are a JSON array of strings. Each placeholder occupies one complete
argument: `{file}` requires one local file; `{files}` expands to separate local
path arguments; `{directory}` supplies the current local folder; `{uris}` supplies
percent-encoded file URIs or explicitly supported provider URIs. Literal arguments
remain literal. Paths remain lossless OS values, including non-UTF-8 names. Use a
literal `--` when the target program accepts an option terminator.

Shell mode is a separately labeled opt-in. Its script is constant: file arguments
are supplied as positional parameters after the script, never interpolated into
source. Quote positional expansions (`"$1"`, `"$@"`). Shell actions always require
confirmation. Direct actions may choose Never, Always, or Destructive confirmation.
Every action runs with the user's permissions. The environment is cleared before
adding selected allowlisted names: LANG, LC_ALL, LC_CTYPE, TZ, DISPLAY,
WAYLAND_DISPLAY, XDG_RUNTIME_DIR, HOME. Programs are absolute paths; PATH and loader
variables are not inherited.

The runner uses null standard streams and a new process group. Timeout kills the
group and reaps the direct child. Missing executables, nonzero exits, and timeouts
are shown in the operation error surface. At most four actions run per browser
window. Targets and the action definition are revalidated after confirmation.

## Script directory

The separate Advanced switch enables `$XDG_CONFIG_HOME/musheen/actions` (normally
`~/.config/musheen/actions`). Each action requires a `*.musheen-action.json`
manifest. Use Reload script-directory actions after changing its contents. An
absent directory contributes nothing. Create script directory makes the displayed
configuration folder with owner-only permissions. Scripts are loaded on a
background worker and revalidated before execution. User-defined
IDs take precedence over conflicting script IDs, with an error explaining the
conflict. Remaining slots in the combined 64-action / 256-KiB limit are filled by
script ID order; overflow reports a warning while accepted actions stay available.
Disable the switch to remove all script contributions.

Example manifest for an executable named `inspect` in that directory:

```json
{
  "version": 1,
  "id": "inspect-script",
  "label": "Inspect selection",
  "script": "inspect",
  "arguments": [{"kind": "files"}],
  "working_directory": {"policy": "current_location"},
  "mime_patterns": ["*/*"],
  "location_prefix": null,
  "supports_provider_uris": false,
  "confirmation": "always",
  "environment": ["LANG"],
  "timeout_ms": 30000
}
```

For a non-UTF-8 script name, `script` accepts `{"unix_bytes": [105, 255]}`.
Symlinks, non-regular files, non-executable scripts, unknown fields, and resource
limit violations fail closed. The loader caps directory entries, individual and
total manifest bytes, script sizes, and action count. Script manifests support
direct execution only; shell interpreters belong in the script's shebang.

## Persistence

Settings schema 3 stores action documents as compact version-1 JSON. Earlier
Boolean custom-action switches migrate to an empty action document. Unknown
Settings fields survive migration. Configuration paths use the existing
StorePath Unix-byte representation, so export and fingerprints remain lossless.
The text editor refuses a non-UTF-8 configured path instead of rewriting it;
its lossless document can be edited externally.

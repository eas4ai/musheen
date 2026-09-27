#!/usr/bin/env python3
"""Check release exceptions and single-owner dependency roles from locked inputs."""

import datetime
import pathlib
import re
import sys
import tomllib


ROLE_CRATES = frozenset(
    {
        "gpui-component",
        "gpui-kit",
        "gpui-pre",
        "native-theme",
        "native-theme-gpui",
        "sevenz-rust2",
    }
)
EXPIRY = re.compile(r"\bExpires (\d{4}-\d{2}-\d{2})\b")


def check(repository: pathlib.Path) -> list[str]:
    with (repository / "deny.toml").open("rb") as source:
        policy = tomllib.load(source)
    with (repository / "Cargo.lock").open("rb") as source:
        lockfile = tomllib.load(source)

    errors = []
    for exception in policy.get("advisories", {}).get("ignore", []):
        if not isinstance(exception, dict):
            errors.append(f"advisory exception {exception!r} needs a dated rationale")
            continue
        advisory = exception.get("id", "<missing id>")
        reason = exception.get("reason", "")
        match = EXPIRY.search(reason)
        if not match or len(reason.strip()) < 40:
            errors.append(f"{advisory} needs a substantive rationale and Expires YYYY-MM-DD")
            continue
        try:
            expiry = datetime.date.fromisoformat(match.group(1))
        except ValueError:
            errors.append(f"{advisory} has an invalid expiry date")
            continue
        if expiry < datetime.date.today():
            errors.append(f"{advisory} exception expired on {expiry}")

    versions: dict[str, set[str]] = {}
    for package in lockfile.get("package", []):
        name = package.get("name")
        if name in ROLE_CRATES:
            versions.setdefault(name, set()).add(package.get("version", "<missing>"))
    for name, selected in versions.items():
        if len(selected) > 1:
            errors.append(f"duplicate role crate {name}: {', '.join(sorted(selected))}")
    return errors


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: verify-dependency-policy.py REPOSITORY")
    problems = check(pathlib.Path(sys.argv[1]))
    for error in problems:
        print(error, file=sys.stderr)
    if problems:
        raise SystemExit(1)
    print("Dated advisory exceptions and role crates verified")

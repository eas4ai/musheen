#!/usr/bin/env python3
"""Generate a deterministic CycloneDX inventory and license notice from Cargo.lock."""

import json
import pathlib
import subprocess
import sys
import tomllib
from urllib.parse import quote


def package_ref(package: dict) -> str:
    name = quote(package["name"], safe="-._~")
    version = quote(package["version"], safe="-._~")
    return f"pkg:cargo/{name}@{version}"


def component(package: dict, locked: dict, *, root: bool = False) -> dict:
    license_expression = package.get("license")
    if not license_expression:
        raise ValueError(f"{package['name']} {package['version']} has no license expression")
    result = {
        "type": "application" if root else "library",
        "bom-ref": package_ref(package),
        "name": package["name"],
        "version": package["version"],
        "purl": package_ref(package),
        "licenses": [{"expression": license_expression}],
    }
    if checksum := locked.get("checksum"):
        result["hashes"] = [{"alg": "SHA-256", "content": checksum}]
    return result


def generate(repository: pathlib.Path) -> tuple[dict, str]:
    lockfile_path = repository / "Cargo.lock"
    lockfile_bytes = lockfile_path.read_bytes()
    lockfile = tomllib.loads(lockfile_bytes.decode("utf-8"))
    locked = {}
    for package in lockfile["package"]:
        key = (package["name"], package["version"])
        if key in locked:
            raise ValueError(f"ambiguous locked package {key[0]} {key[1]}")
        locked[key] = package

    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(repository / "Cargo.toml"),
            "--format-version",
            "1",
            "--locked",
            "--offline",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    if lockfile_path.read_bytes() != lockfile_bytes:
        raise ValueError("Cargo.lock changed during SBOM generation")
    metadata = json.loads(result.stdout)
    packages = metadata["packages"]
    by_id = {package["id"]: package for package in packages}
    root_id = metadata["resolve"]["root"]
    if root_id is None:
        raise ValueError("Cargo metadata did not identify the application root")
    if len({package_ref(package) for package in packages}) != len(packages):
        raise ValueError("Cargo graph has duplicate package URLs")

    components = []
    notice_rows = []
    for package in packages:
        key = (package["name"], package["version"])
        if key not in locked:
            raise ValueError(f"resolved package absent from Cargo.lock: {key[0]} {key[1]}")
        entry = component(package, locked[key], root=package["id"] == root_id)
        if package["id"] != root_id:
            components.append(entry)
            source = package.get("source") or "local"
            if source.startswith("registry+"):
                source_label = "crates.io"
            elif source.startswith("git+"):
                source_label = "Git"
            else:
                source_label = "workspace/local"
            notice_rows.append(
                (package["name"], package["version"], package["license"], source_label)
            )

    dependencies = []
    for node in metadata["resolve"]["nodes"]:
        dependencies.append(
            {
                "ref": package_ref(by_id[node["id"]]),
                "dependsOn": sorted(package_ref(by_id[edge["pkg"]]) for edge in node["deps"]),
            }
        )
    dependencies.sort(key=lambda item: (item["ref"] != package_ref(by_id[root_id]), item["ref"]))
    components.sort(key=lambda item: item["bom-ref"])
    notice_rows.sort()
    root_package = by_id[root_id]
    root_lock = locked[(root_package["name"], root_package["version"])]
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {"component": component(root_package, root_lock, root=True)},
        "components": components,
        "dependencies": dependencies,
    }
    notice = (
        "# Third-Party License Notice\n\n"
        "Generated from the locked Cargo dependency graph. This inventory lists license "
        "expressions; upstream distributions provide the complete license texts.\n\n"
        "| Package | Version | License | Source |\n"
        "| --- | --- | --- | --- |\n"
        + "".join(
            f"| {name} | {version} | {license_expression} | {source} |\n"
            for name, version, license_expression, source in notice_rows
        )
    )
    return bom, notice


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: generate-sbom.py REPOSITORY OUTPUT_DIRECTORY")
    repository = pathlib.Path(sys.argv[1]).resolve()
    output_dir = pathlib.Path(sys.argv[2])
    bom, notice = generate(repository)
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "musheen.cdx.json").write_text(
        json.dumps(bom, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / "THIRD_PARTY_LICENSES.md").write_text(notice, encoding="utf-8")
    print(f"Generated {len(bom['components'])} locked dependency components")

"""Enforce production dependency boundaries and shared lint settings."""

import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BOUNDARIES = {
    "agentdrive-model": set(),
    "agentdrive-ports": {"agentdrive-model"},
    "agentdrive-config": {"agentdrive-model"},
    "agentdrive-engine": {"agentdrive-model", "agentdrive-ports", "agentdrive-config"},
    "agentdrive-local": {"agentdrive-model", "agentdrive-ports"},
    "agentdrive-s3": {"agentdrive-model", "agentdrive-ports", "agentdrive-config"},
    "agentdrive-platform": {"agentdrive-model", "agentdrive-ports"},
    "agentdrive-mcp": {"agentdrive-model", "agentdrive-ports", "agentdrive-config"},
    "agentdrive-test-support": {"agentdrive-model", "agentdrive-ports"},
}
ADAPTER_DEPENDENCIES = {
    "rusqlite", "object_store", "reqwest", "fuser", "winfsp_wrs", "axum", "rmcp",
}


def main():
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version=1", "--locked"],
        cwd=ROOT, text=True, encoding='utf-8',
    ))
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members]
    names = {p["name"] for p in packages}
    failures = []
    for package in packages:
        name = package["name"]
        manifest = Path(package["manifest_path"])
        text = manifest.read_text(encoding="utf-8")
        if not re.search(r"(?m)^\[lints\]\s*\nworkspace\s*=\s*true\s*$", text):
            failures.append(f"{name}: inherit workspace lint settings")
        if manifest.parent.parent.name == "crates" and name not in BOUNDARIES:
            failures.append(f"{name}: declare its production dependency boundary")
        if name not in BOUNDARIES:
            continue
        for dependency in package["dependencies"]:
            if dependency["kind"] == "dev":
                continue
            target = dependency["name"]
            if target in names and target not in BOUNDARIES[name]:
                failures.append(f"{name}: unexpected production dependency on {target}")
            if name in {"agentdrive-engine", "agentdrive-model", "agentdrive-ports"} and target in ADAPTER_DEPENDENCIES:
                failures.append(f"{name}: {target} belongs in an adapter")
    for script in sorted((ROOT / "tests").glob("*.py")):
        compile(script.read_text(encoding="utf-8"), str(script), "exec")
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"Verified module boundaries and shared lints for {len(packages)} packages; Python syntax passed")


if __name__ == "__main__":
    main()

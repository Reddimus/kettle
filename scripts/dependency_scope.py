"""Fail-closed validation of a reviewed all-target reverse dependency graph."""

from typing import Any


def metadata_command() -> list[str]:
    """Resolve the committed, all-feature graph for every target platform.

    `cargo metadata` includes target-specific dependencies for every platform
    unless `--filter-platform` is supplied. Deliberately do not add that flag:
    the audit exception is global, so a Windows- or macOS-only consumer must be
    visible when this guard runs on Linux CI.
    """

    return [
        "cargo",
        "metadata",
        "--locked",
        "--all-features",
        "--format-version",
        "1",
    ]


def scope_errors(
    metadata: dict[str, Any],
    seeds: set[str],
    expected_edges: set[tuple[str, str]],
    reviewed_versions: dict[str, set[str]],
    reviewed_sources: dict[str, set[str | None]],
    workspace_packages: set[str],
) -> list[str]:
    """Return every reason the resolved reverse graph needs re-review."""

    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        return ["cargo metadata omitted packages or the resolved graph"]
    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        return ["cargo metadata omitted resolved nodes"]
    workspace_members = metadata.get("workspace_members")
    if not isinstance(workspace_members, list) or not all(
        isinstance(package_id, str) for package_id in workspace_members
    ):
        return ["cargo metadata omitted valid workspace members"]
    workspace_member_ids = set(workspace_members)

    identities: dict[str, tuple[str, str, str | None]] = {}
    for package in packages:
        if not isinstance(package, dict):
            continue
        package_id = package.get("id")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        if all(isinstance(value, str) for value in (package_id, name, version)):
            if source is not None and not isinstance(source, str):
                return [f"package has malformed source identity: {package_id}"]
            identities[package_id] = (name, version, source)

    reverse: dict[str, set[str]] = {}
    malformed = False
    for node in nodes:
        if not isinstance(node, dict) or not isinstance(node.get("id"), str):
            malformed = True
            continue
        consumer = node["id"]
        deps = node.get("deps")
        if not isinstance(deps, list):
            malformed = True
            continue
        for dependency in deps:
            dependency_id = (
                dependency.get("pkg") if isinstance(dependency, dict) else None
            )
            if not isinstance(dependency_id, str):
                malformed = True
                continue
            reverse.setdefault(dependency_id, set()).add(consumer)
    if malformed:
        return ["cargo metadata contained malformed resolved nodes"]

    seed_ids = {
        package_id for package_id, value in identities.items() if value[0] in seeds
    }
    if not seed_ids:
        return ["reviewed dependencies are absent; remove obsolete advisory exceptions"]

    present = {identities[package_id][0] for package_id in seed_ids}
    if present != seeds:
        return [
            f"reviewed dependencies changed: expected {sorted(seeds)}, got {sorted(present)}"
        ]

    reached = set(seed_ids)
    pending = list(seed_ids)
    edges: set[tuple[str, str]] = set()
    while pending:
        dependency_id = pending.pop()
        dependency = identities.get(dependency_id)
        if dependency is None:
            return [f"resolved dependency has no package record: {dependency_id}"]
        for consumer_id in reverse.get(dependency_id, set()):
            consumer = identities.get(consumer_id)
            if consumer is None:
                return [f"resolved consumer has no package record: {consumer_id}"]
            edges.add((consumer[0], dependency[0]))
            if consumer_id not in reached:
                reached.add(consumer_id)
                pending.append(consumer_id)

    errors: list[str] = []
    if edges != expected_edges:
        render = lambda edge: f"{edge[0]} -> {edge[1]}"
        missing = sorted(expected_edges - edges)
        extra = sorted(edges - expected_edges)
        if missing:
            errors.append("missing reviewed edges: " + ", ".join(map(render, missing)))
        if extra:
            errors.append("new reverse edges: " + ", ".join(map(render, extra)))

    reached_versions: dict[str, set[str]] = {}
    reached_ids: dict[str, set[str]] = {}
    for package_id in reached:
        name, version, _source = identities[package_id]
        reached_versions.setdefault(name, set()).add(version)
        reached_ids.setdefault(name, set()).add(package_id)
    for name, package_ids in reached_ids.items():
        if len(package_ids) != 1:
            errors.append(
                f"multiple resolved packages named {name} occur on the reviewed path: "
                + ", ".join(sorted(package_ids))
            )
    for name, expected in reviewed_versions.items():
        actual = reached_versions.get(name, set())
        if actual != expected:
            errors.append(
                f"{name} versions changed: expected {sorted(expected)}, got {sorted(actual)}"
            )
    reached_sources: dict[str, set[str | None]] = {}
    for package_id in reached:
        name, _version, source = identities[package_id]
        reached_sources.setdefault(name, set()).add(source)
    for name, expected in reviewed_sources.items():
        actual = reached_sources.get(name, set())
        if actual != expected:
            errors.append(
                f"{name} sources changed: expected {sorted(expected)}, "
                f"got {sorted(repr(source) for source in actual)}"
            )
    for name in workspace_packages:
        package_ids = reached_ids.get(name, set())
        if len(package_ids) == 1 and not package_ids <= workspace_member_ids:
            errors.append(
                f"{name} is no longer the reviewed workspace package: "
                + ", ".join(sorted(package_ids))
            )
    return errors

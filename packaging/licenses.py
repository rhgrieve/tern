#!/usr/bin/env python3
"""Prints the licenses of everything that is compiled into the programs."""

import json
import pathlib
import subprocess

TARGET = "x86_64-unknown-linux-gnu"
metadata = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET]
    )
)
own = set(metadata["workspace_members"])
packages = {package["id"]: package for package in metadata["packages"]}

# What is only needed to build or to test is not in the programs.
used, todo = set(), list(own)
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
while todo:
    name = todo.pop()
    if name in used:
        continue
    used.add(name)
    for dependency in nodes[name]["deps"]:
        if any(kind["kind"] is None for kind in dependency["dep_kinds"]):
            todo.append(dependency["pkg"])

print("Tern contains these libraries, under their own licenses.")
for package in sorted((packages[name] for name in used - own), key=lambda p: p["name"]):
    print(f"\n{'=' * 78}\n{package['name']} {package['version']}  ({package['license']})")
    if package["repository"]:
        print(package["repository"])
    folder = pathlib.Path(package["manifest_path"]).parent
    for path in sorted(folder.iterdir()):
        if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")):
            print(f"\n--- {path.name}\n{path.read_text(errors='replace').strip()}")

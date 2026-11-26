#!/bin/sh
# Puts a release build into dist/tern-VERSION-x86_64.tar.gz.
set -eu
cd "$(dirname "$0")/.."
version=$(cargo metadata --format-version 1 --no-deps --locked |
    python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tern"))')
name=tern-$version-x86_64
rm -rf "dist/$name"
mkdir -p "dist/$name/bin" "dist/$name/share"
for program in ternd tern tern-cli tern-login tern-call; do
    install -m755 "target/release/$program" "dist/$name/bin/"
    strip "dist/$name/bin/$program"
done
cp -r packaging/desktop packaging/dbus packaging/systemd "dist/$name/share/"
install -m755 packaging/install.sh packaging/uninstall.sh "dist/$name/"
cp README.md LICENSE "dist/$name/"
cp vendor/engine/LICENSE "dist/$name/LICENSE.rusty-teams"
python3 packaging/licenses.py >"dist/$name/LICENSES.libraries"
tar -C dist --owner=0 --group=0 --numeric-owner -czf "dist/$name.tar.gz" "$name"
cd dist
sha256sum "$name.tar.gz" >"$name.tar.gz.sha256"
echo "dist/$name.tar.gz"

#!/bin/sh
# Fetches the prebuilt WebRTC that calls are built against, checks it, and unpacks it where
# .cargo/config.toml looks. The build itself never downloads it.
set -eu

TAG=webrtc-89d790b
NAME=webrtc-linux-x64-release
SHA256=b167adad5291cea0e4d66a0454d9d52d2ad714e6b0ed70f4410317d3ebde70c5

cd "$(dirname "$0")/.."
DIR=target/phase5/webrtc
if [ -d "$DIR/linux-x64-release/lib" ]; then
    echo "WebRTC is already in $DIR"
    exit 0
fi
mkdir -p "$DIR"
curl --fail --location --output "$DIR/$NAME.zip" \
    "https://github.com/livekit/rust-sdks/releases/download/$TAG/$NAME.zip"
echo "$SHA256  $DIR/$NAME.zip" | sha256sum --check
unzip -q "$DIR/$NAME.zip" -d "$DIR"
rm "$DIR/$NAME.zip"
echo "WebRTC is in $DIR"

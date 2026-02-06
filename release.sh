#!/bin/bash
set -e
VERSION=$1
if [ -z "$VERSION" ]; then
    echo "Usage: ./release.sh <version> (e.g., 1.0.0)"
    exit 1
fi
cargo update
cargo set-version $VERSION
cargo check
git add Cargo.toml Cargo.lock RELEASE_NOTES.md
git commit -m "Update to v$VERSION"
git push
git tag -a "v$VERSION" -m "Release v$VERSION"
git push origin "v$VERSION"
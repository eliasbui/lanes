#!/bin/sh
# Releases a version: bumps it, builds the macOS app (Apple silicon and Intel in one) and the Windows app here,
# tags and pushes, publishes the GitHub release with both zips, and points the Homebrew cask at the macOS one.
#
#   scripts/release.sh 0.2.0
#   NO_WINDOWS=1 scripts/release.sh 0.2.0   # macOS only, when the Windows cross-tools are not set up
set -eu
VERSION=${1:?usage: scripts/release.sh <version, such as 0.2.0>}
cd "$(dirname "$0")/.."

echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "a version is three numbers, such as 0.2.0" >&2; exit 1; }
[ "$(uname)" = Darwin ] || { echo "the macOS app is built here, so a release is made on a Mac" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "commit your changes first" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null && { echo "v$VERSION is already tagged" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "sign in to GitHub first: gh auth login" >&2; exit 1; }
[ "${NO_WINDOWS:-}" = 1 ] || CHECK=1 scripts/package-windows.sh

sed -i.bak "1,/^version = /s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml && rm Cargo.toml.bak
# The extension the app carries goes up with it, so a browser loading it unpacked takes the new service worker too.
sed -i.bak "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" extension/manifest.json && rm extension/manifest.json.bak
cargo update --workspace --offline -q 2>/dev/null || true

# Built and tested before anything is pushed, so a broken build never leaves a tag behind.
cargo test --quiet
UNIVERSAL=1 scripts/bundle-macos.sh
ZIP="dist/Lanes-$VERSION-macos.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "dist/Lanes.app" "$ZIP"
WINZIP=""
if [ "${NO_WINDOWS:-}" != 1 ]; then
    scripts/package-windows.sh
    WINZIP="dist/Lanes-$VERSION-windows-x64.zip"
fi

git diff --quiet Cargo.toml Cargo.lock extension/manifest.json || git commit -q -m "Release $VERSION" Cargo.toml Cargo.lock extension/manifest.json
git tag "v$VERSION"
git push origin HEAD "v$VERSION"

# shellcheck disable=SC2086
gh release create "v$VERSION" "$ZIP" $WINZIP --title "v$VERSION" --generate-notes
echo "published v$VERSION"

# What the app reads to find this release (src/update.rs): the API's answer in shape, fetched through github.com's
# download links, which the API's hourly limit for a whole network does not touch.
asset() {
    printf '{"name": "%s", "size": %s, "digest": "sha256:%s", "browser_download_url": "https://github.com/eliasbui/lanes/releases/download/v%s/%s"}' \
        "$(basename "$1")" "$(stat -f %z "$1")" "$(shasum -a 256 "$1" | cut -d ' ' -f 1)" "$VERSION" "$(basename "$1")"
}
NOTES=$(gh release view "v$VERSION" --json body --jq '.body | tojson')
ASSETS="$(asset "$ZIP")"
[ -n "$WINZIP" ] && ASSETS="$ASSETS, $(asset "$WINZIP")"
printf '{"tag_name": "v%s", "html_url": "https://github.com/eliasbui/lanes/releases/tag/v%s", "body": %s, "assets": [%s]}\n' \
    "$VERSION" "$VERSION" "$NOTES" "$ASSETS" > dist/latest.json
gh release upload "v$VERSION" dist/latest.json

scripts/update-cask.sh "$VERSION" "$ZIP" ||
    echo "the Homebrew cask was not updated; run scripts/update-cask.sh $VERSION $ZIP once the tap can be pushed to" >&2

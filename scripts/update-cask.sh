#!/bin/sh
# Points the Homebrew cask in the tap (eliasbui/homebrew-lanes) at a release: its version and the SHA-256 of its
# macOS zip. scripts/release.sh runs it after publishing; it can also be run by hand.
#
#   scripts/update-cask.sh 0.2.0              # downloads the zip of the GitHub release
#   scripts/update-cask.sh 0.2.0 dist/x.zip   # or takes one already here
#   DRY_RUN=1 scripts/update-cask.sh 0.2.0    # writes the cask and shows it, pushes nothing
set -eu

VERSION=${1:?usage: scripts/update-cask.sh <version> [zip]}
ZIP=${2:-}
REPO=eliasbui/lanes
TAP=eliasbui/homebrew-lanes
ROOT=$(cd "$(dirname "$0")/.." && pwd)

fail() { echo "update-cask: $*" >&2; exit 1; }

echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail "a version is three numbers, such as 0.2.0"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if [ -z "$ZIP" ]; then
    gh release download "v$VERSION" --repo "$REPO" --pattern "Lanes-$VERSION-macos.zip" --dir "$WORK" ||
        fail "no macOS zip in release v$VERSION"
    ZIP="$WORK/Lanes-$VERSION-macos.zip"
fi
[ -f "$ZIP" ] || fail "$ZIP does not exist"
SHA=$( (shasum -a 256 "$ZIP" 2>/dev/null || sha256sum "$ZIP") | cut -d ' ' -f 1)

CASK="$WORK/lanes.rb"
sed -e "s/^  version \".*\"/  version \"$VERSION\"/" -e "s/^  sha256 \".*\"/  sha256 \"$SHA\"/" \
    "$ROOT/packaging/homebrew/lanes.rb" >"$CASK"
grep -q "version \"$VERSION\"" "$CASK" && grep -q "sha256 \"$SHA\"" "$CASK" || fail "could not write the version into the cask"
if [ "${DRY_RUN:-}" = 1 ]; then
    cat "$CASK"
    exit 0
fi

gh repo view "$TAP" >/dev/null 2>&1 ||
    fail "the tap $TAP does not exist yet; create it once with: gh repo create $TAP --public"
gh repo clone "$TAP" "$WORK/tap" -- -q
mkdir -p "$WORK/tap/Casks"
cp "$CASK" "$WORK/tap/Casks/lanes.rb"
cp "$ROOT/packaging/homebrew/README.md" "$WORK/tap/README.md"
cd "$WORK/tap"
git add -A
if git diff --cached --quiet; then
    echo "the cask already points at $VERSION"
    exit 0
fi
git commit -q -m "Update lanes to $VERSION"
git push -q origin HEAD
echo "the cask points at $VERSION ($SHA)"

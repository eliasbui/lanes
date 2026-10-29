# Homebrew tap for Lanes

[Lanes](https://github.com/eliasbui/lanes) is an open multi-connection download manager for macOS: it splits each
file across many connections, resumes after a stop, and takes downloads from the browser through its extension. The
macOS build is universal (Apple silicon and Intel) and needs macOS 13 or later.

    brew tap eliasbui/lanes
    brew install --cask eliasbui/lanes/lanes

Writing the cask out in full installs it from the tap and trusts only that cask, so no `brew trust` is needed. The
app is signed ad hoc and not notarized, so the cask removes the quarantine flag after installing so macOS will open
it.

Update with `brew upgrade --cask lanes`; the app also tells you when a new version is out and installs it itself.
`brew uninstall --cask lanes` removes the app; add `--zap` to delete the download list in
`~/Library/Application Support/Lanes` too.

This tap is updated automatically each time Lanes publishes a release, by `scripts/update-cask.sh` in the app repo.

# rz

`rz` saves and restores native [Ghostty](https://ghostty.org/) workspaces on
macOS. A snapshot records windows, tabs, terminal surfaces, working directories,
focus, window geometry, non-empty scrollback, and Codex conversation IDs.

Restoring creates the complete replacement workspace before closing the old
Ghostty windows. Use `--keep-existing` when you want an additive restore instead.

## Install

```sh
brew tap marianposaceanu/tap
brew install rz
```

`rz` requires macOS, Ruby 2.7 or newer, and a recent Ghostty release with its
AppleScript API enabled:

```ini
macos-applescript = true
```

Grant Accessibility permission to the terminal that runs `rz` if you want it to
restore window geometry and confirm Ghostty's **Close Window?** sheet.

## Use

```sh
rz --save work                  # save a named workspace
rz --save work --no-scrollback  # faster save without terminal history
rz --save work --current-window # save only the front Ghostty window

rz                              # restore the newest snapshot
rz --session work               # restore the newest snapshot named work
rz --session work --dry-run     # preview a restore
rz --keep-existing              # restore without closing existing windows

rz --list                       # list snapshots
rz --list 3                     # inspect snapshot number 3
rz --clean                      # remove snapshots older than seven days
rz --clean 30d                  # choose another maximum age
```

Start a shell-scoped automatic snapshot watcher with:

```sh
rz --watch backup --every 15m
rz --watch-status
rz --watch-stop
```

The watcher stops when its owning shell exits. It saves the current Ghostty
window by default; pass `--all-windows` to save the complete app.

Snapshots and watcher state live under
`~/.local/state/ghostty-rz`. Override that location with `RZ_STATE_DIR`.

## Limits

Ghostty exposes terminal surfaces but not its split tree or pane proportions, so
additional surfaces restore as right-hand splits. Scrollback is replayed as text,
not as a running process. `rz` can resume detected Codex conversations by their
exact IDs, but it cannot reconstruct arbitrary programs.

## Development

The test suite uses only Ruby's standard library:

```sh
ruby -c bin/rz
ruby test/rz_test.rb
```

## License

[MIT](LICENSE)

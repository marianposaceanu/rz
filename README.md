# rz

`rz` saves and restores native [Ghostty](https://ghostty.org/) workspaces on
macOS. A snapshot records windows, tabs, terminal surfaces, split directions and
pane bounds, working directories, focus, window geometry, non-empty scrollback,
Codex conversation IDs, Amp thread
IDs, Claude Code session IDs, and Pi sessions.

`rz` is a small native executable written in Rust. Snapshots created by the
earlier Ruby releases remain fully compatible.

Restoring creates the complete replacement workspace before closing the old
Ghostty windows. Use `--keep-existing` when you want an additive restore instead.

## Install

Install with [Homebrew](https://brew.sh/) from
[Marian’s tap](https://github.com/marianposaceanu/homebrew-tap):

```sh
brew tap marianposaceanu/tap
brew install rz
```

To upgrade to the latest release:

```sh
brew update
brew upgrade rz
```

`rz` requires macOS and a recent Ghostty release with its AppleScript API
enabled:

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
rz --version                    # print the installed version
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

Every output header shows the running `rz` version. Save output and `--list`
include snapshot sizes and total storage on disk, compact agent counts, and pane
counts per tab. `--list NUMBER` shows the saved pane bounds and creating version.

### Scrollback storage

Scrollback is compressed with fast LZ4 by default. Watchers skip scrollback for
fast automatic saves. Compression and playback are built into `rz`; no external compressor
is required. Files use the standard LZ4 frame format with a content checksum.
The save summary shows the original and compressed sizes. Snapshot metadata stays
readable in `state.json` alongside `scrollback/*.txt.lz4` files.

Older snapshots with plain-text scrollback remain restorable. `--no-scrollback`
continues to skip terminal history entirely. Pi, Codex, Amp, and Claude conversation
files remain in their own storage locations; snapshot sizes do not include them.

### Split layouts

For tabs with multiple panes, `rz` reads pane bounds through macOS Accessibility,
reconstructs left/right and top/bottom splits, and adjusts each divider while
restoring. Nested and mixed layouts are supported. Capture temporarily selects
tabs to measure their panes, then restores the original selection.

Grant Accessibility permission to the terminal running `rz`. Unzoom panes before
saving so every pane can be measured. Exact pane bounds can be reproduced at the
saved window size; changed displays, window constraints, and Ghostty's minimum
pane sizes can prevent a match. `rz` reports unavailable layouts and failed divider
adjustments. Snapshots created before this support lack split geometry; save a
fresh snapshot to preserve the layout.

When a terminal is running Codex, [Amp](https://ampcode.com/), Claude Code, or [Pi](https://pi.dev/),
`rz` associates the process with that exact Ghostty terminal. Restore uses
`codex resume` for a Codex conversation, `amp threads continue` for an Amp
thread, `claude --resume` for a Claude Code session, and `pi --session` with the
exact saved session file for Pi. If the same session is
still running during an additive restore, `rz` opens a plain shell instead of
starting a duplicate.

Only interactive terminal CLIs are detected. Codex conversations are looked up in
`~/.codex/state_*.sqlite` and matched to the running TUIs as a set: a thread
created when the process started, then per directory in start order against the
most recently updated threads, then, for a session resumed from another
directory, the newest thread touched since the process started, which is only
attached to a terminal whose title is that thread's directory name. Subagent and
`codex exec` threads are skipped, a thread named on a command line (`codex resume
<id>`) is reserved for that process, and `-C` is honoured. The older open rollout
file remains a fallback. Claude Code sessions come from
`~/.claude/sessions/<pid>.json`; background sessions and sessions started by an
IDE or the SDK are ignored.

### Pi sessions

Install the bundled Pi extension once:

```sh
rz --install-pi-extension
```

Run `/reload` in already-open Pi terminals, or restart Pi. Then save and restore
workspaces with the usual `rz` commands. Run the installer again after an `rz`
upgrade to update the extension.

Pi replaces its process command line and does not keep its session file open, so
the extension records the active session file, ID, working directory, and process
start time under `~/.pi/agent/rz-sessions/`. It updates the mapping when sessions
start, switch, fork, or reload, and removes it on shutdown. `PI_CODING_AGENT_DIR`
overrides the Pi configuration directory for both the installer and session lookup.

`rz` matches each Pi process to its Ghostty terminal by TTY, so identical titles
and multiple sessions in one directory are supported. Pi support requires a
Ghostty version exposing terminal TTYs through AppleScript. Custom session IDs
and storage directories work because restore uses the exact file path. Ephemeral
(`--no-session`), noninteractive, and RPC sessions are not resumed. The original
Pi session files must remain available; snapshots do not copy conversations.
If a Pi session cannot be identified, the snapshot includes a warning instead of
guessing a conversation.

## Limits

When split geometry is unavailable, additional surfaces restore as right-hand
splits. Scrollback is replayed as text,
not as a running process. `rz` can resume detected Codex conversations, Amp
threads, Claude Code sessions, and tracked Pi sessions, but it cannot reconstruct
arbitrary programs. A Codex conversation resumed through the picker is only
recognised once it has received a message, and when several conversations in
one directory are active at the same time none of them is guessed.

## Development

Build and test from source with a recent Rust toolchain:

```sh
cargo build --release
cargo test
cargo clippy --all-targets --all-features -- -D warnings
node --test test/pi-session-tracker.test.mjs
```

## License

[MIT](LICENSE)

# rend

Rip audio CDs from the command line or in an interactive TUI.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `cli/` — `rend`: the command-line interface
- `tui/` — `rend-tui`: an interactive TUI with mouse support

## Usage

```sh
rend drives                    # list CD-ROM devices
rend toc                       # show disc table of contents
rend rip                       # rip all audio tracks to ./trackNN.wav
rend rip -t 3 -t 5 -o ~/rips   # rip selected tracks to a directory
rend rip -f                    # overwrite existing files
rend eject                     # eject the disc

rend -d /dev/sr1 toc           # use a specific device (default: first found)
```

Ripped files are standard 16-bit stereo 44.1 kHz WAV, named after the
track number (`track01.wav`, ...). Progress is reported on stderr.

Exit codes: `0` success, `1` operation error, `2` usage error.

## TUI

`rend-tui` is an interactive TUI for browsing drives, inspecting the
table of contents, and watching rips in real time:

```sh
rend-tui                       # use the first CD-ROM device
rend-tui -d /dev/sr1           # use a specific device
rend-tui -o ~/rips -f          # rip to ~/rips, overwriting existing files
rend-tui --demo                # simulated disc, no hardware required
```

Keyboard:

| key              | action                          |
| ---------------- | ------------------------------- |
| `j`/`k`, `↑`/`↓` | move selection                  |
| `tab`/`shift+tab`| switch focus between panels     |
| `enter`          | activate (load TOC / rip)       |
| `r`              | rip the selected track          |
| `a`              | rip all audio tracks            |
| `e`              | eject the disc                  |
| `f`              | toggle force (overwrite)        |
| `s`              | stop the current rip            |
| `q`/`esc`        | quit                            |

Mouse: click to select, double-click a track to rip it, scroll wheel to
scroll the drives and TOC lists. Buttons can be clicked as well.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

# rend

Rip audio CDs from the command line or in an interactive TUI.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `encode/` — `rend-encode`: output formats (FLAC via ffmpeg, WAV)
- `cli/` — `rend`: the command-line interface
- `tui/` — `rend-tui`: an interactive TUI with mouse support

## Usage

```sh
rend drives                    # list CD-ROM devices
rend toc                       # show disc table of contents
rend rip                       # rip all audio tracks to ./trackNN.flac
rend rip -t 3 -t 5 -o ~/rips   # rip selected tracks to a directory
rend rip -f                    # overwrite existing files
rend rip -F wav                # output WAV instead of FLAC
rend eject                     # eject the disc

rend -d /dev/sr1 toc           # use a specific device (default: first found)
```

Ripped files are named after the track number (`track01.flac`, ...).
Progress is reported on stderr.

### Output format

By default each track is encoded to **FLAC** at the highest compression
(`-compression_level 8`) by shelling out to `ffmpeg`. Pass `-F wav` (or
`--format wav`) to write raw 16-bit stereo 44.1 kHz **WAV** instead, which
needs no external tools. The encode step lives in `rend-encode`, a
self-contained layer so further formats can be added without touching the
CLI or TUI. FLAC output requires `ffmpeg` on `PATH`; the CLI errors out
with a clear message if it is missing and a FLAC rip is requested.

### Ripping several drives in parallel

Pass `-d` more than once (or `--all`) and `rip` reads every drive at
once, one worker thread per drive:

```sh
rend rip --all                 # rip every discovered drive, in parallel
rend rip -d /dev/sr0 -d /dev/sr1   # rip exactly these drives, in parallel
```

Each drive writes into its own subdirectory of the output path, named
after the device (`~/rips/sr0/track01.flac`, `~/rips/sr1/track01.flac`), so
track numbers never collide. Log lines are prefixed with the device
(`[sr0] …`). With a single drive the output stays flat and live progress
is shown, as before.

`toc` and `eject` act on a single drive: pass exactly one `-d` (or none,
for the first device).

Exit codes: `0` success, `1` operation error, `2` usage error.

## TUI

`rend-tui` is an interactive TUI for browsing drives, inspecting the
table of contents, and watching rips in real time:

```sh
rend-tui                       # use the first CD-ROM device
rend-tui -d /dev/sr1           # use a specific device
rend-tui -o ~/rips -f          # rip to ~/rips, overwriting existing files
rend-tui -F wav                # rip to WAV instead of FLAC
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
| `o`              | toggle output format (flac/wav) |
| `s`              | stop the current rip            |
| `q`/`esc`        | quit                            |

Mouse: click to select, double-click a track to rip it, scroll wheel to
scroll the drives and TOC lists. Buttons can be clicked as well.

When more than one drive is present, several can rip at the same time:
each drive keeps its own rip state, so you can start a rip on one drive
and switch to another without interrupting it. A drive that is currently
ripping is marked with `▶` in the drives list, and the progress panel
shows the selected drive's rip. As with the CLI, multi-drive rips write
to per-device subdirectories of the output path so track numbers never
collide.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

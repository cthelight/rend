# rend

Rip audio CDs from the command line or in an interactive TUI.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

> [!NOTE]
> Install `ffmpeg` for the best results: the default output is FLAC,
> which is encoded with it. Without `ffmpeg`, rip with `-F wav` — WAV
> needs no external tools.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `encode/` — `rend-encode`: output formats (FLAC via ffmpeg, WAV)
- `meta/` — `rend-meta`: MusicBrainz disc lookup and tag writing (lofty)
- `cli/` — `rend`: the command-line interface
- `tui/` — `rend-tui`: an interactive TUI with mouse support

## Usage

```sh
rend drives                    # list CD-ROM devices
rend toc                       # show disc table of contents
rend rip                       # rip all audio tracks to ./<artist>/<album>/<NN> <title>.flac
rend rip -t 3 -t 5 -o ~/rips   # rip selected tracks to a directory
rend rip -f                    # overwrite existing files
rend rip -F wav                # output WAV instead of FLAC
rend rip --no-metadata         # rip without looking up or tagging metadata
rend info                      # look up and show the disc's metadata
rend info --matches            # list every candidate match, best first
rend info --match 2            # show the second candidate match
rend rip --match 2             # rip tagged with the second candidate match
rend eject                     # eject the disc

rend -d /dev/sr1 toc           # use a specific device (default: first found)
```

Ripped files are named after the looked-up metadata when it is available:
each track goes into a per-artist, per-album directory as
`<NN> <title>.<ext>` (e.g. `~/rips/The_Band/The_Album/01_First_Song.flac`).
The layout follows a naming template — `<artist>/<album>/<number> <title>`
by default, overridable with `-T`/`--template`, with the tokens
`<artist>`, `<album>`, `<album-artist>`, `<year>`, `<number>`, `<title>`,
and `<track-artist>` (the last component is the file name; a component
that expands to nothing, like a missing year, is dropped). Every component
is sanitized: any character that is not alphanumeric, an underscore, a
period, or a dash is replaced with `_`. When no metadata is found (or
`--no-metadata` is passed), the flat `trackNN.<ext>` naming is used
instead. Progress is reported on stderr.

### Output format

By default each track is encoded to **FLAC** at the highest compression
(`-compression_level 8`) by shelling out to `ffmpeg`. Pass `-F wav` (or
`--format wav`) to write raw 16-bit stereo 44.1 kHz **WAV** instead, which
needs no external tools. The encode step lives in `rend-encode`, a
self-contained layer so further formats can be added without touching the
CLI or TUI. FLAC output requires `ffmpeg` on `PATH`; the CLI errors out
with a clear message if it is missing and a FLAC rip is requested.

### Metadata

Before ripping, `rend` computes the disc's CDDB ID from the track lengths
and looks the disc up on **MusicBrainz** (with cover art from the
**Cover Art Archive**). The result is embedded into each track file: FLAC
gets Vorbis comments plus a `PICTURE` block, WAV gets an ID3v2 tag. Title,
artist, album, album artist, track number/total, and year are written when
known. The lookup is best effort: without network access, or when the disc
isn't found, the rip proceeds with no tags. Use `--no-metadata` to skip
the lookup entirely, or `rend info` to see what would be applied without
ripping anything.

When several releases could be the disc (a reissue, a remaster, a
compilation), the lookup ranks the candidates by how close their track
durations come to the disc's and uses the best one. `rend info --matches`
lists every candidate, and `rend info --match N` or `rend rip --match N`
deliberately picks the Nth one (1 is the best).

A disc's lookup (keyed by its track layout) and a release's cover art are
remembered for the run — or the TUI session — so the same disc is never
fetched twice: re-ripping a disc, or several parallel drives reading the
same one, costs one lookup.

Outgoing requests are paced to stay under MusicBrainz's one-request-per-
second limit, and a request the server throttles (HTTP 503 or 429) is
retried a couple of times with a short backoff.

### Ripping several drives in parallel

Pass `-d` more than once (or `--all`) and `rip` reads every drive at
once, one worker thread per drive:

```sh
rend rip --all                 # rip every discovered drive, in parallel
rend rip -d /dev/sr0 -d /dev/sr1   # rip exactly these drives, in parallel
```

Each drive writes into its own subdirectory of the output path, named
after the device (`~/rips/sr0/<artist>/<album>/01 <title>.flac`,
`~/rips/sr1/<artist>/<album>/01 <title>.flac`), so tracks from different
drives never collide. Log lines are prefixed with the device
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
rend-tui -T '<year>/<album>/<number> <title>'   # custom naming template
rend-tui --demo                # simulated disc, no hardware required
```

Keyboard:

| key              | action                          |
| ---------------- | ------------------------------- |
| `j`/`k`, `↑`/`↓` | move selection                  |
| `tab`/`shift+tab`| switch focus between panels     |
| `enter`          | activate (load TOC / rip)       |
| `m`              | switch to the next candidate match |
| `r`              | rip the selected track          |
| `a`              | rip all audio tracks            |
| `t`              | edit the disc's tags            |
| `e`              | eject the disc                  |
| `f`              | toggle force (overwrite)        |
| `o`              | toggle output format (flac/wav) |
| `s`              | stop the current rip            |
| `?`              | toggle the keybinds window      |
| `q`/`esc`        | quit                            |

Mouse: click to select, double-click a track to rip it, scroll wheel to
scroll the drives and TOC lists. Buttons can be clicked as well.

The drives sit in a left-hand panel, one entry per drive: the device and
its label on the first line, and the loaded disc's artist, album, and
year on the second. While a drive is ripping, its name line becomes a
progress bar for that drive's rip. The table of contents (and the tags
editor) fills the rest of the window on the right; `?` opens a window
with every keybind.

When a disc's table of contents is loaded, the disc is looked up on
MusicBrainz in the background; the TOC panel then shows the track titles
and the panel header shows artist, album, and year. If the lookup finds
several candidate releases, the header shows `match i of n` and `m`
switches to the next one, re-fetching its cover art; rips are tagged with
whichever match is selected. Ripped tracks carry the looked-up metadata
and cover art, embedded the same way as in the CLI (best effort — a
failed lookup never blocks a rip).

`t` opens a tags editor for the selected match: album, artist, album
artist, year, and a title and artist per track. `enter` saves, `esc`
(or `ctrl-c`) cancels, `↑`/`↓` and `tab`/`shift+tab` move between fields.
If the lookup found no candidates, `t` still opens the editor on a blank
"manual" entry (marked `manual ·` in the header), so an unmatched disc
can be tagged by hand; rips of it use those tags.

When more than one drive is present, several can rip at the same time:
each drive keeps its own rip state, so you can start a rip on one drive
and switch to another without interrupting it. A drive that is currently
ripping shows its rip as a progress bar on its name line in the drives
panel, and the bottom progress panel shows the selected drive's rip. As
with the CLI, multi-drive rips write
to per-device subdirectories of the output path so track numbers never
collide.

## Development

```sh
make            # build the debug binary
make release    # build the release binary
make test       # run the test suite
make lint       # fmt check and clippy, warnings denied
```

`make install` installs both `rend` and `rend-tui` to `~/.local/bin`
(`make install-cli` or `make install-tui` for just one of them), and
`make uninstall` removes them. The targets are thin wrappers around
`cargo` — override with `CARGO=…` if needed — and `make help` lists
the full set.

# rend

Rip audio CDs from the command line, in an interactive TUI, or through a
browser UI.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

> [!NOTE]
> Install `ffmpeg` for the best results. `rend` detects the ffmpeg-backed
> formats that are installed (FLAC, MP3, AAC/M4A, Opus, and Vorbis/OGG)
> and uses the first available one by default. Without `ffmpeg`, rip with
> `-F wav` — WAV needs no external tools.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `encode/` — `rend-encode`: output formats (ffmpeg-backed audio formats plus WAV)
- `meta/` — `rend-meta`: MusicBrainz disc lookup and tag writing (lofty)
- `cli/` — `rend`: the command-line interface
- `tui/` — `rend-tui`: an interactive TUI with mouse support
- `web/` — `rend-wui`: a browser UI with live progress over SSE

## Usage

```sh
rend drives                    # list CD-ROM devices
rend toc                       # show disc table of contents
rend rip                       # rip all audio tracks to ./<artist>/<album>/<NN> <title>.flac
rend rip -t 3 -t 5 -o ~/rips   # rip selected tracks to a directory
rend rip -f                    # overwrite existing files
rend rip -F flac -F mp3:192    # output FLAC and 192 kbps MP3 copies
rend rip -F mp3 -q 192         # output MP3 at 192 kbps
rend rip -F wav                # output WAV
rend rip --no-metadata         # rip without looking up or tagging metadata
rend info                      # look up and show the disc's metadata
rend info --matches            # list every candidate match, best first
rend info --match 2            # show the second candidate match
rend rip --match 2             # rip tagged with the second candidate match
rend eject                     # eject the disc

rend -d /dev/sr1 toc           # use a specific device (default: first found)
rend drives --drive-name 'sr0=Living Room'   # label /dev/sr0 with a friendly name
```

Drives are listed by vendor and model. `--drive-name DRIVE=NAME` (repeatable;
`sr0` and `/dev/sr0` both name the same drive) shows a friendly name instead,
and `REND_DRIVE_NAMES` does the same from the environment as
comma-separated pairs (e.g. `sr0=Living Room,sr1=Office`). A flag given on
the command line overrides the environment. The flag and the variable also
work with `rend-tui` and `rend-wui`.

Ripped files are named after the looked-up metadata when it is available:
each track goes into a per-artist, per-album directory as
`<NN> <title>.<ext>` (e.g. `~/rips/The Band/The Album/01 First Song.flac`).
The layout follows a naming template — `<artist>/<album>/<number> <title>`
by default, overridable with `-T`/`--template`, with the tokens
`<artist>`, `<album>`, `<album-artist>`, `<year>`, `<number>`, `<title>`,
and `<track-artist>` (the last component is the file name; a component
that expands to nothing, like a missing year, is dropped). Every component
is sanitized: any character that is not alphanumeric, a space, an
underscore, a period, or a dash is replaced with `_`. When no metadata is found (or
`--no-metadata` is passed), the flat `trackNN.<ext>` naming is used
instead. Progress is reported on stderr.

### Output formats

`rend` discovers the available output formats at startup. WAV is always
available; the ffmpeg-backed formats — FLAC, MP3, AAC/M4A, Opus, and
Vorbis/OGG — are enabled when their encoder is installed.

`-F/--format` may be given repeatedly. Each value is either `format` or
`format:quality`, so `-F flac -F mp3:192` rips every selected track to
both FLAC and 192 kbps MP3. Without an explicit `-F/--format`, the first
available format in the preference order is used.

Quality may also be shared with `-q/--quality` (for example `192`,
`192k`, or `q6`); it applies to selected formats that do not already
include an explicit `:quality`. The quality scale depends on the format:
FLAC uses compression levels 0–8, Vorbis uses quality 0–10, and
MP3/AAC/Opus use bitrates. `-F wav` writes raw 16-bit stereo 44.1 kHz WAV
and has no quality option.

The encode step lives in `rend-encode`, a self-contained layer so further
formats can be added without touching the CLI or TUI. A rip in a format
that the local `ffmpeg` cannot encode fails with a clear message.

### Metadata

Before ripping, `rend` computes the disc's CDDB ID from the track lengths
and looks the disc up on **MusicBrainz** (with cover art from the
**Cover Art Archive**). The result is embedded into each track file: FLAC, OGG, and Opus get
Vorbis comments (with a `PICTURE` block where supported), M4A gets MP4
ilst metadata, and WAV gets an ID3v2 tag. Title,
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
rend-tui -F flac -F mp3:192    # rip to FLAC and 192 kbps MP3
rend-tui -F wav                # rip to WAV
rend-tui -T '<year>/<album>/<number> <title>'   # custom naming template
rend-tui --drive-name 'sr0=Living Room'   # label the drive with a friendly name
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
| `u`              | show the register disc id URL   |
| `e`              | eject the disc                  |
| `f`              | toggle force (overwrite)        |
| `o`              | cycle the active output format  |
| `O`              | toggle the active format in/out |
| `Q`              | cycle the active format's quality |
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
MusicBrainz in the background; the TOC panel then shows the track titles,
the selected release as the panel title, and the disc id, track count,
and total length in a subheader under it. If the lookup finds several
candidate releases, the subheader shows `match i of n` and `m` switches
to the next one, re-fetching its cover art; rips are tagged with
whichever match is selected. Ripped tracks carry the looked-up metadata
and cover art, embedded the same way as in the CLI (best effort — a
failed lookup never blocks a rip).

`t` opens a tags editor for the selected match: album, artist, album
artist, year, and a title and artist per track. `enter` saves, `esc`
(or `ctrl-c`) cancels, `↑`/`↓` and `tab`/`shift+tab` move between fields.
If the lookup found no candidates, `t` still opens the editor on a blank
"manual" entry (marked `manual` in the TOC subheader), so an unmatched
disc can be tagged by hand; rips of it use those tags.

When more than one drive is present, several can rip at the same time:
each drive keeps its own rip state, so you can start a rip on one drive
and switch to another without interrupting it. A drive that is currently
ripping shows its rip as a progress bar on its name line in the drives
panel, and the bottom progress panel shows the selected drive's rip. As
with the CLI, multi-drive rips write
to per-device subdirectories of the output path so track numbers never
collide.

The rip panel's first line lists every available format at a glance: the
active format (the one `o`, `O`, and `Q` act on) is marked with `▸` and
bolded, selected formats carry a `✓` and their chosen quality, and the
rest are dimmed to their name alone. When the panel is wide enough the
`o cycle · O toggle · Q quality` hint is right-aligned on the same line.

## Web

`rend-wui` serves a single-page browser UI for the same drive, metadata,
and ripping flows:

```sh
rend-wui                       # use the first CD-ROM device
rend-wui -d /dev/sr1           # use a specific device
rend-wui -o ~/rips -f          # rip to ~/rips, overwriting existing files
rend-wui -F flac -F mp3:192    # rip to FLAC and 192 kbps MP3
rend-wui -F wav                # rip to WAV
rend-wui -T '<year>/<album>/<number> <title>'   # custom naming template
rend-wui --drive-name 'sr0=Living Room'   # label the drive with a friendly name
rend-wui --demo                # simulated disc, no hardware required
```

Then open the printed URL, normally <http://127.0.0.1:3000/>.

The rip settings also read the environment, handy when running `rend-wui`
as a service:

- `REND_WUI_OUTPUT_DIR` — where the ripped files are written (`-o`)
- `REND_WUI_FORMATS` — comma-separated starting output formats, each
  `format` or `format:quality` (for example `flac,mp3:192`)
- `REND_WUI_TEMPLATE` — the naming template (`-T`)
- `REND_WUI_DRIVE_DIRS` — `true` or `false`: always or never rip each
  drive into its own subdirectory of the output dir. Without it, drives
  get their own subdirectory only when several rip at once
- `REND_WUI_HOST` — the address to bind the server to (`--host`)
- `REND_WUI_PORT` — the port to bind the server to (`--port`)
- `REND_DRIVE_NAMES` — comma-separated `DRIVE=NAME` pairs giving drives
  friendly names (`--drive-name`); `sr0` and `/dev/sr0` name the same drive.
  Works with `rend` and `rend-tui` too

A flag given on the command line overrides the environment.

The settings bar shows every output format the server can serve as
toggles. A selected format that has quality options grows a quality
selector attached to its toggle, so a format and its quality are chosen
together in one control.

Every discovered drive is listed in the drive rail on the left, all of
them at once. Each row shows the drive, what is in it (the album when the
disc is identified, otherwise the disc type or an empty/ready state), and
what it is doing right now: a spinner while the TOC or metadata is being
read, a live progress bar with a track count while a rip is running, and
a done/failed dot afterwards. Select a row — click, or `↑`/`↓`, `j`/`k`,
`Home`/`End` while the rail is focused — to open that drive's workspace:
its table of contents, MusicBrainz candidate matches and cover art,
editable disc and track tags, and rip controls. Drive state and per-track
rip progress are pushed over a server-sent events stream, so the page
updates without polling. As with the TUI, several drives can rip at the
same time, and multi-drive rips write to per-device subdirectories of the
output path.

## Docker

The `Dockerfile` at the repository root builds a minimal image: `rend-wui`
(a static musl binary), a stripped-down `ffmpeg` that accepts raw PCM on a
pipe and can encode the five audio formats `rend` supports — FLAC, MP3,
AAC/M4A, Opus, and Vorbis/OGG — plus exactly the shared libraries that
`ffmpeg` links against (musl's loader, `libmp3lame`, `libopus`,
`libvorbis`, `libogg`). Nothing else is in it: no decoders, no video, no
network, no package manager, and the final stage is `scratch` with every
file copied in explicitly, so the whole image is a few MB.

```sh
make docker       # build the image as rend-wui:local
make docker-demo  # run it with the simulated disc on http://127.0.0.1:3000/
```

To rip a real disc, hand the container the CD-ROM device and a volume for
the output:

```sh
docker run -d --name rend \
    --device /dev/sr0:/dev/sr0 \
    -v ~/rips:/output \
    -p 3000:3000 \
    rend-wui:local
```

Inside the container the server binds `0.0.0.0:3000` by default and writes
to `/output`; the host is an environment default (`REND_WUI_HOST`) that
`--host`, `--port`, or `REND_WUI_PORT` override, `-o` and
`REND_WUI_OUTPUT_DIR` move the output, and every other flag and
`REND_WUI_*` environment variable works as described above. The image
is built for x86_64. Metadata lookup still needs outbound internet access
and stays best effort — without it, rips are simply written untagged.

## Development

```sh
make            # build the debug binary
make release    # build the release binary
make test       # run the test suite
make lint       # fmt check and clippy, warnings denied
```

`make install` installs `rend`, `rend-tui`, and `rend-wui` to
`~/.local/bin` (`make install-cli`, `make install-tui`, or
`make install-wui` for just one of them), and `make uninstall` removes
them. The targets are thin wrappers around
`cargo` — override with `CARGO=…` if needed — and `make help` lists
the full set.

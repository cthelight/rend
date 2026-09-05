# rend

Rip audio CDs from the command line.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `cli/` — `rend`: the command-line interface

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

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

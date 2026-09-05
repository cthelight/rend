# rend

Rip audio CDs from the command line.

A Rust terminal application built on `rend-core`, a low-level wrapper
around the Linux `cdrom` ioctl interface.

## Layout

- `core/` — `rend-core`: device discovery, TOC, CDDA audio reading
- `cli/` — `rend`: the command-line interface

## Usage

```sh
rend drives        # list CD-ROM devices
rend toc           # show disc table of contents
rend rip           # rip audio tracks to WAV files
rend eject         # eject the disc
```

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

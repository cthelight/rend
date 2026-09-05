//! Naming the output files after the looked-up metadata.
//!
//! A track whose disc has been looked up is written to
//! `<out dir>/<artist>/<NN> <title>.<ext>`: the release artist's directory,
//! the track's number zero-padded to two digits, a space, and the title.
//! Every name component is sanitized for SMB shares, which reject the
//! characters Windows forbids in file names.

use std::path::{Path, PathBuf};

use crate::lookup::DiscMeta;

/// Replaces every character an SMB share would reject with `_`: the
/// Windows-reserved set `< > : " / \ | ? *`, any kind of quote (straight or
/// typographic), and control characters. Trailing dots and spaces, which
/// Windows silently drops, are removed.
pub fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if unsafe_char(c) { '_' } else { c })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    out
}

fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '<' | '>'
                | ':'
                | '/'
                | '\\'
                | '|'
                | '?'
                | '*'
                | '"'
                | '\''
                | '\u{2018}'
                | '\u{2019}'
                | '\u{201C}'
                | '\u{201D}'
        )
}

/// The file name of a track: `<NN> <title>.<ext>`, with the title sanitized
/// (a title that sanitizes to nothing falls back to `Unknown`).
pub fn track_filename(number: u8, title: &str, ext: &str) -> String {
    let title = sanitize(title);
    let title = if title.is_empty() {
        "Unknown"
    } else {
        title.as_str()
    };
    format!("{number:02} {title}.{ext}")
}

/// The directory the looked-up disc's tracks are written to:
/// `<out dir>/<artist>`, or the bare output dir when the artist sanitizes
/// to nothing.
pub fn disc_dir(out_dir: &Path, disc: &DiscMeta) -> PathBuf {
    let artist = sanitize(&disc.artist);
    if artist.is_empty() {
        out_dir.to_path_buf()
    } else {
        out_dir.join(artist)
    }
}

/// The output path of the track at the given 1-based position of a
/// looked-up disc: `<out dir>/<artist>/<NN> <title>.<ext>`.
pub fn track_path(
    out_dir: &Path,
    disc: &DiscMeta,
    number: u8,
    position: usize,
    ext: &str,
) -> PathBuf {
    let title = disc.track(position).map_or("Unknown", |t| t.title.as_str());
    disc_dir(out_dir, disc).join(track_filename(number, title, ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::TrackMeta;

    fn disc() -> DiscMeta {
        DiscMeta {
            album: "The Album".into(),
            artist: "The Band".into(),
            album_artist: None,
            year: None,
            release_id: "rel".into(),
            tracks: vec![TrackMeta {
                title: "First Song".into(),
                artist: None,
            }],
        }
    }

    #[test]
    fn sanitize_replaces_windows_reserved_characters() {
        assert_eq!(sanitize("a<b>c:d\"e/f\\g|h?i*j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn sanitize_replaces_every_quote() {
        assert_eq!(sanitize("it's \"fine\""), "it_s _fine_");
        assert_eq!(
            sanitize("curly ‘quotes’ and “double”"),
            "curly _quotes_ and _double_"
        );
    }

    #[test]
    fn sanitize_replaces_control_characters() {
        assert_eq!(sanitize("a\tb\nc"), "a_b_c");
    }

    #[test]
    fn sanitize_strips_trailing_dots_and_spaces() {
        assert_eq!(sanitize("trailing space "), "trailing space");
        assert_eq!(sanitize("dots..."), "dots");
        assert_eq!(sanitize("mix. . "), "mix");
    }

    #[test]
    fn sanitize_leaves_normal_names_alone() {
        assert_eq!(sanitize("Josh Groban"), "Josh Groban");
        assert_eq!(sanitize("Gems"), "Gems");
        // Non-ASCII survives: SMB shares speak UTF-8.
        assert_eq!(sanitize("Éclair №5"), "Éclair №5");
    }

    #[test]
    fn track_filename_is_numbered_and_sanitized() {
        assert_eq!(track_filename(3, "Short One", "flac"), "03 Short One.flac");
        assert_eq!(track_filename(10, "A: B", "wav"), "10 A_ B.wav");
        assert_eq!(track_filename(1, "   ", "flac"), "01 Unknown.flac");
    }

    #[test]
    fn disc_dir_is_the_sanitized_artist() {
        assert_eq!(
            disc_dir(Path::new("out"), &disc()),
            PathBuf::from("out/The Band")
        );
        let mut d = disc();
        d.artist = "A: B's “band”".into();
        assert_eq!(
            disc_dir(Path::new("out"), &d),
            PathBuf::from("out/A_ B_s _band_")
        );
    }

    #[test]
    fn disc_dir_without_an_artist_is_bare() {
        let mut d = disc();
        d.artist = "   ".into();
        assert_eq!(disc_dir(Path::new("out"), &d), PathBuf::from("out"));
    }

    #[test]
    fn track_path_uses_the_release_artist_and_position() {
        let path = track_path(Path::new("out"), &disc(), 1, 1, "flac");
        assert_eq!(path, PathBuf::from("out/The Band/01 First Song.flac"));
    }

    #[test]
    fn track_path_falls_back_without_a_title() {
        let path = track_path(Path::new("out"), &disc(), 9, 5, "flac");
        assert_eq!(path, PathBuf::from("out/The Band/09 Unknown.flac"));
    }
}

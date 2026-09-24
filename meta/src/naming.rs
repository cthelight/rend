//! Naming the output files after the looked-up metadata.
//!
//! A track whose disc has been looked up is written to the output path
//! given by a naming [`Template`]. The default template,
//! `<artist>/<album>/<number> <title>`, puts each track in a per-artist,
//! per-album directory as `<NN> <title>.<ext>`: the release artist's
//! directory, the album's directory, the track's number zero-padded to two
//! digits, a space, and the title. Every name component is sanitized: any
//! character that is not alphanumeric, an underscore, a period, or a dash
//! is replaced with `_`, so the names stay safe on SMB shares.

use std::path::{Path, PathBuf};

use crate::lookup::{DiscMeta, TrackMeta};

/// The default naming template: a per-artist, per-album directory, then
/// `<number> <title>.<ext>` per track.
pub const DEFAULT_TEMPLATE: &str = "<artist>/<album>/<number> <title>";

/// A parsed naming template, as in [`DEFAULT_TEMPLATE`].
///
/// A template is a slash-separated list of name components; the last one is
/// the file name (it gains the file extension), the ones before it are
/// directories. Each component is literal text with `<token>` placeholders:
///
/// - `<artist>` — the release artist
/// - `<album>` — the album title
/// - `<album-artist>` — the album artist (falls back to the release artist)
/// - `<year>` — the release year
/// - `<number>` — the track number, zero-padded to two digits
/// - `<title>` — the track title
/// - `<track-artist>` — the track's artist (falls back to the release artist)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Vec<Segment>>,
}

/// One piece of a template component: literal text or a token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    Token(Token),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Artist,
    Album,
    AlbumArtist,
    Year,
    Number,
    Title,
    TrackArtist,
}

impl Token {
    fn parse(name: &str) -> Result<Self, ParseError> {
        match name {
            "artist" => Ok(Self::Artist),
            "album" => Ok(Self::Album),
            "album-artist" => Ok(Self::AlbumArtist),
            "year" => Ok(Self::Year),
            "number" => Ok(Self::Number),
            "title" => Ok(Self::Title),
            "track-artist" => Ok(Self::TrackArtist),
            other => Err(ParseError::UnknownToken(other.into())),
        }
    }
}

/// A naming template that could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The template names a token that does not exist.
    #[error(
        "unknown token <{0}> (expected <artist>, <album>, <album-artist>, <year>, <number>, <title>, or <track-artist>)"
    )]
    UnknownToken(String),
    /// A `<` with no closing `>`.
    #[error("unterminated token in template: {0}")]
    Unterminated(String),
    /// The template names no file at all.
    #[error("template must name a file")]
    Empty,
}

impl Template {
    /// Parses a naming template.
    pub fn parse(raw: &str) -> Result<Self, ParseError> {
        let mut parts: Vec<Vec<Segment>> = raw
            .split('/')
            .map(parse_part)
            .collect::<Result<Vec<_>, _>>()?;
        // A trailing slash names no extra component.
        while parts.last().is_some_and(Vec::is_empty) {
            parts.pop();
        }
        if parts.iter().all(Vec::is_empty) {
            return Err(ParseError::Empty);
        }
        Ok(Self { parts })
    }

    /// The output path of the track at the given 1-based position of a
    /// looked-up disc, per this template: each component is expanded and
    /// sanitized, a component that expands to nothing is dropped, and the
    /// last component gains the file extension.
    pub fn track_path(
        &self,
        out_dir: &Path,
        disc: &DiscMeta,
        number: u8,
        position: usize,
        ext: &str,
    ) -> PathBuf {
        let track = disc.track(position);
        let mut path = out_dir.to_path_buf();
        let last = self.parts.len() - 1;
        for (i, segments) in self.parts.iter().enumerate() {
            let mut name = String::new();
            for segment in segments {
                match segment {
                    Segment::Text(text) => name.push_str(text),
                    Segment::Token(token) => {
                        name.push_str(&self.token_value(*token, disc, track, number))
                    }
                }
            }
            let mut name = sanitize(&name);
            if name.is_empty() {
                if i != last {
                    // A directory that expands to nothing (e.g. a missing
                    // year) is skipped entirely.
                    continue;
                }
                // A file name that expands to nothing falls back to
                // `Unknown`.
                name = "Unknown".to_string();
            }
            path.push(if i == last {
                format!("{name}.{ext}")
            } else {
                name
            });
        }
        path
    }

    /// The value a token expands to for the given track.
    fn token_value(
        &self,
        token: Token,
        disc: &DiscMeta,
        track: Option<&TrackMeta>,
        number: u8,
    ) -> String {
        match token {
            Token::Artist => disc.artist.clone(),
            Token::Album => disc.album.clone(),
            Token::AlbumArtist => disc
                .album_artist
                .clone()
                .unwrap_or_else(|| disc.artist.clone()),
            Token::Year => disc.year.clone().unwrap_or_default(),
            Token::Number => format!("{number:02}"),
            Token::Title => {
                let title = track.map_or("", |t| t.title.as_str());
                if title.trim().is_empty() {
                    // A title that is nothing but whitespace is not a
                    // title.
                    "Unknown".to_string()
                } else {
                    title.to_string()
                }
            }
            Token::TrackArtist => track
                .and_then(|t| t.artist.as_deref())
                .unwrap_or(disc.artist.as_str())
                .to_string(),
        }
    }
}

impl Default for Template {
    fn default() -> Self {
        Self::parse(DEFAULT_TEMPLATE).expect("the default template is valid")
    }
}

/// Parses one template component: literal text with `<token>` placeholders.
fn parse_part(part: &str) -> Result<Vec<Segment>, ParseError> {
    let mut segments = Vec::new();
    let mut rest = part;
    while let Some(start) = rest.find('<') {
        let literal = &rest[..start];
        if !literal.is_empty() {
            segments.push(Segment::Text(literal.into()));
        }
        let inner = &rest[start + 1..];
        let Some(end) = inner.find('>') else {
            return Err(ParseError::Unterminated(part.into()));
        };
        segments.push(Segment::Token(Token::parse(&inner[..end])?));
        rest = &inner[end + 1..];
    }
    if !rest.is_empty() {
        segments.push(Segment::Text(rest.into()));
    }
    Ok(segments)
}

/// Replaces every character that is not alphanumeric, an underscore, a
/// period, or a dash with `_`. Trailing dots are dropped, since Windows
/// silently removes them.
pub fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.ends_with('.') {
        out.pop();
    }
    out
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
            year: Some("2024".into()),
            release_id: "rel".into(),
            release_artist_id: None,
            disc_number: None,
            disc_count: None,
            tracks: vec![TrackMeta {
                title: "First Song".into(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            }],
        }
    }

    #[test]
    fn sanitize_keeps_alphanumerics_underscores_dots_and_dashes() {
        assert_eq!(sanitize("a-b.c_d E"), "a-b.c_d_E");
        assert_eq!(sanitize("0123456789"), "0123456789");
    }

    #[test]
    fn sanitize_replaces_everything_else() {
        assert_eq!(sanitize("a<b>c:d\"e/f\\g|h?i*j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn sanitize_replaces_spaces_and_every_quote() {
        assert_eq!(sanitize("it's \"fine\""), "it_s__fine_");
        assert_eq!(
            sanitize("curly ‘quotes’ and “double”"),
            "curly__quotes__and__double_"
        );
    }

    #[test]
    fn sanitize_replaces_control_characters() {
        assert_eq!(sanitize("a\tb\nc"), "a_b_c");
    }

    #[test]
    fn sanitize_drops_trailing_dots() {
        assert_eq!(sanitize("dots..."), "dots");
        assert_eq!(sanitize("dots. ."), "dots._");
    }

    #[test]
    fn sanitize_leaves_safe_names_alone() {
        assert_eq!(sanitize("Josh_Groban"), "Josh_Groban");
        assert_eq!(sanitize("Gems"), "Gems");
        // Non-ASCII alphanumerics survive.
        assert_eq!(sanitize("Éclair-café-5"), "Éclair-café-5");
    }

    #[test]
    fn default_template_is_artist_album_number_title() {
        let template = Template::default();
        assert_eq!(
            template.track_path(Path::new("out"), &disc(), 1, 1, "flac"),
            PathBuf::from("out/The_Band/The_Album/01_First_Song.flac")
        );
        assert_eq!(
            template.track_path(Path::new("out"), &disc(), 10, 1, "wav"),
            PathBuf::from("out/The_Band/The_Album/10_First_Song.wav")
        );
    }

    #[test]
    fn custom_template_reorders_and_renames_tokens() {
        let template = Template::parse("<year>/<album>/<track-artist>/<number>-<title>").unwrap();
        let mut d = disc();
        d.tracks[0].artist = Some("Solo Artist".into());
        assert_eq!(
            template.track_path(Path::new("out"), &d, 3, 1, "wav"),
            PathBuf::from("out/2024/The_Album/Solo_Artist/03-First_Song.wav")
        );
    }

    #[test]
    fn album_artist_falls_back_to_the_release_artist() {
        let template = Template::parse("<album-artist>/<number> <title>").unwrap();
        assert_eq!(
            template.track_path(Path::new("out"), &disc(), 1, 1, "flac"),
            PathBuf::from("out/The_Band/01_First_Song.flac")
        );
        let mut d = disc();
        d.album_artist = Some("Various".into());
        assert_eq!(
            template.track_path(Path::new("out"), &d, 1, 1, "flac"),
            PathBuf::from("out/Various/01_First_Song.flac")
        );
    }

    #[test]
    fn track_artist_falls_back_to_the_release_artist() {
        let template = Template::parse("<track-artist>/<number> <title>").unwrap();
        assert_eq!(
            template.track_path(Path::new("out"), &disc(), 1, 1, "flac"),
            PathBuf::from("out/The_Band/01_First_Song.flac")
        );
        let mut d = disc();
        d.tracks[0].artist = Some("Guest Artist".into());
        assert_eq!(
            template.track_path(Path::new("out"), &d, 1, 1, "flac"),
            PathBuf::from("out/Guest_Artist/01_First_Song.flac")
        );
    }

    #[test]
    fn a_directory_that_expands_to_nothing_is_dropped() {
        let template = Template::parse("<artist>/<year>/<number> <title>").unwrap();
        let mut d = disc();
        d.year = None;
        assert_eq!(
            template.track_path(Path::new("out"), &d, 1, 1, "flac"),
            PathBuf::from("out/The_Band/01_First_Song.flac")
        );
    }

    #[test]
    fn a_titleless_track_falls_back_to_unknown() {
        let mut d = disc();
        d.tracks[0].title = "   ".into();
        assert_eq!(
            Template::default().track_path(Path::new("out"), &d, 1, 1, "flac"),
            PathBuf::from("out/The_Band/The_Album/01_Unknown.flac")
        );
    }

    #[test]
    fn a_file_name_that_expands_to_nothing_falls_back_to_unknown() {
        let template = Template::parse("<year>").unwrap();
        let mut d = disc();
        d.year = None;
        assert_eq!(
            template.track_path(Path::new("out"), &d, 1, 1, "flac"),
            PathBuf::from("out/Unknown.flac")
        );
    }

    #[test]
    fn parse_rejects_unknown_tokens() {
        assert!(matches!(
            Template::parse("<artist>/<bogus>"),
            Err(ParseError::UnknownToken(t)) if t == "bogus"
        ));
    }

    #[test]
    fn parse_rejects_unterminated_tokens() {
        assert!(matches!(
            Template::parse("<artist>/<album"),
            Err(ParseError::Unterminated(_))
        ));
    }

    #[test]
    fn parse_rejects_templates_without_a_file() {
        assert!(matches!(Template::parse(""), Err(ParseError::Empty)));
        assert!(matches!(Template::parse("/"), Err(ParseError::Empty)));
        assert!(matches!(Template::parse("//"), Err(ParseError::Empty)));
    }

    #[test]
    fn parse_ignores_a_trailing_slash() {
        assert_eq!(
            Template::parse("<artist>/<album>/<number> <title>/").unwrap(),
            Template::default()
        );
    }
}

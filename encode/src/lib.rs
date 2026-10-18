//! rend-encode: the output-format layer for rend.
//!
//! Ripping a disc yields a stream of raw CDDA PCM — 16-bit little-endian,
//! 44 100 Hz, 2 channels. This crate turns that stream into finished audio
//! files, keeping the "which format" decision separate from the "read the
//! disc" machinery in `rend-core`.
//!
//! WAV is always available and is written directly. Every other format is
//! transcoded in a single pass by streaming the PCM into `ffmpeg`; the
//! formats the local `ffmpeg` can actually encode are discovered at runtime
//! with [`FormatSet::probe`].

use std::io;
use std::path::Path;
use std::process::Command;

pub mod ffmpeg;
pub mod wav;

/// The kind of quality scale a format uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityKind {
    /// The format has no user-selectable quality.
    None,
    /// The quality is an output bitrate in kilobits per second.
    Bitrate,
    /// The quality is the FLAC compression level (0–8).
    FlacCompression,
    /// The quality is the Vorbis quality setting (0–10).
    VorbisQuality,
}

/// The static description of one output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatSpec {
    /// Stable lowercase name, used on the command line and in the web API.
    pub id: &'static str,
    /// Short lowercase label, for UIs and CLI help.
    pub label: &'static str,
    /// The file extension of this format, without the dot.
    pub extension: &'static str,
    /// The ffmpeg encoder name, or `None` for formats written directly.
    pub encoder: Option<&'static str>,
    /// How this format's quality scale should be rendered and applied.
    pub quality_kind: QualityKind,
    /// The quality chosen when the user does not specify one.
    pub default_quality: i32,
    /// Every quality the user may choose, in cycle order.
    pub options: &'static [i32],
}

impl FormatSpec {
    /// The ffmpeg arguments that apply this format's quality setting.
    pub fn ffmpeg_args(&self, quality: i32) -> Vec<String> {
        match self.quality_kind {
            QualityKind::None => Vec::new(),
            QualityKind::Bitrate => vec!["-b:a".to_string(), format!("{quality}k")],
            QualityKind::FlacCompression => {
                vec!["-compression_level".to_string(), quality.to_string()]
            }
            QualityKind::VorbisQuality => vec!["-q:a".to_string(), quality.to_string()],
        }
    }

    /// The short user-facing label of one quality value.
    pub fn quality_label(&self, quality: i32) -> String {
        match self.quality_kind {
            QualityKind::None => String::new(),
            QualityKind::Bitrate => format!("{quality}k"),
            QualityKind::FlacCompression => quality.to_string(),
            QualityKind::VorbisQuality => format!("q{quality}"),
        }
    }
}

const FLAC_QUALITIES: [i32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
const VORBIS_QUALITIES: [i32; 11] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
const MP3_BITRATES: [i32; 6] = [128, 160, 192, 224, 256, 320];
const AAC_BITRATES: [i32; 6] = [96, 128, 160, 192, 256, 320];
const OPUS_BITRATES: [i32; 7] = [64, 96, 128, 160, 192, 256, 320];

pub const FLAC: &FormatSpec = &FormatSpec {
    id: "flac",
    label: "flac",
    extension: "flac",
    encoder: Some("flac"),
    quality_kind: QualityKind::FlacCompression,
    default_quality: 8,
    options: &FLAC_QUALITIES,
};

pub const MP3: &FormatSpec = &FormatSpec {
    id: "mp3",
    label: "mp3",
    extension: "mp3",
    encoder: Some("libmp3lame"),
    quality_kind: QualityKind::Bitrate,
    default_quality: 320,
    options: &MP3_BITRATES,
};

pub const AAC: &FormatSpec = &FormatSpec {
    id: "aac",
    label: "aac",
    extension: "m4a",
    encoder: Some("aac"),
    quality_kind: QualityKind::Bitrate,
    default_quality: 256,
    options: &AAC_BITRATES,
};

pub const OPUS: &FormatSpec = &FormatSpec {
    id: "opus",
    label: "opus",
    extension: "opus",
    encoder: Some("libopus"),
    quality_kind: QualityKind::Bitrate,
    default_quality: 128,
    options: &OPUS_BITRATES,
};

pub const VORBIS: &FormatSpec = &FormatSpec {
    id: "vorbis",
    label: "vorbis",
    extension: "ogg",
    encoder: Some("libvorbis"),
    quality_kind: QualityKind::VorbisQuality,
    default_quality: 6,
    options: &VORBIS_QUALITIES,
};

pub const WAV: &FormatSpec = &FormatSpec {
    id: "wav",
    label: "wav",
    extension: "wav",
    encoder: None,
    quality_kind: QualityKind::None,
    default_quality: 0,
    options: &[],
};

/// Every format rend knows about, in default preference order.
pub const ALL_SPECS: [FormatSpec; 6] = [*FLAC, *MP3, *AAC, *OPUS, *VORBIS, *WAV];

/// The audio format and quality of a ripped track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    spec: &'static FormatSpec,
    quality: i32,
}

impl Default for Format {
    fn default() -> Self {
        Self {
            spec: FLAC,
            quality: FLAC.default_quality,
        }
    }
}

impl Format {
    /// Parses a format name, case-insensitively, at its default quality.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::from_id(s, None)
    }

    /// Builds a format from a name and an optional explicit quality.
    pub fn from_id(id: &str, quality: Option<i32>) -> Result<Self, String> {
        let spec = ALL_SPECS
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| {
                let known = ALL_SPECS
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown format '{id}' (expected {known})")
            })?;
        let quality = quality.unwrap_or(spec.default_quality);
        if spec.options.is_empty() {
            if quality != spec.default_quality {
                return Err(format!(
                    "invalid quality '{quality}' for {}: this format has no quality options",
                    spec.label
                ));
            }
        } else if !spec.options.contains(&quality) {
            let choices = spec
                .options
                .iter()
                .map(|q| spec.quality_label(*q))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "invalid quality '{quality}' for {}: choose one of {choices}",
                spec.label
            ));
        }
        Ok(Self { spec, quality })
    }

    /// Parses a quality value, accepting plain numbers, `320k`, and `q6`.
    pub fn parse_quality(s: &str) -> Result<i32, String> {
        let t = s.trim().to_ascii_lowercase();
        let digits = t
            .strip_suffix('k')
            .or_else(|| t.strip_prefix('q'))
            .unwrap_or(t.as_str());
        digits
            .parse::<i32>()
            .map_err(|_| format!("invalid quality '{s}'"))
    }

    /// The static description of this format.
    pub fn spec(&self) -> &'static FormatSpec {
        self.spec
    }

    /// The stable lowercase name of this format.
    pub fn id(self) -> &'static str {
        self.spec.id
    }

    /// Short lowercase label, for UIs and CLI help.
    pub fn label(self) -> &'static str {
        self.spec.label
    }

    /// The file extension of this format, without the dot.
    pub fn extension(self) -> &'static str {
        self.spec.extension
    }

    /// The selected quality value.
    pub fn quality(self) -> i32 {
        self.quality
    }

    /// The qualities the user may choose for this format.
    pub fn quality_options(self) -> &'static [i32] {
        self.spec.options
    }

    /// The short user-facing label of the selected quality.
    pub fn quality_label(self) -> String {
        self.spec.quality_label(self.quality)
    }

    /// The format label plus its quality, e.g. `mp3 320k`.
    pub fn display_label(self) -> String {
        let quality = self.quality_label();
        if quality.is_empty() {
            self.spec.label.to_string()
        } else {
            format!("{} {quality}", self.spec.label)
        }
    }

    /// Returns a copy of this format with a different quality.
    pub fn with_quality(self, quality: i32) -> Result<Self, String> {
        if self.spec.options.is_empty() {
            if quality != self.spec.default_quality {
                return Err(format!(
                    "invalid quality '{quality}' for {}: this format has no quality options",
                    self.spec.label
                ));
            }
        } else if !self.spec.options.contains(&quality) {
            let choices = self
                .spec
                .options
                .iter()
                .map(|q| self.spec.quality_label(*q))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "invalid quality '{quality}' for {}: choose one of {choices}",
                self.spec.label
            ));
        }
        Ok(Self {
            spec: self.spec,
            quality,
        })
    }

    /// The next quality option for this format, wrapping around.
    pub fn cycle_quality(self) -> Self {
        let options = self.spec.options;
        if options.is_empty() {
            return self;
        }
        let i = options.iter().position(|q| *q == self.quality).unwrap_or(0);
        Self {
            spec: self.spec,
            quality: options[(i + 1) % options.len()],
        }
    }

    /// `true` if encoding this format needs an external tool.
    pub fn requires_ffmpeg(self) -> bool {
        self.spec.encoder.is_some()
    }

    /// Creates a new track output file of this format at `path`.
    pub fn create_file(self, path: &Path) -> io::Result<TrackFile> {
        match self.spec.encoder {
            Some(encoder) => Ok(TrackFile::Ffmpeg(ffmpeg::FfmpegWriter::create(
                path,
                encoder,
                self.spec.ffmpeg_args(self.quality),
            )?)),
            None => Ok(TrackFile::Wav(wav::WavWriter::create(path)?)),
        }
    }
}

/// The formats available on this machine, in preference order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSet {
    formats: Vec<&'static FormatSpec>,
}

impl FormatSet {
    /// Builds a set from explicit specs.
    pub fn new(formats: Vec<&'static FormatSpec>) -> Self {
        Self { formats }
    }

    /// Every format rend knows about, whether or not ffmpeg can encode it.
    pub fn all() -> Self {
        Self {
            formats: ALL_SPECS.iter().collect(),
        }
    }

    /// Only the directly-written WAV format.
    pub fn without_ffmpeg() -> Self {
        Self { formats: vec![WAV] }
    }

    /// Probes the local `ffmpeg` and returns the formats it can serve.
    ///
    /// WAV is always present; the ffmpeg-backed formats are included only
    /// when their encoder is installed.
    pub fn probe() -> Self {
        let encoders = probe_encoders();
        let formats = ALL_SPECS
            .iter()
            .filter(|spec| {
                spec.encoder.is_none()
                    || encoders.as_deref().is_some_and(|encoders| {
                        spec.encoder
                            .is_some_and(|encoder| encoders.iter().any(|name| name == encoder))
                    })
            })
            .collect();
        Self { formats }
    }

    /// The available format specs, in preference order.
    pub fn formats(&self) -> &[&'static FormatSpec] {
        &self.formats
    }

    /// The available format ids, in preference order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.formats.iter().map(|s| s.id).collect()
    }

    /// The available spec with this id, if any.
    pub fn get(&self, id: &str) -> Option<&FormatSpec> {
        self.formats
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(id))
            .copied()
    }

    /// `true` if this format is in the set.
    pub fn contains(&self, format: Format) -> bool {
        self.formats.contains(&format.spec)
    }

    /// The preferred default format: the first available one.
    pub fn default_format(&self) -> Format {
        self.formats
            .first()
            .copied()
            .map(|spec| Format {
                spec,
                quality: spec.default_quality,
            })
            .unwrap_or_default()
    }

    /// The next available format after `format`, wrapping around.
    pub fn next_format(&self, format: Format) -> Format {
        if self.formats.is_empty() {
            return Format::default();
        }
        let i = self
            .formats
            .iter()
            .position(|s| *s == format.spec)
            .unwrap_or(0);
        let next = self.formats[(i + 1) % self.formats.len()];
        Format::from_id(next.id, Some(format.quality))
            .unwrap_or_else(|_| Format::from_id(next.id, None).expect("static spec is parseable"))
    }

    /// Resolves a user's format/quality choice against the available set.
    ///
    /// With no explicit format the set's default is used; an explicit
    /// quality is applied to the chosen (or default) format.
    pub fn resolve(&self, format: Option<Format>, quality: Option<&str>) -> Result<Format, String> {
        let quality = quality.map(Format::parse_quality).transpose()?;
        let base = format.unwrap_or_else(|| self.default_format());
        let next = base.with_quality(quality.unwrap_or_else(|| base.quality()))?;
        if !self.contains(next) {
            return Err(format!(
                "{} is not available on this system (available: {})",
                next.label(),
                self.ids().join(", ")
            ));
        }
        Ok(next)
    }
}

/// A track's output file, fed raw PCM as the track is read.
pub enum TrackFile {
    Ffmpeg(ffmpeg::FfmpegWriter),
    Wav(wav::WavWriter),
}

impl TrackFile {
    /// Appends raw PCM samples (s16le, 44 100 Hz, stereo).
    pub fn write(&mut self, pcm: &[u8]) -> io::Result<()> {
        match self {
            Self::Ffmpeg(f) => f.write(pcm),
            Self::Wav(w) => w.write(pcm),
        }
    }

    /// Finalizes the file and flushes it to disk.
    ///
    /// The writer must not be used afterwards.
    pub fn finish(self) -> io::Result<()> {
        match self {
            Self::Ffmpeg(f) => f.finish(),
            Self::Wav(w) => w.finish(),
        }
    }
}

/// `true` if the `ffmpeg` executable can be run from the current PATH.
pub fn ffmpeg_available() -> bool {
    Command::new(ffmpeg::FFMPEG)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Parses the audio encoder names out of `ffmpeg -encoders` output.
fn parse_encoder_list(stdout: &str) -> Vec<String> {
    let mut encoders = Vec::new();
    let mut in_encoders = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed == "------" {
            in_encoders = true;
            continue;
        }
        if !in_encoders || trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let (Some(flags), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if flags.len() != 6 || !flags.starts_with('A') {
            continue;
        }
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            encoders.push(name.to_string());
        }
    }
    encoders
}

/// The audio encoder names the local `ffmpeg` reports, if it can be run.
fn probe_encoders() -> Option<Vec<String>> {
    let output = Command::new(ffmpeg::FFMPEG)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(parse_encoder_list(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Format::parse("flac").unwrap(), Format::default());
        assert_eq!(Format::parse("FLAC").unwrap().id(), "flac");
        assert_eq!(Format::parse("wav").unwrap().id(), "wav");
        assert_eq!(Format::parse("WAV").unwrap().id(), "wav");
        assert_eq!(Format::parse("mp3").unwrap().id(), "mp3");
        assert_eq!(Format::parse("aac").unwrap().extension(), "m4a");
        assert!(Format::parse("mpegts").is_err());
        assert!(Format::parse("").is_err());
    }

    #[test]
    fn default_is_flac() {
        assert_eq!(Format::default().id(), "flac");
        assert_eq!(Format::default().quality(), 8);
    }

    #[test]
    fn labels_and_extensions() {
        assert_eq!(Format::default().extension(), "flac");
        assert_eq!(Format::parse("wav").unwrap().extension(), "wav");
        assert_eq!(Format::default().label(), "flac");
        assert_eq!(Format::parse("wav").unwrap().label(), "wav");
        assert_eq!(Format::parse("mp3").unwrap().display_label(), "mp3 320k");
        assert_eq!(Format::parse("wav").unwrap().display_label(), "wav");
    }

    #[test]
    fn quality_validation() {
        assert_eq!(Format::from_id("mp3", Some(192)).unwrap().quality(), 192);
        assert!(Format::from_id("mp3", Some(319)).is_err());
        assert!(Format::from_id("flac", Some(8)).is_ok());
        assert!(Format::from_id("flac", Some(9)).is_err());
        assert!(Format::from_id("wav", Some(0)).is_ok());
        assert!(Format::from_id("wav", Some(1)).is_err());
    }

    #[test]
    fn quality_parsing() {
        assert_eq!(Format::parse_quality("320").unwrap(), 320);
        assert_eq!(Format::parse_quality("320k").unwrap(), 320);
        assert_eq!(Format::parse_quality("q6").unwrap(), 6);
        assert!(Format::parse_quality("fast").is_err());
    }

    #[test]
    fn quality_cycling() {
        let mp3 = Format::from_id("mp3", Some(320)).unwrap();
        assert_eq!(mp3.cycle_quality().quality(), 128);
        let flac = Format::default();
        assert_eq!(flac.cycle_quality().quality(), 0);
        let wav = Format::parse("wav").unwrap();
        assert_eq!(wav.cycle_quality(), wav);
    }

    #[test]
    fn ffmpeg_args_follow_the_quality_kind() {
        let flac = Format::default();
        assert_eq!(
            flac.spec().ffmpeg_args(8),
            vec!["-compression_level".to_string(), "8".to_string()]
        );
        let mp3 = Format::from_id("mp3", Some(192)).unwrap();
        assert_eq!(
            mp3.spec().ffmpeg_args(192),
            vec!["-b:a".to_string(), "192k".to_string()]
        );
        let vorbis = Format::from_id("vorbis", Some(6)).unwrap();
        assert_eq!(
            vorbis.spec().ffmpeg_args(6),
            vec!["-q:a".to_string(), "6".to_string()]
        );
        let wav = Format::parse("wav").unwrap();
        assert!(wav.spec().ffmpeg_args(0).is_empty());
    }

    #[test]
    fn encoder_list_parsing_ignores_the_legend() {
        let stdout = "\
Encoders:
 V..... = Video
 A..... = Audio
 ------
 V....D h264                 H.264
 A....D flac                 FLAC
 A....D libmp3lame           MP3
 A..X.D opus                 Opus
 A....D aac                  AAC
";
        assert_eq!(
            parse_encoder_list(stdout),
            vec!["flac", "libmp3lame", "opus", "aac"]
        );
    }

    #[test]
    fn format_set_without_ffmpeg_only_has_wav() {
        let set = FormatSet::without_ffmpeg();
        assert_eq!(set.ids(), vec!["wav"]);
        assert_eq!(set.default_format().id(), "wav");
        assert!(!set.contains(Format::default()));
        assert!(set.contains(Format::parse("wav").unwrap()));
    }

    #[test]
    fn format_set_cycles_through_available_formats() {
        let set = FormatSet::new(vec![FLAC, WAV]);
        assert_eq!(set.next_format(Format::default()).id(), "wav");
        assert_eq!(set.next_format(Format::parse("wav").unwrap()).id(), "flac");
    }

    #[test]
    fn resolve_rejects_unavailable_formats() {
        let set = FormatSet::without_ffmpeg();
        let flac = Format::default();
        assert!(set.resolve(Some(flac), None).is_err());
        assert_eq!(set.resolve(None, None).unwrap().id(), "wav");
    }

    #[test]
    fn flac_requires_ffmpeg() {
        assert!(Format::default().requires_ffmpeg());
        assert!(!Format::parse("wav").unwrap().requires_ffmpeg());
    }
}

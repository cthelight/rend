//! Disc identifiers.
//!
//! Two ids, both derived from the audio track start positions alone so
//! they identify a disc without reading any of its content:
//!
//! * [`disc_id`] — the 10-character hex CDDB id, shown by `rend info`.
//! * [`mb_discid`] — the 28-character id MusicBrainz's `/ws/2/discid/`
//!   endpoint looks up.

use base64::Engine;
use sha1::{Digest, Sha1};

/// Frames of lead-in between the disc start and the first track.
const LEAD_IN: u32 = 150;

/// Computes the CDDB disc id for the given audio track start LBAs, in
/// disc order.
///
/// The id is the sum (8 hex digits) followed by the number of audio tracks
/// minus one (2 hex digits). The sum adds one per track after the first,
/// plus each track's start LBA minus the lead-in — for every track except
/// the last, whose position depends on disc length, not on the disc's
/// identity.
pub fn disc_id(audio_track_lbas: &[u32]) -> String {
    let tracks = audio_track_lbas.len();
    if tracks == 0 {
        return String::new();
    }
    let mut sum = (tracks - 1) as u32;
    for &lba in &audio_track_lbas[..tracks - 1] {
        sum += lba.saturating_sub(LEAD_IN);
    }
    format!("{:08x}{:02x}", sum, tracks - 1)
}

/// Computes the MusicBrainz disc id for a TOC whose tracks start at
/// `track_lbas` (audio-relative, i.e. index-0 addresses) and whose
/// leadout is at `leadout`.
///
/// The message is `"01"`, the 2-hex-digit track count, the 8-hex-digit
/// leadout, and 99 × 8-hex-digit track offsets (zero-padded past the last
/// track); the id is base64(sha1(message)) with the alphabet remapped
/// `+ → .`, `/ → _`, `= → -`, giving 28 characters.
pub fn mb_discid(leadout: u32, track_lbas: &[u32]) -> String {
    let count = track_lbas.len().min(99);
    let mut msg = String::with_capacity(2 + 2 + 8 + 99 * 8);
    msg.push_str("01");
    msg.push_str(&format!("{count:02X}"));
    msg.push_str(&format!("{leadout:08X}"));
    for i in 0..99 {
        let lba = track_lbas.get(i).copied().unwrap_or(0);
        msg.push_str(&format!("{lba:08X}"));
    }
    let digest = Sha1::digest(msg.as_bytes());
    base64::engine::general_purpose::STANDARD
        .encode(digest)
        .replace('+', ".")
        .replace('/', "_")
        .replace('=', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_track() {
        assert_eq!(disc_id(&[150]), "0000000000");
    }

    #[test]
    fn three_tracks() {
        // (150-150) + (3000-150) = 2850, plus 2 for the two extra tracks:
        // 2852 = 0xb24, and 3 - 1 = 2.
        assert_eq!(disc_id(&[150, 3000, 6000]), "00000b2402");
    }

    #[test]
    fn last_track_is_excluded_from_the_sum() {
        // Only the last track differs: the disc id is unchanged.
        assert_eq!(disc_id(&[150, 3000, 6000]), disc_id(&[150, 3000, 20_000]));
        // A non-last track differs: the disc id changes.
        assert_ne!(disc_id(&[150, 3000, 6000]), disc_id(&[150, 4000, 6000]));
    }

    #[test]
    fn empty_input_yields_empty_id() {
        assert_eq!(disc_id(&[]), "");
    }

    #[test]
    fn shape() {
        let id = disc_id(&[150, 2_000, 4_000, 6_000, 8_000]);
        assert_eq!(id.len(), 10);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, id.to_ascii_lowercase());
        assert!(id.ends_with("04"));
    }

    #[test]
    fn mb_discid_matches_registered_ids() {
        // Josh Groban — Gems (2025). These are the audio-relative
        // (index-0) track addresses; the id must match the one MusicBrainz
        // has registered for the disc, or the exact lookup will 404.
        let gems = [
            150, 16_892, 36_268, 58_040, 79_717, 97_185, 120_549, 141_522, 161_267, 177_788,
            191_999, 210_264, 233_696, 254_232, 274_228, 292_188, 314_146, 333_285,
        ];
        assert_eq!(mb_discid(352_425, &gems), "mxO4b9UETc1arkNnkElaBykTeLQ-");

        // A second, independently registered 18-track disc.
        let other = [
            150, 18_496, 35_690, 57_990, 80_426, 100_252, 119_035, 138_993, 159_993, 175_646,
            192_342, 212_559, 232_646, 246_212, 272_830, 291_606, 307_389, 332_309,
        ];
        assert_eq!(mb_discid(350_189, &other), "EizxVzE4RttjlJ7i13Khbb5mBIk-");
    }

    #[test]
    fn mb_discid_shape() {
        let id = mb_discid(352_275, &[0, 16_742, 36_118]);
        assert_eq!(id.len(), 28);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        );
        assert!(id.ends_with('-'));
    }

    #[test]
    fn mb_discid_zero_pads_missing_tracks() {
        // Fewer than 99 tracks: the remaining offset slots are zero, so the
        // id is a pure function of the real layout.
        let a = mb_discid(3_300, &[150, 1_650, 2_450]);
        let b = mb_discid(3_300, &[150, 1_650, 2_450]);
        assert_eq!(a, b);
        // A different layout gives a different id.
        let c = mb_discid(3_300, &[150, 1_650, 2_500]);
        assert_ne!(a, c);
    }
}

//! CDDB disc-id computation.
//!
//! The disc id is a 10-character hex string derived from the audio track
//! start positions alone, so it identifies a disc without reading any of
//! its content. It is the same id the CDDB protocol and MusicBrainz's disc
//! lookup both accept.

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
}

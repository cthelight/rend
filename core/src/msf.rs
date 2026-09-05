//! Minute:Second:Frame (MSF) address handling.
//!
//! MSF is the time notation printed on CD labels: 75 frames per second,
//! and the first playable frame (LBA 0) is labeled `02:00:00`.

use crate::sys::{CD_FRAMES, CD_MSF_OFFSET};

/// A CD address in minute:second:frame (MSF) notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Msf {
    /// Minutes.
    pub min: u16,
    /// Seconds (0..=59).
    pub sec: u8,
    /// Frames (0..=74).
    pub frame: u8,
}

impl Msf {
    /// Creates an MSF address from its components.
    ///
    /// # Panics
    ///
    /// Panics if `sec > 59` or `frame > 74`.
    pub const fn new(min: u16, sec: u8, frame: u8) -> Self {
        assert!(sec <= 59 && frame <= 74, "MSF component out of range");
        Self { min, sec, frame }
    }

    /// The LBA corresponding to this MSF address.
    ///
    /// MSF addresses before `02:00:00` (the lead-in) yield negative values.
    pub fn to_lba(self) -> i64 {
        (self.min as i64 * 60 + self.sec as i64) * CD_FRAMES as i64 + self.frame as i64
            - CD_MSF_OFFSET
    }

    /// The MSF address for LBA `lba`, or `None` if `lba` lies outside the
    /// range representable in MSF (roughly `02:00:00`..=`99:59:74`).
    pub fn from_lba(lba: i64) -> Option<Self> {
        let t = lba + CD_MSF_OFFSET;
        if t < 0 {
            return None;
        }
        let min = t / (60 * CD_FRAMES as i64);
        if min > 99 {
            return None;
        }
        Some(Self {
            min: min as u16,
            sec: (t % (60 * CD_FRAMES as i64) / CD_FRAMES as i64) as u8,
            frame: (t % CD_FRAMES as i64) as u8,
        })
    }

    /// The little-endian word encoding used by the kernel for MSF-format
    /// addresses (minute in the low byte, then second, then frame).
    pub fn to_word(self) -> u32 {
        self.min as u32 | (self.sec as u32) << 8 | (self.frame as u32) << 16
    }

    /// Decodes a little-endian MSF word as produced by the kernel.
    ///
    /// Returns `None` if the second or frame component is out of range.
    pub fn from_word(word: u32) -> Option<Self> {
        let sec = (word >> 8) as u8;
        let frame = (word >> 16) as u8;
        if sec > 59 || frame > 74 {
            return None;
        }
        Some(Self {
            min: (word & 0xff) as u16,
            sec,
            frame,
        })
    }
}

impl std::fmt::Display for Msf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.min, self.sec, self.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lba_zero_is_msf_two_seconds() {
        // The first playable frame (LBA 0) is labeled 00:02:00.
        assert_eq!(Msf::new(0, 2, 0).to_lba(), 0);
        assert_eq!(Msf::from_lba(0), Some(Msf::new(0, 2, 0)));
    }

    #[test]
    fn to_lba() {
        assert_eq!(Msf::new(0, 3, 0).to_lba(), 75);
        assert_eq!(Msf::new(1, 2, 0).to_lba(), 4500);
        assert_eq!(Msf::new(0, 2, 74).to_lba(), 74);
    }

    #[test]
    fn lead_in_is_negative() {
        assert_eq!(Msf::new(0, 0, 0).to_lba(), -150);
        assert_eq!(Msf::from_lba(-1), Some(Msf::new(0, 1, 74)));
        assert_eq!(Msf::from_lba(-150), Some(Msf::new(0, 0, 0)));
    }

    #[test]
    fn from_lba_out_of_range() {
        assert_eq!(Msf::from_lba(-151), None);
        // 99:59:74 is the largest representable MSF address.
        let max = Msf::new(99, 59, 74).to_lba();
        assert_eq!(Msf::from_lba(max), Some(Msf::new(99, 59, 74)));
        assert_eq!(Msf::from_lba(max + 1), None);
    }

    #[test]
    fn round_trip() {
        for lba in [0i64, 1, 74, 75, 4499, 333000, 449849] {
            let msf = Msf::from_lba(lba).unwrap();
            assert_eq!(msf.to_lba(), lba);
        }
    }

    #[test]
    fn word_round_trip() {
        let msf = Msf::new(58, 41, 33);
        assert_eq!(Msf::from_word(msf.to_word()), Some(msf));
        assert_eq!(msf.to_word(), 0x21_29_3a);
    }

    #[test]
    fn word_rejects_bad_components() {
        // second = 60
        assert_eq!(Msf::from_word(0x003c00), None);
        // frame = 75
        assert_eq!(Msf::from_word(0x4b0000), None);
    }

    #[test]
    fn display() {
        assert_eq!(Msf::new(2, 0, 0).to_string(), "02:00:00");
        assert_eq!(Msf::new(58, 41, 33).to_string(), "58:41:33");
    }
}

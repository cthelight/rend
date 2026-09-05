/// A track on a CD, in MSF (minute:second:frame) notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msf {
    /// Minutes (0..=99).
    pub min: u8,
    /// Seconds (0..=59).
    pub sec: u8,
    /// Frames (0..=74).
    pub frame: u8,
}

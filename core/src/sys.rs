//! FFI bindings to the Linux `cdrom` ioctl interface (`<linux/cdrom.h>`).
//!
//! Ioctl numbers and struct layouts match the kernel UAPI header; the tests
//! at the bottom pin the ABI so any drift breaks the build.

use std::fs::File;
use std::io;
use std::os::unix::prelude::AsRawFd;

use libc::{c_char, c_int, c_uchar, c_uint, c_ulong};

/// Runs a cdrom ioctl that takes no argument, returning its status value.
pub(crate) fn ioctl_cmd(file: &File, req: c_ulong) -> io::Result<i32> {
    // SAFETY: the fd is owned by `file` and valid; no user data is passed.
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), req) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Runs a cdrom ioctl operating on a `#[repr(C)]` argument.
pub(crate) fn ioctl_with<T>(file: &File, req: c_ulong, arg: &mut T) -> io::Result<()> {
    // SAFETY: `arg` is a repr(C) struct whose layout matches the kernel's
    // (see the ABI tests in this module), and it outlives the call.
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), req, arg as *mut T) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ioctl command numbers
// ---------------------------------------------------------------------------

pub const CDROMPAUSE: c_ulong = 0x5301;
pub const CDROMRESUME: c_ulong = 0x5302;
pub const CDROMPLAYMSF: c_ulong = 0x5303;
pub const CDROMPLAYTRKIND: c_ulong = 0x5304;
pub const CDROMREADTOCHDR: c_ulong = 0x5305;
pub const CDROMREADTOCENTRY: c_ulong = 0x5306;
pub const CDROMSTOP: c_ulong = 0x5307;
pub const CDROMSTART: c_ulong = 0x5308;
pub const CDROMEJECT: c_ulong = 0x5309;
pub const CDROMVOLCTRL: c_ulong = 0x530a;
pub const CDROMSUBCHNL: c_ulong = 0x530b;
pub const CDROMREADAUDIO: c_ulong = 0x530e;
pub const CDROMEJECT_SW: c_ulong = 0x530f;
pub const CDROMMULTISESSION: c_ulong = 0x5310;
pub const CDROM_GET_MCN: c_ulong = 0x5311;
pub const CDROMCLOSETRAY: c_ulong = 0x5319;
pub const CDROM_SET_OPTIONS: c_ulong = 0x5320;
pub const CDROM_CLEAR_OPTIONS: c_ulong = 0x5321;
pub const CDROM_SELECT_SPEED: c_ulong = 0x5322;
pub const CDROM_MEDIA_CHANGED: c_ulong = 0x5325;
pub const CDROM_DRIVE_STATUS: c_ulong = 0x5326;
pub const CDROM_DISC_STATUS: c_ulong = 0x5327;
pub const CDROM_LOCKDOOR: c_ulong = 0x5329;
pub const CDROM_GET_CAPABILITY: c_ulong = 0x5331;
pub const CDROM_SEND_PACKET: c_ulong = 0x5393;

// ---------------------------------------------------------------------------
// Address formats (cdrom_tocentry.cdte_format, cdrom_subchnl.cdsc_format,
// cdrom_read_audio.addr_format)
// ---------------------------------------------------------------------------

/// Logical block addressing (first frame is #0).
pub const CDROM_LBA: c_uchar = 0x01;
/// Minute-second-frame addressing (binary, not BCD).
pub const CDROM_MSF: c_uchar = 0x02;

/// Bit in `cdrom_tocentry.adr_ctrl` marking a data track.
pub const CDROM_DATA_TRACK: c_uchar = 0x04;

/// Track number of the lead-out entry in the table of contents.
pub const CDROM_LEADOUT: c_uchar = 0xAA;

// ---------------------------------------------------------------------------
// Audio status (cdrom_subchnl.cdsc_audiostatus)
// ---------------------------------------------------------------------------

pub const CDROM_AUDIO_INVALID: c_uchar = 0x00;
pub const CDROM_AUDIO_PLAY: c_uchar = 0x11;
pub const CDROM_AUDIO_PAUSED: c_uchar = 0x12;
pub const CDROM_AUDIO_COMPLETED: c_uchar = 0x13;
pub const CDROM_AUDIO_ERROR: c_uchar = 0x14;
pub const CDROM_AUDIO_NO_STATUS: c_uchar = 0x15;

// ---------------------------------------------------------------------------
// Drive / disc status (CDROM_DRIVE_STATUS / CDROM_DISC_STATUS return values)
// ---------------------------------------------------------------------------

pub const CDS_NO_INFO: c_int = 0;
pub const CDS_NO_DISC: c_int = 1;
pub const CDS_TRAY_OPEN: c_int = 2;
pub const CDS_DRIVE_NOT_READY: c_int = 3;
pub const CDS_DISC_OK: c_int = 4;

pub const CDS_AUDIO: c_int = 100;
pub const CDS_DATA_1: c_int = 101;
pub const CDS_DATA_2: c_int = 102;
pub const CDS_XA_2_1: c_int = 103;
pub const CDS_XA_2_2: c_int = 104;
pub const CDS_MIXED: c_int = 105;

// ---------------------------------------------------------------------------
// Generic command data direction (cdrom_generic_command.data_direction)
// ---------------------------------------------------------------------------

pub const CGC_DATA_UNKNOWN: c_uchar = 0;
pub const CGC_DATA_WRITE: c_uchar = 1;
pub const CGC_DATA_READ: c_uchar = 2;
pub const CGC_DATA_NONE: c_uchar = 3;

// ---------------------------------------------------------------------------
// Selected MMC opcodes (cdrom_generic_command.cmd)
// ---------------------------------------------------------------------------

pub const GPCMD_INQUIRY: c_uchar = 0x12;
/// START/STOP UNIT (byte 4: bit 0 start, bit 1 LOEJ/eject).
pub const GPCMD_START_STOP_UNIT: c_uchar = 0x1E;

/// Length of the SCSI command block in `cdrom_generic_command`.
pub const CDROM_PACKET_SIZE: usize = 12;

// ---------------------------------------------------------------------------
// Red Book constants
// ---------------------------------------------------------------------------

/// Frames per second.
pub const CD_FRAMES: u32 = 75;
/// Bytes per raw (audio) frame.
pub const CD_FRAMESIZE_RAW: usize = 2352;
/// MSF numbering offset of the first frame (LBA 0 == MSF 2:00:00).
pub const CD_MSF_OFFSET: i64 = 150;

// ---------------------------------------------------------------------------
// ioctl structures
// ---------------------------------------------------------------------------

/// `struct cdrom_tochdr` — returned by CDROMREADTOCHDR.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Cdrom_tochdr {
    /// First track number.
    pub trk0: c_uchar,
    /// Last track number.
    pub trk1: c_uchar,
}

/// `struct cdrom_tocentry` — passed to and returned by CDROMREADTOCENTRY.
///
/// `addr` is the track start address, interpreted per `format`; on
/// little-endian targets its low byte is the MSF minute when
/// `format == CDROM_MSF`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cdrom_tocentry {
    /// Track number (1..=100, or `CDROM_LEADOUT` for the lead-out).
    pub track: c_uchar,
    /// ADR (high nibble) and control (low nibble) bits.
    pub adr_ctrl: c_uchar,
    /// Address format: `CDROM_LBA` or `CDROM_MSF`.
    pub format: c_uchar,
    /// Track start address, interpreted per `format`.
    pub addr: u32,
    /// Data mode.
    pub datamode: c_uchar,
}

impl Cdrom_tocentry {
    /// An entry requesting the TOC record for `track` in `format`.
    pub fn new(track: u8, format: u8) -> Self {
        Self {
            track,
            adr_ctrl: 0,
            format,
            addr: 0,
            datamode: 0,
        }
    }

    /// `true` if this is a data (non-audio) track.
    pub fn is_data(&self) -> bool {
        self.adr_ctrl & CDROM_DATA_TRACK != 0
    }
}

/// `struct cdrom_subchnl` — passed to and returned by CDROMSUBCHNL.
///
/// `absaddr`/`reladdr` are interpreted per `format`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cdrom_subchnl {
    /// Address format: `CDROM_LBA` or `CDROM_MSF`.
    pub format: c_uchar,
    /// Audio status (see `CDROM_AUDIO_*`).
    pub audiostatus: c_uchar,
    /// ADR (high nibble) and control (low nibble) bits.
    pub adr_ctrl: c_uchar,
    /// Track number.
    pub trk: c_uchar,
    /// Index number.
    pub ind: c_uchar,
    /// Absolute position on the disc.
    pub absaddr: u32,
    /// Position relative to the start of the current track.
    pub reladdr: u32,
}

impl Cdrom_subchnl {
    /// A query requesting subchannel data in `format`.
    pub fn new(format: u8) -> Self {
        Self {
            format,
            audiostatus: 0,
            adr_ctrl: 0,
            trk: 0,
            ind: 0,
            absaddr: 0,
            reladdr: 0,
        }
    }
}

/// `struct cdrom_read_audio` — passed to CDROMREADAUDIO.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cdrom_read_audio {
    /// Start address, interpreted per `addr_format`.
    pub addr: u32,
    /// Address format: `CDROM_LBA` or `CDROM_MSF`.
    pub addr_format: c_uchar,
    /// Number of frames to read (1..=`CD_FRAMES`).
    pub nframes: c_int,
    /// Caller-provided buffer of `nframes * CD_FRAMESIZE_RAW` bytes.
    pub buf: *mut c_char,
}

impl Cdrom_read_audio {
    /// A read of `nframes` frames starting at LBA `addr` into `buf`.
    pub fn new_lba(addr: u32, nframes: i32, buf: *mut c_char) -> Self {
        Self {
            addr,
            addr_format: CDROM_LBA,
            nframes,
            buf,
        }
    }
}

/// `struct cdrom_mcn` — returned by CDROM_GET_MCN.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Cdrom_mcn {
    /// 13 ASCII digits, null-terminated.
    pub mcn: [c_uchar; 14],
}

impl Cdrom_mcn {
    /// The medium catalog number as a trimmed string, or `None` if absent.
    pub fn as_str(&self) -> Option<&str> {
        let end = self.mcn.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&self.mcn[..end]).ok()
    }
}

/// `struct cdrom_generic_command` — passed to CDROMSENDPACKET.
///
/// The kernel copies the struct, executes the SCSI command, and copies it
/// back; `stat` is 0 on success.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cdrom_generic_command {
    /// SCSI command block.
    pub cmd: [c_uchar; CDROM_PACKET_SIZE],
    /// Data buffer (valid when `data_direction != CGC_DATA_NONE`).
    pub buffer: *mut c_char,
    /// Length of `buffer`.
    pub buflen: c_uint,
    /// Command status (0 on success). In/out.
    pub stat: c_int,
    /// Caller-provided 32-byte sense buffer.
    pub sense: *mut c_char,
    /// One of `CGC_DATA_*`.
    pub data_direction: c_uchar,
    /// Non-zero to suppress error reporting.
    pub quiet: c_int,
    /// Timeout in seconds (0 for the driver default).
    pub timeout: c_uint,
    /// Reserved (unused by the kernel).
    pub reserved: *mut c_char,
}

impl Cdrom_generic_command {
    /// A new generic command with zeroed fields.
    pub fn new(cmd: [u8; CDROM_PACKET_SIZE]) -> Self {
        Self {
            cmd,
            buffer: std::ptr::null_mut(),
            buflen: 0,
            stat: 0,
            sense: std::ptr::null_mut(),
            data_direction: CGC_DATA_NONE,
            quiet: 0,
            timeout: 0,
            reserved: std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: pin the ABI
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn tochdr_layout() {
        assert_eq!(size_of::<Cdrom_tochdr>(), 2);
        assert_eq!(offset_of!(Cdrom_tochdr, trk0), 0);
        assert_eq!(offset_of!(Cdrom_tochdr, trk1), 1);
    }

    #[test]
    fn tocentry_layout() {
        assert_eq!(size_of::<Cdrom_tocentry>(), 12);
        assert_eq!(offset_of!(Cdrom_tocentry, track), 0);
        assert_eq!(offset_of!(Cdrom_tocentry, adr_ctrl), 1);
        assert_eq!(offset_of!(Cdrom_tocentry, format), 2);
        assert_eq!(offset_of!(Cdrom_tocentry, addr), 4);
        assert_eq!(offset_of!(Cdrom_tocentry, datamode), 8);
    }

    #[test]
    fn subchnl_layout() {
        assert_eq!(size_of::<Cdrom_subchnl>(), 16);
        assert_eq!(offset_of!(Cdrom_subchnl, format), 0);
        assert_eq!(offset_of!(Cdrom_subchnl, audiostatus), 1);
        assert_eq!(offset_of!(Cdrom_subchnl, adr_ctrl), 2);
        assert_eq!(offset_of!(Cdrom_subchnl, trk), 3);
        assert_eq!(offset_of!(Cdrom_subchnl, ind), 4);
        assert_eq!(offset_of!(Cdrom_subchnl, absaddr), 8);
        assert_eq!(offset_of!(Cdrom_subchnl, reladdr), 12);
    }

    #[test]
    fn read_audio_layout() {
        assert_eq!(size_of::<Cdrom_read_audio>(), 24);
        assert_eq!(offset_of!(Cdrom_read_audio, addr), 0);
        assert_eq!(offset_of!(Cdrom_read_audio, addr_format), 4);
        assert_eq!(offset_of!(Cdrom_read_audio, nframes), 8);
        assert_eq!(offset_of!(Cdrom_read_audio, buf), 16);
    }

    #[test]
    fn mcn_layout() {
        assert_eq!(size_of::<Cdrom_mcn>(), 14);
    }

    #[test]
    fn generic_command_layout() {
        assert_eq!(size_of::<Cdrom_generic_command>(), 64);
        assert_eq!(offset_of!(Cdrom_generic_command, cmd), 0);
        assert_eq!(offset_of!(Cdrom_generic_command, buffer), 16);
        assert_eq!(offset_of!(Cdrom_generic_command, buflen), 24);
        assert_eq!(offset_of!(Cdrom_generic_command, stat), 28);
        assert_eq!(offset_of!(Cdrom_generic_command, sense), 32);
        assert_eq!(offset_of!(Cdrom_generic_command, data_direction), 40);
        assert_eq!(offset_of!(Cdrom_generic_command, quiet), 44);
        assert_eq!(offset_of!(Cdrom_generic_command, timeout), 48);
        assert_eq!(offset_of!(Cdrom_generic_command, reserved), 56);
    }
}

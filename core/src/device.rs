//! CD-ROM device access: opening, status, identification, and TOC.

use std::fs::File;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::prelude::AsRawFd;

use libc::c_char;

use crate::error::{Error, Result};
use crate::sys::{
    self, Cdrom_generic_command, Cdrom_mcn, Cdrom_tocentry, Cdrom_tochdr, ioctl_cmd, ioctl_with,
};
use crate::toc::Toc;

/// A CD-ROM device (e.g. `/dev/sr0`).
///
/// Devices are opened read-only and non-blocking, as required by the
/// `cdrom` ioctl interface: with `O_NONBLOCK`, ioctls fail with
/// descriptive errors (e.g. `ENOMEDIUM`) when no disc is present.
#[derive(Debug)]
pub struct Device {
    path: String,
    file: File,
}

impl Device {
    /// Opens a CD-ROM device for reading.
    pub fn open(path: impl AsRef<std::ffi::OsStr>) -> Result<Self> {
        let path = path.as_ref();
        let file = match File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                return Err(Error::DeviceNotFound {
                    path: path.to_string_lossy().into_owned(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            file,
        })
    }

    /// The device path (e.g. `/dev/sr0`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The open file handle, for raw reads and audio streaming.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Discovers CD-ROM devices by scanning `/dev` for `sr*` entries,
    /// returned in device-number order. Devices that cannot be opened
    /// (e.g. permission denied) are skipped.
    pub fn discover() -> Result<Vec<Device>> {
        let mut candidates: Vec<(u32, String)> = Vec::new();
        for entry in std::fs::read_dir("/dev")? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(num) = name
                .to_str()
                .and_then(|n| n.strip_prefix("sr"))
                .and_then(|s| s.parse::<u32>().ok())
            {
                candidates.push((num, name.to_string_lossy().into_owned()));
            }
        }
        candidates.sort_by_key(|(num, _)| *num);
        let mut devices = Vec::new();
        for (_, name) in candidates {
            if let Ok(device) = Self::open(format!("/dev/{name}")) {
                devices.push(device);
            }
        }
        Ok(devices)
    }

    /// The drive status (tray position, presence of a disc, ...).
    pub fn drive_status(&self) -> Result<DriveStatus> {
        let code = ioctl_cmd(&self.file, sys::CDROM_DRIVE_STATUS)?;
        DriveStatus::from_code(code)
            .ok_or_else(|| Error::Unexpected(format!("{}: unknown drive status {code}", self.path)))
    }

    /// The disc status (media type), if a disc is present.
    pub fn disc_status(&self) -> Result<DiscStatus> {
        let code = ioctl_cmd(&self.file, sys::CDROM_DISC_STATUS)?;
        DiscStatus::from_code(code)
            .ok_or_else(|| Error::Unexpected(format!("{}: unknown disc status {code}", self.path)))
    }

    /// Ensures a disc is present and ready, translating the drive status
    /// into a meaningful error otherwise.
    pub fn require_disc(&self) -> Result<()> {
        match self.drive_status()? {
            DriveStatus::DiscOk => Ok(()),
            DriveStatus::NoDisc | DriveStatus::TrayOpen => Err(Error::NoDisc {
                path: self.path.clone(),
            }),
            DriveStatus::DriveNotReady => Err(Error::DriveNotReady {
                path: self.path.clone(),
            }),
            DriveStatus::NoInfo => Err(Error::Unexpected(format!(
                "{}: drive status not supported",
                self.path
            ))),
        }
    }

    /// Status and identification information about the device.
    ///
    /// Identification fields are `None` if the drive does not support the
    /// SCSI INQUIRY command.
    pub fn info(&self) -> Result<DeviceInfo> {
        let drive_status = self.drive_status()?;
        let disc_status = match drive_status {
            DriveStatus::DiscOk => Some(self.disc_status()?),
            _ => None,
        };
        let (vendor, model, version) = self.inquire().unwrap_or((None, None, None));
        Ok(DeviceInfo {
            path: self.path.clone(),
            drive_status,
            disc_status,
            vendor,
            model,
            version,
        })
    }

    /// The table of contents of the disc in the drive.
    pub fn toc(&self) -> Result<Toc> {
        self.require_disc()?;

        let mut header = Cdrom_tochdr::default();
        ioctl_with(&self.file, sys::CDROMREADTOCHDR, &mut header)?;
        let first = (header.trk0 as u32).max(1) as usize;
        let last = header.trk1 as usize;
        if last < 1 {
            return Err(Error::NoDisc {
                path: self.path.clone(),
            });
        }

        let mut entries = Vec::with_capacity(last - first + 1);
        for track in first..=last {
            let mut entry = Cdrom_tocentry::new(track as u8, sys::CDROM_LBA);
            ioctl_with(&self.file, sys::CDROMREADTOCENTRY, &mut entry)?;
            entries.push(entry);
        }
        let mut leadout = Cdrom_tocentry::new(sys::CDROM_LEADOUT, sys::CDROM_LBA);
        ioctl_with(&self.file, sys::CDROMREADTOCENTRY, &mut leadout)?;

        Ok(Toc::from_entries(&entries, &leadout))
    }

    /// The medium catalog number (UPC), if the disc has one.
    pub fn mcn(&self) -> Result<Option<String>> {
        let mut mcn = Cdrom_mcn::default();
        ioctl_with(&self.file, sys::CDROM_GET_MCN, &mut mcn)?;
        Ok(mcn.as_str().map(str::to_string))
    }

    /// Ejects the disc (opens the tray).
    ///
    /// The kernel refuses `CDROMEJECT` with `EBUSY` when the door is
    /// locked or the device is held open elsewhere (e.g. the disc is
    /// mounted), so the door is unlocked first and a raw SCSI eject is
    /// used as a fallback — the same strategy as the standard `eject`
    /// utility.
    pub fn eject(&self) -> Result<()> {
        // Unlock the door; a plain value, not a pointer. Best effort:
        // drives without a lockable door ignore it.
        // SAFETY: the fd is owned by `file` and valid; no user data is passed.
        let _ = unsafe { libc::ioctl(self.file.as_raw_fd(), sys::CDROM_LOCKDOOR, 0i32) };

        if ioctl_cmd(&self.file, sys::CDROMEJECT).is_ok() {
            return Ok(());
        }
        self.scsi_eject()
    }

    /// Ejects with a raw SCSI START/STOP UNIT command, which bypasses the
    /// uniform CD-ROM layer's checks on open file descriptors and the
    /// door lock.
    fn scsi_eject(&self) -> Result<()> {
        // Allow medium removal first, then request the eject (LOEJ bit).
        let _ = self.scsi_start_stop(0);
        self.scsi_start_stop(1)
    }

    /// START/STOP UNIT with the given value in byte 4 (0: stop, 1: eject).
    fn scsi_start_stop(&self, byte4: u8) -> Result<()> {
        let mut sense = [0u8; 32];
        let mut cgc = Cdrom_generic_command::new([
            sys::GPCMD_START_STOP_UNIT,
            0,
            0,
            0,
            byte4,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        cgc.sense = sense.as_mut_ptr().cast::<c_char>();
        cgc.data_direction = sys::CGC_DATA_NONE;

        // SAFETY: the fd is owned by `file` and valid; `cgc` outlives the call.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), sys::CDROM_SEND_PACKET, &mut cgc) };
        if ret < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if cgc.stat != 0 {
            return Err(Error::Unexpected(format!(
                "{}: START/STOP UNIT failed (stat={})",
                self.path, cgc.stat
            )));
        }
        Ok(())
    }

    /// Closes the tray.
    pub fn close_tray(&self) -> Result<()> {
        ioctl_cmd(&self.file, sys::CDROMCLOSETRAY)?;
        Ok(())
    }

    /// Spins the drive up (no-op if already running).
    pub fn spin_up(&self) -> Result<()> {
        ioctl_cmd(&self.file, sys::CDROMSTART)?;
        Ok(())
    }

    /// Spins the drive down (best effort; some drives ignore it).
    pub fn spin_down(&self) -> Result<()> {
        ioctl_cmd(&self.file, sys::CDROMSTOP)?;
        Ok(())
    }

    fn inquire(&self) -> Result<(Option<String>, Option<String>, Option<String>)> {
        const INQUIRY_LEN: usize = 96;
        let mut data = [0u8; INQUIRY_LEN];
        let mut sense = [0u8; 32];
        let mut cgc = Cdrom_generic_command::new([
            sys::GPCMD_INQUIRY,
            0,
            0,
            0,
            INQUIRY_LEN as u8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        cgc.buffer = data.as_mut_ptr().cast::<c_char>();
        cgc.buflen = INQUIRY_LEN as u32;
        cgc.sense = sense.as_mut_ptr().cast::<c_char>();
        cgc.data_direction = sys::CGC_DATA_READ;

        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), sys::CDROM_SEND_PACKET, &mut cgc) };
        if ret < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if cgc.stat != 0 {
            return Err(Error::Unexpected(format!(
                "{}: INQUIRY failed (stat={})",
                self.path, cgc.stat
            )));
        }

        let field = |start: usize, len: usize| -> Option<String> {
            std::str::from_utf8(&data[start..start + len])
                .ok()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Ok((field(8, 8), field(16, 16), field(32, 4)))
    }
}

/// Information about a device and the disc in it.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device path.
    pub path: String,
    /// Drive status.
    pub drive_status: DriveStatus,
    /// Disc status, if a disc is present.
    pub disc_status: Option<DiscStatus>,
    /// Vendor string, if discoverable.
    pub vendor: Option<String>,
    /// Model string, if discoverable.
    pub model: Option<String>,
    /// Firmware version, if discoverable.
    pub version: Option<String>,
}

/// Status of the drive itself (from CDROM_DRIVE_STATUS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStatus {
    /// The driver does not report status.
    NoInfo,
    /// No disc in the drive.
    NoDisc,
    /// The tray is open.
    TrayOpen,
    /// The drive is not ready.
    DriveNotReady,
    /// A disc is present and ready.
    DiscOk,
}

impl DriveStatus {
    pub fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            sys::CDS_NO_INFO => Self::NoInfo,
            sys::CDS_NO_DISC => Self::NoDisc,
            sys::CDS_TRAY_OPEN => Self::TrayOpen,
            sys::CDS_DRIVE_NOT_READY => Self::DriveNotReady,
            sys::CDS_DISC_OK => Self::DiscOk,
            _ => return None,
        })
    }
}

impl std::fmt::Display for DriveStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::NoInfo => "no information",
            Self::NoDisc => "no disc",
            Self::TrayOpen => "tray open",
            Self::DriveNotReady => "not ready",
            Self::DiscOk => "disc present",
        };
        f.write_str(s)
    }
}

/// Status of the disc in the drive (from CDROM_DISC_STATUS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscStatus {
    /// Audio disc (Red Book).
    Audio,
    /// Data disc, mode 1 (Yellow Book).
    Data1,
    /// Data disc, mode 2.
    Data2,
    /// XA disc, mode 2 form 1.
    Xa21,
    /// XA disc, mode 2 form 2.
    Xa22,
    /// Mixed audio/data disc.
    Mixed,
}

impl DiscStatus {
    pub fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            sys::CDS_AUDIO => Self::Audio,
            sys::CDS_DATA_1 => Self::Data1,
            sys::CDS_DATA_2 => Self::Data2,
            sys::CDS_XA_2_1 => Self::Xa21,
            sys::CDS_XA_2_2 => Self::Xa22,
            sys::CDS_MIXED => Self::Mixed,
            _ => return None,
        })
    }
}

impl std::fmt::Display for DiscStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Audio => "audio",
            Self::Data1 => "data (mode 1)",
            Self::Data2 => "data (mode 2)",
            Self::Xa21 => "XA (2,1)",
            Self::Xa22 => "XA (2,2)",
            Self::Mixed => "mixed audio/data",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_missing_device() {
        let err = Device::open("/dev/sr-definitely-not-there").unwrap_err();
        assert!(
            matches!(&err, Error::DeviceNotFound { path } if path == "/dev/sr-definitely-not-there")
        );
    }

    #[test]
    fn drive_status_codes() {
        assert_eq!(DriveStatus::from_code(0), Some(DriveStatus::NoInfo));
        assert_eq!(DriveStatus::from_code(1), Some(DriveStatus::NoDisc));
        assert_eq!(DriveStatus::from_code(2), Some(DriveStatus::TrayOpen));
        assert_eq!(DriveStatus::from_code(3), Some(DriveStatus::DriveNotReady));
        assert_eq!(DriveStatus::from_code(4), Some(DriveStatus::DiscOk));
        assert_eq!(DriveStatus::from_code(99), None);
    }

    #[test]
    fn disc_status_codes() {
        assert_eq!(DiscStatus::from_code(100), Some(DiscStatus::Audio));
        assert_eq!(DiscStatus::from_code(101), Some(DiscStatus::Data1));
        assert_eq!(DiscStatus::from_code(102), Some(DiscStatus::Data2));
        assert_eq!(DiscStatus::from_code(103), Some(DiscStatus::Xa21));
        assert_eq!(DiscStatus::from_code(104), Some(DiscStatus::Xa22));
        assert_eq!(DiscStatus::from_code(105), Some(DiscStatus::Mixed));
        assert_eq!(DiscStatus::from_code(106), None);
    }

    #[test]
    fn status_display() {
        assert_eq!(DriveStatus::NoDisc.to_string(), "no disc");
        assert_eq!(DriveStatus::TrayOpen.to_string(), "tray open");
        assert_eq!(DiscStatus::Audio.to_string(), "audio");
        assert_eq!(DiscStatus::Mixed.to_string(), "mixed audio/data");
    }
}

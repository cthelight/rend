/// A Linux CD-ROM device (`/dev/sr0`, ...).
#[derive(Debug, Clone)]
pub struct Device {
    /// Device path, e.g. `/dev/sr0`.
    pub path: String,
    /// Drive status as last observed.
    pub status: DriveStatus,
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
}

/// Status of the drive itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStatus {
    NoInfo,
    NoDisc,
    TrayOpen,
    DriveNotReady,
    DiscOk,
}

/// Status of the disc in the drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscStatus {
    Audio,
    Data1,
    Data2,
    Xa2_1,
    Xa2_2,
    Mixed,
}

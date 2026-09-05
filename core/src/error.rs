use thiserror::Error;

/// Errors returned by rend-core operations.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// No CD-ROM devices were found on the system.
    #[error("no CD-ROM devices found")]
    NoDevices,

    /// The given device path does not exist or is not a CD-ROM device.
    #[error("device {path} not found")]
    DeviceNotFound { path: String },

    /// No disc is present in the drive.
    #[error("no disc in {path}")]
    NoDisc { path: String },

    /// The drive is present but not ready.
    #[error("drive {path} is not ready")]
    DriveNotReady { path: String },

    /// The disc does not contain any audio tracks.
    #[error("disc in {path} has no audio tracks")]
    NoAudioTracks { path: String },

    /// The device reported data in an unexpected form.
    #[error("{0}")]
    Unexpected(String),
}

/// Result type for rend-core operations.
pub type Result<T> = std::result::Result<T, Error>;

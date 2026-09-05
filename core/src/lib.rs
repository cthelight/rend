//! rend-core: low-level access to Linux CD-ROM drives.
//!
//! This crate wraps the Linux `cdrom` ioctl interface (see
//! `<linux/cdrom.h>`) to expose device discovery, table-of-contents
//! reading, subchannel queries, and raw CDDA (Red Book audio) frame
//! reading.

pub mod audio;
pub mod device;
pub mod error;
pub mod msf;
pub mod sys;
pub mod toc;

pub use audio::{AudioStatus, CddaStream, FRAME_SIZE, FRAMES_PER_SECOND, FrameSource, Subchannel};
pub use device::{Device, DeviceInfo, DiscStatus, DriveStatus};
pub use error::Error;
pub use msf::Msf;
pub use toc::{Toc, Track, TrackType};

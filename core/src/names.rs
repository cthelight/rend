//! Friendly names for drives.
//!
//! A drive is labeled with its vendor and model by default. The
//! `--drive-name` option and the `REND_DRIVE_NAMES` environment variable
//! map drives to friendly labels instead, one `DRIVE=NAME` pair each
//! (e.g. `sr0=Living Room`), comma-separated in the environment.

use std::collections::BTreeMap;

use crate::device::DeviceInfo;

/// Friendly names for drives, as `DRIVE=NAME` pairs.
///
/// A drive may be named by device number (`sr0`) or by path
/// (`/dev/sr0`); both refer to the same drive.
#[derive(Debug, Clone, Default)]
pub struct DriveNames {
    names: BTreeMap<String, String>,
}

impl DriveNames {
    /// Builds the mapping from `DRIVE=NAME` pairs; with duplicates, the
    /// last pair wins.
    pub fn from_pairs(pairs: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, String> {
        let mut names = Self::default();
        for pair in pairs {
            names.insert(pair.as_ref())?;
        }
        Ok(names)
    }

    /// Records one `DRIVE=NAME` pair.
    pub fn insert(&mut self, pair: &str) -> Result<(), String> {
        let (key, name) = pair
            .split_once('=')
            .ok_or_else(|| format!("expected DRIVE=NAME, got `{pair}`"))?;
        let key = normalize_key(key);
        let name = name.trim();
        if key.is_empty() {
            return Err(format!("empty drive name in `{pair}`"));
        }
        if name.is_empty() {
            return Err(format!("empty friendly name in `{pair}`"));
        }
        self.names.insert(key, name.to_string());
        Ok(())
    }

    /// The friendly name of the drive at `path`, if it is mapped.
    pub fn name_for(&self, path: &str) -> Option<&str> {
        self.names.get(&normalize_key(path)).map(String::as_str)
    }

    /// `true` when no drive is mapped.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// `sr0` and `/dev/sr0` name the same drive.
fn normalize_key(key: &str) -> String {
    key.trim().trim_start_matches("/dev/").to_string()
}

/// The label for a drive: its friendly name when mapped, otherwise its
/// vendor and model, with the firmware version when the drive reports one.
pub fn drive_label(names: &DriveNames, info: &DeviceInfo) -> String {
    if let Some(name) = names.name_for(&info.path) {
        return name.to_string();
    }
    let vendor = info.vendor.as_deref().unwrap_or("").trim();
    let model = info.model.as_deref().unwrap_or("unknown").trim();
    let mut label = if vendor.is_empty() {
        model.to_string()
    } else {
        format!("{vendor} {model}")
    };
    if let Some(version) = info
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        label.push_str(&format!(" v{version}"));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{DiscStatus, DriveStatus};

    fn info(path: &str) -> DeviceInfo {
        DeviceInfo {
            path: path.into(),
            drive_status: DriveStatus::DiscOk,
            disc_status: Some(DiscStatus::Audio),
            vendor: Some("PIONEER".into()),
            model: Some("DVR-KX6".into()),
            version: Some("V1.17".into()),
        }
    }

    #[test]
    fn parses_pairs() {
        let mut names = DriveNames::default();
        names.insert("sr0=Living Room").unwrap();
        names.insert("/dev/sr1=Office").unwrap();
        assert_eq!(names.name_for("/dev/sr0"), Some("Living Room"));
        assert_eq!(names.name_for("/dev/sr1"), Some("Office"));
        assert_eq!(names.name_for("/dev/sr2"), None);
        assert!(!names.is_empty());

        let empty = DriveNames::default();
        assert!(empty.is_empty());
        assert_eq!(empty.name_for("/dev/sr0"), None);
    }

    #[test]
    fn the_same_key_by_number_or_path() {
        let mut names = DriveNames::default();
        names.insert("sr0=Living Room").unwrap();
        assert_eq!(names.name_for("/dev/sr0"), Some("Living Room"));
        assert_eq!(names.name_for("sr0"), Some("Living Room"));
    }

    #[test]
    fn a_later_pair_overrides_an_earlier_one() {
        let names = DriveNames::from_pairs(["sr0=Old", "sr0=New", "/dev/sr0=Latest"]).unwrap();
        assert_eq!(names.name_for("/dev/sr0"), Some("Latest"));
    }

    #[test]
    fn pairs_surrounding_whitespace_is_ignored() {
        let names = DriveNames::from_pairs(["  sr0 =  Living Room  "]).unwrap();
        assert_eq!(names.name_for("/dev/sr0"), Some("Living Room"));
    }

    #[test]
    fn malformed_pairs_are_rejected() {
        for pair in ["", "sr0", "=Living Room", "sr0=", "  =  "] {
            let names = DriveNames::from_pairs([pair]);
            assert!(
                names.is_err(),
                "expected `{pair}` to be rejected, got {names:?}"
            );
        }
    }

    #[test]
    fn a_friendly_name_replaces_the_vendor_and_model() {
        let names = DriveNames::from_pairs(["/dev/sr0=Living Room"]).unwrap();
        assert_eq!(drive_label(&names, &info("/dev/sr0")), "Living Room");
        // Unmapped drives keep the vendor, model, and version.
        assert_eq!(
            drive_label(&names, &info("/dev/sr1")),
            "PIONEER DVR-KX6 vV1.17"
        );
    }

    #[test]
    fn the_default_label_is_vendor_and_model() {
        let names = DriveNames::default();
        let mut unknown = info("/dev/sr0");
        unknown.vendor = None;
        unknown.model = None;
        unknown.version = None;
        assert_eq!(
            drive_label(&names, &info("/dev/sr0")),
            "PIONEER DVR-KX6 vV1.17"
        );
        assert_eq!(drive_label(&names, &unknown), "unknown");
    }
}

//! The order the drives are listed in.
//!
//! Drives are listed in discovery order by default. The `--drive-order`
//! option and the `REND_DRIVE_ORDER` environment variable name an explicit
//! order instead: a list of drives, one `sr0` or `/dev/sr0` per entry,
//! comma-separated in the environment. The drives the list names come
//! first, in the order the list names them; the drives it does not name
//! follow in their original relative order.

/// An explicit order for drives, as a list of drive identifiers.
///
/// A drive may be named by device number (`sr0`) or by path (`/dev/sr0`);
/// both refer to the same drive.
#[derive(Debug, Clone, Default)]
pub struct DriveOrder {
    order: Vec<String>,
}

impl DriveOrder {
    /// Builds the order from drive identifiers; blank entries are ignored,
    /// and duplicates are dropped, keeping the first occurrence.
    pub fn from_parts(parts: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut order = Self::default();
        for part in parts {
            let key = normalize_key(part.as_ref());
            if !key.is_empty() && !order.order.contains(&key) {
                order.order.push(key);
            }
        }
        order
    }

    /// The position of the drive at `path` in the order, or `None` when
    /// the drive is not named.
    pub fn position_of(&self, path: &str) -> Option<usize> {
        self.order
            .iter()
            .position(|key| *key == normalize_key(path))
    }

    /// `true` when no drive is named.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// `sr0` and `/dev/sr0` name the same drive.
fn normalize_key(key: &str) -> String {
    key.trim().trim_start_matches("/dev/").to_string()
}

/// The drives reordered by `order`: the drives it names, in the order it
/// names them, followed by the rest in their original relative order.
pub fn order_drives<T>(mut drives: Vec<T>, order: &DriveOrder, key: impl Fn(&T) -> &str) -> Vec<T> {
    drives.sort_by_key(|drive| order.position_of(key(drive)).unwrap_or(order.order.len()));
    drives
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(drives: &[&str]) -> Vec<String> {
        drives.iter().map(|p| format!("/dev/{p}")).collect()
    }

    fn ordered(order: &DriveOrder, drives: &[&str]) -> Vec<String> {
        order_drives(paths(drives), order, |d| d.as_str())
    }

    #[test]
    fn the_named_drives_come_first_in_the_given_order() {
        let order = DriveOrder::from_parts(["sr1", "sr0"]);
        assert_eq!(
            ordered(&order, &["sr0", "sr1", "sr2"]),
            paths(&["sr1", "sr0", "sr2"])
        );
    }

    #[test]
    fn the_unnamed_drives_keep_their_relative_order() {
        let order = DriveOrder::from_parts(["sr2"]);
        assert_eq!(
            ordered(&order, &["sr0", "sr1", "sr2"]),
            paths(&["sr2", "sr0", "sr1"])
        );
    }

    #[test]
    fn a_number_and_a_path_name_the_same_drive() {
        let order = DriveOrder::from_parts(["/dev/sr1"]);
        assert_eq!(ordered(&order, &["sr0", "sr1"]), paths(&["sr1", "sr0"]));
        assert_eq!(order.position_of("/dev/sr1"), Some(0));
        assert_eq!(order.position_of("sr1"), Some(0));
    }

    #[test]
    fn a_repeated_drive_keeps_its_first_position() {
        let order = DriveOrder::from_parts(["sr1", "sr0", "sr1"]);
        assert_eq!(ordered(&order, &["sr0", "sr1"]), paths(&["sr1", "sr0"]));
    }

    #[test]
    fn blank_entries_are_ignored() {
        let order = DriveOrder::from_parts(["", "  ", "sr0", " "]);
        assert!(!order.is_empty());
        assert_eq!(ordered(&order, &["sr0", "sr1"]), paths(&["sr0", "sr1"]));
    }

    #[test]
    fn drives_the_order_does_not_name_are_left_alone() {
        let order = DriveOrder::from_parts(["sr9"]);
        assert_eq!(ordered(&order, &["sr0", "sr1"]), paths(&["sr0", "sr1"]));
    }

    #[test]
    fn an_empty_order_keeps_the_discovery_order() {
        let order = DriveOrder::default();
        assert!(order.is_empty());
        assert_eq!(ordered(&order, &["sr0", "sr1"]), paths(&["sr0", "sr1"]));
    }
}

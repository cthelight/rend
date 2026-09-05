//! A gate that keeps this process's request rate under the servers' limit.
//!
//! MusicBrainz allows at most one request per second per IP address and
//! answers faster bursts with HTTP 503. A [`Throttle`] spaces calls to
//! [`Throttle::wait`] at least a fixed interval apart — across threads —
//! so the process as a whole never sends faster than that.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Spaces calls to [`Throttle::wait`] at least a fixed interval apart.
///
/// Waiting threads reserve slots one after another, so concurrent callers
/// emit at most one call per interval, in the order they arrived.
#[derive(Debug)]
pub struct Throttle {
    next_slot: Mutex<Instant>,
    interval: Duration,
}

impl Throttle {
    /// A throttle that allows a new call every `interval`.
    ///
    /// The first call is allowed immediately.
    pub fn new(interval: Duration) -> Self {
        Self {
            next_slot: Mutex::new(Instant::now()),
            interval,
        }
    }

    /// Blocks until the next call may be made, then returns.
    ///
    /// Each caller reserves the next free slot and sleeps until it, so
    /// callers are spaced at least `interval` apart no matter how many
    /// threads call concurrently.
    pub fn wait(&self) {
        let slot = {
            let mut next_slot = self.next_slot.lock().unwrap();
            let slot = next_slot.max(Instant::now());
            *next_slot = slot + self.interval;
            slot
        };
        let now = Instant::now();
        if slot > now {
            thread::sleep(slot - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_call_is_immediate() {
        let throttle = Throttle::new(Duration::from_secs(1));
        let start = Instant::now();
        throttle.wait();
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn consecutive_waits_are_spaced() {
        let throttle = Throttle::new(Duration::from_millis(100));
        let start = Instant::now();
        throttle.wait();
        throttle.wait();
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn concurrent_waiters_queue_up() {
        let throttle = Throttle::new(Duration::from_millis(100));
        let start = Instant::now();
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..4).map(|_| s.spawn(|| throttle.wait())).collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        // The last waiter cannot finish before its own slot, the fourth one
        // reserved: three full intervals after the first.
        assert!(start.elapsed() >= Duration::from_millis(270));
    }
}

//! Self-measurement of the running process for proof and benchmark artifacts.
//!
//! A proof artifact that carries `process_peak_rss_bytes: null` cannot be told apart from one
//! whose instrument was never attempted. Every reading therefore names its instrument, and an
//! unsupported platform says so in words instead of leaving a hole (#338).

use serde::{Deserialize, Serialize};

/// Peak resident set size of this process, with the instrument that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeakRss {
    /// Peak RSS in bytes; `None` only when no instrument exists for this platform.
    pub bytes: Option<u64>,
    /// The instrument that produced `bytes`, or why none could.
    pub instrument: String,
}

/// The instrument name recorded for a `getrusage(2)` reading.
pub const GETRUSAGE_INSTRUMENT: &str = "getrusage(RUSAGE_SELF).ru_maxrss";
/// The instrument name recorded for a Linux `/proc/self/status` reading.
pub const PROC_STATUS_INSTRUMENT: &str = "/proc/self/status VmHWM";

/// Measures this process's peak resident set size.
///
/// Linux reads `VmHWM` from `/proc/self/status` and falls back to `getrusage(2)`; every other
/// Unix uses `getrusage(2)` directly. Elsewhere the result is `Unsupported` with a reason.
pub fn process_peak_rss() -> PeakRss {
    if let Some(bytes) = proc_status_peak_rss_bytes() {
        return PeakRss {
            bytes: Some(bytes),
            instrument: PROC_STATUS_INSTRUMENT.to_string(),
        };
    }
    if let Some(bytes) = getrusage_peak_rss_bytes() {
        return PeakRss {
            bytes: Some(bytes),
            instrument: GETRUSAGE_INSTRUMENT.to_string(),
        };
    }
    PeakRss {
        bytes: None,
        instrument: format!(
            "unsupported: no peak RSS instrument on {}",
            std::env::consts::OS
        ),
    }
}

/// Peak RSS in bytes, or `None` where no instrument exists. Prefer [`process_peak_rss`] when
/// the value is written to an artifact, so the absence is explained rather than silent.
pub fn process_peak_rss_bytes() -> Option<u64> {
    process_peak_rss().bytes
}

#[cfg(target_os = "linux")]
fn proc_status_peak_rss_bytes() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    content.lines().find_map(|line| {
        let value = line.strip_prefix("VmHWM:")?;
        let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
        kib.checked_mul(1024)
    })
}

#[cfg(not(target_os = "linux"))]
fn proc_status_peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(unix)]
fn getrusage_peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `RUSAGE_SELF` is always a valid target and the pointer names a live, writable
    // `rusage` buffer; the call writes only into that buffer.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: a zero return means the kernel initialised every field.
    let usage = unsafe { usage.assume_init() };
    let max_rss = u64::try_from(usage.ru_maxrss).ok()?;
    bytes_from_max_rss(max_rss, MAX_RSS_UNIT)
}

#[cfg(not(unix))]
fn getrusage_peak_rss_bytes() -> Option<u64> {
    None
}

/// The unit `ru_maxrss` is reported in: bytes on Apple platforms, kilobytes on Linux and the
/// BSDs. Mixing them up is a silent 1024x error, so choosing the unit and applying it are kept
/// apart — the choice is a per-platform constant, the arithmetic is a pure function, and both
/// branches of the arithmetic are tested on every platform rather than only where they are live.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaxRssUnit {
    Bytes,
    Kilobytes,
}

#[cfg(unix)]
const MAX_RSS_UNIT: MaxRssUnit = if cfg!(any(target_os = "macos", target_os = "ios")) {
    MaxRssUnit::Bytes
} else {
    MaxRssUnit::Kilobytes
};

#[cfg(unix)]
fn bytes_from_max_rss(max_rss: u64, unit: MaxRssUnit) -> Option<u64> {
    match unit {
        MaxRssUnit::Bytes => Some(max_rss),
        MaxRssUnit::Kilobytes => max_rss.checked_mul(1024),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_measures_a_positive_peak_rss_with_a_named_instrument() {
        let reading = process_peak_rss();
        let bytes = reading.bytes.expect("unix always has an instrument");
        assert!(bytes > 0);
        assert!(
            reading.instrument == GETRUSAGE_INSTRUMENT
                || reading.instrument == PROC_STATUS_INSTRUMENT,
            "instrument was {}",
            reading.instrument
        );
        // Peak RSS only rises, so a second reading may exceed the first; what must hold is that
        // the instrument stays available and never reports less than it already did.
        let later = process_peak_rss_bytes().expect("the instrument stays available");
        assert!(later >= bytes);
    }

    /// The unit conversion is the only arithmetic this module owns, so it is pinned with
    /// synthetic inputs on both branches, on every platform, rather than only where a branch is
    /// live. There is deliberately no test comparing a live `ru_maxrss` against a live `VmHWM`:
    /// they are separate kernel accounting paths, they diverged by 24x inside a CI container,
    /// and a tolerance loose enough to survive that is far too loose to catch a 1024x error.
    #[cfg(unix)]
    #[test]
    fn max_rss_converts_by_its_documented_unit() {
        assert_eq!(
            bytes_from_max_rss(142_876_672, MaxRssUnit::Bytes),
            Some(142_876_672)
        );
        assert_eq!(
            bytes_from_max_rss(139_528, MaxRssUnit::Kilobytes),
            Some(139_528 * 1024)
        );
        assert_eq!(bytes_from_max_rss(0, MaxRssUnit::Kilobytes), Some(0));
        // An overflowing multiply must yield no reading, never a wrapped one that looks real.
        assert_eq!(bytes_from_max_rss(u64::MAX, MaxRssUnit::Kilobytes), None);
    }

    /// The live reading is what checks that this platform picked the right unit: reading Linux
    /// kilobytes as bytes lands far below 1 MiB, and reading Apple bytes as kilobytes lands far
    /// above 64 GiB. Neither bound can be met by a real test process measured correctly.
    #[cfg(unix)]
    #[test]
    fn getrusage_peak_rss_is_in_bytes_on_this_platform() {
        let bytes = getrusage_peak_rss_bytes().expect("getrusage is available on unix");
        assert!(bytes >= 1 << 20, "{bytes} bytes is below any real process");
        assert!(
            bytes < 64 << 30,
            "{bytes} bytes is above any real test process"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_names_the_reason_instead_of_null() {
        let reading = process_peak_rss();
        assert_eq!(reading.bytes, None);
        assert!(reading.instrument.starts_with("unsupported:"));
    }
}

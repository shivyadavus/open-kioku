//! Deterministic allocation accounting for memory work such as #329.
//!
//! Peak RSS is the wrong instrument for comparing two builds of the indexer.
//! Two runs of the same binary over the same corpus on the same host have been
//! observed 438 MB apart — roughly the size of the savings the #329 work is
//! trying to produce — because RSS reflects the allocator's page-return policy,
//! fragmentation, and page-cache behavior rather than what the program asked
//! for. This counts requested bytes instead, which is far steadier: over three
//! identical indexing runs the counter spread 0.21% while peak RSS of the same
//! runs spread 1.66%. It is not perfectly repeatable — thread scheduling changes
//! which transient allocations overlap, and the allocation count itself varies
//! run to run — so treat a difference below roughly 1% as noise.
//!
//! What it does not see: anything obtained by `mmap` rather than the Rust global
//! allocator. Tantivy segments, the SQLite page cache, and allocations made by C
//! dependencies through their own allocators are all invisible here. This number
//! is therefore a floor on retained memory, never a substitute for peak RSS, and
//! the two must not be reported as if they were the same measurement.
//!
//! Compiled only under the `mem-profile` feature. Two atomics on every
//! allocation is a real cost, so a binary built with this feature must never be
//! used to produce a timing number.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

pub struct CountingAllocator;

impl CountingAllocator {
    #[inline]
    fn grew(size: usize) {
        let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
        // Relaxed ordering means a concurrent peak can be missed by the width of
        // one interleaving. That is acceptable for a profiling aid and keeps the
        // hot path to two uncontended atomics.
        PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
        TOTAL_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    fn shrank(size: usize) {
        LIVE_BYTES.fetch_sub(size, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to the system allocator and only adds counter
// updates, which allocate nothing themselves.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            Self::grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            Self::grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        Self::shrank(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            // A failed realloc leaves the original block intact, so only adjust
            // the counters when the call actually succeeded.
            if new_size >= layout.size() {
                Self::grew(new_size - layout.size());
            } else {
                Self::shrank(layout.size() - new_size);
            }
        }
        new_ptr
    }
}

/// Bytes currently allocated and not yet freed.
pub fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// High-water mark of [`live_bytes`] over the life of the process.
pub fn peak_live_bytes() -> usize {
    PEAK_LIVE_BYTES.load(Ordering::Relaxed)
}

/// Total allocation calls served. Useful for spotting churn that peak alone hides.
pub fn total_allocations() -> usize {
    TOTAL_ALLOCATIONS.load(Ordering::Relaxed)
}

/// Write the accounting summary to stderr.
///
/// stderr keeps this out of `--json` stdout, so a profiling build stays safe to
/// pipe into the same tooling as a normal one.
pub fn report() {
    eprintln!(
        "ok[mem-profile] peak_live_bytes={} peak_live_mib={:.1} live_at_exit_bytes={} allocations={}",
        peak_live_bytes(),
        peak_live_bytes() as f64 / (1024.0 * 1024.0),
        live_bytes(),
        total_allocations(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    // These drive the allocator directly rather than relying on it being
    // installed globally. `#[global_allocator]` lives in the binary, so a lib
    // test runs under the system allocator and the counters would never move.
    //
    // The counters are process-global, so these tests must not run concurrently
    // with each other: a sibling test's allocation would land between a read of
    // `live_bytes()` and its assertion. Serialize rather than depending on
    // `--test-threads=1`, which CI does not use.
    static COUNTER_LOCK: Mutex<()> = Mutex::new(());

    fn exclusive() -> MutexGuard<'static, ()> {
        // A panicking test poisons the lock; the counters are still coherent,
        // so recover rather than cascading the failure into every sibling.
        COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn alloc_and_dealloc_balance() {
        let _guard = exclusive();
        let alloc = CountingAllocator;
        let layout = Layout::from_size_align(4096, 8).unwrap();
        let live_before = live_bytes();
        let peak_before = peak_live_bytes();

        let ptr = unsafe { alloc.alloc(layout) };
        assert!(!ptr.is_null());
        assert_eq!(live_bytes(), live_before + 4096);
        // `peak_live_bytes` is a global high-water mark over the whole process,
        // so it only advances when this allocation exceeds every previous peak,
        // which depends on what other tests already ran. Assert the invariants
        // that always hold instead: peak covers live, and peak never decreases.
        assert!(
            peak_live_bytes() >= live_bytes(),
            "peak must cover current live bytes"
        );
        assert!(peak_live_bytes() >= peak_before, "peak must never decrease");

        unsafe { alloc.dealloc(ptr, layout) };
        assert_eq!(live_bytes(), live_before, "dealloc must return to baseline");
    }

    #[test]
    fn realloc_accounts_for_growth_and_shrink() {
        let _guard = exclusive();
        let alloc = CountingAllocator;
        let layout = Layout::from_size_align(1024, 8).unwrap();
        let live_before = live_bytes();

        let ptr = unsafe { alloc.alloc(layout) };
        assert_eq!(live_bytes(), live_before + 1024);

        let grown = unsafe { alloc.realloc(ptr, layout, 4096) };
        assert!(!grown.is_null());
        assert_eq!(live_bytes(), live_before + 4096, "growth must be counted");

        let grown_layout = Layout::from_size_align(4096, 8).unwrap();
        let shrunk = unsafe { alloc.realloc(grown, grown_layout, 512) };
        assert!(!shrunk.is_null());
        assert_eq!(
            live_bytes(),
            live_before + 512,
            "shrink must be credited back"
        );

        unsafe { alloc.dealloc(shrunk, Layout::from_size_align(512, 8).unwrap()) };
        assert_eq!(live_bytes(), live_before);
    }

    #[test]
    fn peak_is_monotonic_across_a_free() {
        let _guard = exclusive();
        let alloc = CountingAllocator;
        let layout = Layout::from_size_align(8192, 8).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        let peak_while_held = peak_live_bytes();
        unsafe { alloc.dealloc(ptr, layout) };
        assert_eq!(
            peak_live_bytes(),
            peak_while_held,
            "peak must not decrease when memory is released"
        );
    }

    #[test]
    fn alloc_zeroed_is_counted_and_zeroed() {
        let _guard = exclusive();
        let alloc = CountingAllocator;
        let layout = Layout::from_size_align(256, 8).unwrap();
        let live_before = live_bytes();
        let ptr = unsafe { alloc.alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        assert_eq!(live_bytes(), live_before + 256);
        assert!(
            unsafe { std::slice::from_raw_parts(ptr, 256) }
                .iter()
                .all(|b| *b == 0),
            "alloc_zeroed must still zero the block"
        );
        unsafe { alloc.dealloc(ptr, layout) };
    }
}

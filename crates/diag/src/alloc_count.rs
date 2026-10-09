use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use serde::Serialize;

/// Number of allocations of 16 MB or more, and the largest single one seen.
static HUGE_COUNT: AtomicU64 = AtomicU64::new(0);
static LARGEST: AtomicU64 = AtomicU64::new(0);
/// Set to the size of the latest allocation >= 16 MB so a logger thread can report it.
static LAST_HUGE: AtomicU64 = AtomicU64::new(0);

#[inline]
fn size_class(size: u64) -> usize {
    match size {
        0..65_536 => 0,
        65_536..1_048_576 => 1,
        1_048_576..16_777_216 => 2,
        _ => 3,
    }
}

#[inline]
fn record_size(slot: Slot, size: u64) {
    let c = size_class(size);
    slot.bump(&slot.counters.class_count[c], 1);
    slot.bump(&slot.counters.class_bytes[c], size);
    if c == 3 {
        HUGE_COUNT.fetch_add(1, Ordering::Relaxed);
        LARGEST.fetch_max(size, Ordering::Relaxed);
        LAST_HUGE.store(size, Ordering::Relaxed);
    }
}

/// Allocation counts and total bytes ever requested per size class, plus the number of
/// allocations of 16 MB or more and the largest single one. Totals, not live bytes.
pub fn size_class_report() -> String {
    let used = &SLOTS[..process_allocation_slots_used()];
    let mb = |b: u64| b / (1024 * 1024);
    let n = |i: usize| -> u64 {
        used.iter()
            .map(|s| s.class_count[i].load(Ordering::Relaxed))
            .sum()
    };
    let b = |i: usize| {
        mb(used
            .iter()
            .map(|s| s.class_bytes[i].load(Ordering::Relaxed))
            .sum())
    };
    format!(
        "allocs requested: <64KB {}x {}MB | 64KB-1MB {}x {}MB | 1-16MB {}x {}MB | >=16MB {}x {}MB (largest {}MB)",
        n(0), b(0), n(1), b(1), n(2), b(2), n(3), b(3),
        mb(LARGEST.load(Ordering::Relaxed))
    )
}

/// Size of the newest allocation of 16 MB or more since the last call, if any.
pub fn take_last_huge() -> Option<u64> {
    let v = LAST_HUGE.swap(0, Ordering::Relaxed);
    (v != 0).then_some(v)
}

const FALLBACK_SLOT: usize = 0;
const MAX_THREADS: usize = 256;

/// One whole cache line per thread: Apple chips move 128-byte lines, and two threads
/// whose slots shared one would contend as if the counters were shared.
#[repr(C, align(128))]
struct ThreadCounters {
    alloc_count: AtomicU64,
    alloc_bytes: AtomicU64,
    dealloc_count: AtomicU64,
    dealloc_bytes: AtomicU64,
    /// Allocation size classes, cheap enough to leave on:
    /// <64 KB, 64 KB-1 MB, 1-16 MB, >=16 MB. A big heap made of a few huge buffers and
    /// one made of millions of small objects need different fixes; this tells them apart.
    /// Counted per thread even when counting is off: one process-wide pair that every
    /// allocation on every thread bumped made alloc+free several times slower.
    class_count: [AtomicU64; 4],
    class_bytes: [AtomicU64; 4],
}

impl ThreadCounters {
    const fn new() -> Self {
        Self {
            alloc_count: AtomicU64::new(0),
            alloc_bytes: AtomicU64::new(0),
            dealloc_count: AtomicU64::new(0),
            dealloc_bytes: AtomicU64::new(0),
            class_count: [const { AtomicU64::new(0) }; 4],
            class_bytes: [const { AtomicU64::new(0) }; 4],
        }
    }
}

/// The calling thread's counters. A slot that only this thread writes is bumped with a
/// plain load and store: no other thread can race the update, and readers on other
/// threads only ever load. The fallback slot and the last slot, which threads past the
/// 256th share, take a real atomic add.
#[derive(Clone, Copy)]
struct Slot {
    counters: &'static ThreadCounters,
    exclusive: bool,
}

impl Slot {
    #[inline]
    fn bump(self, counter: &AtomicU64, by: u64) {
        if self.exclusive {
            counter.store(
                counter.load(Ordering::Relaxed).wrapping_add(by),
                Ordering::Relaxed,
            );
        } else {
            counter.fetch_add(by, Ordering::Relaxed);
        }
    }
}

static SLOTS: [ThreadCounters; MAX_THREADS] = [const { ThreadCounters::new() }; MAX_THREADS];

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(1);

static SATURATED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn slot() -> Slot {
    let idx = SLOT
        .try_with(|cell| {
            let mut idx = cell.get();
            if idx == usize::MAX {
                idx = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
                if idx >= MAX_THREADS {
                    SATURATED.fetch_add(1, Ordering::Relaxed);
                    idx = MAX_THREADS - 1;
                    NEXT_SLOT.store(MAX_THREADS, Ordering::Relaxed);
                }
                cell.set(idx);
            }
            idx
        })
        .unwrap_or(FALLBACK_SLOT);
    Slot {
        counters: &SLOTS[idx],
        exclusive: idx != FALLBACK_SLOT && idx != MAX_THREADS - 1,
    }
}

fn record_alloc(size: u64) {
    let s = slot();
    record_size(s, size);
    if !counting_enabled() {
        return;
    }
    s.bump(&s.counters.alloc_count, 1);
    s.bump(&s.counters.alloc_bytes, size);
}

fn record_dealloc(size: u64) {
    if !counting_enabled() {
        return;
    }
    let s = slot();
    s.bump(&s.counters.dealloc_count, 1);
    s.bump(&s.counters.dealloc_bytes, size);
}

/// Whether the counting allocator is actually counting. It is off unless
/// `IW4L_COUNTING_ALLOC` is set, so every figure derived from it is absent
/// rather than zero on an ordinary run — and a caller that cannot tell those
/// apart will report "no allocations" for "nobody was counting".
pub fn counting_enabled() -> bool {
    static STATE: AtomicU8 = AtomicU8::new(2);
    match STATE.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = env_key_present(b"IW4L_COUNTING_ALLOC\0");
            STATE.store(u8::from(on), Ordering::Relaxed);
            on
        }
    }
}

fn env_key_present(key_nul: &[u8]) -> bool {
    debug_assert_eq!(key_nul.last().copied(), Some(0));
    !unsafe { getenv(key_nul.as_ptr().cast()) }.is_null()
}

unsafe extern "C" {
    fn getenv(name: *const core::ffi::c_char) -> *mut core::ffi::c_char;
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn malloc_trim(pad: usize) -> core::ffi::c_int;
}

pub fn release_freed_heap() -> std::time::Duration {
    let at = std::time::Instant::now();
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe { malloc_trim(0) };
    }
    at.elapsed()
}

#[cfg(windows)]
static BACKING: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(not(windows))]
static BACKING: std::alloc::System = std::alloc::System;

pub struct ProcessCountingAllocator;

unsafe impl GlobalAlloc for ProcessCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size() as u64);
        unsafe { BACKING.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size() as u64);
        unsafe { BACKING.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_dealloc(layout.size() as u64);
        unsafe { BACKING.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_alloc(new_size as u64);
        record_dealloc(layout.size() as u64);
        unsafe { BACKING.realloc(ptr, layout, new_size) }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ProcessAllocationStats {
    pub process_allocations: u64,
    pub process_allocation_bytes: u64,
    pub process_deallocations: u64,
    pub process_deallocation_bytes: u64,

    pub process_allocation_saturated: u64,
}

pub fn process_allocations() -> ProcessAllocationStats {
    // Off, every slot's totals are zero; reading them would pull a cache line from each
    // thread that allocates, many times a frame, because the size classes keep them hot.
    if !counting_enabled() {
        return ProcessAllocationStats::default();
    }
    let n = NEXT_SLOT.load(Ordering::Relaxed).clamp(1, MAX_THREADS);
    let mut stats = ProcessAllocationStats::default();
    for slot in &SLOTS[..n] {
        stats.process_allocations += slot.alloc_count.load(Ordering::Relaxed);
        stats.process_allocation_bytes += slot.alloc_bytes.load(Ordering::Relaxed);
        stats.process_deallocations += slot.dealloc_count.load(Ordering::Relaxed);
        stats.process_deallocation_bytes += slot.dealloc_bytes.load(Ordering::Relaxed);
    }
    stats.process_allocation_saturated = SATURATED.load(Ordering::Relaxed);
    stats
}

pub fn process_live_heap_bytes() -> Option<u64> {
    if !counting_enabled() {
        return None;
    }
    let stats = process_allocations();
    Some(
        stats
            .process_allocation_bytes
            .saturating_sub(stats.process_deallocation_bytes),
    )
}

pub fn process_allocation_slots_used() -> usize {
    NEXT_SLOT.load(Ordering::Relaxed).clamp(1, MAX_THREADS)
}

pub fn process_allocation_saturated() -> u64 {
    SATURATED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The test binary keeps the system allocator, so these calls are the only counts.
    #[test]
    fn size_classes_add_up_across_threads() {
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        for size in [16, 70_000, 2 << 20] {
                            record_alloc(size);
                        }
                    }
                });
            }
        });
        let report = size_class_report();
        assert!(
            report.starts_with(
                "allocs requested: <64KB 4000x 0MB | 64KB-1MB 4000x 267MB | 1-16MB 4000x 8000MB | >=16MB 0x 0MB"
            ),
            "{report}"
        );
        if !counting_enabled() {
            assert_eq!(process_allocations(), ProcessAllocationStats::default());
        }
    }
}

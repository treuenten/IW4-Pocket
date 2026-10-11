//! One log line per effect element kind that stops moving partway through its
//! life, to find particles that hang in the air. Capped per process.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_LINES: usize = 128;

static LOGGED: AtomicUsize = AtomicUsize::new(0);
static SEEN: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

pub(crate) fn note(
    cause: &'static str,
    def_name: &str,
    catalog_index: u16,
    def_index: u8,
    detail: std::fmt::Arguments<'_>,
) {
    if LOGGED.load(Ordering::Relaxed) >= MAX_LINES {
        return;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (cause, def_name, catalog_index, def_index).hash(&mut hasher);
    let Ok(mut seen) = SEEN.lock() else {
        return;
    };
    if !seen
        .get_or_insert_with(HashSet::new)
        .insert(hasher.finish())
    {
        return;
    }
    LOGGED.fetch_add(1, Ordering::Relaxed);
    diag::info!(
        World,
        "fx frozen: {cause} def={def_name} catalog={catalog_index} elem={def_index} {detail}"
    );
}

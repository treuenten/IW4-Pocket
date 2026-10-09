//! Thread QoS classes on macOS and iOS (`IW4L_THREAD_QOS=1`, off by default).
//!
//! The system picks a core and a clock for a thread by its QoS class. Bevy's
//! pool threads and the pipelined render thread are plain threads, so they run
//! at Default (0x15) and the scheduler is free to put them on the efficiency
//! cores, while the main thread (User Interactive, 0x21) waits for the server
//! tick and for the render world they run. With the switch on:
//!
//! - compute pool threads (the multi-threaded schedules, the fixed tick among
//!   them) are User Interactive;
//! - async compute pool threads (audio decode, material compiles) are User
//!   Initiated;
//! - the IO pool is left alone, so loading can still use the efficiency cores;
//! - the render thread raises itself the first time it runs the `Render`
//!   schedule. Without pipelined rendering that schedule runs on the main
//!   thread, which is left as it is.
//!
//! One launch line says what each thread ended up with, e.g.
//! `thread qos: compute=0x21 async=0x19 render=0x21 main=0x21 (render was 0x15)`.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bevy::app::{TaskPoolOptions, TaskPoolPlugin};
use bevy::prelude::*;
use libc::qos_class_t::{QOS_CLASS_USER_INITIATED, QOS_CLASS_USER_INTERACTIVE};

/// No class read yet (0 is a real class, unspecified).
const UNREAD: u32 = u32::MAX;

/// The class each pool thread read back after raising itself; all threads of a
/// pool ask for the same one, so the last to start stands for them all.
static COMPUTE: AtomicU32 = AtomicU32::new(UNREAD);
static ASYNC_COMPUTE: AtomicU32 = AtomicU32::new(UNREAD);
static MAIN: AtomicU32 = AtomicU32::new(UNREAD);

/// Moves the calling thread to `class` (`pthread_set_qos_class_self_np`).
pub(crate) fn set_self(class: libc::qos_class_t) -> bool {
    // SAFETY: changes only the calling thread's own class; no pointers.
    unsafe { libc::pthread_set_qos_class_self_np(class, 0) == 0 }
}

/// The calling thread's class (`pthread_get_qos_class_np`).
pub(crate) fn current() -> Option<u32> {
    diag::health::thread_qos()
}

/// Bevy's task pools with the compute and async compute threads raised as they
/// start. Takes the place of `DefaultPlugins`' own `TaskPoolPlugin`.
pub(crate) fn task_pool_plugin() -> TaskPoolPlugin {
    let mut options = TaskPoolOptions::default();
    options.compute.on_thread_spawn = Some(Arc::new(|| {
        raise_pool_thread(QOS_CLASS_USER_INTERACTIVE, &COMPUTE);
    }));
    options.async_compute.on_thread_spawn = Some(Arc::new(|| {
        raise_pool_thread(QOS_CLASS_USER_INITIATED, &ASYNC_COMPUTE);
    }));
    TaskPoolPlugin {
        task_pool_options: options,
    }
}

fn raise_pool_thread(class: libc::qos_class_t, seen: &AtomicU32) {
    set_self(class);
    seen.store(current().unwrap_or(UNREAD), Ordering::Relaxed);
}

/// The render thread's half, added after `RenderPlugin` builds the render app.
pub(crate) struct ThreadQosPlugin;

impl Plugin for ThreadQosPlugin {
    fn build(&self, app: &mut App) {
        // Plugins build on the main thread.
        MAIN.store(current().unwrap_or(UNREAD), Ordering::Relaxed);
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.add_systems(bevy::render::Render, raise_render_thread);
        }
    }
}

/// Once, on whichever thread runs `Render`: raise it unless it is the main
/// thread, then log every class.
fn raise_render_thread(mut done: Local<bool>) {
    if *done {
        return;
    }
    *done = true;
    let was = current();
    if diag::health::on_main_thread() != Some(true) {
        set_self(QOS_CLASS_USER_INTERACTIVE);
    }
    diag::info!(
        Launch,
        "thread qos: compute={} async={} render={} main={} (render was {})",
        hex(COMPUTE.load(Ordering::Relaxed)),
        hex(ASYNC_COMPUTE.load(Ordering::Relaxed)),
        hex(current().unwrap_or(UNREAD)),
        hex(MAIN.load(Ordering::Relaxed)),
        hex(was.unwrap_or(UNREAD)),
    );
}

/// `0x21` user interactive, `0x19` user initiated, `0x15` default, `0x11`
/// utility, `0x09` background, `0x00` unspecified.
fn hex(class: u32) -> String {
    if class == UNREAD {
        "n/a".to_owned()
    } else {
        format!("{class:#04x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_reads_back_the_class_it_set() {
        std::thread::spawn(|| {
            assert!(set_self(QOS_CLASS_USER_INITIATED));
            assert_eq!(current(), Some(0x19));
            assert!(set_self(QOS_CLASS_USER_INTERACTIVE));
            assert_eq!(current(), Some(0x21));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn pool_threads_record_their_class() {
        let seen = AtomicU32::new(UNREAD);
        std::thread::scope(|scope| {
            scope.spawn(|| raise_pool_thread(QOS_CLASS_USER_INTERACTIVE, &seen));
        });
        assert_eq!(seen.load(Ordering::Relaxed), 0x21);
    }

    #[test]
    fn classes_print_as_two_hex_digits() {
        assert_eq!(hex(0x21), "0x21");
        assert_eq!(hex(0x09), "0x09");
        assert_eq!(hex(0), "0x00");
        assert_eq!(hex(UNREAD), "n/a");
    }
}

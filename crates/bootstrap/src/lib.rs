pub mod args;
pub mod bench;
mod frame_owner;
mod frame_pace;
mod idle_timer;
#[cfg(target_os = "ios")]
mod ios_input;
mod launch;
mod plugins;
mod promotion;
#[cfg(any(target_os = "ios", test))]
mod touch_controls;

pub use args::{AcceptanceLaunch, LaunchMode, parse_cli};
pub use launch::launch;
pub use plugins::assemble_listen_app;

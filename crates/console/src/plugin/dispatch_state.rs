use bevy::prelude::*;

use crate::ConsoleCommand;

#[derive(Resource, Default)]
pub struct ConsoleCommandQueue(pub std::collections::VecDeque<ConsoleCommand>);

impl ConsoleCommandQueue {
    pub fn push_script(&mut self, script: &str) {
        self.0.extend(ConsoleCommand::parse_script(script));
    }
}

#[derive(Resource, Default)]
pub struct ConsoleDispatch {
    pub paused: bool,

    pub wait_remaining: f32,

    pub wait_world: bool,
    pub wait_world_elapsed: f32,

    pub wait_spawn: bool,
    pub wait_spawn_elapsed: f32,

    pub wait_spawn_admit: bool,

    pub pending_spawn_class: Option<String>,

    pub wait_torn: bool,
    pub wait_torn_elapsed: f32,

    pub wait_ambient: bool,
    pub wait_ambient_elapsed: f32,

    pub wait_move: Option<WaitMovePose>,
    pub wait_move_elapsed: f32,

    pub wait_playing: bool,
    pub wait_playing_elapsed: f32,

    pub wait_tick: Option<u32>,
    pub wait_tick_elapsed: f32,

    pub wait_alive: Option<(sim::ClientId, sim::LifeSequence)>,
    pub wait_alive_elapsed: f32,

    pub wait_local_alive: bool,
    pub wait_local_alive_since: Option<u32>,
    pub wait_local_alive_elapsed: f32,

    pub quit_jumps: u64,

    pub fifo_jumps: u64,
}

impl ConsoleDispatch {
    pub fn release(&mut self) {
        self.paused = false;
        self.wait_remaining = 0.0;
        self.wait_world = false;
        self.wait_spawn = false;
        self.wait_spawn_admit = false;
        self.pending_spawn_class = None;
        self.wait_torn = false;
        self.wait_ambient = false;
        self.wait_move = None;
        self.wait_playing = false;
        self.wait_tick = None;
        self.wait_alive = None;
        self.wait_local_alive = false;
        self.wait_local_alive_since = None;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WaitMovePose {
    pub client: sim::ClientId,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
}

pub(super) const WAIT_WORLD_TIMEOUT_SECS: f32 = 120.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WaitKind {
    Seconds(f32),
    Ticks(u32),
    World,
    Spawn,
    Alive,
    Torn,
    Ambient,
    Invalid(String),
}

pub(crate) const WAIT_ARGS_HINT: &str = "seconds | <n>t | world | spawn | alive | torn | ambient";

/// The authority tick `wait alive` measures against: the clock only where
/// this process runs the authority (a listen host). NetPlugin keeps an
/// `AuthorityClock` for every role, but on a joined client it is frozen
/// (normally at 0) and says nothing about the snapshots shown, so a client
/// gets none and trusts the presented snapshot alone.
pub(crate) fn wait_alive_clock(runs_authority: bool, clock_tick: Option<u32>) -> Option<u32> {
    clock_tick.filter(|_| runs_authority)
}

/// Whether the presented snapshot is new enough for `wait alive` to trust.
///
/// `since` and `clock_tick` come from [`wait_alive_clock`]: the host's
/// authority tick when the wait began and now, none on a joined client. On a
/// listen host a `kill` or `damage` sent just before the wait is applied on
/// the next authority tick, so the snapshot presented at the wait still shows
/// the life that is about to end as Alive. Requiring a presented tick after
/// `since` keeps `kill; wait alive` from passing on that life. A clock that
/// went back behind `since` belongs to a new world, where the old tick means
/// nothing.
pub(crate) fn wait_alive_settled(
    since: Option<u32>,
    presented_tick: Option<u32>,
    clock_tick: Option<u32>,
) -> bool {
    let Some(since) = since else {
        return true;
    };
    if clock_tick.is_some_and(|now| now.wrapping_sub(since) >= u32::MAX / 2) {
        return true;
    }
    presented_tick.is_some_and(|tick| {
        let ahead = tick.wrapping_sub(since);
        ahead > 0 && ahead < u32::MAX / 2
    })
}

/// Reads the word after `wait`. A bare `wait` is one second. Anything that is
/// neither a known hold nor a number of seconds or ticks is `Invalid`, so a
/// misspelt hold (`wait alvie`) is reported instead of quietly becoming a
/// one-second pause that lets the script run on.
pub(crate) fn parse_wait_args(args: &[String]) -> WaitKind {
    let Some(word) = args.first().map(String::as_str) else {
        return WaitKind::Seconds(1.0);
    };
    if args.len() > 1 {
        return WaitKind::Invalid(args.join(" "));
    }
    if word.eq_ignore_ascii_case("world") {
        return WaitKind::World;
    }
    if word.eq_ignore_ascii_case("spawn") {
        return WaitKind::Spawn;
    }
    if word.eq_ignore_ascii_case("alive") {
        return WaitKind::Alive;
    }
    if word.eq_ignore_ascii_case("torn") {
        return WaitKind::Torn;
    }
    if word.eq_ignore_ascii_case("ambient") {
        return WaitKind::Ambient;
    }
    if let Some(ticks) = word.strip_suffix(['t', 'T'])
        && let Ok(n) = ticks.parse::<u32>()
    {
        return WaitKind::Ticks(n);
    }
    let number = word.strip_suffix(['s', 'S']).unwrap_or(word);
    match number.parse::<f32>() {
        Ok(secs) if secs.is_finite() => WaitKind::Seconds(secs.clamp(0.0, WAIT_WORLD_TIMEOUT_SECS)),
        _ => WaitKind::Invalid(word.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> WaitKind {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        parse_wait_args(&args)
    }

    #[test]
    fn bare_wait_is_one_second() {
        assert_eq!(parse(&[]), WaitKind::Seconds(1.0));
    }

    #[test]
    fn alive_is_its_own_hold_in_any_case() {
        assert_eq!(parse(&["alive"]), WaitKind::Alive);
        assert_eq!(parse(&["Alive"]), WaitKind::Alive);
        assert_eq!(parse(&["ALIVE"]), WaitKind::Alive);
    }

    #[test]
    fn named_holds_are_unchanged() {
        assert_eq!(parse(&["world"]), WaitKind::World);
        assert_eq!(parse(&["Spawn"]), WaitKind::Spawn);
        assert_eq!(parse(&["torn"]), WaitKind::Torn);
        assert_eq!(parse(&["AMBIENT"]), WaitKind::Ambient);
    }

    #[test]
    fn numbers_are_seconds_or_ticks() {
        assert_eq!(parse(&["10"]), WaitKind::Seconds(10.0));
        assert_eq!(parse(&["2s"]), WaitKind::Seconds(2.0));
        assert_eq!(parse(&["2S"]), WaitKind::Seconds(2.0));
        assert_eq!(parse(&["0.05"]), WaitKind::Seconds(0.05));
        assert_eq!(parse(&["1.5s"]), WaitKind::Seconds(1.5));
        assert_eq!(parse(&["0"]), WaitKind::Seconds(0.0));
        assert_eq!(parse(&["5t"]), WaitKind::Ticks(5));
        assert_eq!(parse(&["40T"]), WaitKind::Ticks(40));
    }

    #[test]
    fn seconds_are_clamped_to_the_gate_timeout() {
        assert_eq!(parse(&["-3"]), WaitKind::Seconds(0.0));
        assert_eq!(parse(&["600s"]), WaitKind::Seconds(WAIT_WORLD_TIMEOUT_SECS));
    }

    #[test]
    fn unknown_words_are_invalid_not_one_second() {
        assert_eq!(parse(&["banana"]), WaitKind::Invalid("banana".into()));
        assert_eq!(parse(&["alvie"]), WaitKind::Invalid("alvie".into()));
        assert_eq!(parse(&["s"]), WaitKind::Invalid("s".into()));
        assert_eq!(parse(&["t"]), WaitKind::Invalid("t".into()));
        assert_eq!(parse(&["2ss"]), WaitKind::Invalid("2ss".into()));
        assert_eq!(parse(&["1.5t"]), WaitKind::Invalid("1.5t".into()));
        assert_eq!(parse(&["nan"]), WaitKind::Invalid("nan".into()));
        assert_eq!(parse(&["inf"]), WaitKind::Invalid("inf".into()));
    }

    #[test]
    fn extra_words_are_invalid() {
        assert_eq!(parse(&["2", "s"]), WaitKind::Invalid("2 s".into()));
        assert_eq!(
            parse(&["alive", "now"]),
            WaitKind::Invalid("alive now".into())
        );
    }

    #[test]
    fn a_joined_client_trusts_the_presented_snapshot() {
        assert!(wait_alive_settled(None, Some(7), None));
        assert!(wait_alive_settled(None, None, None));
    }

    #[test]
    fn a_joined_client_ignores_its_frozen_authority_clock() {
        // A client's AuthorityClock exists but never runs: usually 0, and
        // nothing stops it holding an older world's tick.
        for frozen in [0, 5_000] {
            let since = wait_alive_clock(false, Some(frozen));
            assert_eq!(since, None);
            assert!(wait_alive_settled(
                since,
                Some(300),
                wait_alive_clock(false, Some(frozen))
            ));
        }
        // Read unfiltered, a frozen non-zero clock would hold until the
        // 120 s timeout.
        assert!(!wait_alive_settled(Some(5_000), Some(300), Some(5_000)));
    }

    #[test]
    fn a_host_at_tick_zero_waits_for_tick_one() {
        let since = wait_alive_clock(true, Some(0));
        assert_eq!(since, Some(0));
        assert!(!wait_alive_settled(since, Some(0), Some(0)));
        assert!(!wait_alive_settled(since, None, Some(0)));
        assert!(wait_alive_settled(since, Some(1), Some(1)));
    }

    #[test]
    fn a_host_waits_for_a_tick_after_the_wait_began() {
        assert!(!wait_alive_settled(Some(48), Some(47), Some(48)));
        assert!(!wait_alive_settled(Some(48), Some(48), Some(48)));
        assert!(!wait_alive_settled(Some(48), None, Some(49)));
        assert!(wait_alive_settled(Some(48), Some(49), Some(49)));
        assert!(wait_alive_settled(Some(48), Some(60), Some(61)));
    }

    #[test]
    fn the_tick_barrier_survives_wrap_and_a_new_world() {
        assert!(!wait_alive_settled(
            Some(u32::MAX),
            Some(u32::MAX),
            Some(u32::MAX)
        ));
        assert!(wait_alive_settled(Some(u32::MAX), Some(0), Some(1)));
        // A new match restarts the authority clock below the old tick.
        assert!(wait_alive_settled(Some(5_000), Some(3), Some(4)));
    }
}

//! ProMotion on iOS. The game presents through a CAMetalLayer with nothing that
//! asks the display for more than 60 Hz, so on a 120 Hz iPhone every frame still
//! lands on the 16.7 ms grid: at the usual 35-45 fps in a match the frames show
//! for one or two refreshes in turn (16.7 / 33.3 ms), which is visible judder.
//! A display link whose preferred range is 120 Hz is the system's way to ask for
//! the faster refresh; it calls nothing useful, it only holds the request. It runs
//! during a match (class select and in game) and is paused elsewhere, so the
//! menus stay at 60 Hz and do not draw twice as often for nothing.
//!
//! The first minute of a match stays at 60 Hz. Phone logs showed 85-200 ms
//! graphics stalls in the first 15-55 s after spawning, only at 120 Hz and only
//! until iOS settled the graphics chip's clock; a whole match at 60 Hz had none
//! but ran at about 40 fps instead of 52-55. So the request starts once the
//! player has been in the match for `IW4L_PROMOTION_DELAY` seconds (default 60,
//! 0 asks at once).
//!
//! `IW4L_PROMOTION=0` in `iw4l-env.txt` leaves the display alone. Every other
//! platform registers nothing.
use bevy::prelude::*;

pub(crate) fn register(app: &mut App) {
    #[cfg(target_os = "ios")]
    ios::register(app);
    #[cfg(not(target_os = "ios"))]
    let _ = app;
}

#[cfg(target_os = "ios")]
mod ios {
    use bevy::prelude::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{ClassType, Encode, Encoding, class, define_class, msg_send, sel};

    const PROMOTION_ENV: &str = "IW4L_PROMOTION";
    const DELAY_ENV: &str = "IW4L_PROMOTION_DELAY";
    const DEFAULT_DELAY_SECS: f64 = 60.0;

    /// QuartzCore's `CAFrameRateRange` (iOS 15).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CAFrameRateRange {
        minimum: f32,
        maximum: f32,
        preferred: f32,
    }

    // SAFETY: matches the C struct `{CAFrameRateRange=fff}`.
    unsafe impl Encode for CAFrameRateRange {
        const ENCODING: Encoding = Encoding::Struct(
            "CAFrameRateRange",
            &[f32::ENCODING, f32::ENCODING, f32::ENCODING],
        );
    }

    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {
        static NSRunLoopCommonModes: &'static AnyObject;
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and this type has no
        // Drop impl.
        #[unsafe(super(NSObject))]
        #[name = "IW4LDisplayRateHint"]
        struct DisplayRateHint;

        impl DisplayRateHint {
            #[unsafe(method(tick:))]
            fn tick(&self, _link: *mut AnyObject) {}
        }
    );

    /// The display link, touched on the main thread only.
    struct RateLink(Retained<AnyObject>);

    /// Seconds in a match before the display is asked for 120 Hz.
    #[derive(Resource)]
    struct FastAfter(f64);

    pub(super) fn register(app: &mut App) {
        if std::env::var(PROMOTION_ENV).is_ok_and(|value| value.trim() == "0") {
            diag::info!(
                Launch,
                "promotion: off ({PROMOTION_ENV}=0), display left at 60 Hz"
            );
            return;
        }
        let Some(link) = create_link() else {
            diag::warn!(
                Launch,
                "promotion: CADisplayLink unavailable, display left at 60 Hz"
            );
            return;
        };
        let delay = std::env::var(DELAY_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map_or(DEFAULT_DELAY_SECS, f64::from);
        app.insert_non_send(RateLink(link))
            .insert_resource(FastAfter(delay))
            .add_systems(Last, follow_screen);
        diag::info!(
            Launch,
            "promotion: 120 Hz display link ready (matches only, after {delay:.0} s in game)"
        );
    }

    fn create_link() -> Option<Retained<AnyObject>> {
        // SAFETY: plain Foundation/QuartzCore messages with their documented
        // signatures, on the main thread during app setup. The run loop retains
        // the link and the link retains its target.
        unsafe {
            let target: Retained<DisplayRateHint> = msg_send![DisplayRateHint::class(), new];
            let link: Option<Retained<AnyObject>> = msg_send![
                class!(CADisplayLink),
                displayLinkWithTarget: &*target,
                selector: sel!(tick:)
            ];
            let link = link?;
            let range = CAFrameRateRange {
                minimum: 80.0,
                maximum: 120.0,
                preferred: 120.0,
            };
            let _: () = msg_send![&*link, setPreferredFrameRateRange: range];
            let _: () = msg_send![&*link, setPaused: true];
            let run_loop: Retained<AnyObject> = msg_send![class!(NSRunLoop), mainRunLoop];
            let _: () = msg_send![
                &*link,
                addToRunLoop: &*run_loop,
                forMode: NSRunLoopCommonModes
            ];
            Some(link)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        OutOfMatch,
        MatchStart,
        Fast,
    }

    fn follow_screen(
        link: NonSend<RateLink>,
        fast_after: Res<FastAfter>,
        time: Res<Time<Real>>,
        screen: Option<Res<frame::AppScreen>>,
        mut in_game_since: Local<Option<f64>>,
        mut live: Local<Option<Phase>>,
    ) {
        let screen = screen.as_deref();
        let in_match = matches!(
            screen,
            Some(frame::AppScreen::ClassSelect | frame::AppScreen::InGame)
        );
        let now = time.elapsed_secs_f64();
        if !in_match {
            *in_game_since = None;
        } else if in_game_since.is_none() && screen == Some(&frame::AppScreen::InGame) {
            *in_game_since = Some(now);
        }
        let phase = if !in_match {
            Phase::OutOfMatch
        } else if in_game_since.is_some_and(|since| now - since >= fast_after.0) {
            Phase::Fast
        } else {
            Phase::MatchStart
        };
        if *live == Some(phase) {
            return;
        }
        let first = live.replace(phase).is_none();
        // SAFETY: NonSend keeps this on the main thread; `paused` is a BOOL property.
        unsafe {
            let _: () = msg_send![&*link.0, setPaused: phase != Phase::Fast];
        }
        match phase {
            Phase::Fast => diag::info!(
                Launch,
                "promotion: {:.0} s into the match, asking the display for 120 Hz",
                fast_after.0
            ),
            Phase::MatchStart => diag::info!(
                Launch,
                "promotion: match start, display at 60 Hz for the first {:.0} s",
                fast_after.0
            ),
            Phase::OutOfMatch if !first => {
                diag::info!(Launch, "promotion: out of match, display back to 60 Hz")
            }
            Phase::OutOfMatch => {}
        }
    }
}

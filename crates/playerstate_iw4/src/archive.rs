use crate::{PlayerState, other_flags};

impl PlayerState {
    pub const fn is_live_frame(&self) -> bool {
        self.delta_time == 0
    }

    /// A live view of another player: a spectator following a teammate. The
    /// seat is that player's own state with the follow bit set (MW2 copies the
    /// followed client's playerstate and marks it `POF_FOLLOW`), so it is
    /// drawn from their eyes, with their gun, and never predicted.
    pub const fn is_live_follow(&self) -> bool {
        self.is_live_frame() && self.other_flags & other_flags::DEAD_KILLCAM_TPV != 0
    }

    /// The view is someone else's: an archived killcam frame or a live follow.
    /// Either one is presented from the snapshots, not from local prediction.
    pub const fn is_seat_frame(&self) -> bool {
        !self.is_live_frame() || self.is_live_follow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follow_seat_is_live_but_still_a_seat() {
        let own = PlayerState {
            other_flags: other_flags::PLAYER,
            ..PlayerState::ZERO
        };
        assert!(own.is_live_frame() && !own.is_live_follow() && !own.is_seat_frame());

        let follow = PlayerState {
            other_flags: other_flags::DEAD_KILLCAM_TPV,
            ..PlayerState::ZERO
        };
        assert!(follow.is_live_follow() && follow.is_seat_frame());

        let archived = PlayerState {
            delta_time: 4_000,
            ..follow
        };
        assert!(!archived.is_live_follow() && archived.is_seat_frame());
    }
}

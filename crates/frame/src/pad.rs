use bevy::prelude::{Entity, Message, Resource};

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivePad(pub Option<Entity>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptStyle {
    Xbox,
    PlayStation,
    #[default]
    Generic,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputDevices {
    pub pad_prompts: bool,
    pub style: PromptStyle,
    pub focused: bool,
    pub aiming_with_pad: bool,
    /// The iOS on-screen controls are up. Their swipe-look arrives as mouse
    /// motion, which must not switch the prompts away from the pad's labels.
    pub touch_controls: bool,
    /// The gamepad the on-screen controls drive. Its stick is always the move
    /// stick, whatever stick layout a controller was set up with.
    pub touch_pad: Option<Entity>,
}

#[derive(Message)]
pub struct TestControllerRumble;

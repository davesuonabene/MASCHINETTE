// crates/driver/src/modes/mod.rs
pub mod custom_midi;
pub mod play_mode;

pub use custom_midi::CustomMidiMode;
pub use play_mode::PlayMode;

use crate::context::DriverContext;
use crate::input::HardwareEvent;

pub trait MachineMode {
    /// Called when the user switches to this mode
    fn on_enter(&mut self, ctx: &mut DriverContext);

    /// Called for every hardware event (button, pad, etc)
    fn handle_event(&mut self, event: &HardwareEvent, ctx: &mut DriverContext);
}
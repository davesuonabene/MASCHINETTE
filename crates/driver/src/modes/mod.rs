use crate::context::DriverContext;
use crate::input::HardwareEvent;

pub trait MachineMode {
    /// Called when the driver switches to this mode.
    fn on_enter(&mut self, context: &mut DriverContext);

    /// Called whenever a hardware event occurs.
    fn handle_event(&mut self, event: &HardwareEvent, context: &mut DriverContext);
}

pub mod custom_midi;
pub mod play_mode;
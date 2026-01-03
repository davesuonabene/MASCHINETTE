// crates/driver/src/modes/play_mode.rs
use maschine_library::lights::Brightness;
use crate::context::DriverContext;
use crate::input::HardwareEvent;
use super::MachineMode;

pub struct PlayMode {}

impl PlayMode {
    pub fn new() -> Self {
        Self {}
    }
}

impl MachineMode for PlayMode {
    fn on_enter(&mut self, _ctx: &mut DriverContext) {
        // Clear lights or set specific defaults for Play Mode
    }

    fn handle_event(&mut self, event: &HardwareEvent, ctx: &mut DriverContext) {
        if let HardwareEvent::Button { index, pressed } = event {
            if *pressed {
                // Visual feedback: Light up on press
                if ctx.lights.button_has_light(*index) {
                    ctx.lights.set_button(*index, Brightness::Bright);
                }
            } else {
                // Turn off on release
                if ctx.lights.button_has_light(*index) {
                    ctx.lights.set_button(*index, Brightness::Off);
                }
            }
        }
    }
}
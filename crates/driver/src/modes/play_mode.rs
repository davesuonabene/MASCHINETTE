use maschine_library::lights::{Brightness, PadColors};
use crate::context::DriverContext;
use crate::input::HardwareEvent;
use super::MachineMode;

pub struct PlayMode;

impl PlayMode {
    pub fn new() -> Self {
        Self
    }
}

impl MachineMode for PlayMode {
    fn on_enter(&mut self, _ctx: &mut DriverContext) {
        // Setup initial state for Play Mode here if needed
    }

    fn handle_event(&mut self, event: &HardwareEvent, ctx: &mut DriverContext) {
        match event {
            HardwareEvent::Button { index, pressed } => {
                if *pressed {
                    println!("Play Mode: Button {:?} pressed", index);
                    // Simple feedback
                    if ctx.lights.button_has_light(*index) {
                        ctx.lights.set_button(*index, Brightness::Bright);
                    }
                } else {
                    if ctx.lights.button_has_light(*index) {
                        ctx.lights.set_button(*index, Brightness::Off);
                    }
                }
            }
            HardwareEvent::Pad { index, event_type: _, value } => {
                // Simple Pad Feedback (Blue)
                let is_pressed = *value > 0;
                let b = if is_pressed { Brightness::Normal } else { Brightness::Off };
                ctx.lights.set_pad(*index, PadColors::Blue, b);
            }
            _ => {}
        }
    }
}
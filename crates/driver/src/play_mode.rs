use maschine_library::controls::Buttons;
use maschine_library::lights::{Brightness, Lights};

pub struct PlayMode {
}

impl PlayMode {
    pub fn new() -> Self {
        Self {}
    }

    pub fn refresh(&self, lights: &mut Lights) {
        // Clear all lights (except reserved ones) when entering Play Mode.
        // Since we don't have a "clear all" method exposed, we can iterate if we had a list.
        // For now, PlayMode assumes a blank canvas. 
        // We'll rely on the user or future logic to clear specific buttons if needed.
        // Or we could implement a loop over 0..41 if the 'Buttons' enum supports iteration easily.
        // A manual clear of common buttons:
        // lights.set_button(Buttons::Play, Brightness::Off);
        // lights.set_button(Buttons::Rec, Brightness::Off);
        // ... (can be expanded)
    }

    pub fn handle_button(
        &mut self,
        button: Buttons,
        is_pressed: bool,
        lights: &mut Lights,
    ) -> bool {
        let mut changed_lights = false;

        if is_pressed {
            println!("Playability Mode: Button {:?} pressed", button);
            
            // Visual feedback
            if lights.button_has_light(button) {
                lights.set_button(button, Brightness::Bright);
                changed_lights = true;
            }
        } else {
            // Turn off on release
            if lights.button_has_light(button) {
                lights.set_button(button, Brightness::Off);
                changed_lights = true;
            }
        }

        changed_lights
    }
}
use std::collections::HashMap;
use midly::{live::LiveEvent, MidiMessage};
use rosc::{OscMessage, OscPacket, OscType};
use maschine_library::controls::{Buttons, PadEventType};
use maschine_library::lights::{Brightness, PadColors};
use crate::settings::{ButtonMode, Settings};
use crate::context::DriverContext;
use crate::input::HardwareEvent;
use super::MachineMode;

// Helper to look up buttons by name for exclusive groups
fn button_from_name(name: &str) -> Option<Buttons> {
    for i in 0..41 {
        if let Some(button) = num::FromPrimitive::from_usize(i) {
            if format!("{:?}", button).to_string().eq_ignore_ascii_case(name) {
                return Some(button);
            }
        }
    }
    None
}

pub struct CustomMidiMode {
    toggle_states: HashMap<Buttons, bool>,
    exclusive_groups: HashMap<u8, Vec<String>>,
    last_encoder_val: u8,
    encoder_is_pressed: bool,
}

impl CustomMidiMode {
    pub fn new(settings: &Settings) -> Self {
        let mut exclusive_groups: HashMap<u8, Vec<String>> = HashMap::new();
        for (button_name, config) in settings.button_configs.iter() {
            if config.mode == ButtonMode::Toggle {
                if let Some(group_id) = config.group_id {
                    exclusive_groups
                        .entry(group_id)
                        .or_default()
                        .push(button_name.clone());
                }
            }
        }

        Self {
            toggle_states: HashMap::new(),
            exclusive_groups,
            last_encoder_val: 0,
            encoder_is_pressed: false,
        }
    }

    fn process_button(&mut self, button: Buttons, is_pressed: bool, ctx: &mut DriverContext) -> bool {
        let mut changed_lights = false;

        if button == Buttons::EncoderPress {
            if is_pressed != self.encoder_is_pressed {
                self.encoder_is_pressed = is_pressed;
                self.send_osc("/maschine/encoderPress", if is_pressed { 1 } else { 0 }, ctx);
            }
            return false;
        }

        let button_name = format!("{:?}", button).to_string();
        let config = ctx.settings.button_configs.get(&button_name);
        let mode = config.map(|c| c.mode).unwrap_or_default();
        let current_light_state = ctx.lights.get_button(button) != Brightness::Off;

        let mut should_send_osc = false;
        let mut osc_value: i32 = 0;
        let mut target_light_brightness: Option<Brightness> = None;

        match mode {
            ButtonMode::Trigger => {
                if is_pressed != current_light_state {
                    should_send_osc = true;
                    osc_value = if is_pressed { 1 } else { 0 };
                    target_light_brightness = Some(if is_pressed { Brightness::Normal } else { Brightness::Off });
                }
            }
            ButtonMode::Toggle => {
                if is_pressed && ctx.lights.get_button(button) != Brightness::Bright {
                    let new_toggle_state = !*self.toggle_states.entry(button).or_default();

                    if new_toggle_state {
                        if let Some(group_id) = config.and_then(|c| c.group_id) {
                            if let Some(member_names) = self.exclusive_groups.get(&group_id) {
                                for other_name in member_names {
                                    if other_name != &button_name {
                                        if let Some(other_button) = button_from_name(other_name) {
                                            self.toggle_states.insert(other_button, false);
                                            ctx.lights.set_button(other_button, Brightness::Off);
                                            changed_lights = true;
                                            self.send_osc(&format!("/maschine/{}", other_name.to_lowercase()), 0, ctx);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    self.toggle_states.insert(button, new_toggle_state);
                    should_send_osc = true;
                    osc_value = if new_toggle_state { 1 } else { 0 };
                    target_light_brightness = Some(Brightness::Bright);
                }

                if !is_pressed && current_light_state {
                    target_light_brightness = Some(if *self.toggle_states.get(&button).unwrap_or(&false) { Brightness::Bright } else { Brightness::Off });
                }
            }
        }

        if should_send_osc {
            self.send_osc(&format!("/maschine/{}", button_name.to_lowercase()), osc_value, ctx);
        }

        if let Some(cc_num) = config.and_then(|c| c.cc) {
            if should_send_osc {
                let cc_val = if osc_value == 1 { 127 } else { 0 };
                self.send_midi_cc(cc_num, cc_val, ctx);
            }
        }

        if let Some(b) = target_light_brightness {
            if ctx.lights.button_has_light(button) {
                ctx.lights.set_button(button, b);
                changed_lights = true;
            }
        }

        changed_lights
    }

    fn process_pad(&self, index: usize, event_type: PadEventType, value: u16, ctx: &mut DriverContext) -> bool {
        let mut changed_lights = false;
        
        let (_, prev_b) = ctx.lights.get_pad(index);
        let b = match event_type {
            PadEventType::NoteOn | PadEventType::PressOn | PadEventType::Aftertouch if value > 0 => Brightness::Normal,
            _ => Brightness::Off,
        };
        if prev_b != b {
            ctx.lights.set_pad(index, PadColors::Blue, b);
            changed_lights = true;
        }

        let note = ctx.settings.notemaps[index];
        let mut velocity = (value >> 5) as u8;
        if value > 0 && velocity == 0 { velocity = 1; }

        let event = match event_type {
            PadEventType::NoteOn | PadEventType::PressOn => Some(MidiMessage::NoteOn { key: note.into(), vel: velocity.into() }),
            PadEventType::NoteOff | PadEventType::PressOff => Some(MidiMessage::NoteOff { key: note.into(), vel: velocity.into() }),
            _ => None,
        };

        if let Some(evt) = event {
            let l_ev = LiveEvent::Midi { channel: 0.into(), message: evt };
            let mut midibuf = Vec::new();
            if l_ev.write(&mut midibuf).is_ok() {
                let _ = ctx.midi_port.send(&midibuf[..]);
            }
        }
        
        changed_lights
    }

    fn process_encoder(&mut self, val: u8, ctx: &DriverContext) {
        if val != 0 && val != self.last_encoder_val {
            let diff = val as i8 - self.last_encoder_val as i8;
            let direction = if (diff > 0 && diff < 8) || (diff < -8) { 1 } else { -1 };
            self.send_osc("/maschine/encoder", direction, ctx);
        }
        if val != 0 {
            self.last_encoder_val = val;
        }
    }

    fn process_slider(&self, val: u8, ctx: &mut DriverContext) -> bool {
        if val != 0 {
            self.send_osc("/maschine/slider", val as i32, ctx);
            
            let cnt = (val as i32 - 1 + 5) * 25 / 200 - 1;
            for i in 0..25 {
                let b = match cnt - i {
                    0 => Brightness::Normal,
                    1..=25 => Brightness::Dim,
                    _ => Brightness::Off,
                };
                ctx.lights.set_slider(i as usize, b);
            }
            return true;
        }
        false
    }

    fn send_osc(&self, addr: &str, val: i32, ctx: &DriverContext) {
        let msg = OscMessage {
            addr: addr.to_string(),
            args: vec![OscType::Int(val)],
        };
        if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
            let _ = ctx.osc_socket.send_to(&encoded_buf, ctx.osc_addr);
        }
    }

    fn send_midi_cc(&self, cc: u8, val: u8, ctx: &mut DriverContext) {
        let cc_message = MidiMessage::Controller { controller: cc.into(), value: val.into() };
        let live_event = LiveEvent::Midi { channel: 0.into(), message: cc_message };
        let mut midibuf = Vec::new();
        if live_event.write(&mut midibuf).is_ok() {
            let _ = ctx.midi_port.send(&midibuf[..]);
        }
    }
}

impl MachineMode for CustomMidiMode {
    fn on_enter(&mut self, ctx: &mut DriverContext) {
        for (button, is_active) in &self.toggle_states {
            if *is_active {
                ctx.lights.set_button(*button, Brightness::Bright);
            } else {
                ctx.lights.set_button(*button, Brightness::Off);
            }
        }
    }

    fn handle_event(&mut self, event: &HardwareEvent, ctx: &mut DriverContext) {
        match event {
            HardwareEvent::Button { index, pressed } => {
                self.process_button(*index, *pressed, ctx);
            }
            HardwareEvent::Pad { index, event_type, value } => {
                self.process_pad(*index, *event_type, *value, ctx);
            }
            HardwareEvent::Encoder { value } => {
                self.process_encoder(*value, ctx);
            }
            HardwareEvent::Slider { value } => {
                self.process_slider(*value, ctx);
            }
        }
    }
}
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use midir::MidiOutputConnection;
use midly::{live::LiveEvent, MidiMessage};
use rosc::{OscMessage, OscPacket, OscType};
use maschine_library::controls::Buttons;
use maschine_library::lights::{Brightness, Lights};
use crate::settings::{ButtonMode, Settings};

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
        // Pre-calculate exclusive groups
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

    /// Restore lights for toggle buttons when re-entering this mode
    pub fn refresh(&self, lights: &mut Lights) {
        // Iterate over stored toggle states and restore lights
        for (button, is_active) in &self.toggle_states {
            if *is_active {
                lights.set_button(*button, Brightness::Bright);
            } else {
                lights.set_button(*button, Brightness::Off);
            }
        }
    }

    pub fn handle_button(
        &mut self,
        button: Buttons,
        is_pressed: bool,
        settings: &Settings,
        lights: &mut Lights,
        osc_socket: &UdpSocket,
        osc_addr: &SocketAddr,
        port: &mut MidiOutputConnection,
    ) -> bool {
        let mut changed_lights = false;

        // Special handling for Encoder Press
        if button == Buttons::EncoderPress {
            if is_pressed != self.encoder_is_pressed {
                self.encoder_is_pressed = is_pressed;
                let osc_value = if is_pressed { 1 } else { 0 };
                let msg = OscMessage {
                    addr: "/maschine/encoderPress".to_string(),
                    args: vec![OscType::Int(osc_value)],
                };
                if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                    let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                }
            }
            return false;
        }

        let button_name = format!("{:?}", button).to_string();
        let config = settings.button_configs.get(&button_name);
        let mode = config.map(|c| c.mode).unwrap_or_default();
        let current_light_state = lights.get_button(button) != Brightness::Off;

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
                if is_pressed && lights.get_button(button) != Brightness::Bright {
                    let new_toggle_state = !*self.toggle_states.entry(button).or_default();

                    // Handle Exclusive Groups
                    if new_toggle_state {
                        if let Some(group_id) = config.and_then(|c| c.group_id) {
                            if let Some(member_names) = self.exclusive_groups.get(&group_id) {
                                for other_name in member_names {
                                    if other_name != &button_name {
                                        if let Some(other_button) = button_from_name(other_name) {
                                            self.toggle_states.insert(other_button, false);
                                            lights.set_button(other_button, Brightness::Off);
                                            changed_lights = true;
                                            
                                            // Send OFF OSC for the sibling button
                                            let msg = OscMessage {
                                                addr: format!("/maschine/{}", other_name.to_lowercase()),
                                                args: vec![OscType::Int(0)],
                                            };
                                            if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                                                let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                                            }
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
            let address = format!("/maschine/{}", button_name.to_lowercase());
            let msg = OscMessage { addr: address, args: vec![OscType::Int(osc_value)] };
            if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                let _ = osc_socket.send_to(&encoded_buf, osc_addr);
            }
        }

        if let Some(cc_num) = config.and_then(|c| c.cc) {
            if should_send_osc {
                let cc_val = if osc_value == 1 { 127 } else { 0 };
                let cc_message = MidiMessage::Controller { controller: cc_num.into(), value: cc_val.into() };
                let live_event = LiveEvent::Midi { channel: 0.into(), message: cc_message };
                let mut midibuf = Vec::new();
                if live_event.write(&mut midibuf).is_ok() {
                     let _ = port.send(&midibuf[..]);
                }
            }
        }

        if let Some(b) = target_light_brightness {
            if lights.button_has_light(button) {
                lights.set_button(button, b);
                changed_lights = true;
            }
        }

        changed_lights
    }

    pub fn handle_encoder(
        &mut self,
        val: u8,
        osc_socket: &UdpSocket,
        osc_addr: &SocketAddr,
    ) {
        if val != 0 && val != self.last_encoder_val {
            let diff = val as i8 - self.last_encoder_val as i8;
            let direction = if (diff > 0 && diff < 8) || (diff < -8) { 1 } else { -1 };
            let msg = OscMessage {
                addr: "/maschine/encoder".to_string(),
                args: vec![OscType::Int(direction)],
            };
            if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                let _ = osc_socket.send_to(&encoded_buf, osc_addr);
            }
        }
        if val != 0 {
            self.last_encoder_val = val;
        }
    }

    pub fn handle_slider(
        &mut self,
        val: u8,
        lights: &mut Lights,
        osc_socket: &UdpSocket,
        osc_addr: &SocketAddr,
    ) -> bool {
        if val != 0 {
            let address = "/maschine/slider".to_string();
            let msg = OscMessage { addr: address, args: vec![OscType::Int(val as i32)] };
            if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                let _ = osc_socket.send_to(&encoded_buf, osc_addr);
            }
            let cnt = (val as i32 - 1 + 5) * 25 / 200 - 1;
            for i in 0..25 {
                let b = match cnt - i {
                    0 => Brightness::Normal,
                    1..=25 => Brightness::Dim,
                    _ => Brightness::Off,
                };
                lights.set_slider(i as usize, b);
            }
            return true;
        }
        false
    }
}
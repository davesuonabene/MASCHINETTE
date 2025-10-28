mod self_test;
mod settings;

use crate::self_test::self_test;
use crate::settings::{Settings, ButtonMode};
use clap::Parser;
use config::Config;
use hidapi::{HidDevice, HidResult};
use maschine_library::controls::{Buttons, PadEventType};
use maschine_library::lights::{Brightness, Lights, PadColors};
use maschine_library::screen::Screen;
use maschine_library::font::Font;
use midir::os::unix::VirtualOutput;
use midir::{MidiOutput, MidiOutputConnection};
use midly::{MidiMessage, live::LiveEvent};

use rosc::{OscMessage, OscPacket, OscType};
use rosc::decoder;
use std::net::{UdpSocket, ToSocketAddrs};
use std::error::Error as StdError;
use std::collections::HashMap;
use std::io::ErrorKind;

// Helper function to safely look up button by name.
fn button_from_name(name: &str) -> Option<Buttons> {
    // Iterate over all possible button indices (0 through 40)
    for i in 0..41 {
        if let Some(button) = num::FromPrimitive::from_usize(i) {
            // Compare the string representation of the enum variant (e.g., "Events")
            // with the incoming OSC string (e.g., "events"), ignoring case.
            if format!("{:?}", button).to_string().eq_ignore_ascii_case(name) {
                return Some(button);
            }
        }
    }
    None
}

#[derive(Parser, Debug)]
#[clap(
    name = "Maschine Mikro MK3 Userspace MIDI driver",
    version = env!("CARGO_PKG_VERSION"),
    author = env!("CARGO_PKG_AUTHORS"),
)]
struct Args {
    #[clap(short, long, help = "Config file (see example_config.toml)")]
    config: Option<String>,
}

fn main() -> Result<(), Box<dyn StdError>> {
    let args = Args::parse();

    let mut cfg = Config::builder();

    if let Some(config_fn) = args.config {
        cfg = cfg.add_source(config::File::with_name(config_fn.as_str()));
    }

    let cfg = cfg.build().expect("Can't create settings");
    let settings: Settings = cfg.try_deserialize().expect("Can't parse settings");

    settings.validate().unwrap();

    println!("Running with settings:");
    println!("{settings:?}");

    // --- OSC INITIALIZATION (Sender) ---
    let osc_socket = UdpSocket::bind("0.0.0.0:0").expect("Failed to bind UDP socket for OSC");
    let osc_sender_local_port = osc_socket.local_addr()?.port();
    let osc_addr: std::net::SocketAddr = format!("{}:{}", settings.osc_ip, settings.osc_port)
        .to_socket_addrs()?
        .next().unwrap();
        
    println!("OSC output source port (dynamic): {}", osc_sender_local_port);
    println!("OSC output destination: {}", osc_addr);
    // --- END OSC INITIALIZATION (Sender) ---

    // --- OSC LISTENER INITIALIZATION ---
    let osc_listener = UdpSocket::bind(format!("{}:{}", settings.osc_ip, settings.osc_listen_port))
        .expect("Failed to bind OSC listener socket");
    osc_listener.set_nonblocking(true)
        .expect("Failed to set OSC listener to non-blocking");
    let osc_listener_port = osc_listener.local_addr()?.port();
    println!("OSC listener successfully bound to port {}", osc_listener_port);
    // --- END OSC LISTENER INITIALIZATION ---

    let output = MidiOutput::new(&settings.client_name).expect("Couldn't open MIDI output");
    let mut port = output
        .create_virtual(&settings.port_name)
        .expect("Couldn't create virtual port");

    let api = hidapi::HidApi::new()?;
    #[allow(non_snake_case)]
    let (VID, PID) = (0x17cc, 0x1700);
    let device = api.open(VID, PID)?;

    device.set_blocking_mode(false)?;

    let mut screen = Screen::new();
    let mut lights = Lights::new();

    self_test(&device, &mut screen, &mut lights)?;

    main_loop(
        &device, 
        &mut screen, 
        &mut lights, 
        &mut port, 
        &settings, 
        &osc_socket, 
        &osc_addr,
        &osc_listener, 
    ).map_err(|e| Box::<dyn StdError>::from(e))?; 
    
    Ok(())
}

fn main_loop(
    device: &HidDevice,
    _screen: &mut Screen,
    lights: &mut Lights,
    port: &mut MidiOutputConnection,
    settings: &Settings,
    osc_socket: &UdpSocket,
    osc_addr: &std::net::SocketAddr,
    osc_listener: &UdpSocket, 
) -> HidResult<()> {
    
    let mut toggle_states: HashMap<Buttons, bool> = HashMap::new();
    let mut last_encoder_val: u8 = 0; 
    let mut encoder_is_pressed = false;
    
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
    
    let mut buf = [0u8; 64];
    let mut osc_recv_buf = [0u8; 1024]; 
    
    loop {
        let size = device.read_timeout(&mut buf, 10)?;
        let mut changed_lights = false;
        if size > 0 {
        // --- HID DEVICE INPUT (BUTTONS) ---
            if buf[0] == 0x01 {
                // BUTTON HANDLE
                for i in 0..6 {
                    for j in 0..8 {
                        let idx = i * 8 + j;
                        let button: Option<Buttons> = num::FromPrimitive::from_usize(idx);
                        let button = match button {
                            Some(val) => val,
                            None => continue,
                        };

                        if button == Buttons::EncoderTouch { continue; }

                        let status = buf[i + 1] & (1 << j);
                        let is_pressed = status > 0;
                        
                        if button == Buttons::EncoderPress {
                            if is_pressed != encoder_is_pressed {
                                encoder_is_pressed = is_pressed;
                                let osc_value = if is_pressed { 1 } else { 0 };
                                let msg = OscMessage {
                                    addr: "/maschine/encoderPress".to_string(),
                                    args: vec![OscType::Int(osc_value)],
                                };
                                if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                                    let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                                }
                            }
                            continue;
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
                                    let new_toggle_state = !*toggle_states.entry(button).or_default();
                                    
                                    if new_toggle_state {
                                        if let Some(group_id) = config.and_then(|c| c.group_id) {
                                            if let Some(member_names) = exclusive_groups.get(&group_id) {
                                                for other_name in member_names {
                                                    if other_name != &button_name {
                                                        if let Some(other_button) = button_from_name(other_name) {
                                                            toggle_states.insert(other_button, false);
                                                            lights.set_button(other_button, Brightness::Off);
                                                            changed_lights = true;
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
                                    
                                    toggle_states.insert(button, new_toggle_state);
                                    should_send_osc = true;
                                    osc_value = if new_toggle_state { 1 } else { 0 }; 
                                    target_light_brightness = Some(Brightness::Bright);
                                }
                                
                                if !is_pressed && current_light_state {
                                    target_light_brightness = Some(if *toggle_states.get(&button).unwrap_or(&false) { Brightness::Dim } else { Brightness::Off });
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
                                live_event.write(&mut midibuf).unwrap();
                                port.send(&midibuf[..]).unwrap();
                            }
                        }
                        
                        if let Some(b) = target_light_brightness {
                            if lights.button_has_light(button) {
                                lights.set_button(button, b);
                                changed_lights = true;
                            }
                        }
                    }
                }
                
                let encoder_val = buf[7];
                if encoder_val != 0 && encoder_val != last_encoder_val {
                    let diff = encoder_val as i8 - last_encoder_val as i8;
                    let direction = if (diff > 0 && diff < 8) || (diff < -8) {
                        1 // Clockwise
                    } else {
                        -1 // Counter-clockwise
                    };
                    let msg = OscMessage {
                        addr: "/maschine/encoder".to_string(),
                        args: vec![OscType::Int(direction)],
                    };
                    if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                        let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                    }
                }
                if buf[7] != 0 {
                    last_encoder_val = buf[7];
                }
                
                let slider_val = buf[10];
                if slider_val != 0 {
                    let address = "/maschine/slider".to_string();
                    let msg = OscMessage { addr: address, args: vec![OscType::Int(slider_val as i32)] };
                    if let Ok(encoded_buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
                        let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                    }
                    let cnt = (slider_val as i32 - 1 + 5) * 25 / 200 - 1;
                    for i in 0..25 {
                        let b = match cnt - i {
                            0 => Brightness::Normal,
                            1..=25 => Brightness::Dim,
                            _ => Brightness::Off,
                        };
                        lights. set_slider(i as usize, b);
                    }
                    changed_lights = true;
                }
            } else if buf[0] == 0x02 {
                // PAD HANDLE
                for i in (1..buf.len()).step_by(3) {
                    let idx = buf[i];
                    let evt = buf[i + 1] & 0xf0;
                    let val = ((buf[i + 1] as u16 & 0x0f) << 8) + buf[i + 2] as u16;
                    if i > 1 && idx == 0 && evt == 0 && val == 0 { break; }
                    let pad_evt: PadEventType = num::FromPrimitive::from_u8(evt).unwrap();
                    let (_, prev_b) = lights.get_pad(idx as usize);
                    let b = match pad_evt {
                        PadEventType::NoteOn | PadEventType::PressOn | PadEventType::Aftertouch if val > 0 => Brightness::Normal,
                        _ => Brightness::Off,
                    };
                    if prev_b != b {
                        lights.set_pad(idx as usize, PadColors::Blue, b);
                        changed_lights = true;
                    }

                    let note = settings.notemaps[idx as usize];
                    let mut velocity = (val >> 5) as u8;
                    if val > 0 && velocity == 0 { velocity = 1; }

                    let event = match pad_evt {
                        PadEventType::NoteOn | PadEventType::PressOn => Some(MidiMessage::NoteOn { key: note.into(), vel: velocity.into() }),
                        PadEventType::NoteOff | PadEventType::PressOff => Some(MidiMessage::NoteOff { key: note.into(), vel: velocity.into() }),
                        _ => None,
                    };

                    if let Some(evt) = event {
                        let l_ev = LiveEvent::Midi { channel: 0.into(), message: evt };
                        let mut midibuf = Vec::new();
                        l_ev.write(&mut midibuf).unwrap();
                        port.send(&midibuf[..]).unwrap()
                    }
                }
            }
        }
        
        // HANDLE INCOMING OSC
        match osc_listener.recv_from(&mut osc_recv_buf) {
            Ok((size, _addr)) => {
                if let Ok((_remaining, packet)) = decoder::decode_udp(&osc_recv_buf[..size]) {
                    if let OscPacket::Message(msg) = packet {
                        if msg.addr == "/maschine/screen/text" {
                            if let Some(OscType::String(s)) = msg.args.first() {
                                _screen.reset();
                                Font::write_string(_screen, 0, 0, s, 1);
                                _screen.write(device)?;
                            }
                        }

                        let address_parts: Vec<&str> = msg.addr.split('/').filter(|&s| !s.is_empty()).collect();
                        match address_parts.as_slice() {
                            ["slider"] => {
                                if let Some(OscType::Int(val)) = msg.args.first() {
                                    let slider_val = (*val as u8).clamp(0, 200);
                                    let cnt = (slider_val as i32 - 1 + 5) * 25 / 200 - 1;
                                    for i in 0..25 {
                                        lights.set_slider(i as usize, match cnt - i {
                                            0 => Brightness::Normal,
                                            1..=25 => Brightness::Dim,
                                            _ => Brightness::Off,
                                        });
                                    }
                                    changed_lights = true;
                                }
                            }
                            ["pad", pad_str] => {
                                if let Ok(pad_id) = pad_str.parse::<usize>() {
                                    if pad_id < 16 {
                                        if let (Some(OscType::Int(color_val)), Some(OscType::Int(brightness_val))) = (msg.args.get(0), msg.args.get(1)) {
                                            let color: PadColors = num::FromPrimitive::from_i32(*color_val).unwrap_or(PadColors::Off);
                                            let brightness: Brightness = match brightness_val {
                                                1 => Brightness::Dim,
                                                2 => Brightness::Normal,
                                                3 => Brightness::Bright,
                                                _ => Brightness::Off,
                                            };
                                            lights.set_pad(pad_id, color, brightness);
                                            changed_lights = true;
                                        }
                                    }
                                }
                            }
                            [button_name] => {
                                if let Some(button) = button_from_name(button_name) {
                                    if let Some(OscType::Int(val)) = msg.args.first() {
                                        let new_brightness = if *val == 1 { Brightness::Bright } else { Brightness::Off };
                                        if lights.button_has_light(button) {
                                            lights.set_button(button, new_brightness);
                                            changed_lights = true;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("OSC listener error: {}", e),
        }

        if changed_lights {
            lights.write(device)?;
        }
    }
}
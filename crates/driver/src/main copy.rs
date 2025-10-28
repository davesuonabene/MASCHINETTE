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
    // This binds to a dynamic port (0) for sending data.
    let osc_socket = UdpSocket::bind("0.0.0.0:0").expect("Failed to bind UDP socket for OSC");
    
    // FIX: Get and print the actual dynamic sender port
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
        
    // FIX: Get and print the actual listener port (should match config value, e.g., 57121)
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
    
    // --- Pre-process EXCLUSIVE GROUPS based on group_id ---
    let mut exclusive_groups: HashMap<u8, Vec<String>> = HashMap::new();

    for (button_name, config) in settings.button_configs.iter() {
        if config.mode == ButtonMode::Toggle {
            if let Some(group_id) = config.group_id {
                exclusive_groups
                    .entry(group_id)
                    .or_insert_with(Vec::new)
                    .push(button_name.clone());
            }
        }
    }
    // --- END Pre-processing ---
    
    let mut buf = [0u8; 64];
    let mut osc_recv_buf = [0u8; 1024]; 
    
    loop {
        let size = device.read_timeout(&mut buf, 10)?;
        if size < 1 {
            // Check for OSC input even if HID has no data
        }

        let mut changed_lights = false;
        
        // --- HID DEVICE INPUT (BUTTONS) ---
        if buf[0] == 0x01 {
            // button mode
            for i in 0..6 {
                // bytes
                for j in 0..8 {
                    // bits
                    let idx = i * 8 + j;
                    let button: Option<Buttons> = num::FromPrimitive::from_usize(idx);
                    let button = match button {
                        Some(val) => val,
                        None => continue,
                    };
                    
                    let status = buf[i + 1] & (1 << j);
                    let is_pressed = status > 0;
                    
                    let button_name = format!("{:?}", button).to_string();
                    let config = settings.button_configs.get(&button_name);
                    
                    let mode = config.map(|c| c.mode).unwrap_or(ButtonMode::Trigger); 
                    
                    let current_light_state = lights.get_button(button) != Brightness::Off;
                    
                    let mut should_send_osc = false;
                    let mut osc_value: i32 = 0;
                    let mut target_light_brightness: Option<Brightness> = None;
                    
                    match mode {
                        ButtonMode::Trigger => {
                            if is_pressed != current_light_state {
                                should_send_osc = true;
                                osc_value = if is_pressed { 1 } else { 0 };
                                target_light_brightness = Some(if is_pressed { 
                                    Brightness::Normal 
                                } else { 
                                    Brightness::Off 
                                });
                            }
                        }
                        ButtonMode::Toggle => {
                            if is_pressed && lights.get_button(button) != Brightness::Bright { 
                                
                                let current_toggle_state = *toggle_states.entry(button).or_insert(false);
                                let new_toggle_state = !current_toggle_state;
                                
                                // --- START: EXCLUSIVE GROUP LOGIC ---
                                if new_toggle_state { // Only run exclusivity check when turning ON
                                    if let Some(group_id) = config.and_then(|c| c.group_id) { 
                                        if let Some(member_names) = exclusive_groups.get(&group_id) {
                                            
                                            for other_name in member_names {
                                                if other_name != &button_name {
                                                    // Find the Buttons enum value by name and reset its state
                                                    for other_idx in 0..41 { 
                                                        if let Some(other_button) = num::FromPrimitive::from_usize(other_idx) {
                                                            if format!("{:?}", other_button).to_string() == *other_name {
                                                                
                                                                // 1. Reset toggle state
                                                                toggle_states.insert(other_button, false);
                                                                
                                                                // 2. Reset light
                                                                lights.set_button(other_button, Brightness::Off);
                                                                changed_lights = true;
                                                                
                                                                // 3. Send OSC 0
                                                                let address = format!("/maschine/{}", other_name.to_lowercase());
                                                                let msg_contents = OscMessage {
                                                                    addr: address,
                                                                    args: vec![OscType::Int(0)], 
                                                                };
                                                                let packet = OscPacket::Message(msg_contents);
                                                                if let Ok(encoded_buf) = rosc::encoder::encode(&packet) {
                                                                    let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                                                                }
                                                                break; // Stop searching once found
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // --- END: EXCLUSIVE GROUP LOGIC ---
                                
                                toggle_states.insert(button, new_toggle_state);
                                should_send_osc = true;
                                osc_value = if new_toggle_state { 1 } else { 0 }; 
                                
                                target_light_brightness = Some(Brightness::Bright);
                            }
                            
                            // Handle light release feedback (Dim = ON, Released)
                            if !is_pressed && current_light_state {
                                if *toggle_states.get(&button).unwrap_or(&false) {
                                    target_light_brightness = Some(Brightness::Dim); 
                                } else {
                                    target_light_brightness = Some(Brightness::Off);
                                }
                            }
                        }
                    }
                    
                    // Handle OSC sending
                    if should_send_osc {
                        let address = format!("/maschine/{}", button_name.to_lowercase());
                        let msg_contents = OscMessage {
                            addr: address,
                            args: vec![OscType::Int(osc_value)], 
                        };
                        let packet = OscPacket::Message(msg_contents);

                        if let Ok(encoded_buf) = rosc::encoder::encode(&packet) {
                            if let Err(e) = osc_socket.send_to(&encoded_buf, osc_addr) {
                                eprintln!("Failed to send OSC message for {:?}: {}", button, e);
                            }
                        }
                    }

                    // --- BEGIN ADDITION ---
                    // Handle MIDI CC sending
                    if let Some(cc_num) = config.and_then(|c| c.cc) {
                        // Only send CC on a state change (press or release for trigger, only press for toggle)
                        if should_send_osc { 
                            let cc_val = if osc_value == 1 { 127 } else { 0 };
                            
                            let cc_message = MidiMessage::Controller {
                                controller: cc_num.into(),
                                value: cc_val.into(),
                            };

                            let live_event = LiveEvent::Midi {
                                channel: 0.into(),
                                message: cc_message,
                            };

                            let mut buf = Vec::new();
                            live_event.write(&mut buf).unwrap();
                            port.send(&buf[..]).unwrap();

                            println!("Sent CC: {} on controller {}", cc_val, cc_num);
                        }
                    }
                    // --- END ADDITION ---
                    
                    // Handle light update
                    if let Some(b) = target_light_brightness {
                        if lights.button_has_light(button) {
                            lights.set_button(button, b);
                            changed_lights = true;
                        }
                    }

                    if mode == ButtonMode::Trigger && is_pressed {
                        println!("{:?}", button); 
                    }
                }
            }
            
            // Encoder logic (filtered printing)
            let encoder_val = buf[7];
            
            if encoder_val != 0 || last_encoder_val != encoder_val {
                println!("Encoder: {}", encoder_val);
            }
            last_encoder_val = encoder_val;
            
            // Slider logic
            let slider_val = buf[10];
            if slider_val != 0 {
                println!("Slider: {}", slider_val);

                // --- BEGIN ADDITION ---
                // Send the slider value out via OSC
                let address = "/maschine/slider".to_string();
                let msg_contents = OscMessage {
                    addr: address,
                    args: vec![OscType::Int(slider_val as i32)],
                };
                let packet = OscPacket::Message(msg_contents);
                if let Ok(encoded_buf) = rosc::encoder::encode(&packet) {
                    let _ = osc_socket.send_to(&encoded_buf, osc_addr);
                }
                // --- END ADDITION ---
                let cnt = (slider_val as i32 - 1 + 5) * 25 / 200 - 1;
                for i in 0..25 {
                    let b = match cnt - i {
                        0 => Brightness::Normal,
                        1..=25 => Brightness::Dim,
                        _ => Brightness::Off,
                    };
                    lights.set_slider(i as usize, b);
                }
                changed_lights = true;
            }
        } else if buf[0] == 0x02 {
            // pad mode
            for i in (1..buf.len()).step_by(3) {
                let idx = buf[i];
                let evt = buf[i + 1] & 0xf0;
                let val = ((buf[i + 1] as u16 & 0x0f) << 8) + buf[i + 2] as u16;
                if i > 1 && idx == 0 && evt == 0 && val == 0 {
                    break;
                }
                let pad_evt: PadEventType = num::FromPrimitive::from_u8(evt).unwrap();
                // if evt != PadEventType::Aftertouch {
                println!("Pad {}: {:?} @ {}", idx, pad_evt, val);
                // }
                let (_, prev_b) = lights.get_pad(idx as usize);
                let b = match pad_evt {
                    PadEventType::NoteOn | PadEventType::PressOn => Brightness::Normal,
                    PadEventType::NoteOff | PadEventType::PressOff => Brightness::Off,
                    PadEventType::Aftertouch => {
                        if val > 0 {
                            Brightness::Normal
                        } else {
                            Brightness::Off
                        }
                    }
                    #[allow(unreachable_patterns)]
                    _ => prev_b,
                };
                if prev_b != b {
                    lights.set_pad(idx as usize, PadColors::Blue, b);
                    changed_lights = true;
                }

                let note = settings.notemaps[idx as usize];
                let mut velocity = (val >> 5) as u8;
                if val > 0 && velocity == 0 {
                    velocity = 1;
                }

                let event = match pad_evt {
                    PadEventType::NoteOn | PadEventType::PressOn => Some(MidiMessage::NoteOn {
                        key: note.into(),
                        vel: velocity.into(),
                    }),
                    PadEventType::NoteOff | PadEventType::PressOff => Some(MidiMessage::NoteOff {
                        key: note.into(),
                        vel: velocity.into(),
                    }),
                    _ => None,
                };

                if let Some(evt) = event {
                    let l_ev = LiveEvent::Midi {
                        channel: 0.into(),
                        message: evt,
                    };
                    let mut buf = Vec::new();
                    l_ev.write(&mut buf).unwrap();
                    port.send(&buf[..]).unwrap()
                }
            }
        }
        
        // --- NEW: HANDLE INCOMING OSC NETWORK INPUT ---
        match osc_listener.recv_from(&mut osc_recv_buf) {
            Ok((size, _addr)) => {
                if let Ok((_remaining, packet)) = decoder::decode_udp(&osc_recv_buf[..size]) {
                    if let OscPacket::Message(msg) = packet {
                        // Split the address path into parts
                        let address_parts: Vec<&str> = msg.addr.split('/').filter(|&s| !s.is_empty()).collect();

                        // Match on the address parts to determine the control type
                        match address_parts.as_slice() {
                            // Match /slider
                            ["slider"] => {
                                if let Some(OscType::Int(val)) = msg.args.first() {
                                    println!("OSC RX: Controlling slider with value: {}", val);

                                    let slider_val = (*val as u8).clamp(0, 200);
                                    let cnt = (slider_val as i32 - 1 + 5) * 25 / 200 - 1;
                                    for i in 0..25 {
                                        let b = match cnt - i {
                                            0 => Brightness::Normal,
                                            1..=25 => Brightness::Dim,
                                            _ => Brightness::Off,
                                        };
                                        lights.set_slider(i as usize, b);
                                    }
                                    changed_lights = true;
                                }
                            }
                            // Match /pad/N
                            ["pad", pad_str] => {
                                if let Ok(pad_id) = pad_str.parse::<usize>() {
                                    if pad_id < 16 { // Ensure pad_id is 0-15
                                        if let (Some(OscType::Int(color_val)), Some(OscType::Int(brightness_val))) = (msg.args.get(0), msg.args.get(1)) {
                                            
                                            let color: PadColors = num::FromPrimitive::from_i32(*color_val).unwrap_or(PadColors::Off);
                                            let brightness: Brightness = match brightness_val {
                                                1 => Brightness::Dim,
                                                2 => Brightness::Normal,
                                                3 => Brightness::Bright,
                                                _ => Brightness::Off,
                                            };

                                            println!("OSC RX: Setting Pad {} to Color: {:?}, Brightness: {:?}", pad_id, color, brightness);
                                            lights.set_pad(pad_id, color, brightness);
                                            changed_lights = true;
                                        }
                                    }
                                }
                            }
                            // Match any other single-part address as a button (e.g., /play)
                            [button_name] => {
                                if let Some(button) = button_from_name(button_name) {
                                    if let Some(OscType::Int(val)) = msg.args.first() {
                                        println!("OSC RX: Controlling button {}: {}", button_name, val);
                                        
                                        let new_brightness = if *val == 1 { Brightness::Bright } else { Brightness::Off };
                                        
                                        if lights.button_has_light(button) {
                                            lights.set_button(button, new_brightness);
                                            changed_lights = true;
                                        }
                                    }
                                }
                            }
                            // Ignore any other message format
                            _ => {}
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                // No data available right now, which is normal.
            }
            Err(e) => {
                // A genuine network error occurred.
                eprintln!("OSC listener error: {}", e);
            }
        }
        // --- END OSC NETWORK INPUT ---
        match osc_listener.recv_from(&mut osc_recv_buf) {
            Ok((size, _addr)) => {
                if let Ok((_remaining, packet)) = decoder::decode_udp(&osc_recv_buf[..size]) { 
                    if let OscPacket::Message(msg) = packet {
                        // Example Address: /puredata/events/1
                        let address_parts: Vec<&str> = msg.addr.trim_start_matches('/').split('/').collect();

                        // --- HANDLE SCREEN TEXT ---
                        if msg.addr == "/maschine/screen/text" {
                            if let Some(OscType::String(s)) = msg.args.first() {
                                println!("OSC RX: Displaying text: {}", s);
                                _screen.reset();
                                Font::write_string(_screen, 0, 0, s, 1);
                                _screen.write(device)?;
                            }
                        }
                        // --- END HANDLE SCREEN TEXT ---

                        // Check for the expected address structure: [PREFIX]/[BUTTON_NAME]/[VALUE]
                        if address_parts.len() >= 2 {
                            
                            let button_name_str = address_parts[1];
                            
                            if let Some(button) = button_from_name(button_name_str) {
                                
                                if let Some(OscType::Int(val)) = msg.args.first() {
                                    
                                    println!("OSC RX: Controlling button {}: {}", button_name_str, val);
                                    
                                    let new_brightness = match *val {
                                        1 => Brightness::Bright, 
                                        _ => Brightness::Off,     
                                    };
                                    
                                    if lights.button_has_light(button) {
                                        lights.set_button(button, new_brightness);
                                        changed_lights = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                // No data available right now. This is expected in non-blocking mode.
            } 
            Err(e) => {
                // A genuine network error occurred
                eprintln!("OSC listener error: {}", e);
            }
        }
        if changed_lights {
            lights.write(device)?;
        }
    }
}
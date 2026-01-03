// crates/driver/src/main.rs
mod self_test;
mod settings;
mod custom_midi;
mod play_mode;

use crate::self_test::self_test;
use crate::settings::{Settings};
use crate::custom_midi::CustomMidiMode;
use crate::play_mode::PlayMode;

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

use rosc::{OscPacket, OscType};
use rosc::decoder;
use std::net::{UdpSocket, ToSocketAddrs};
use std::error::Error as StdError;
use std::io::ErrorKind;
use std::time::Duration;
use std::thread;

#[derive(Debug, PartialEq, Clone, Copy)]
enum DriverMode {
    CustomMidi,
    Playability,
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
    println!("Running with settings: {:?}", settings);

    // --- OSC INITIALIZATION ---
    let osc_socket = UdpSocket::bind("0.0.0.0:0")?;
    let osc_addr: std::net::SocketAddr = format!("{}:{}", settings.osc_ip, settings.osc_port)
        .to_socket_addrs()?.next().unwrap();
    
    let osc_listener = UdpSocket::bind(format!("{}:{}", settings.osc_ip, settings.osc_listen_port))?;
    osc_listener.set_nonblocking(true)?;
    // --------------------------

    let output = MidiOutput::new(&settings.client_name).expect("Couldn't open MIDI output");
    let mut port = output.create_virtual(&settings.port_name).expect("Couldn't create virtual port");

    let api = hidapi::HidApi::new()?;
    let device = api.open(0x17cc, 0x1700)?;
    device.set_blocking_mode(false)?; // Ensure non-blocking at API level

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
    
    let mut current_mode = DriverMode::CustomMidi;
    
    // Initialize Mode Handlers
    let mut custom_midi = CustomMidiMode::new(settings);
    let mut play_mode = PlayMode::new();

    println!("Starting in Custom MIDI Mode.");
    // Initial Light Setup
    lights.set_button(Buttons::Maschine, Brightness::Bright);
    lights.set_button(Buttons::Star, Brightness::Dim);
    lights.set_button(Buttons::Browse, Brightness::Dim);
    lights.write(device)?;

    let mut buf = [0u8; 64];
    let mut osc_recv_buf = [0u8; 1024]; 
    
    loop {
        let mut loop_activity = false;
        let mut changed_lights = false;

        // --- BATCH HID READ ---
        // Drain the OS buffer completely before processing anything else.
        // This prevents stuck notes caused by slow writes blocking reads.
        loop {
            // Read with 0 timeout (non-blocking)
            let size = device.read_timeout(&mut buf, 0)?;
            
            if size == 0 {
                break;
            }
            loop_activity = true;
        
            if buf[0] == 0x01 {
                // BUTTONS
                for i in 0..6 {
                    for j in 0..8 {
                        let idx = i * 8 + j;
                        let button = match num::FromPrimitive::from_usize(idx) {
                            Some(val) => val,
                            None => continue,
                        };

                        if button == Buttons::EncoderTouch { continue; }

                        let is_pressed = (buf[i + 1] & (1 << j)) > 0;
                        
                        // --- RESERVED BUTTONS (MODE SWITCHING) ---
                        if button == Buttons::Maschine {
                            if is_pressed {
                                current_mode = DriverMode::CustomMidi;
                                // println!("Mode Switched: Custom MIDI"); // REMOVED
                                
                                lights.set_button(Buttons::Maschine, Brightness::Bright);
                                lights.set_button(Buttons::Star, Brightness::Dim);
                                lights.set_button(Buttons::Browse, Brightness::Dim);
                                
                                custom_midi.refresh(lights);
                                
                                // Screen Update - careful, this is slow, but acceptable on mode switch
                                _screen.reset();
                                Font::write_string(_screen, 0, 0, "MIDI MODE", 1);
                                _screen.write(device)?;
                                changed_lights = true;
                            }
                            continue;
                        }

                        if button == Buttons::Star {
                            if is_pressed {
                                current_mode = DriverMode::Playability;
                                // println!("Mode Switched: Playability"); // REMOVED
                                
                                lights.set_button(Buttons::Star, Brightness::Bright);
                                lights.set_button(Buttons::Maschine, Brightness::Dim);
                                lights.set_button(Buttons::Browse, Brightness::Dim);

                                play_mode.refresh(lights);

                                _screen.reset();
                                Font::write_string(_screen, 0, 0, "PLAY MODE", 1);
                                _screen.write(device)?;
                                changed_lights = true;
                            }
                            continue;
                        }

                        if button == Buttons::Browse {
                             // Reserved button - consume event
                             continue; 
                        }
                        // --- END RESERVED BUTTONS ---

                        // --- DELEGATE TO MODES ---
                        match current_mode {
                            DriverMode::CustomMidi => {
                                if custom_midi.handle_button(button, is_pressed, settings, lights, osc_socket, osc_addr, port) {
                                    changed_lights = true;
                                }
                            },
                            DriverMode::Playability => {
                                if play_mode.handle_button(button, is_pressed, lights) {
                                    changed_lights = true;
                                }
                            }
                        }
                    }
                }
                
                // ENCODER & SLIDER
                if current_mode == DriverMode::CustomMidi {
                    custom_midi.handle_encoder(buf[7], osc_socket, osc_addr);
                    if custom_midi.handle_slider(buf[10], lights, osc_socket, osc_addr) {
                        changed_lights = true;
                    }
                }

            } else if buf[0] == 0x02 {
                // PADS
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
                    
                    // Only update lights if actually changed to prevent excessive USB writes
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
                        // Optimize: use stack buffer for midi encoding
                        let mut midibuf = [0u8; 3]; 
                        // Using a small vec or slice directly with midly if possible, but 3 bytes is typical max for note on/off
                        let mut vec_buf = Vec::with_capacity(3); // Fallback to vec as `write` needs `std::io::Write`
                        if l_ev.write(&mut vec_buf).is_ok() {
                            let _ = port.send(&vec_buf);
                        }
                    }
                }
            }
        } // End of HID Drain Loop

        // Write lights ONLY once per batch if needed
        if changed_lights {
            lights.write(device)?;
        }
        
        // --- OSC LISTENER ---
        // Quick check, non-blocking
        loop {
            match osc_listener.recv_from(&mut osc_recv_buf) {
                Ok((size, _)) => {
                    loop_activity = true;
                    if let Ok((_, packet)) = decoder::decode_udp(&osc_recv_buf[..size]) {
                        if let OscPacket::Message(msg) = packet {
                             if msg.addr == "/maschine/screen/text" {
                                if let Some(OscType::String(s)) = msg.args.first() {
                                    _screen.reset();
                                    Font::write_string(_screen, 0, 0, s, 1);
                                    _screen.write(device)?; // This is slow, but infrequent
                                }
                            }
                        }
                    }
                },
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    break; // No more OSC messages
                }
                Err(e) => {
                    eprintln!("OSC error: {}", e);
                    break;
                },
            }
        }

        // --- IDLE SLEEP ---
        // If we did absolutely nothing this frame (no HID, no OSC), sleep a tiny bit to save CPU.
        // If we processed HID, we loop immediately to catch the next burst.
        if !loop_activity {
            thread::sleep(Duration::from_millis(1));
        }
    }
}
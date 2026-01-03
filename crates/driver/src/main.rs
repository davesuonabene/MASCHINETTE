// crates/driver/src/main.rs
mod self_test;
mod settings;
// Removed: mod custom_midi; (now in modes/custom_midi.rs)
// Removed: mod play_mode;   (now in modes/play_mode.rs)
mod input;
mod context;
mod modes;

use crate::self_test::self_test;
use crate::settings::Settings;
use crate::context::DriverContext;
use crate::input::{parse_hid_report, HardwareEvent};
use crate::modes::{MachineMode, CustomMidiMode, PlayMode};

use clap::Parser;
use config::Config;
// use hidapi::HidDevice; // Removed unused import
use maschine_library::controls::Buttons;
use maschine_library::lights::{Brightness, Lights};
use maschine_library::screen::Screen;
use maschine_library::font::Font;
use midir::MidiOutput;
use midir::os::unix::VirtualOutput; // FIX: Added this trait for create_virtual
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

    // --- MIDI & HID ---
    let output = MidiOutput::new(&settings.client_name).expect("Couldn't open MIDI output");
    let mut port = output.create_virtual(&settings.port_name).expect("Couldn't create virtual port");

    let api = hidapi::HidApi::new()?;
    let device = api.open(0x17cc, 0x1700)?;
    device.set_blocking_mode(false)?;

    let mut screen = Screen::new();
    let mut lights = Lights::new();

    self_test(&device, &mut screen, &mut lights)?;

    // --- CONTEXT ---
    let mut context = DriverContext {
        lights: &mut lights,
        midi_port: &mut port,
        osc_socket: &osc_socket,
        osc_addr: &osc_addr,
        settings: &settings,
    };

    let mut current_mode_id = DriverMode::CustomMidi;
    let mut custom_midi = CustomMidiMode::new(&settings);
    let mut play_mode = PlayMode::new();
    
    // Initial Setup
    println!("Starting in Custom MIDI Mode.");
    context.lights.set_button(Buttons::Maschine, Brightness::Bright);
    context.lights.set_button(Buttons::Star, Brightness::Dim);
    context.lights.set_button(Buttons::Browse, Brightness::Dim);
    context.lights.write(&device)?;
    
    custom_midi.on_enter(&mut context);

    let mut buf = [0u8; 64];
    let mut osc_recv_buf = [0u8; 1024]; 

    loop {
        let mut loop_activity = false;
        let mut changed_lights = false;

        // --- BATCH HID READ ---
        loop {
            // Non-blocking read
            let size = match device.read_timeout(&mut buf, 0) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("HID Error: {}", e);
                    0 
                }
            };
            
            if size == 0 {
                break;
            }
            loop_activity = true;

            // Parse raw bytes into Events using your input.rs logic
            let events = parse_hid_report(&buf[..size]);

            for event in events {
                match event {
                    // --- GLOBAL MODE SWITCHING ---
                    HardwareEvent::Button { index: Buttons::Maschine, pressed: true } => {
                        current_mode_id = DriverMode::CustomMidi;
                        
                        context.lights.set_button(Buttons::Maschine, Brightness::Bright);
                        context.lights.set_button(Buttons::Star, Brightness::Dim);
                        context.lights.set_button(Buttons::Browse, Brightness::Dim);
                        
                        custom_midi.on_enter(&mut context);
                        
                        // FIX: Changed _screen to screen
                        screen.reset();
                        Font::write_string(&mut screen, 0, 0, "MIDI MODE", 1);
                        screen.write(&device)?;
                        changed_lights = true;
                    },
                    HardwareEvent::Button { index: Buttons::Star, pressed: true } => {
                        current_mode_id = DriverMode::Playability;
                        
                        context.lights.set_button(Buttons::Star, Brightness::Bright);
                        context.lights.set_button(Buttons::Maschine, Brightness::Dim);
                        context.lights.set_button(Buttons::Browse, Brightness::Dim);

                        play_mode.on_enter(&mut context);

                        // FIX: Changed _screen to screen
                        screen.reset();
                        Font::write_string(&mut screen, 0, 0, "PLAY MODE", 1);
                        screen.write(&device)?;
                        changed_lights = true;
                    },
                    HardwareEvent::Button { index: Buttons::Browse, pressed: true } => {
                        // Reserved (ignore)
                    },
                    
                    // --- DELEGATE TO ACTIVE MODE ---
                    _ => {
                        let mode_changed_lights = match current_mode_id {
                            DriverMode::CustomMidi => {
                                let mut mode_ctx = DriverContext {
                                    lights: context.lights,
                                    midi_port: context.midi_port,
                                    osc_socket: context.osc_socket,
                                    osc_addr: context.osc_addr,
                                    settings: context.settings,
                                };
                                custom_midi.handle_event(&event, &mut mode_ctx);
                                true 
                            },
                            DriverMode::Playability => {
                                let mut mode_ctx = DriverContext {
                                    lights: context.lights,
                                    midi_port: context.midi_port,
                                    osc_socket: context.osc_socket,
                                    osc_addr: context.osc_addr,
                                    settings: context.settings,
                                };
                                play_mode.handle_event(&event, &mut mode_ctx);
                                true
                            }
                        };
                        if mode_changed_lights { changed_lights = true; }
                    }
                }
            }
        }

        // Write lights ONLY once per batch
        if changed_lights {
            context.lights.write(&device)?;
        }

        // --- OSC LISTENER ---
        loop {
            match osc_listener.recv_from(&mut osc_recv_buf) {
                Ok((size, _)) => {
                    loop_activity = true;
                    if let Ok((_, packet)) = decoder::decode_udp(&osc_recv_buf[..size]) {
                        if let OscPacket::Message(msg) = packet {
                            if msg.addr == "/maschine/screen/text" {
                                if let Some(OscType::String(s)) = msg.args.first() {
                                    // FIX: Changed _screen to screen
                                    screen.reset();
                                    Font::write_string(&mut screen, 0, 0, s, 1);
                                    screen.write(&device)?; 
                                }
                            }
                        }
                    }
                },
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    break; 
                }
                Err(e) => {
                    eprintln!("OSC error: {}", e);
                    break;
                },
            }
        }

        if !loop_activity {
            thread::sleep(Duration::from_millis(1));
        }
    }
}
use std::time::{Duration, Instant};
use midly::{live::LiveEvent, MidiMessage};
use maschine_library::lights::{Brightness, PadColors};
use maschine_library::controls::{Buttons, PadEventType};
use crate::context::DriverContext;
use crate::input::HardwareEvent;
use super::MachineMode;

#[derive(Clone, Debug)]
struct SeqEvent {
    offset: Duration,
    note: u8,
    velocity: u8,
    is_note_on: bool,
}

pub struct PlayMode {
    // State
    armed: bool,      // Waiting for first note to start initial recording
    recording: bool,  // Currently recording (Initial or Overdub)
    playing: bool,    // Sequencer is running (Looping)
    
    // Timing
    start_time: Option<Instant>,      // Start of the Initial Recording
    playback_start: Option<Instant>,  // Start of the current loop iteration
    loop_duration: Duration,
    
    // Data
    events: Vec<SeqEvent>,
    playback_cursor: usize,
    
    // Visuals
    user_holding: [bool; 16], // Tracks pads physically held by user
    seq_holding: [bool; 16],  // Tracks pads held by sequencer
}

impl PlayMode {
    pub fn new() -> Self {
        Self {
            armed: false,
            recording: false,
            playing: false,
            start_time: None,
            playback_start: None,
            loop_duration: Duration::from_millis(0),
            events: Vec::new(),
            playback_cursor: 0,
            user_holding: [false; 16],
            seq_holding: [false; 16],
        }
    }

    pub fn tick(&mut self, ctx: &mut DriverContext) -> bool {
        let mut changed = false;
        let now = Instant::now();

        // --- 1. SEQUENCER PLAYBACK & LOOPING ---
        if self.playing && self.loop_duration > Duration::ZERO {
            // Initialize playback anchor if missing
            if self.playback_start.is_none() {
                self.playback_start = Some(now);
            }

            let start = self.playback_start.unwrap();
            let mut elapsed = now.duration_since(start);

            // Loop Wrap
            if elapsed >= self.loop_duration {
                self.playback_start = Some(now);
                self.playback_cursor = 0;
                elapsed = Duration::from_millis(0);
                
                // We do NOT clear lights here anymore to support sustain.
                // We rely on Seq NoteOff events to clear lights.
                // However, to be safe against stuck notes at loop boundaries:
                // (Optional: Logic to handle wrap-around notes would go here)
            }

            // Fire Events
            while self.playback_cursor < self.events.len() {
                let event = &self.events[self.playback_cursor];
                if event.offset <= elapsed {
                    // Send MIDI
                    let midi_msg = if event.is_note_on {
                        MidiMessage::NoteOn { key: event.note.into(), vel: event.velocity.into() }
                    } else {
                        MidiMessage::NoteOff { key: event.note.into(), vel: event.velocity.into() }
                    };
                    
                    let live_event = LiveEvent::Midi { channel: 0.into(), message: midi_msg };
                    let mut buf = Vec::new();
                    if live_event.write(&mut buf).is_ok() {
                        let _ = ctx.midi_port.send(&buf);
                    }

                    // Update Sequence State & Lights
                    if let Some(pad_index) = ctx.settings.notemaps.iter().position(|&n| n == event.note) {
                        self.seq_holding[pad_index] = event.is_note_on;
                        self.update_pad_light(ctx, pad_index);
                        changed = true;
                    }

                    self.playback_cursor += 1;
                } else {
                    break;
                }
            }
        }

        // --- 2. RECORDING BUTTON BLINK ---
        // Blink logic: On for 500ms, Off for 500ms
        if self.recording {
            let blink_on = (now.elapsed().as_millis() / 500) % 2 == 0;
            let brightness = if blink_on { Brightness::Bright } else { Brightness::Dim };
            ctx.lights.set_button(Buttons::Rec, brightness);
            changed = true;
        }

        changed
    }

    fn update_pad_light(&self, ctx: &mut DriverContext, pad_index: usize) {
        // Priority: User Input (White) > Sequencer (Orange) > Off
        if self.user_holding[pad_index] {
            ctx.lights.set_pad(pad_index, PadColors::White, Brightness::Bright);
        } else if self.seq_holding[pad_index] {
            ctx.lights.set_pad(pad_index, PadColors::Orange, Brightness::Normal);
        } else {
            ctx.lights.set_pad(pad_index, PadColors::Off, Brightness::Off);
        }
    }

    fn update_transport_lights(&self, ctx: &mut DriverContext) {
        // Rec button is handled by tick() blinking if recording
        if !self.recording {
            if self.armed {
                ctx.lights.set_button(Buttons::Rec, Brightness::Dim);
            } else {
                ctx.lights.set_button(Buttons::Rec, Brightness::Off);
            }
        }

        if self.playing {
            ctx.lights.set_button(Buttons::Play, Brightness::Bright);
            ctx.lights.set_button(Buttons::Stop, Brightness::Dim);
        } else {
            ctx.lights.set_button(Buttons::Play, Brightness::Off);
            ctx.lights.set_button(Buttons::Stop, Brightness::Bright); // Bright when stopped
        }
        
        ctx.lights.set_button(Buttons::Erase, Brightness::Dim);
    }
    
    fn clear_all(&mut self, ctx: &mut DriverContext) {
        self.playing = false;
        self.recording = false;
        self.armed = false;
        self.start_time = None;
        self.playback_start = None;
        self.loop_duration = Duration::from_millis(0);
        self.events.clear();
        self.playback_cursor = 0;
        self.seq_holding = [false; 16];
        self.user_holding = [false; 16];
        
        // Clear all pad lights
        for i in 0..16 {
            ctx.lights.set_pad(i, PadColors::Off, Brightness::Off);
        }
        self.update_transport_lights(ctx);
    }
}

impl MachineMode for PlayMode {
    fn on_enter(&mut self, ctx: &mut DriverContext) {
        self.update_transport_lights(ctx);
    }

    fn handle_event(&mut self, event: &HardwareEvent, ctx: &mut DriverContext) {
        match event {
            HardwareEvent::Button { index, pressed } => {
                if *pressed {
                    match index {
                        Buttons::Rec => {
                            if self.recording {
                                // STOP RECORDING (Finish Initial or Stop Overdub) -> KEEP PLAYING
                                if self.loop_duration == Duration::ZERO {
                                    // Finish Initial Recording
                                    if let Some(start) = self.start_time {
                                        self.loop_duration = Instant::now().duration_since(start);
                                    }
                                    self.playback_start = Some(Instant::now()); // Align loop start
                                }
                                self.recording = false;
                                self.playing = true;
                            } else if self.playing {
                                // START OVERDUB
                                self.recording = true;
                            } else if self.armed {
                                // DISARM
                                self.armed = false;
                            } else {
                                // ARM (for initial recording)
                                self.armed = true;
                            }
                        },
                        Buttons::Play => {
                            if self.recording && self.loop_duration == Duration::ZERO {
                                // Finish Initial Rec -> Play
                                if let Some(start) = self.start_time {
                                    self.loop_duration = Instant::now().duration_since(start);
                                }
                                self.recording = false;
                                self.playing = true;
                                self.playback_start = Some(Instant::now());
                            } else if self.playing {
                                // Stop/Pause
                                self.playing = false;
                                self.recording = false;
                                // Turn off sequencer lights
                                self.seq_holding = [false; 16];
                                for i in 0..16 {
                                    self.update_pad_light(ctx, i);
                                }
                            } else if self.loop_duration > Duration::ZERO {
                                // Start Playing existing loop
                                self.playing = true;
                                self.playback_start = Some(Instant::now());
                                self.playback_cursor = 0;
                            }
                        },
                        Buttons::Stop => {
                             self.playing = false;
                             self.recording = false;
                             self.armed = false;
                             self.seq_holding = [false; 16];
                             for i in 0..16 {
                                self.update_pad_light(ctx, i);
                             }
                        },
                        Buttons::Erase => {
                            self.clear_all(ctx);
                        },
                        _ => {}
                    }
                    self.update_transport_lights(ctx);
                }
            },
            HardwareEvent::Pad { index, event_type, value } => {
                let note = ctx.settings.notemaps[*index];
                
                // 1. Track User State
                match event_type {
                    PadEventType::NoteOn | PadEventType::PressOn if *value > 0 => {
                        self.user_holding[*index] = true;
                    },
                    PadEventType::NoteOff | PadEventType::PressOff => {
                        self.user_holding[*index] = false;
                    },
                    _ => {}
                }
                
                // 2. Visual Feedback (User Input Priority)
                self.update_pad_light(ctx, *index);

                // 3. MIDI Thru
                let velocity = (value >> 5) as u8;
                let midi_msg = match event_type {
                    PadEventType::NoteOn | PadEventType::PressOn => Some(MidiMessage::NoteOn { key: note.into(), vel: velocity.into() }),
                    PadEventType::NoteOff | PadEventType::PressOff => Some(MidiMessage::NoteOff { key: note.into(), vel: velocity.into() }),
                    _ => None,
                };

                if let Some(msg) = midi_msg {
                    let live_event = LiveEvent::Midi { channel: 0.into(), message: msg };
                    let mut buf = Vec::new();
                    if live_event.write(&mut buf).is_ok() {
                        let _ = ctx.midi_port.send(&buf);
                    }

                    // 4. Recording Logic
                    // A. Trigger Initial Recording on First Note
                    if self.armed && (*event_type == PadEventType::NoteOn || *event_type == PadEventType::PressOn) && *value > 0 {
                        self.armed = false;
                        self.recording = true;
                        self.events.clear();
                        self.start_time = Some(Instant::now());
                        self.loop_duration = Duration::ZERO; // Mark as Initial Recording
                        self.update_transport_lights(ctx);
                    }

                    // B. Capture Events
                    if self.recording {
                        let now = Instant::now();
                        let offset = if self.loop_duration == Duration::ZERO {
                            // Initial Recording: Offset from Start Time
                            if let Some(start) = self.start_time {
                                now.duration_since(start)
                            } else {
                                Duration::ZERO
                            }
                        } else {
                            // Overdub: Offset from Playback Start (Modulo Loop Duration)
                            if let Some(start) = self.playback_start {
                                let raw = now.duration_since(start);
                                // Simple modulo simulation if we drifted past loop end before tick reset it
                                if raw > self.loop_duration {
                                    raw - self.loop_duration // Approx wrap
                                } else {
                                    raw
                                }
                            } else {
                                Duration::ZERO
                            }
                        };
                        
                        let is_note_on = matches!(event_type, PadEventType::NoteOn | PadEventType::PressOn);
                        if is_note_on || matches!(event_type, PadEventType::NoteOff | PadEventType::PressOff) {
                            // Insert sorted? Or just append and sort later?
                            // For simplicity, append. If we append out of order in Overdub, 
                            // the tick loop might miss it if cursor passed, but it will catch it next loop.
                            // Ideally we sort events by offset.
                            self.events.push(SeqEvent {
                                offset,
                                note,
                                velocity,
                                is_note_on,
                            });
                            
                            // Optimization: Keep events sorted by offset for the tick loop
                            self.events.sort_by(|a, b| a.offset.cmp(&b.offset));
                        }
                    }
                }
            },
            _ => {}
        }
    }
}
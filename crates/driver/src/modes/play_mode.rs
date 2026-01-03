// crates/driver/src/modes/play_mode.rs
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
    paused_position: Option<Duration>, // Saved offset for resume

    // Data
    events: Vec<SeqEvent>,
    playback_cursor: usize,
    
    // Visuals
    user_holding: [bool; 16], // Tracks pads physically held by user
    seq_holding: [bool; 16],  // Tracks pads held by sequencer
    
    // Button States (for momentary lights)
    is_restart_pressed: bool,
    is_erase_pressed: bool,
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
            paused_position: None,
            events: Vec::new(),
            playback_cursor: 0,
            user_holding: [false; 16],
            seq_holding: [false; 16],
            is_restart_pressed: false,
            is_erase_pressed: false,
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
            // When blinking off, use Dim to match "half lit when off" request
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
        // Rec Button Logic:
        // Always active logic because it's the entry point for creating a loop.
        // If recording, tick() handles blinking. If not, we set static state here.
        if !self.recording {
            if self.armed {
                ctx.lights.set_button(Buttons::Rec, Brightness::Bright);
            } else {
                ctx.lights.set_button(Buttons::Rec, Brightness::Dim); // Dim when idle
            }
        }

        // Other Transport Buttons Logic:
        if self.loop_duration == Duration::ZERO {
            // NO LOOP STORED: Everything else OFF
            ctx.lights.set_button(Buttons::Play, Brightness::Off);
            ctx.lights.set_button(Buttons::Stop, Brightness::Off);
            ctx.lights.set_button(Buttons::Restart, Brightness::Off);
            ctx.lights.set_button(Buttons::Erase, Brightness::Off);
        } else {
            // LOOP STORED: Standard Dim/Bright logic

            // Play
            if self.playing {
                ctx.lights.set_button(Buttons::Play, Brightness::Bright);
            } else {
                ctx.lights.set_button(Buttons::Play, Brightness::Dim);
            }

            // Stop
            if !self.playing {
                ctx.lights.set_button(Buttons::Stop, Brightness::Bright);
            } else {
                ctx.lights.set_button(Buttons::Stop, Brightness::Dim);
            }

            // Restart
            if self.is_restart_pressed {
                ctx.lights.set_button(Buttons::Restart, Brightness::Bright);
            } else {
                 ctx.lights.set_button(Buttons::Restart, Brightness::Dim);
            }
            
            // Erase
            if self.is_erase_pressed {
                ctx.lights.set_button(Buttons::Erase, Brightness::Bright);
            } else {
                ctx.lights.set_button(Buttons::Erase, Brightness::Dim);
            }
        }
    }
    
    fn clear_all(&mut self, ctx: &mut DriverContext) {
        self.playing = false;
        self.recording = false;
        self.armed = false;
        self.start_time = None;
        self.playback_start = None;
        self.paused_position = None;
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
                match index {
                    Buttons::Rec => {
                        if *pressed {
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
                        }
                    },
                    Buttons::Play => {
                        if *pressed {
                            if self.recording && self.loop_duration == Duration::ZERO {
                                // Finish Initial Rec -> Play
                                if let Some(start) = self.start_time {
                                    self.loop_duration = Instant::now().duration_since(start);
                                }
                                self.recording = false;
                                self.playing = true;
                                self.playback_start = Some(Instant::now());
                                self.paused_position = None;
                            } else if self.playing {
                                // PAUSE
                                self.playing = false;
                                self.recording = false; // Stop recording if we pause
                                
                                // Calculate where we paused relative to loop start
                                if let Some(start) = self.playback_start {
                                    let elapsed = Instant::now().duration_since(start);
                                    let pos = if self.loop_duration > Duration::ZERO {
                                        let millis = elapsed.as_millis() % self.loop_duration.as_millis();
                                        Duration::from_millis(millis as u64)
                                    } else {
                                        Duration::ZERO
                                    };
                                    self.paused_position = Some(pos);
                                }
                                
                                // Turn off sequencer lights as we paused
                                self.seq_holding = [false; 16];
                                for i in 0..16 {
                                    self.update_pad_light(ctx, i);
                                }
                            } else if self.loop_duration > Duration::ZERO {
                                // RESUME
                                self.playing = true;
                                
                                let offset = self.paused_position.unwrap_or(Duration::ZERO);
                                // Set playback start in the past so that (now - start) == offset
                                self.playback_start = Some(Instant::now() - offset);
                                
                                // Re-sync cursor
                                self.playback_cursor = 0;
                                for (i, event) in self.events.iter().enumerate() {
                                    // We look for the first event that hasn't happened yet relative to offset
                                    if event.offset > offset {
                                        self.playback_cursor = i;
                                        break;
                                    }
                                    // Handle exact match if necessary, mostly covered by loop logic
                                    if event.offset == offset {
                                        self.playback_cursor = i;
                                        break;
                                    }
                                    // If we are past the event, move cursor forward
                                    self.playback_cursor = i + 1;
                                }
                            }
                        }
                    },
                    Buttons::Stop => {
                        if *pressed {
                             self.playing = false;
                             self.recording = false;
                             self.armed = false;
                             
                             // Reset position to Start
                             self.paused_position = Some(Duration::ZERO);
                             self.playback_cursor = 0;
                             
                             self.seq_holding = [false; 16];
                             for i in 0..16 {
                                self.update_pad_light(ctx, i);
                             }
                        }
                    },
                    Buttons::Restart => {
                        self.is_restart_pressed = *pressed;
                        if *pressed {
                            // Restart Loop logic
                            if self.playing {
                                self.playback_start = Some(Instant::now());
                                self.playback_cursor = 0;
                            }
                            // Reset position regardless
                            self.paused_position = Some(Duration::ZERO);
                            if !self.playing {
                                self.playback_cursor = 0;
                            }
                        }
                    },
                    Buttons::Erase => {
                        self.is_erase_pressed = *pressed;
                        if *pressed {
                            self.clear_all(ctx);
                        }
                    },
                    _ => {}
                }
                self.update_transport_lights(ctx);
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
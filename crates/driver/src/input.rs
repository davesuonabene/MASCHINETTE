use maschine_library::controls::{Buttons, PadEventType};

#[derive(Debug, Clone)]
pub enum HardwareEvent {
    Button { index: Buttons, pressed: bool },
    Pad { index: usize, event_type: PadEventType, value: u16 },
    Encoder { value: u8 },
    Slider { value: u8 },
}

/// Parses the raw HID report buffer into a vector of high-level events.
pub fn parse_hid_report(buf: &[u8]) -> Vec<HardwareEvent> {
    let mut events = Vec::new();

    if buf.is_empty() {
        return events;
    }

    if buf[0] == 0x01 {
        // --- BUTTONS (Bytes 1-6) ---
        // We iterate through all mapped buttons to check their state in the report.
        for i in 0..6 {
            if i + 1 >= buf.len() { break; }
            for j in 0..8 {
                let idx = i * 8 + j;
                
                // Convert index to Button Enum
                if let Some(button) = num::FromPrimitive::from_usize(idx) {
                    // Skip EncoderTouch if preferred, otherwise include it.
                    // (Matches original logic which skipped it, but we can emit it and ignore later)
                    if button == Buttons::EncoderTouch { continue; }

                    let pressed = (buf[i + 1] & (1 << j)) > 0;
                    events.push(HardwareEvent::Button { index: button, pressed });
                }
            }
        }

        // --- ENCODER (Byte 7) ---
        if buf.len() > 7 {
            events.push(HardwareEvent::Encoder { value: buf[7] });
        }

        // --- SLIDER (Byte 10) ---
        if buf.len() > 10 {
            events.push(HardwareEvent::Slider { value: buf[10] });
        }

    } else if buf[0] == 0x02 {
        // --- PADS ---
        // Pad reports are variable length, stepping by 3 bytes per event.
        for i in (1..buf.len()).step_by(3) {
            if i + 2 >= buf.len() { break; }
            
            let idx = buf[i] as usize;
            let evt_byte = buf[i + 1] & 0xf0;
            let val = ((buf[i + 1] as u16 & 0x0f) << 8) + buf[i + 2] as u16;

            // Check for empty/end of report
            if i > 1 && idx == 0 && evt_byte == 0 && val == 0 { break; }

            if let Some(pad_evt) = num::FromPrimitive::from_u8(evt_byte) {
                events.push(HardwareEvent::Pad {
                    index: idx,
                    event_type: pad_evt,
                    value: val,
                });
            }
        }
    }

    events
}
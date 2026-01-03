use maschine_library::lights::Lights;
use midir::MidiOutputConnection;
use std::net::{SocketAddr, UdpSocket};
use crate::settings::Settings;

/// Holds references to the shared resources needed by the driver modes.
pub struct DriverContext<'a> {
    pub lights: &'a mut Lights,
    pub midi_port: &'a mut MidiOutputConnection,
    pub osc_socket: &'a UdpSocket,
    pub osc_addr: &'a SocketAddr,
    pub settings: &'a Settings,
}
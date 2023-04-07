use byteorder::{ByteOrder, LittleEndian};
use color_eyre::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

use crate::lifx::to_lifx_state;
use crate::mqtt_device::MqttDevice;

pub const MAX_UDP_PACKET_SIZE: usize = 1 << 16;
pub const LIFX_UDP_PORT: u16 = 56700;

#[derive(Clone)]
pub struct LifxSocket {
    pub udp_socket: Arc<UdpSocket>,
}

impl LifxSocket {
    pub async fn init() -> Result<Self> {
        // Setup the UDP socket. LIFX uses port 56700.
        let addr: SocketAddr = format!("0.0.0.0:{}", LIFX_UDP_PORT).parse()?;
        let udp_socket: UdpSocket = UdpSocket::bind(addr).await?;
        let udp_socket = Arc::new(udp_socket);

        Ok(LifxSocket { udp_socket })
    }

    pub async fn send(&self, addr: &SocketAddr, lifx_msg: LifxMsg) -> Result<()> {
        let buf = mk_lifx_udp_msg(lifx_msg);
        self.udp_socket.send_to(&buf, &addr).await?;
        Ok(())
    }

    pub async fn send_state_to_lifx(
        &self,
        addr: SocketAddr,
        mqtt_device: &MqttDevice,
    ) -> Result<()> {
        let lifx_state = to_lifx_state(&addr, mqtt_device)?;

        self.send(&addr, LifxMsg::SetPower(lifx_state.clone()))
            .await?;
        self.send(&addr, LifxMsg::SetColor(lifx_state)).await?;

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LifxState {
    pub hue: u16,
    pub sat: u16,
    pub bri: u16,
    pub power: u16,
    pub label: String,
    pub addr: SocketAddr,
    pub transition: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum LifxMsg {
    Get(SocketAddr),
    SetColor(LifxState),
    State(LifxState),
    SetPower(LifxState),
    Unknown,
}

pub fn lifx_msg_type_to_u16(msg_type: LifxMsg) -> u16 {
    match msg_type {
        LifxMsg::Get(_) => 101,
        LifxMsg::SetColor(_) => 102,
        LifxMsg::State(_) => 107,
        LifxMsg::SetPower(_) => 117,
        LifxMsg::Unknown => panic!("Cannot convert LifxMsg::Unknown to u16"),
    }
}

fn mk_lifx_msg_payload(lifx_msg: LifxMsg) -> Option<Vec<u8>> {
    // TODO: might have to do some trickery here with comparing to a device's
    // old state to figure out whether we should apply transitions only to
    // SetPower or SetColor message.
    //
    // Currently simultaneously powering on and switching a light's color will
    // first transition the light to its old state, then transition from the old
    // state to the desired state.
    match lifx_msg {
        LifxMsg::SetPower(state) => {
            let mut buf: [u8; 16 + 32] = [0; 16 + 32];

            LittleEndian::write_u16(&mut buf, state.power);

            if let Some(t) = state.transition {
                LittleEndian::write_u32(&mut buf[2..], t)
            }

            Some(buf.to_vec())
        }
        LifxMsg::SetColor(state) => {
            let mut buf: [u8; 8 + 16 * 4 + 32] = [0; 8 + 16 * 4 + 32];

            LittleEndian::write_u16(&mut buf[1..], state.hue);
            LittleEndian::write_u16(&mut buf[3..], state.sat);
            LittleEndian::write_u16(&mut buf[5..], state.bri);
            LittleEndian::write_u16(&mut buf[7..], 6500); // lifx requires this weird color temperature parameter?

            let t = state.transition.unwrap_or(500);
            LittleEndian::write_u32(&mut buf[9..], t);

            Some(buf.to_vec())
        }
        _ => None,
    }
}

pub fn mk_lifx_udp_msg(lifx_msg: LifxMsg) -> Vec<u8> {
    // frame
    // https://lan.developer.lifx.com/docs/header-description#frame
    let mut frame: [u8; 8] = [0; 8];
    let protocol = 1024;
    let origin = 0;
    let tagged = 1;
    let addressable = 1;

    LittleEndian::write_u16(&mut frame, 0); // size to be filled in later
    LittleEndian::write_u16(
        &mut frame[2..],
        protocol | (origin << 14) | (tagged << 13) | (addressable << 12),
    );
    LittleEndian::write_u16(&mut frame[1..], 4);

    // frame address
    // https://lan.developer.lifx.com/docs/header-description#frame-address
    let mut frame_address: [u8; 16] = [0; 16];
    let ack_required = 0;
    let res_required = match lifx_msg {
        LifxMsg::Get(_) => 1,
        _ => 0,
    };

    frame_address[14] = (ack_required << 1) | res_required;

    // protocol header
    // https://lan.developer.lifx.com/docs/header-description#protocol-header
    let mut protocol_header: [u8; 12] = [0; 12];
    let msg_type = lifx_msg_type_to_u16(lifx_msg.clone());
    LittleEndian::write_u16(&mut protocol_header[8..], msg_type);

    let payload = mk_lifx_msg_payload(lifx_msg);
    let payload_size = payload.clone().map(|p| p.len()).unwrap_or(0);
    let msg_size = frame.len() + frame_address.len() + protocol_header.len() + payload_size;

    // we now know the total size - write it into the beginning of the frame header
    LittleEndian::write_u16(&mut frame, msg_size as u16);

    let mut msg: Vec<u8> = vec![];
    msg.append(&mut frame.to_vec());
    msg.append(&mut frame_address.to_vec());
    msg.append(&mut protocol_header.to_vec());

    if let Some(payload) = payload {
        msg.append(&mut payload.to_vec());
    };

    msg
}

pub fn read_lifx_msg(buf: &[u8], addr: SocketAddr) -> LifxMsg {
    let msg_type = LittleEndian::read_u16(&buf[32..]);
    let payload = &buf[36..];

    match msg_type {
        107 => {
            // State (107) message, response to Get (101)
            // https://lan.developer.lifx.com/docs/light-messages#section-state-107

            let hue = LittleEndian::read_u16(payload);
            let sat = LittleEndian::read_u16(&payload[2..]);
            let bri = LittleEndian::read_u16(&payload[4..]);

            let power = LittleEndian::read_u16(&payload[10..]);

            let label = std::str::from_utf8(&payload[12..(12 + 32)])
                .unwrap_or("Unknown")
                .to_owned()
                .replace('\0', "");

            let state = LifxState {
                hue,
                sat,
                bri,
                power,
                label,
                addr,
                transition: None,
            };

            LifxMsg::State(state)
        }
        _ => LifxMsg::Unknown,
    }
}

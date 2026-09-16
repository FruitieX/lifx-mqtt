use byteorder::{ByteOrder, LittleEndian};
use color_eyre::Result;
use std::sync::Arc;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU8, Ordering},
};
use tokio::net::UdpSocket;

use crate::lifx::to_lifx_state;
use crate::mqtt_device::MqttDevice;

pub const MAX_UDP_PACKET_SIZE: usize = 1 << 16;
pub const LIFX_UDP_PORT: u16 = 56700;

static NEXT_SEQUENCE: AtomicU8 = AtomicU8::new(0);

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
        current_state: Option<&MqttDevice>,
    ) -> Result<()> {
        // MQTT commands are partial updates: an omitted field must not reset
        // the corresponding value on the bulb. Use the last state reported by
        // the bulb to fill in values needed by SetColor.
        let effective_device = MqttDevice {
            id: mqtt_device.id.clone(),
            name: if mqtt_device.name.is_empty() {
                current_state
                    .map(|state| state.name.clone())
                    .unwrap_or_default()
            } else {
                mqtt_device.name.clone()
            },
            power: mqtt_device
                .power
                .or_else(|| current_state.and_then(|state| state.power)),
            brightness: mqtt_device
                .brightness
                .or_else(|| current_state.and_then(|state| state.brightness)),
            color: mqtt_device
                .color
                .clone()
                .or_else(|| current_state.and_then(|state| state.color.clone())),
            transition_ms: mqtt_device.transition_ms,
            sensor_value: mqtt_device
                .sensor_value
                .clone()
                .or_else(|| current_state.and_then(|state| state.sensor_value.clone())),
            capabilities: mqtt_device
                .capabilities
                .clone()
                .or_else(|| current_state.and_then(|state| state.capabilities.clone())),
        };

        if mqtt_device.power.is_some() {
            self.send(
                &addr,
                LifxMsg::SetPower(to_lifx_state(&addr, &effective_device)?),
            )
            .await?;
        }

        if mqtt_device.color.is_some() || mqtt_device.brightness.is_some() {
            self.send(
                &addr,
                LifxMsg::SetColor(to_lifx_state(&addr, &effective_device)?),
            )
            .await?;
        }

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
    Get,
    SetColor(LifxState),
    State(LifxState),
    SetPower(LifxState),
    Unknown,
}

pub fn lifx_msg_type_to_u16(msg_type: LifxMsg) -> u16 {
    match msg_type {
        LifxMsg::Get => 101,
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
            // level (u16) + duration (u32)
            let mut buf: [u8; 6] = [0; 6];

            LittleEndian::write_u16(&mut buf[..2], state.power);

            if let Some(t) = state.transition {
                LittleEndian::write_u32(&mut buf[2..6], t)
            }

            Some(buf.to_vec())
        }
        LifxMsg::SetColor(state) => {
            // reserved (u8) + HSBK (4 * u16) + duration (u32)
            let mut buf: [u8; 13] = [0; 13];

            LittleEndian::write_u16(&mut buf[1..3], state.hue);
            LittleEndian::write_u16(&mut buf[3..5], state.sat);
            LittleEndian::write_u16(&mut buf[5..7], state.bri);
            LittleEndian::write_u16(&mut buf[7..9], 6500); // Kelvin is required by HSBK.

            let t = state.transition.unwrap_or(500);
            LittleEndian::write_u32(&mut buf[9..13], t);

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
    // Tagged messages are broadcasts. These packets are sent to one bulb's
    // UDP address, so they must be untagged.
    let tagged = 0;
    let addressable = 1;

    LittleEndian::write_u16(&mut frame, 0); // size to be filled in later
    LittleEndian::write_u16(
        &mut frame[2..],
        protocol | (origin << 14) | (tagged << 13) | (addressable << 12),
    );
    // A non-zero source is required by older LIFX firmware. The sequence lets
    // responses be associated with individual requests.
    LittleEndian::write_u32(&mut frame[4..8], 0x4c46_584d);

    // frame address
    // https://lan.developer.lifx.com/docs/header-description#frame-address
    let mut frame_address: [u8; 16] = [0; 16];
    let ack_required: u8 = if matches!(lifx_msg, LifxMsg::Get) {
        0
    } else {
        1
    };
    let res_required: u8 = match lifx_msg {
        LifxMsg::Get => 1,
        _ => 0,
    };

    frame_address[14] = (ack_required << 1) | res_required;
    frame_address[15] = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);

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
    if buf.len() < 36 {
        return LifxMsg::Unknown;
    }

    let declared_size = LittleEndian::read_u16(&buf[..2]) as usize;
    if declared_size < 36 || declared_size > buf.len() {
        return LifxMsg::Unknown;
    }

    let msg_type = LittleEndian::read_u16(&buf[32..]);
    let payload = &buf[36..];

    match msg_type {
        107 => {
            // State (107) message, response to Get (101)
            // https://lan.developer.lifx.com/docs/light-messages#section-state-107

            if payload.len() < 44 {
                return LifxMsg::Unknown;
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> LifxState {
        LifxState {
            hue: 100,
            sat: 200,
            bri: 300,
            power: 400,
            label: String::new(),
            addr: "127.0.0.1:56700".parse().unwrap(),
            transition: Some(500),
        }
    }

    #[test]
    fn set_power_packet_has_protocol_payload_size_and_ack() {
        let packet = mk_lifx_udp_msg(LifxMsg::SetPower(state()));

        assert_eq!(packet.len(), 42);
        assert_eq!(LittleEndian::read_u16(&packet[..2]), 42);
        assert_eq!(LittleEndian::read_u16(&packet[2..4]), 1024 | (1 << 12));
        assert_ne!(LittleEndian::read_u32(&packet[4..8]), 0);
        assert_eq!(packet[22] & (1 << 1), 1 << 1);
        assert_eq!(LittleEndian::read_u16(&packet[32..34]), 117);
        assert_eq!(LittleEndian::read_u16(&packet[36..38]), 400);
        assert_eq!(LittleEndian::read_u32(&packet[38..42]), 500);
    }

    #[test]
    fn set_color_packet_has_protocol_payload_size() {
        let packet = mk_lifx_udp_msg(LifxMsg::SetColor(state()));

        assert_eq!(packet.len(), 49);
        assert_eq!(LittleEndian::read_u16(&packet[..2]), 49);
        assert_eq!(LittleEndian::read_u16(&packet[32..34]), 102);
        assert_eq!(packet[36], 0);
        assert_eq!(LittleEndian::read_u16(&packet[37..39]), 100);
        assert_eq!(LittleEndian::read_u16(&packet[39..41]), 200);
        assert_eq!(LittleEndian::read_u16(&packet[41..43]), 300);
        assert_eq!(LittleEndian::read_u16(&packet[43..45]), 6500);
        assert_eq!(LittleEndian::read_u32(&packet[45..49]), 500);
    }

    #[test]
    fn malformed_packets_are_ignored() {
        let addr = "127.0.0.1:56700".parse().unwrap();

        assert!(matches!(read_lifx_msg(&[], addr), LifxMsg::Unknown));
        assert!(matches!(
            read_lifx_msg(&[36, 0, 0, 0], addr),
            LifxMsg::Unknown
        ));
    }
}

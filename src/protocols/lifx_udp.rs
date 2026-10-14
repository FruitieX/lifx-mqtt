use byteorder::{ByteOrder, LittleEndian};
use color_eyre::Result;
use eyre::eyre;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{net::UdpSocket, sync::oneshot, time};

pub const MAX_UDP_PACKET_SIZE: usize = 1 << 16;
pub const LIFX_UDP_PORT: u16 = 56700;

/// How long to wait for a bulb to acknowledge a message before resending it.
const ACK_TIMEOUT: Duration = Duration::from_millis(250);
const SEND_ATTEMPTS: usize = 4;

static NEXT_SEQUENCE: AtomicU8 = AtomicU8::new(0);

type PendingAcks = HashMap<(IpAddr, u8), oneshot::Sender<()>>;

#[derive(Clone)]
pub struct LifxSocket {
    pub udp_socket: Arc<UdpSocket>,
    pending_acks: Arc<Mutex<PendingAcks>>,
}

impl LifxSocket {
    pub async fn init() -> Result<Self> {
        // Setup the UDP socket. LIFX uses port 56700.
        let addr: SocketAddr = format!("0.0.0.0:{}", LIFX_UDP_PORT).parse()?;
        let udp_socket: UdpSocket = UdpSocket::bind(addr).await?;
        let udp_socket = Arc::new(udp_socket);

        Ok(LifxSocket {
            udp_socket,
            pending_acks: Default::default(),
        })
    }

    /// Sends a message without waiting for any kind of response.
    pub async fn send(&self, addr: &SocketAddr, lifx_msg: LifxMsg) -> Result<()> {
        let buf = mk_lifx_udp_msg(lifx_msg, next_sequence());
        self.udp_socket.send_to(&buf, addr).await?;
        Ok(())
    }

    /// Sends a message and waits for the bulb to acknowledge it, resending a
    /// few times if needed. UDP over WiFi does drop packets, and without this
    /// a lost packet means a command that silently never happened.
    pub async fn send_acked(&self, addr: &SocketAddr, lifx_msg: LifxMsg) -> Result<()> {
        let sequence = next_sequence();
        let key = (addr.ip(), sequence);
        let buf = mk_lifx_udp_msg(lifx_msg, sequence);

        let (tx, mut rx) = oneshot::channel();
        self.pending_acks.lock().unwrap().insert(key, tx);

        let result = async {
            for _ in 0..SEND_ATTEMPTS {
                self.udp_socket.send_to(&buf, addr).await?;

                match time::timeout(ACK_TIMEOUT, &mut rx).await {
                    Ok(Ok(())) => return Ok(()),
                    // Sender replaced by another message reusing the same
                    // sequence number, the ack can no longer be told apart.
                    Ok(Err(_)) => break,
                    Err(_) => {}
                }
            }

            Err(eyre!(
                "No acknowledgement from {} after {} attempts",
                addr,
                SEND_ATTEMPTS
            ))
        }
        .await;

        self.pending_acks.lock().unwrap().remove(&key);

        result
    }

    pub fn handle_ack(&self, addr: SocketAddr, sequence: u8) {
        if let Some(tx) = self
            .pending_acks
            .lock()
            .unwrap()
            .remove(&(addr.ip(), sequence))
        {
            tx.send(()).ok();
        }
    }
}

fn next_sequence() -> u8 {
    NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
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
    Acknowledgement { addr: SocketAddr, sequence: u8 },
    Unknown,
}

pub fn lifx_msg_type_to_u16(msg_type: &LifxMsg) -> u16 {
    match msg_type {
        LifxMsg::Acknowledgement { .. } => 45,
        LifxMsg::Get => 101,
        LifxMsg::SetColor(_) => 102,
        LifxMsg::State(_) => 107,
        LifxMsg::SetPower(_) => 117,
        LifxMsg::Unknown => panic!("Cannot convert LifxMsg::Unknown to u16"),
    }
}

fn mk_lifx_msg_payload(lifx_msg: LifxMsg) -> Option<Vec<u8>> {
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

pub fn mk_lifx_udp_msg(lifx_msg: LifxMsg, sequence: u8) -> Vec<u8> {
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
    let ack_required: u8 = match lifx_msg {
        LifxMsg::SetColor(_) | LifxMsg::SetPower(_) => 1,
        _ => 0,
    };
    let res_required: u8 = match lifx_msg {
        LifxMsg::Get => 1,
        _ => 0,
    };

    frame_address[14] = (ack_required << 1) | res_required;
    frame_address[15] = sequence;

    // protocol header
    // https://lan.developer.lifx.com/docs/header-description#protocol-header
    let mut protocol_header: [u8; 12] = [0; 12];
    let msg_type = lifx_msg_type_to_u16(&lifx_msg);
    LittleEndian::write_u16(&mut protocol_header[8..], msg_type);

    let payload = mk_lifx_msg_payload(lifx_msg).unwrap_or_default();
    let msg_size = frame.len() + frame_address.len() + protocol_header.len() + payload.len();

    // we now know the total size - write it into the beginning of the frame header
    LittleEndian::write_u16(&mut frame, msg_size as u16);

    let mut msg: Vec<u8> = Vec::with_capacity(msg_size);
    msg.extend_from_slice(&frame);
    msg.extend_from_slice(&frame_address);
    msg.extend_from_slice(&protocol_header);
    msg.extend_from_slice(&payload);

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

    let sequence = buf[23];
    let msg_type = LittleEndian::read_u16(&buf[32..]);
    let payload = &buf[36..declared_size];

    match msg_type {
        // https://lan.developer.lifx.com/docs/information-messages#acknowledgement---packet-45
        45 => LifxMsg::Acknowledgement { addr, sequence },
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

            let label = String::from_utf8_lossy(&payload[12..(12 + 32)]).replace('\0', "");

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
        let packet = mk_lifx_udp_msg(LifxMsg::SetPower(state()), 42);

        assert_eq!(packet.len(), 42);
        assert_eq!(LittleEndian::read_u16(&packet[..2]), 42);
        assert_eq!(LittleEndian::read_u16(&packet[2..4]), 1024 | (1 << 12));
        assert_ne!(LittleEndian::read_u32(&packet[4..8]), 0);
        assert_eq!(packet[22] & (1 << 1), 1 << 1);
        assert_eq!(packet[23], 42);
        assert_eq!(LittleEndian::read_u16(&packet[32..34]), 117);
        assert_eq!(LittleEndian::read_u16(&packet[36..38]), 400);
        assert_eq!(LittleEndian::read_u32(&packet[38..42]), 500);
    }

    #[test]
    fn set_color_packet_has_protocol_payload_size() {
        let packet = mk_lifx_udp_msg(LifxMsg::SetColor(state()), 0);

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
    fn get_packet_requests_response_without_ack() {
        let packet = mk_lifx_udp_msg(LifxMsg::Get, 7);

        assert_eq!(packet.len(), 36);
        assert_eq!(packet[22], 1);
        assert_eq!(packet[23], 7);
        assert_eq!(LittleEndian::read_u16(&packet[32..34]), 101);
    }

    #[test]
    fn acknowledgement_is_parsed_with_sequence() {
        let addr: SocketAddr = "127.0.0.1:56700".parse().unwrap();
        let packet = mk_lifx_udp_msg(LifxMsg::Acknowledgement { addr, sequence: 0 }, 123);

        assert!(matches!(
            read_lifx_msg(&packet, addr),
            LifxMsg::Acknowledgement { sequence: 123, .. }
        ));
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

    #[tokio::test]
    async fn send_acked_retries_until_acknowledged() {
        let bulb = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bulb_addr = bulb.local_addr().unwrap();
        let socket = LifxSocket {
            udp_socket: Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
            pending_acks: Default::default(),
        };

        let sender = socket.clone();
        let send = tokio::spawn(async move {
            sender
                .send_acked(&bulb_addr, LifxMsg::SetPower(state()))
                .await
        });

        // Ignore the first packet to simulate packet loss, ack the resend.
        let mut buf = [0; 128];
        bulb.recv_from(&mut buf).await.unwrap();
        bulb.recv_from(&mut buf).await.unwrap();
        socket.handle_ack(bulb_addr, buf[23]);

        send.await.unwrap().unwrap();
        assert!(socket.pending_acks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn send_acked_gives_up_eventually() {
        let bulb = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = LifxSocket {
            udp_socket: Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
            pending_acks: Default::default(),
        };

        let result = socket
            .send_acked(&bulb.local_addr().unwrap(), LifxMsg::SetPower(state()))
            .await;

        assert!(result.is_err());
        assert!(socket.pending_acks.lock().unwrap().is_empty());
    }
}

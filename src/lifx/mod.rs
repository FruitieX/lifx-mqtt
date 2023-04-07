use std::{net::SocketAddr, time::Duration};

use color_eyre::Result;
use palette::Hsv;
use tokio::time;

use crate::{
    mqtt_device::MqttDevice,
    protocols::{
        lifx_udp::{
            read_lifx_msg, LifxMsg, LifxSocket, LifxState, LIFX_UDP_PORT, MAX_UDP_PACKET_SIZE,
        },
        mqtt::{publish_mqtt_device, MqttClient},
    },
    settings::Settings,
};

pub fn from_lifx_state(settings: &Settings, lifx_state: LifxState) -> Result<MqttDevice> {
    let ip = lifx_state.addr.ip().to_string();

    let device_settings = settings
        .devices
        .iter()
        .find(|(_, device)| device.ip == ip)
        .ok_or_else(|| eyre::eyre!("Could not find device settings by IP for {}", ip))?;

    let id = device_settings.0.clone();

    let hue = (f32::from(lifx_state.hue) / 65535.0) * 360.0;
    let sat = f32::from(lifx_state.sat) / 65535.0;
    let bri = f32::from(lifx_state.bri) / 65535.0;

    let power = lifx_state.power == 65535;

    let color = Hsv::new(hue, sat, bri);

    let transition_ms = lifx_state.transition.map(|transition| transition as f32);

    let mqtt_device = MqttDevice {
        id,
        name: lifx_state.label,
        power: Some(power),
        color: Some(color),
        brightness: Some(bri),
        transition_ms,
        cct: None,
        sensor_value: None,
    };

    Ok(mqtt_device)
}

pub fn to_lifx_state(addr: &SocketAddr, device: &MqttDevice) -> Result<LifxState> {
    let power = if device.power == Some(true) { 65535 } else { 0 };
    let transition = device
        .transition_ms
        .map(|transition_ms| transition_ms as u32);

    match device.color {
        Some(color) => {
            let hue = ((color.hue.to_positive_degrees() / 360.0) * 65535.0).floor() as u16;
            let sat = (color.saturation * 65535.0).floor() as u16;
            let bri = (device.brightness.unwrap_or(1.0) * color.value * 65535.0).floor() as u16;

            Ok(LifxState {
                hue,
                sat,
                bri,
                power,
                label: device.name.clone(),
                addr: *addr,
                transition,
            })
        }
        None => Ok(LifxState {
            hue: 0,
            sat: 0,
            bri: 0,
            power,
            label: device.name.clone(),
            addr: *addr,
            transition,
        }),
    }
}

pub fn start_light_polling_loop(settings: &Settings, lifx_socket: &LifxSocket) {
    let settings = settings.clone();
    let lifx_socket = lifx_socket.clone();

    tokio::spawn(async move {
        let poll_rate = Duration::from_millis(1000);
        let mut interval = time::interval(poll_rate);

        loop {
            interval.tick().await;

            for device in settings.devices.values() {
                let addr: SocketAddr = format!("{}:{}", device.ip, LIFX_UDP_PORT)
                    .parse()
                    .unwrap_or_else(|e| {
                        panic!(
                            "Error: {}, Expected to be able to parse IP address {} for device {}",
                            e, device.ip, device.name
                        )
                    });
                let msg = LifxMsg::Get(addr);

                lifx_socket
                    .send(&addr, msg)
                    .await
                    .map_err(|e| eprintln!("Error in lifx_socket.send {}", e))
                    .ok();
            }
        }
    });
}

async fn handle_incoming_lifx_msg(
    mqtt_client: &MqttClient,
    settings: &Settings,
    msg: LifxMsg,
) -> Result<()> {
    if let LifxMsg::State(state) = msg {
        let mqtt_device = from_lifx_state(settings, state)?;
        publish_mqtt_device(mqtt_client, settings, &mqtt_device).await?;
    }

    Ok(())
}

pub fn start_udp_receiver_loop(
    mqtt_client: &MqttClient,
    settings: &Settings,
    lifx_socket: &LifxSocket,
) {
    let mqtt_client = mqtt_client.clone();
    let settings = settings.clone();
    let lifx_socket = lifx_socket.clone();

    tokio::spawn(async move {
        let mut buf: [u8; MAX_UDP_PACKET_SIZE] = [0; MAX_UDP_PACKET_SIZE];

        loop {
            let res = lifx_socket.udp_socket.recv_from(&mut buf).await;

            let res = match res {
                // FIXME: should probably do some sanity checks on bytes_read
                Ok((_bytes_read, addr)) => {
                    let msg = read_lifx_msg(&buf, addr);
                    handle_incoming_lifx_msg(&mqtt_client, &settings, msg).await
                }
                Err(e) => Err(e.into()),
            };

            if let Err(e) = res {
                eprintln!("Error while handling incoming lifx msg {}", e);
            }
        }
    });
}

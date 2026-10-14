use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use color_eyre::Result;
use tokio::{
    sync::mpsc::{self, Receiver},
    task::JoinHandle,
    time,
};

use crate::{
    mqtt_device::{Capabilities, DeviceColor, Hs, MqttDevice},
    protocols::{
        lifx_udp::{read_lifx_msg, LifxMsg, LifxSocket, LifxState, MAX_UDP_PACKET_SIZE},
        mqtt::MqttClient,
    },
    settings::Settings,
};

const POLL_RATE: Duration = Duration::from_secs(1);

/// Log a light as unresponsive after this long without a state reply.
const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(10);

struct KnownState {
    device: MqttDevice,
    last_seen: Instant,
}

/// Last known state of each light, keyed by device id.
#[derive(Clone, Default)]
pub struct Lights(Arc<Mutex<HashMap<String, KnownState>>>);

impl Lights {
    fn get(&self, id: &str) -> Option<MqttDevice> {
        self.0.lock().unwrap().get(id).map(|s| s.device.clone())
    }

    /// Records a state reported by the bulb itself.
    fn reported(&self, device: MqttDevice) {
        self.0.lock().unwrap().insert(
            device.id.clone(),
            KnownState {
                device,
                last_seen: Instant::now(),
            },
        );
    }

    /// Records a state the bulb has acknowledged but not reported yet, so
    /// that partial commands arriving before the next poll merge correctly.
    fn commanded(&self, device: MqttDevice) {
        if let Some(state) = self.0.lock().unwrap().get_mut(&device.id) {
            state.device = device;
        }
    }

    fn last_seen(&self, id: &str) -> Option<Instant> {
        self.0.lock().unwrap().get(id).map(|s| s.last_seen)
    }
}

pub fn from_lifx_state(settings: &Settings, lifx_state: LifxState) -> Result<MqttDevice> {
    let ip = lifx_state.addr.ip().to_string();

    let device_settings = settings
        .devices
        .iter()
        .find(|(_, device)| device.ip == ip)
        .ok_or_else(|| eyre::eyre!("Could not find device settings by IP for {}", ip))?;

    let id = device_settings.0.clone();

    let h = (f32::from(lifx_state.hue) / 65535.0) * 360.0;
    let s = f32::from(lifx_state.sat) / 65535.0;
    let bri = f32::from(lifx_state.bri) / 65535.0;

    let power = lifx_state.power == 65535;

    let color = DeviceColor::Hs(Hs { h: h as u16, s });

    let transition_ms = lifx_state.transition.map(|transition| transition as f32);

    let mqtt_device = MqttDevice {
        id,
        name: lifx_state.label,
        power: Some(power),
        color: Some(color),
        brightness: Some(bri),
        transition_ms,
        sensor_value: None,
        capabilities: Some(Capabilities::default()),
    };

    Ok(mqtt_device)
}

pub fn to_lifx_state(addr: &SocketAddr, device: &MqttDevice) -> LifxState {
    let power = if device.power == Some(true) { 65535 } else { 0 };
    let transition = device
        .transition_ms
        .map(|transition_ms| transition_ms as u32);

    let (hue, sat) = match device.color {
        Some(DeviceColor::Hs(ref color)) => (
            ((color.h as f32 / 360.0) * 65535.0).floor() as u16,
            (color.s * 65535.0).floor() as u16,
        ),
        None => (0, 0),
    };
    let bri = (device.brightness.unwrap_or(1.0) * 65535.0).floor() as u16;

    LifxState {
        hue,
        sat,
        bri,
        power,
        label: device.name.clone(),
        addr: *addr,
        transition,
    }
}

/// MQTT commands are partial updates: an omitted field must not reset the
/// corresponding value on the bulb. Fill in the gaps from the last known state.
fn merge_command(command: &MqttDevice, current: Option<&MqttDevice>) -> MqttDevice {
    MqttDevice {
        id: command.id.clone(),
        name: if command.name.is_empty() {
            current.map(|state| state.name.clone()).unwrap_or_default()
        } else {
            command.name.clone()
        },
        power: command.power.or_else(|| current.and_then(|s| s.power)),
        brightness: command
            .brightness
            .or_else(|| current.and_then(|s| s.brightness)),
        color: command
            .color
            .clone()
            .or_else(|| current.and_then(|s| s.color.clone())),
        transition_ms: command.transition_ms,
        sensor_value: command
            .sensor_value
            .clone()
            .or_else(|| current.and_then(|s| s.sensor_value.clone())),
        capabilities: command
            .capabilities
            .clone()
            .or_else(|| current.and_then(|s| s.capabilities.clone())),
    }
}

/// The messages needed to apply a command, in the order they should be sent.
fn command_messages(
    addr: &SocketAddr,
    command: &MqttDevice,
    current: Option<&MqttDevice>,
    effective: &MqttDevice,
) -> Vec<LifxMsg> {
    let state = to_lifx_state(addr, effective);
    let set_power = command.power.is_some();
    let set_color = command.color.is_some() || command.brightness.is_some();
    let turning_on =
        command.power == Some(true) && current.and_then(|state| state.power) != Some(true);

    match (set_power, set_color) {
        // Change the color while the bulb is still off so that it doesn't
        // first fade in to its old color.
        (true, true) if turning_on => vec![
            LifxMsg::SetColor(LifxState {
                transition: Some(0),
                ..state.clone()
            }),
            LifxMsg::SetPower(state),
        ],
        (true, true) => vec![LifxMsg::SetPower(state.clone()), LifxMsg::SetColor(state)],
        (true, false) => vec![LifxMsg::SetPower(state)],
        (false, true) => vec![LifxMsg::SetColor(state)],
        (false, false) => vec![],
    }
}

/// Sends commands received over MQTT to the lights. Each light gets its own
/// worker so that an unreachable bulb cannot hold up commands to the others.
pub fn start_command_loop(
    settings: &Settings,
    lifx_socket: &LifxSocket,
    lights: &Lights,
    mut commands: Receiver<MqttDevice>,
) -> Result<JoinHandle<()>> {
    let mut workers = HashMap::new();

    for (id, device) in &settings.devices {
        let addr = device.addr()?;
        let (tx, mut rx) = mpsc::channel::<MqttDevice>(16);
        let lifx_socket = lifx_socket.clone();
        let lights = lights.clone();

        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                let current = lights.get(&command.id);
                let effective = merge_command(&command, current.as_ref());

                let mut result = Ok(());
                for msg in command_messages(&addr, &command, current.as_ref(), &effective) {
                    result = lifx_socket.send_acked(&addr, msg).await;
                    if result.is_err() {
                        break;
                    }
                }

                match result {
                    Ok(()) => lights.commanded(effective),
                    Err(e) => eprintln!("Failed to send command to {}: {}", command.id, e),
                }
            }
        });

        workers.insert(id.clone(), tx);
    }

    Ok(tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match workers.get(&command.id) {
                Some(tx) => {
                    if let Err(e) = tx.try_send(command) {
                        eprintln!("Dropping light command: {}", e);
                    }
                }
                None => eprintln!("Device with id {} not found in settings", command.id),
            }
        }
    }))
}

pub fn start_light_polling_loop(
    settings: &Settings,
    lifx_socket: &LifxSocket,
    lights: &Lights,
) -> Result<JoinHandle<()>> {
    let devices = settings
        .devices
        .iter()
        .map(|(id, device)| Ok((id.clone(), device.addr()?)))
        .collect::<Result<Vec<_>>>()?;
    let lifx_socket = lifx_socket.clone();
    let lights = lights.clone();

    Ok(tokio::spawn(async move {
        let mut interval = time::interval(POLL_RATE);
        let started = Instant::now();
        let mut unresponsive: HashMap<String, bool> = HashMap::new();

        loop {
            interval.tick().await;

            for (id, addr) in &devices {
                if let Err(e) = lifx_socket.send(addr, LifxMsg::Get).await {
                    eprintln!("Error polling {} at {}: {}", id, addr, e);
                }

                // Log transitions only, so there's a trace of when a light
                // dropped off the network without flooding the logs.
                let silence = lights.last_seen(id).unwrap_or(started).elapsed();
                let is_unresponsive = silence > UNRESPONSIVE_AFTER;
                let was_unresponsive = unresponsive.insert(id.clone(), is_unresponsive);
                match (was_unresponsive, is_unresponsive) {
                    (Some(false) | None, true) => eprintln!(
                        "Light {} at {} has not responded in {}s",
                        id,
                        addr,
                        silence.as_secs()
                    ),
                    (Some(true), false) => {
                        eprintln!("Light {} at {} is responding again", id, addr)
                    }
                    _ => {}
                }
            }
        }
    }))
}

fn handle_incoming_lifx_msg(
    mqtt_client: &MqttClient,
    settings: &Settings,
    lifx_socket: &LifxSocket,
    lights: &Lights,
    msg: LifxMsg,
) -> Result<()> {
    match msg {
        LifxMsg::State(state) => {
            let mqtt_device = from_lifx_state(settings, state)?;
            lights.reported(mqtt_device.clone());
            mqtt_client.publish_mqtt_device(&mqtt_device)?;
        }
        LifxMsg::Acknowledgement { addr, sequence } => lifx_socket.handle_ack(addr, sequence),
        _ => {}
    }

    Ok(())
}

pub fn start_udp_receiver_loop(
    mqtt_client: &MqttClient,
    settings: &Settings,
    lifx_socket: &LifxSocket,
    lights: &Lights,
) -> JoinHandle<()> {
    let mqtt_client = mqtt_client.clone();
    let settings = settings.clone();
    let lifx_socket = lifx_socket.clone();
    let lights = lights.clone();

    tokio::spawn(async move {
        let mut buf = vec![0; MAX_UDP_PACKET_SIZE];

        loop {
            let res = match lifx_socket.udp_socket.recv_from(&mut buf).await {
                Ok((bytes_read, addr)) => {
                    let msg = read_lifx_msg(&buf[..bytes_read], addr);
                    handle_incoming_lifx_msg(&mqtt_client, &settings, &lifx_socket, &lights, msg)
                }
                Err(e) => Err(e.into()),
            };

            if let Err(e) = res {
                eprintln!("Error while handling incoming lifx msg: {}", e);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:56700".parse().unwrap()
    }

    fn device(power: Option<bool>, brightness: Option<f32>) -> MqttDevice {
        MqttDevice {
            id: "light-1".into(),
            power,
            brightness,
            ..Default::default()
        }
    }

    #[test]
    fn partial_command_keeps_current_values() {
        let current = MqttDevice {
            name: "Lamp".into(),
            color: Some(DeviceColor::Hs(Hs { h: 120, s: 0.5 })),
            ..device(Some(true), Some(0.3))
        };

        let merged = merge_command(&device(None, Some(0.8)), Some(&current));

        assert_eq!(merged.name, "Lamp");
        assert_eq!(merged.power, Some(true));
        assert_eq!(merged.brightness, Some(0.8));
        assert_eq!(merged.color, current.color);
    }

    #[test]
    fn turning_on_sets_color_before_power() {
        let current = MqttDevice {
            color: Some(DeviceColor::Hs(Hs { h: 0, s: 0.0 })),
            ..device(Some(false), Some(1.0))
        };
        let command = device(Some(true), Some(0.5));
        let effective = merge_command(&command, Some(&current));

        let msgs = command_messages(&addr(), &command, Some(&current), &effective);

        assert!(matches!(
            msgs.as_slice(),
            [
                LifxMsg::SetColor(LifxState {
                    transition: Some(0),
                    ..
                }),
                LifxMsg::SetPower(_)
            ]
        ));
    }

    #[test]
    fn power_only_command_does_not_touch_color() {
        let command = device(Some(false), None);
        let effective = merge_command(&command, None);

        let msgs = command_messages(&addr(), &command, None, &effective);

        assert!(matches!(msgs.as_slice(), [LifxMsg::SetPower(_)]));
    }
}

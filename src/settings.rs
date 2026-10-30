use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

use color_eyre::Result;
use eyre::WrapErr;
use serde::Deserialize;

use crate::protocols::lifx_udp::LIFX_UDP_PORT;

#[derive(Clone, Deserialize, Debug)]
pub struct DeviceSettings {
    pub name: String,
    pub ip: String,
}

impl DeviceSettings {
    pub fn addr(&self) -> Result<SocketAddr> {
        let ip: IpAddr = self
            .ip
            .parse()
            .wrap_err_with(|| format!("Invalid IP address {} for device {}", self.ip, self.name))?;

        Ok(SocketAddr::new(ip, LIFX_UDP_PORT))
    }
}

pub type DevicesSettings = HashMap<String, DeviceSettings>;

#[derive(Clone, Deserialize, Debug)]
pub struct MqttSettings {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub light_topic: String,
    pub light_topic_set: String,
}

#[derive(Clone, Deserialize, Debug)]
pub struct Settings {
    pub devices: DevicesSettings,
    pub mqtt: MqttSettings,
}

pub fn read_settings() -> Result<Settings> {
    parse_settings(config::File::with_name("Settings"))
}

fn parse_settings<S>(source: S) -> Result<Settings>
where
    S: config::Source + Send + Sync + 'static,
{
    let settings = config::Config::builder()
        .add_source(source)
        .build()?
        .try_deserialize::<Settings>()?;

    // Fail at startup rather than in a background task later on.
    for device in settings.devices.values() {
        device.addr()?;
    }

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_settings() {
        let settings = parse_settings(config::File::from_str(
            include_str!("../Settings.example.toml"),
            config::FileFormat::Toml,
        ))
        .unwrap();

        assert_eq!(settings.mqtt.id, "lifx-mqtt");
        assert_eq!(settings.mqtt.port, 1883);
        assert_eq!(settings.mqtt.light_topic_set, "home/lights/lifx/{id}/set");
        assert_eq!(settings.devices.len(), 2);
        assert_eq!(
            settings.devices["light-1"].addr().unwrap(),
            "192.168.1.48:56700".parse().unwrap()
        );
    }

    #[test]
    fn rejects_invalid_device_ip() {
        let result = parse_settings(config::File::from_str(
            r#"
            [mqtt]
            id = "lifx-mqtt"
            host = "localhost"
            port = 1883
            light_topic = "lights/{id}"
            light_topic_set = "lights/{id}/set"

            [devices]
            light-1 = { name = "Lamp", ip = "not-an-ip" }
            "#,
            config::FileFormat::Toml,
        ));

        assert!(result.is_err());
    }
}

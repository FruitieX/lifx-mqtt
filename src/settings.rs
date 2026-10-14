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
    let settings = config::Config::builder()
        .add_source(config::File::with_name("Settings"))
        .build()?
        .try_deserialize::<Settings>()?;

    // Fail at startup rather than in a background task later on.
    for device in settings.devices.values() {
        device.addr()?;
    }

    Ok(settings)
}

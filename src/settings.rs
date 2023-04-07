use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Deserialize, Debug)]
pub struct DeviceSettings {
    pub name: String,
    pub ip: String,
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

pub fn read_settings() -> Result<Settings, config::ConfigError> {
    config::Config::builder()
        .add_source(config::File::with_name("Settings"))
        .build()?
        .try_deserialize::<Settings>()
}

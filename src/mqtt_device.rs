use derive_builder::Builder;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Capabilities {
    /// Hue (0 - 360) and saturation (0.0 - 1.0)
    #[serde(default)]
    pub hs: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            hs: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Hs {
    pub h: u16,
    pub s: f32,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DeviceColor {
    Hs(Hs),
}

#[derive(Builder, Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[builder(setter(into, strip_option), default)]
pub struct MqttDevice {
    pub id: String,
    pub name: String,
    pub power: Option<bool>,
    pub brightness: Option<f32>,
    pub color: Option<DeviceColor>,
    pub transition_ms: Option<f32>,
    pub sensor_value: Option<String>,
    pub capabilities: Option<Capabilities>
}

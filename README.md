# lifx-mqtt

This program synchronizes LIFX devices with an MQTT broker.

## Setup

- Figure out the IP addresses of your LIFX devices
- Copy `Settings.example.toml` to `Settings.toml`.
- Edit `Settings.toml` with values matching your setup.
- Try running lifx-mqtt with `cargo run`.

Now you should be able to view all your configured LIFX devices through e.g. [MQTT Explorer](http://mqtt-explorer.com/) once connected to the same MQTT broker.

## Topics

The default MQTT topics are as follows:

- `/home/lights/lifx/{id}`: Current state of the device serialized as JSON
- `/home/lights/lifx/{id}/set`: Sets state of the light to given JSON

## State messages

MQTT messages follow this structure, serialized as JSON:

```
struct MqttDevice {
    pub id: String,
    pub name: String,
    pub power: Option<bool>,
    pub brightness: Option<f32>,
    pub cct: Option<f32>,
    pub color: Option<Hsv>,
    pub transition_ms: Option<f32>,
    pub sensor_value: Option<String>,
}
```

Example light state:

```
{
  "id": "my-light-1",
  "name": "Office",
  "power": null,
  "brightness": 0.5,
  "cct": null,
  "color": {
    "hue": 31.238605,
    "saturation": 0.7411992,
    "value": 1
  },
  "transition_ms": null,
  "sensor_value": null
}
```

use color_eyre::Result;
use eyre::eyre;
use futures::future::select_all;
use lifx::{start_command_loop, start_light_polling_loop, start_udp_receiver_loop, Lights};
use protocols::lifx_udp::LifxSocket;
use protocols::mqtt::mk_mqtt_client;
use tokio::signal::unix::{signal, SignalKind};

use crate::settings::read_settings;

mod lifx;
mod mqtt_device;
mod protocols;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let settings = read_settings()?;
    let lifx_socket = LifxSocket::init().await?;
    let lights = Lights::default();
    let (mqtt_client, commands, mqtt_task) = mk_mqtt_client(&settings);

    let tasks = vec![
        ("MQTT event loop", mqtt_task),
        ("MQTT watchdog", mqtt_client.start_watchdog()),
        (
            "command loop",
            start_command_loop(&settings, &lifx_socket, &lights, commands)?,
        ),
        (
            "UDP receiver",
            start_udp_receiver_loop(&mqtt_client, &settings, &lifx_socket, &lights),
        ),
        (
            "light polling",
            start_light_polling_loop(&settings, &lifx_socket, &lights)?,
        ),
    ];
    let (names, handles): (Vec<_>, Vec<_>) = tasks.into_iter().unzip();

    let mut sigterm = signal(SignalKind::terminate())?;

    // All tasks are supposed to run forever. If one stops (or panics), exit
    // with an error so that the process gets restarted instead of carrying
    // on half-dead.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = sigterm.recv() => Ok(()),
        (result, index, _) = select_all(handles) => {
            Err(eyre!("Task '{}' stopped: {:?}", names[index], result))
        }
    }
}

use color_eyre::Result;
use lifx::{start_light_polling_loop, start_udp_receiver_loop};
use protocols::lifx_udp::LifxSocket;
use protocols::mqtt::{mk_mqtt_client, start_mqtt_events_loop};

use crate::settings::read_settings;

mod lifx;
mod mqtt_device;
mod protocols;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let settings = read_settings()?;
    let mqtt_client = mk_mqtt_client(&settings).await?;
    let lifx_socket = LifxSocket::init().await?;

    start_mqtt_events_loop(&mqtt_client, &settings, &lifx_socket);
    start_udp_receiver_loop(&mqtt_client, &settings, &lifx_socket);
    start_light_polling_loop(&settings, &lifx_socket);

    tokio::signal::ctrl_c().await?;

    Ok(())
}

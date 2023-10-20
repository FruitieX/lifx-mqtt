#![allow(clippy::redundant_closure_call)]

use color_eyre::Result;
use eyre::eyre;
use rand::{distributions::Alphanumeric, Rng};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc::Receiver, RwLock},
    task,
};

use crate::{mqtt_device::MqttDevice, settings::Settings};

use super::lifx_udp::{LifxSocket, LIFX_UDP_PORT};

pub type MqttRx = Arc<RwLock<Receiver<Option<MqttDevice>>>>;

#[derive(Clone)]
pub struct MqttClient {
    pub client: AsyncClient,
    pub rx: MqttRx,
}

pub async fn mk_mqtt_client(settings: &Settings) -> Result<MqttClient> {
    let random_string: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();

    let mut options = MqttOptions::new(
        format!("{}-{}", settings.mqtt.id.clone(), random_string),
        settings.mqtt.host.clone(),
        settings.mqtt.port,
    );
    options.set_keep_alive(Duration::from_secs(5));
    let (client, mut eventloop) = AsyncClient::new(options, 10);

    let (tx, rx) = tokio::sync::mpsc::channel(100);
    let tx = Arc::new(RwLock::new(tx));
    let rx = Arc::new(RwLock::new(rx));

    {
        let settings = settings.clone();
        let client = client.clone();

        task::spawn(async move {
            loop {
                {
                    let notification = eventloop.poll().await;
                    let mqtt_tx = tx.clone();
                    let settings = settings.clone();
                    let client = client.clone();

                    let res = (|| async move {
                        match notification? {
                            rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_)) => {
                                client
                                    .subscribe(
                                        settings.mqtt.light_topic_set.replace("{id}", "+"),
                                        QoS::AtMostOnce,
                                    )
                                    .await?;
                            }

                            rumqttc::Event::Incoming(rumqttc::Packet::Publish(msg)) => {
                                let device: MqttDevice = serde_json::from_slice(&msg.payload)?;

                                let tx = mqtt_tx.write().await;
                                tx.send(Some(device)).await?;
                            }
                            _ => {}
                        }

                        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                    })()
                    .await;

                    if let Err(e) = res {
                        eprintln!("MQTT error: {:?}", e);
                        tokio::time::sleep(Duration::from_millis(1000)).await;
                    }
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }

    Ok(MqttClient { client, rx })
}

pub async fn publish_mqtt_device(
    mqtt_client: &MqttClient,
    settings: &Settings,
    mqtt_device: &MqttDevice,
) -> Result<()> {
    let topic_template = &settings.mqtt.light_topic;

    let topic = topic_template.replace("{id}", &mqtt_device.id);

    let json = serde_json::to_string(&mqtt_device)?;

    mqtt_client
        .client
        .publish(topic, rumqttc::QoS::AtLeastOnce, true, json)
        .await?;

    Ok(())
}

pub fn start_mqtt_events_loop(
    mqtt_client: &MqttClient,
    settings: &Settings,
    lifx_socket: &LifxSocket,
) {
    let mqtt_rx = mqtt_client.rx.clone();
    let settings = settings.clone();
    let lifx_socket = lifx_socket.clone();

    tokio::spawn(async move {
        loop {
            let result =
                process_next_mqtt_message(mqtt_rx.clone(), &settings, &lifx_socket.clone()).await;

            if let Err(e) = result {
                eprintln!("{:?}", e);
            }
        }
    });
}

async fn process_next_mqtt_message(
    mqtt_rx: MqttRx,
    settings: &Settings,
    lifx_socket: &LifxSocket,
) -> Result<()> {
    let mqtt_device = {
        let mut mqtt_rx = mqtt_rx.write().await;
        let value = mqtt_rx
            .recv()
            .await
            .expect("Expected mqtt_rx channel to never close");
        value.ok_or_else(|| eyre!("Expected to receive mqtt message from rx channel"))?
    };

    let device_settings = settings
        .devices
        .get(&mqtt_device.id)
        .ok_or_else(|| eyre!("Device with id {} not found in settings", mqtt_device.id))?;

    let ip = &device_settings.ip;
    let addr: SocketAddr = format!("{}:{}", ip, LIFX_UDP_PORT).parse()?;

    lifx_socket.send_state_to_lifx(addr, &mqtt_device).await?;

    Ok(())
}

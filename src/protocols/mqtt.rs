use color_eyre::Result;
use rand::{distributions::Alphanumeric, Rng};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc::{self, Receiver},
    task::JoinHandle,
    time,
};

use crate::{mqtt_device::MqttDevice, settings::Settings};

const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// If nothing has been received from the broker for this long (not even a
/// ping response), consider the connection wedged and give up.
const MAX_SILENCE: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct MqttClient {
    client: AsyncClient,
    light_topic: String,
    /// Last state queued for publishing per device. Cleared on reconnect so
    /// that every state gets republished in the new session.
    published: Arc<Mutex<HashMap<String, MqttDevice>>>,
    last_incoming: Arc<Mutex<Instant>>,
}

/// Connects to the broker and spawns the task driving the MQTT event loop.
/// Light commands received from the broker are delivered through the
/// returned channel.
pub fn mk_mqtt_client(settings: &Settings) -> (MqttClient, Receiver<MqttDevice>, JoinHandle<()>) {
    let random_string: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();

    let mut options = MqttOptions::new(
        format!("{}-{}", settings.mqtt.id, random_string),
        settings.mqtt.host.clone(),
        settings.mqtt.port,
    );
    options.set_keep_alive(KEEP_ALIVE);
    let (client, mut eventloop) = AsyncClient::new(options, 64);

    let (tx, rx) = mpsc::channel(100);

    let mqtt_client = MqttClient {
        client: client.clone(),
        light_topic: settings.mqtt.light_topic.clone(),
        published: Default::default(),
        last_incoming: Arc::new(Mutex::new(Instant::now())),
    };

    let set_topic = settings.mqtt.light_topic_set.replace("{id}", "+");
    let published = mqtt_client.published.clone();
    let last_incoming = mqtt_client.last_incoming.clone();

    // Nothing in this loop may await anything other than eventloop.poll():
    // the event loop is what drains the client's request channel, so blocking
    // here on a full channel deadlocks the whole MQTT connection.
    let handle = tokio::spawn(async move {
        let mut needs_subscribe = false;

        loop {
            let event = match eventloop.poll().await {
                Ok(event) => event,
                Err(e) => {
                    eprintln!("MQTT error: {:?}", e);
                    time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            if let Event::Incoming(packet) = event {
                *last_incoming.lock().unwrap() = Instant::now();

                match packet {
                    Packet::ConnAck(_) => {
                        eprintln!("Connected to MQTT broker");
                        // New session: subscriptions are gone and anything
                        // queued in the old session has been discarded.
                        needs_subscribe = true;
                        published.lock().unwrap().clear();
                    }
                    Packet::Publish(msg) => {
                        match serde_json::from_slice::<MqttDevice>(&msg.payload) {
                            Ok(device) => {
                                if let Err(e) = tx.try_send(device) {
                                    eprintln!("Dropping MQTT light command: {}", e);
                                }
                            }
                            Err(e) => eprintln!("Invalid MQTT light command: {}", e),
                        }
                    }
                    _ => {}
                }
            }

            if needs_subscribe {
                match client.try_subscribe(&set_topic, QoS::AtMostOnce) {
                    Ok(()) => needs_subscribe = false,
                    Err(e) => eprintln!("Could not queue MQTT subscription: {}", e),
                }
            }
        }
    });

    (mqtt_client, rx, handle)
}

impl MqttClient {
    /// Publishes the device state as a retained message, unless it is
    /// identical to what was last published in this session.
    pub fn publish_mqtt_device(&self, mqtt_device: &MqttDevice) -> Result<()> {
        let mut published = self.published.lock().unwrap();

        if published.get(&mqtt_device.id) == Some(mqtt_device) {
            return Ok(());
        }

        let topic = self.light_topic.replace("{id}", &mqtt_device.id);
        let json = serde_json::to_string(&mqtt_device)?;

        // Never block here: a slow or disconnected broker must not hold up
        // LIFX traffic. A failed publish is retried on the next poll since
        // `published` is only updated on success.
        self.client
            .try_publish(topic, QoS::AtMostOnce, true, json)?;

        published.insert(mqtt_device.id.clone(), mqtt_device.clone());

        Ok(())
    }

    /// Resolves once the broker has been silent for too long, which means
    /// that the event loop is stuck or that reconnecting keeps failing.
    pub fn start_watchdog(&self) -> JoinHandle<()> {
        let last_incoming = self.last_incoming.clone();

        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(5));

            loop {
                interval.tick().await;

                let silence = last_incoming.lock().unwrap().elapsed();
                if silence > MAX_SILENCE {
                    eprintln!(
                        "Nothing received from MQTT broker in {}s, giving up",
                        silence.as_secs()
                    );
                    return;
                }
            }
        })
    }
}

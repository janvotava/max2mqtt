//! MQTT client task, using the codec in `max2mqtt_bridge::mqtt`.

use alloc::string::String;
use alloc::vec::Vec;

use core::sync::atomic::Ordering;
use embassy_futures::select::{Either3, select3};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{TcpSocket, TcpWriter};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;

use max2mqtt_bridge::bridge::OFFLINE_PAYLOAD;
use max2mqtt_bridge::mqtt::{self, Packet, Will};
use max2mqtt_bridge::settings::MqttSettings;
use max2mqtt_bridge::{Effect, Settings};

use crate::shared::{MQTT_CONNECTED, MQTT_OUT, ctx, dispatch, with_bridge};

const KEEP_ALIVE_SECS: u16 = 60;
/// Largest message we buffer from the broker.
const MAX_INBOUND: usize = 8 * 1024;

#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>, client_id: &'static str, settings: &'static Settings) {
    let mut rx = alloc::vec![0u8; 2048];
    let mut tx = alloc::vec![0u8; 2048];
    loop {
        stack.wait_config_up().await;
        let result = session(stack, &mut rx, &mut tx, client_id, &settings.mqtt).await;
        MQTT_CONNECTED.store(false, Ordering::Relaxed);
        if let Err(e) = result {
            log::warn!("MQTT: {e}");
        }
        Timer::after_secs(5).await;
    }
}

/// Device states and announcements after a connect, one device at a time: all of them at
/// once do not fit in memory.
async fn announce(session: &mut Session<'_>) -> Result<(), &'static str> {
    while let Some(effects) = with_bridge(|b| b.next_announcement()).await {
        for effect in effects {
            session.apply(effect).await?;
        }
    }
    Ok(())
}

struct Session<'a> {
    writer: TcpWriter<'a>,
    next_packet_id: u16,
    last_sent: Instant,
}

impl Session<'_> {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        self.writer.write_all(bytes).await.map_err(|_| "write failed")?;
        self.last_sent = Instant::now();
        Ok(())
    }

    fn packet_id(&mut self) -> u16 {
        self.next_packet_id = self.next_packet_id.wrapping_add(1).max(1);
        self.next_packet_id
    }

    async fn apply(&mut self, effect: Effect) -> Result<(), &'static str> {
        let bytes = match effect {
            Effect::Publish { topic, payload, retain } => mqtt::publish(&topic, payload.as_bytes(), retain),
            Effect::Subscribe(topic) => {
                let id = self.packet_id();
                mqtt::subscribe(id, &[(&topic, 1)])
            }
            Effect::Unsubscribe(topic) => {
                let id = self.packet_id();
                mqtt::unsubscribe(id, &[&topic])
            }
            _ => return Ok(()),
        };
        self.send(&bytes).await
    }
}

async fn session(
    stack: Stack<'static>,
    rx: &mut [u8],
    tx: &mut [u8],
    client_id: &str,
    settings: &MqttSettings,
) -> Result<(), &'static str> {
    let port = settings.port;
    let addresses = stack.dns_query(&settings.host, DnsQueryType::A).await.map_err(|_| "DNS lookup failed")?;
    let address = *addresses.first().ok_or("no address for MQTT host")?;

    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(KEEP_ALIVE_SECS as u64 * 2)));
    socket.connect((address, port)).await.map_err(|_| "connect failed")?;
    log::info!("MQTT connected to {address}:{port}");

    let will = Will { topic: &settings.prefix, payload: OFFLINE_PAYLOAD.as_bytes(), qos: 1, retain: true };
    let credentials = (!settings.user.is_empty()).then_some((settings.user.as_str(), settings.password.as_str()));
    socket
        .write_all(&mqtt::connect(client_id, KEEP_ALIVE_SECS, Some(&will), credentials))
        .await
        .map_err(|_| "write failed")?;

    let mut inbox: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    // CONNACK
    with_timeout(Duration::from_secs(10), async {
        loop {
            let n = socket.read(&mut buf).await.map_err(|_| "read failed")?;
            if n == 0 {
                return Err("closed by broker");
            }
            inbox.extend_from_slice(&buf[..n]);
            if let Some((packet, used)) = mqtt::decode(&inbox).map_err(|_| "malformed packet")? {
                let result = match packet {
                    Packet::ConnAck { return_code: 0, .. } => Ok(()),
                    Packet::ConnAck { .. } => Err("broker refused connection"),
                    _ => Err("expected CONNACK"),
                };
                inbox.drain(..used);
                return result;
            }
        }
    })
    .await
    .map_err(|_| "no CONNACK")??;

    MQTT_CONNECTED.store(true, Ordering::Relaxed);
    let (mut reader, writer) = socket.split();
    let mut session = Session { writer, next_packet_id: 0, last_sent: Instant::now() };

    // Whatever was queued while offline is stale; the bridge republishes all of it.
    while MQTT_OUT.try_receive().is_ok() {}
    for effect in with_bridge(|b| b.on_mqtt_connected()).await {
        session.apply(effect).await?;
    }
    announce(&mut session).await?;

    let half = Duration::from_secs(KEEP_ALIVE_SECS as u64 / 2);
    // Sent a PINGREQ and heard nothing since.
    let mut ping_sent: Option<Instant> = None;
    loop {
        let event = select3(reader.read(&mut buf), MQTT_OUT.receive(), Timer::after_secs(5)).await;
        match event {
            Either3::First(Ok(0)) => return Err("closed by broker"),
            Either3::First(Err(_)) => return Err("read failed"),
            Either3::First(Ok(n)) => {
                ping_sent = None;
                inbox.extend_from_slice(&buf[..n]);
                while let Some((packet, used)) = mqtt::decode(&inbox).map_err(|_| "malformed packet")? {
                    let incoming = match packet {
                        Packet::Publish { topic, payload, qos, retain, packet_id } => Some((
                            String::from(topic),
                            payload.to_vec(),
                            retain,
                            if qos > 0 { packet_id } else { None },
                        )),
                        _ => None,
                    };
                    inbox.drain(..used);
                    if let Some((topic, payload, retain, ack_id)) = incoming {
                        if let Some(id) = ack_id {
                            session.send(&mqtt::puback(id)).await?;
                        }
                        log::info!("MQTT <- {topic}");
                        let effects = with_bridge(|b| b.handle_mqtt(&topic, &payload, retain, ctx())).await;
                        dispatch(effects);
                    }
                }
                if inbox.len() > MAX_INBOUND {
                    return Err("message from broker too large");
                }
            }
            Either3::Second(effect) => session.apply(effect).await?,
            Either3::Third(()) => {}
        }
        // Home Assistant came back and wants everything again.
        announce(&mut session).await?;
        // Keep alive: the broker needs to hear from us, and we want to know it is there.
        if ping_sent.is_some_and(|t| t.elapsed() > half) {
            return Err("broker stopped answering");
        }
        if ping_sent.is_none() && session.last_sent.elapsed() >= half {
            session.send(&mqtt::pingreq()).await?;
            ping_sent = Some(Instant::now());
        }
    }
}

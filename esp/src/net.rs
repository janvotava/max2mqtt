//! Wi-Fi: the home network as a station, and a setup hotspot when that does not work.
//!
//! - No network configured: the hotspot is on for good.
//! - Configured, but not connected for [`FALLBACK_AFTER`]: the hotspot comes on next to the
//!   station, which keeps retrying every [`RETRY_EVERY`] unless someone is using the
//!   hotspot (retrying scans all channels, which interrupts the hotspot).
//! - Connected again and nobody used the hotspot for [`HOTSPOT_IDLE`]: hotspot off.
//!
//! Two IP stacks: DHCP client on the station, 192.168.4.1/24 with our own DHCP and DNS
//! (see [`crate::portal`]) on the hotspot.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{DhcpConfig, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::rng::Rng;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController};
use max2mqtt_bridge::Settings;
use static_cell::StaticCell;

use crate::shared::{
    HOTSPOT, Network, SCAN_REQUEST, SCAN_RESULT, STATION, hotspot_idle_secs, set_unix_millis, unix_millis,
};

/// Seconds between 1900 (NTP epoch) and 1970 (Unix epoch).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
pub const HOTSPOT_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const FALLBACK_AFTER: Duration = Duration::from_secs(60);
const RETRY_EVERY: Duration = Duration::from_secs(60);
const HOTSPOT_IDLE: u64 = 120;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Connected to Wi-Fi but no IP address after this long: see `wifi_task`.
const NO_ADDRESS_AFTER: Duration = Duration::from_secs(30);

pub struct Net {
    pub station: Stack<'static>,
    pub hotspot: Stack<'static>,
    pub mac: [u8; 6],
    pub hotspot_ssid: &'static str,
}

/// Bring up Wi-Fi and both IP stacks.
pub fn start(spawner: &Spawner, wifi: esp_hal::peripherals::WIFI<'static>, settings: &'static Settings) -> Net {
    // Espressif's "memory saving" buffer numbers instead of esp-radio's defaults (10 static
    // receive buffers of 1.6 KiB, up to 32 dynamic ones each way): plenty for a web UI and
    // MQTT, and still tens of Mbit/s.
    let wifi_config = ControllerConfig::default()
        .with_static_rx_buf_num(6)
        .with_dynamic_rx_buf_num(12)
        .with_dynamic_tx_buf_num(12)
        .with_rx_ba_win(6);
    let controller = WifiController::new(wifi, wifi_config).expect("Wi-Fi controller");
    let sta = Interface::station();
    let ap = Interface::access_point();
    let mac = sta.mac_address();

    let mut dhcp = DhcpConfig::default();
    dhcp.hostname = heapless::String::try_from(settings.hostname.as_str()).ok();
    let dhcp_config = dhcp.clone();
    let rng = Rng::new();
    let seed = || ((rng.random() as u64) << 32) | rng.random() as u64;

    // DHCP client, embassy-net's DNS client, SNTP, MQTT, four HTTP; plus headroom.
    static STA_RESOURCES: StaticCell<StackResources<11>> = StaticCell::new();
    let (station, sta_runner) =
        embassy_net::new(sta, embassy_net::Config::dhcpv4(dhcp), STA_RESOURCES.init(StackResources::new()), seed());

    // DHCP server, DNS server, embassy-net's DNS client, two HTTP; plus headroom.
    static AP_RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();
    let ap_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(HOTSPOT_IP, 24),
        gateway: None,
        dns_servers: Default::default(),
    });
    let (hotspot, ap_runner) = embassy_net::new(ap, ap_config, AP_RESOURCES.init(StackResources::new()), seed());

    static SSID: StaticCell<String> = StaticCell::new();
    let hotspot_ssid = SSID.init(format!("max2mqtt-{:02X}{:02X}", mac[4], mac[5]));

    spawner.spawn(wifi_task(controller, settings, hotspot_ssid, station, dhcp_config).unwrap());
    spawner.spawn(net_task(sta_runner).unwrap());
    spawner.spawn(net_task(ap_runner).unwrap());
    spawner.spawn(sntp_task(station, settings).unwrap());
    Net { station, hotspot, mac, hotspot_ssid }
}

fn configs(settings: &Settings, hotspot_ssid: &str) -> Option<(StationConfig, AccessPointConfig)> {
    let wifi = &settings.wifi;
    let auth = if wifi.password.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::WpaWpa2Personal(wifi.password.as_str().try_into().ok()?)
    };
    let sta = StationConfig::default().with_ssid(wifi.ssid.as_str().try_into().ok()?).with_authentication(auth);
    let ap = AccessPointConfig::default()
        .with_ssid(hotspot_ssid.try_into().ok()?)
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(settings.password.as_str().try_into().ok()?));
    Some((sta, ap))
}

#[embassy_executor::task]
async fn wifi_task(
    mut controller: WifiController<'static>,
    settings: &'static Settings,
    hotspot_ssid: &'static str,
    station: Stack<'static>,
    dhcp: DhcpConfig,
) {
    let Some((sta, ap)) = configs(settings, hotspot_ssid) else {
        log::error!("Invalid Wi-Fi settings");
        return;
    };
    let configured = !settings.wifi.ssid.is_empty();
    let set = |c: &mut WifiController<'static>, hotspot: bool| {
        let config =
            if hotspot { Config::AccessPointStation(sta.clone(), ap.clone()) } else { Config::Station(sta.clone()) };
        match c.set_config(&config) {
            Ok(()) => {
                HOTSPOT.store(hotspot, Ordering::Relaxed);
                if hotspot {
                    log::warn!(
                        "Setup hotspot {hotspot_ssid:?} on, password {:?}. Open http://{HOTSPOT_IP}/",
                        settings.password
                    );
                } else {
                    log::info!("Setup hotspot off");
                }
            }
            Err(e) => log::error!("Wi-Fi configuration rejected: {e:?}"),
        }
    };

    if !configured {
        log::warn!("No Wi-Fi network configured");
        set(&mut controller, true);
        loop {
            SCAN_REQUEST.wait().await;
            scan(&mut controller).await;
        }
    }

    set(&mut controller, false);
    let mut hotspot = false;
    let mut disconnected_since = Instant::now();
    loop {
        if !hotspot && disconnected_since.elapsed() >= FALLBACK_AFTER {
            set(&mut controller, true);
            hotspot = true;
        }
        // Someone is setting the board up: keep the radio on the hotspot channel.
        if hotspot && hotspot_idle_secs() < HOTSPOT_IDLE {
            if let Either::Second(()) = select(Timer::after_secs(5), SCAN_REQUEST.wait()).await {
                scan(&mut controller).await;
            }
            continue;
        }

        log::info!("Connecting to Wi-Fi {:?}", settings.wifi.ssid);
        match with_timeout(CONNECT_TIMEOUT, controller.connect_async()).await {
            Ok(Ok(_)) => {
                log::info!("Wi-Fi connected");
                STATION.store(true, Ordering::Relaxed);
                let connected_at = Instant::now();
                let mut dhcp_restarted = false;
                loop {
                    let event =
                        select3(controller.wait_for_disconnect_async(), SCAN_REQUEST.wait(), Timer::after_secs(10))
                            .await;
                    match event {
                        Either3::First(_) => break,
                        Either3::Second(()) => scan(&mut controller).await,
                        Either3::Third(()) => {
                            // After an access point vanished for a moment, the DHCP client
                            // can stay without a lease although Wi-Fi is back: ask again,
                            // then reconnect from scratch.
                            if !station.is_config_up() && connected_at.elapsed() >= NO_ADDRESS_AFTER {
                                if !dhcp_restarted {
                                    log::warn!("No address from DHCP, asking again");
                                    station.set_config_v4(embassy_net::ConfigV4::Dhcp(dhcp.clone()));
                                    dhcp_restarted = true;
                                } else if connected_at.elapsed() >= NO_ADDRESS_AFTER * 2 {
                                    log::warn!("Still no address, reconnecting to Wi-Fi");
                                    let _ = controller.disconnect_async().await;
                                    break;
                                }
                            }
                            if hotspot && hotspot_idle_secs() >= HOTSPOT_IDLE {
                                // Switching modes drops the station for a moment.
                                set(&mut controller, false);
                                hotspot = false;
                                break;
                            }
                        }
                    }
                }
                STATION.store(false, Ordering::Relaxed);
                log::warn!("Wi-Fi disconnected");
                disconnected_since = Instant::now();
                Timer::after_secs(1).await;
                continue;
            }
            Ok(Err(e)) => log::warn!("Wi-Fi connection failed: {e:?}"),
            Err(_) => {
                log::warn!("Wi-Fi connection timed out");
                let _ = controller.disconnect_async().await;
            }
        }
        // Wait before retrying; answer scans meanwhile.
        let wait = if hotspot { RETRY_EVERY } else { Duration::from_secs(5) };
        let until = Instant::now() + wait;
        while Instant::now() < until {
            match select(Timer::at(until), SCAN_REQUEST.wait()).await {
                Either::Second(()) => scan(&mut controller).await,
                Either::First(()) => break,
            }
        }
    }
}

async fn scan(controller: &mut WifiController<'static>) {
    let result = match controller.scan_async(&ScanConfig::default().with_max(20)).await {
        Ok(found) => {
            let mut networks: Vec<Network> = found
                .into_iter()
                .filter(|ap| !ap.ssid.is_empty())
                .map(|ap| Network {
                    ssid: String::from(ap.ssid.as_str()),
                    rssi: ap.signal_strength,
                    secure: !matches!(ap.auth_method, Some(esp_radio::wifi::AuthenticationMethod::None)),
                })
                .collect();
            // Strongest first, one entry per name (mesh systems show many access points).
            networks.sort_by_key(|n| core::cmp::Reverse(n.rssi));
            let mut seen = alloc::collections::BTreeSet::new();
            networks.retain(|n| seen.insert(n.ssid.clone()));
            Ok(networks)
        }
        Err(e) => {
            log::warn!("Wi-Fi scan failed: {e:?}");
            Err(())
        }
    };
    SCAN_RESULT.signal(result);
}

#[embassy_executor::task(pool_size = 2)]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>, settings: &'static Settings) {
    let mut announced = None;
    loop {
        stack.wait_config_up().await;
        let ip = stack.config_v4().map(|c| c.address.address());
        if ip != announced {
            if let Some(ip) = ip {
                log::info!("Web UI at http://{ip}/ (http://{}/ if your router knows the name)", settings.hostname);
            }
            announced = ip;
        }
        match sntp_query(stack, &settings.ntp_server).await {
            Ok(unix_ms) => {
                // The log's own timestamps show the time; say that the clock is set, and on each resync how
                // far it had drifted (positive: our clock was behind). Set first, so the line carries wall time.
                let before = unix_millis();
                set_unix_millis(unix_ms);
                match before {
                    None => log::info!("Clock set from {}", settings.ntp_server),
                    Some(before) => log::info!("Clock resynced, drift {} ms", unix_ms as i64 - before as i64),
                }
                Timer::after_secs(3600).await;
            }
            Err(e) => {
                log::warn!("SNTP failed: {e}");
                Timer::after_secs(30).await;
            }
        }
    }
}

/// Unix time in milliseconds.
async fn sntp_query(stack: Stack<'static>, server: &str) -> Result<u64, &'static str> {
    let addresses = stack.dns_query(server, DnsQueryType::A).await.map_err(|_| "DNS lookup failed")?;
    let server = *addresses.first().ok_or("no address for NTP server")?;

    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 128];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(0).map_err(|_| "bind failed")?;

    let mut request = [0u8; 48];
    request[0] = 0x23; // LI 0, version 4, client mode
    socket.send_to(&request, (server, 123)).await.map_err(|_| "send failed")?;
    let sent = Instant::now();

    let mut reply = [0u8; 48];
    let (n, _) = with_timeout(Duration::from_secs(5), socket.recv_from(&mut reply))
        .await
        .map_err(|_| "no answer")?
        .map_err(|_| "receive failed")?;
    let round_trip = sent.elapsed().as_millis();
    if n < 48 || reply[0] & 0x07 != 4 {
        return Err("bad reply");
    }
    let received = ntp_millis(&reply[32..40])?;
    let transmitted = ntp_millis(&reply[40..48])?;
    // Half the time on the wire, not counting how long the server held the request, has passed since it answered.
    let in_flight = round_trip.saturating_sub(transmitted.saturating_sub(received));
    Ok(transmitted + in_flight / 2)
}

/// An NTP timestamp (seconds and a 32-bit fraction since 1900) as Unix milliseconds.
fn ntp_millis(t: &[u8]) -> Result<u64, &'static str> {
    let mut secs = u32::from_be_bytes([t[0], t[1], t[2], t[3]]) as u64;
    // NTP era 1 starts in February 2036.
    if secs < 0x8000_0000 {
        secs += 1 << 32;
    }
    let fraction = u32::from_be_bytes([t[4], t[5], t[6], t[7]]) as u64;
    let secs = secs.checked_sub(NTP_UNIX_OFFSET).filter(|&t| t > 1_700_000_000).ok_or("implausible time")?;
    Ok(secs * 1000 + ((fraction * 1000) >> 32))
}

//! Captive portal on the setup hotspot: a DHCP server that hands out 192.168.4.50-200 and
//! points DNS (and the RFC 8910 captive portal option) at us, and a DNS server that answers
//! every name with our address. Phones then open the web UI by themselves.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_dhcp::server::{Server, ServerOptions};
use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::Stack;
use embassy_time::Timer;
use static_cell::StaticCell;

use crate::net::HOTSPOT_IP;

/// One socket each for DHCP and DNS.
type Buffers = UdpBuffers<2>;

fn udp(stack: Stack<'static>) -> Udp<'static> {
    static BUFFERS: StaticCell<Buffers> = StaticCell::new();
    Udp::new(stack, BUFFERS.init(Buffers::new()))
}

#[embassy_executor::task]
pub async fn portal_task(stack: Stack<'static>) {
    let udp = udp(stack);
    embassy_futures::join::join(dhcp(udp), dns(udp)).await;
}

async fn dhcp(udp: Udp<'static>) {
    let mut buf = [0u8; 1500];
    let dns = [HOTSPOT_IP];
    let mut gateway = [HOTSPOT_IP];
    let mut options = ServerOptions::new(HOTSPOT_IP, Some(&mut gateway));
    options.dns = &dns;
    options.captive_url = Some("http://192.168.4.1/");
    options.lease_duration_secs = 3600;
    let mut server = Server::<_, 8>::new_with_et(HOTSPOT_IP);
    loop {
        let bound = udp.bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 67)).await;
        match bound {
            Ok(mut socket) => {
                if let Err(e) = edge_dhcp::io::server::run(&mut server, &options, &mut socket, &mut buf).await {
                    log::warn!("DHCP server: {e:?}");
                }
            }
            Err(e) => log::warn!("DHCP server bind: {e:?}"),
        }
        Timer::after_secs(1).await;
    }
}

async fn dns(udp: Udp<'static>) {
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    loop {
        let local = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 53);
        if let Err(e) =
            edge_captive::io::run(&udp, local, &mut tx, &mut rx, HOTSPOT_IP, core::time::Duration::from_secs(60)).await
        {
            log::warn!("DNS server: {e:?}");
        }
        Timer::after_secs(1).await;
    }
}

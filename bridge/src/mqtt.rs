//! Minimal MQTT 3.1.1 client codec: just what the bridge needs.
//!
//! Written here instead of pulling in an MQTT crate so the firmware does not depend on yet
//! another fast-moving embedded API, and so the encoding is tested on the host.

use alloc::vec::Vec;

pub mod packet_type {
    pub const CONNECT: u8 = 1;
    pub const CONNACK: u8 = 2;
    pub const PUBLISH: u8 = 3;
    pub const PUBACK: u8 = 4;
    pub const SUBSCRIBE: u8 = 8;
    pub const SUBACK: u8 = 9;
    pub const UNSUBSCRIBE: u8 = 10;
    pub const UNSUBACK: u8 = 11;
    pub const PINGREQ: u8 = 12;
    pub const PINGRESP: u8 = 13;
    pub const DISCONNECT: u8 = 14;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    ConnAck { session_present: bool, return_code: u8 },
    Publish { topic: &'a str, payload: &'a [u8], qos: u8, retain: bool, packet_id: Option<u16> },
    PubAck(u16),
    SubAck(u16),
    UnsubAck(u16),
    PingResp,
    Other(u8),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    Malformed,
}

pub struct Will<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
    pub qos: u8,
    pub retain: bool,
}

fn put_remaining_length(out: &mut Vec<u8>, mut len: usize) {
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
}

fn packet(first: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(first);
    put_remaining_length(&mut out, body.len());
    out.extend_from_slice(&body);
    out
}

pub fn connect(
    client_id: &str,
    keep_alive_secs: u16,
    will: Option<&Will>,
    credentials: Option<(&str, &str)>,
) -> Vec<u8> {
    let mut body = Vec::new();
    put_str(&mut body, b"MQTT");
    body.push(4); // protocol level 3.1.1
    let mut flags = 0x02; // clean session
    if let Some(w) = will {
        flags |= 0x04 | ((w.qos & 0x03) << 3) | if w.retain { 0x20 } else { 0 };
    }
    if credentials.is_some() {
        flags |= 0xC0;
    }
    body.push(flags);
    body.extend_from_slice(&keep_alive_secs.to_be_bytes());
    put_str(&mut body, client_id.as_bytes());
    if let Some(w) = will {
        put_str(&mut body, w.topic.as_bytes());
        put_str(&mut body, w.payload);
    }
    if let Some((user, pass)) = credentials {
        put_str(&mut body, user.as_bytes());
        put_str(&mut body, pass.as_bytes());
    }
    packet(packet_type::CONNECT << 4, body)
}

/// QoS 0 publish.
pub fn publish(topic: &str, payload: &[u8], retain: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 2);
    put_str(&mut body, topic.as_bytes());
    body.extend_from_slice(payload);
    packet((packet_type::PUBLISH << 4) | retain as u8, body)
}

pub fn subscribe(packet_id: u16, topics: &[(&str, u8)]) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    for (topic, qos) in topics {
        put_str(&mut body, topic.as_bytes());
        body.push(*qos);
    }
    packet((packet_type::SUBSCRIBE << 4) | 0x02, body)
}

pub fn unsubscribe(packet_id: u16, topics: &[&str]) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    for topic in topics {
        put_str(&mut body, topic.as_bytes());
    }
    packet((packet_type::UNSUBSCRIBE << 4) | 0x02, body)
}

pub fn puback(packet_id: u16) -> Vec<u8> {
    packet(packet_type::PUBACK << 4, packet_id.to_be_bytes().to_vec())
}

pub fn pingreq() -> Vec<u8> {
    alloc::vec![packet_type::PINGREQ << 4, 0]
}

pub fn disconnect() -> Vec<u8> {
    alloc::vec![packet_type::DISCONNECT << 4, 0]
}

/// Decode one packet from the start of `buf`. `Ok(None)` means more bytes are needed.
/// On success also returns how many bytes it used.
pub fn decode(buf: &[u8]) -> Result<Option<(Packet<'_>, usize)>, Error> {
    let Some(&first) = buf.first() else { return Ok(None) };
    let mut len: usize = 0;
    let mut i = 1;
    loop {
        let Some(&b) = buf.get(i) else { return Ok(None) };
        len |= ((b & 0x7F) as usize) << (7 * (i - 1));
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        if i > 4 {
            return Err(Error::Malformed);
        }
    }
    let total = i + len;
    let Some(body) = buf.get(i..total) else { return Ok(None) };
    let id = |b: &[u8]| -> Result<u16, Error> {
        b.get(..2).map(|x| u16::from_be_bytes([x[0], x[1]])).ok_or(Error::Malformed)
    };

    let packet = match first >> 4 {
        packet_type::CONNACK => match body {
            [flags, code, ..] => Packet::ConnAck { session_present: flags & 1 != 0, return_code: *code },
            _ => return Err(Error::Malformed),
        },
        packet_type::PUBLISH => {
            let qos = (first >> 1) & 0x03;
            let topic_len = id(body)? as usize;
            let topic = body.get(2..2 + topic_len).ok_or(Error::Malformed)?;
            let topic = core::str::from_utf8(topic).map_err(|_| Error::Malformed)?;
            let mut rest = &body[2 + topic_len..];
            let packet_id = if qos > 0 {
                let pid = id(rest)?;
                rest = &rest[2..];
                Some(pid)
            } else {
                None
            };
            Packet::Publish { topic, payload: rest, qos, retain: first & 1 != 0, packet_id }
        }
        packet_type::PUBACK => Packet::PubAck(id(body)?),
        packet_type::SUBACK => Packet::SubAck(id(body)?),
        packet_type::UNSUBACK => Packet::UnsubAck(id(body)?),
        packet_type::PINGRESP => Packet::PingResp,
        other => Packet::Other(other),
    };
    Ok(Some((packet, total)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn connect_with_will() {
        let will = Will { topic: "max", payload: b"{}", qos: 1, retain: true };
        let p = connect("max-1", 30, Some(&will), None);
        assert_eq!(p[0], 0x10);
        assert_eq!(p[1] as usize, p.len() - 2);
        assert_eq!(&p[2..8], b"\x00\x04MQTT");
        assert_eq!(p[8], 4);
        assert_eq!(p[9], 0x02 | 0x04 | 0x08 | 0x20);
        assert_eq!(&p[10..12], &[0, 30]);
    }

    #[test]
    fn publish_roundtrip() {
        let p = publish("max/living-room", b"{\"a\":1}", true);
        let (packet, used) = decode(&p).unwrap().unwrap();
        assert_eq!(used, p.len());
        assert_eq!(
            packet,
            Packet::Publish { topic: "max/living-room", payload: b"{\"a\":1}", qos: 0, retain: true, packet_id: None }
        );
    }

    #[test]
    fn decodes_qos1_publish_and_long_lengths() {
        let payload = vec![b'x'; 300];
        let mut body = vec![0, 3, b'a', b'/', b'b', 0x12, 0x34];
        body.extend_from_slice(&payload);
        let mut raw = vec![0x32];
        put_remaining_length(&mut raw, body.len());
        assert_eq!(&raw[1..], &[0xB3, 0x02]); // 307 bytes
        raw.extend_from_slice(&body);
        let (packet, _) = decode(&raw).unwrap().unwrap();
        assert_eq!(
            packet,
            Packet::Publish { topic: "a/b", payload: &payload, qos: 1, retain: false, packet_id: Some(0x1234) }
        );
    }

    #[test]
    fn partial_input_needs_more() {
        let p = publish("t", b"payload", false);
        for n in 0..p.len() {
            assert_eq!(decode(&p[..n]), Ok(None), "{n}");
        }
    }

    #[test]
    fn small_packets() {
        assert_eq!(
            decode(&[0x20, 2, 0, 0]).unwrap().unwrap().0,
            Packet::ConnAck { session_present: false, return_code: 0 }
        );
        assert_eq!(decode(&[0x90, 3, 0, 7, 1]).unwrap().unwrap().0, Packet::SubAck(7));
        assert_eq!(decode(&[0xD0, 0]).unwrap().unwrap().0, Packet::PingResp);
        assert_eq!(
            subscribe(7, &[("max/set", 1)]),
            vec![0x82, 12, 0, 7, 0, 7, b'm', b'a', b'x', b'/', b's', b'e', b't', 1]
        );
        assert_eq!(puback(5), vec![0x40, 2, 0, 5]);
        assert_eq!(decode(&[0x30, 1, 0]), Err(Error::Malformed));
    }
}

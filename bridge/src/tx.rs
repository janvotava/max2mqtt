//! Transmit queue.
//!
//! Two lanes:
//! - **Responses** (ACKs to devices, time replies) go out next, with a short preamble, and
//!   are never waited on. A device that sent us something listens only briefly for the answer.
//!   The old firmware queued them behind everything else, including commands still waiting
//!   for their own ACK, so devices often gave up before our ACK arrived.
//! - **Commands** go out one at a time with a long preamble to wake the device. A command
//!   that expects an ACK stays at the head until the ACK arrives or 3 s pass, and is retried
//!   up to 3 times (FHEM's `ackTimeout` and `maxRetryCnt`). The old firmware resent after
//!   ~1.2 s regardless and matched ACKs by message counter alone.
//!
//! - **Parked** commands are for devices that sleep (window sensors, eco switches). They
//!   wait until the device sends something, which is when it listens for a moment, and then
//!   go out right away. If the device misses them, they are parked again.
//!
//! Airtime is budgeted like culfw: 10 ms of credit per second, at most 9 s banked, 1 s for a
//! long preamble plus 8 ms per byte.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::Millis;
use crate::address::Address;
use crate::protocol::{Frame, command};

pub const ACK_TIMEOUT: Millis = 3_000;
pub const MAX_ATTEMPTS: u8 = 4;
const CREDIT_MAX_MS: u32 = 9_000;
const LONG_PREAMBLE_MS: u32 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub frame: Frame,
    pub long_preamble: bool,
    pub expect_ack: bool,
}

impl Outgoing {
    /// A command to a device: long preamble, ACK expected unless broadcast.
    pub fn command(frame: Frame) -> Self {
        let expect_ack = !frame.dst.is_broadcast();
        Outgoing { frame, long_preamble: true, expect_ack }
    }

    /// An answer to a device that is awake right now.
    pub fn response(frame: Frame) -> Self {
        Outgoing { frame, long_preamble: false, expect_ack: false }
    }

    fn airtime_ms(&self) -> u32 {
        let bytes = crate::protocol::HEADER_LEN + self.frame.payload.len();
        (if self.long_preamble { LONG_PREAMBLE_MS } else { 0 }) + bytes as u32 * 8
    }

    /// Messages that a newer one makes obsolete if neither has been sent yet.
    fn supersedes(&self, other: &Outgoing) -> bool {
        let (a, b) = (&self.frame, &other.frame);
        // Setting and removing the group id are one setting: the newer replaces either, or
        // set, remove, set could go out as set, remove and leave the device without a group.
        let group = |c| c == command::SET_GROUP_ID || c == command::REMOVE_GROUP_ID;
        a.dst == b.dst
            && (a.command == b.command || (group(a.command) && group(b.command)))
            && match a.command {
                // One schedule message per day and part.
                command::CONFIG_WEEK_PROFILE => a.payload.first() == b.payload.first(),
                // Each link partner is its own message.
                command::ADD_LINK_PARTNER | command::REMOVE_LINK_PARTNER => a.payload == b.payload,
                _ => true,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxId {
    Response,
    Command(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Send {
        id: TxId,
        bytes: Vec<u8>,
        long_preamble: bool,
    },
    /// Nothing can go out before this time.
    WaitUntil(Millis),
    Idle,
}

/// Something worth logging or reporting that happened inside the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// With the payload, so the bridge can tell which setting a device confirmed.
    Acked {
        dst: Address,
        command: u8,
        payload: Vec<u8>,
    },
    Nacked {
        dst: Address,
        command: u8,
        payload: Vec<u8>,
    },
    GaveUp {
        dst: Address,
        command: u8,
    },
}

#[derive(Debug)]
struct Entry {
    seq: u32,
    out: Outgoing,
    attempts: u8,
    sent_at: Option<Millis>,
    /// Back to `parked` if it was not acknowledged.
    park_on_fail: bool,
}

/// Attempts for a command to a device that just woke up: it does not stay awake long.
const WOKEN_ATTEMPTS: u8 = 2;

#[derive(Debug)]
pub struct TxQueue {
    responses: VecDeque<Outgoing>,
    commands: VecDeque<Entry>,
    parked: Vec<Outgoing>,
    next_seq: u32,
    pub duty_cycle_limit: bool,
    credit_ms: u32,
    credit_updated: Millis,
}

impl TxQueue {
    pub fn new(duty_cycle_limit: bool, now: Millis) -> Self {
        TxQueue {
            responses: VecDeque::new(),
            commands: VecDeque::new(),
            parked: Vec::new(),
            next_seq: 0,
            duty_cycle_limit,
            credit_ms: CREDIT_MAX_MS,
            credit_updated: now,
        }
    }

    pub fn len(&self) -> usize {
        self.responses.len() + self.commands.len() + self.parked.len()
    }

    /// Commands waiting for their sleeping device to wake up.
    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    /// Commands that go out as airtime allows (not parked).
    pub fn active(&self) -> usize {
        self.responses.len() + self.commands.len()
    }

    /// Anything queued or parked for `dst`.
    pub fn has_for(&self, dst: Address) -> bool {
        self.commands.iter().any(|e| e.out.frame.dst == dst) || self.parked.iter().any(|p| p.frame.dst == dst)
    }

    /// Hold a command until `wake` is called for its device.
    pub fn park(&mut self, out: Outgoing) {
        if let Some(p) = self.parked.iter_mut().find(|p| out.supersedes(p)) {
            *p = out;
        } else {
            self.parked.push(out);
        }
    }

    /// `from` sent something and listens briefly: send what is parked for it now, with a
    /// short preamble, ahead of other commands (behind one on air).
    pub fn wake(&mut self, from: Address) {
        let (now_due, still): (Vec<_>, Vec<_>) =
            core::mem::take(&mut self.parked).into_iter().partition(|p| p.frame.dst == from);
        self.parked = still;
        let at = usize::from(self.commands.front().is_some_and(|e| e.attempts > 0));
        for (k, mut out) in now_due.into_iter().enumerate() {
            out.long_preamble = false;
            let seq = self.alloc_seq();
            self.commands.insert(at + k, Entry { seq, out, attempts: 0, sent_at: None, park_on_fail: true });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn credit_ms(&self) -> u32 {
        self.credit_ms
    }

    pub fn push_response(&mut self, out: Outgoing) {
        self.responses.push_back(out);
    }

    pub fn push(&mut self, out: Outgoing) {
        if let Some(e) =
            self.commands.iter_mut().find(|e| e.sent_at.is_none() && e.attempts == 0 && out.supersedes(&e.out))
        {
            e.out = out;
            return;
        }
        let seq = self.alloc_seq();
        let entry = Entry { seq, out, attempts: 0, sent_at: None, park_on_fail: false };
        // Someone is waiting for a temperature change; configuration (week programs, links)
        // can take a while at 1 % airtime. It goes ahead of other devices' commands that
        // are not on air yet, but stays behind its own device's (a group id must land first).
        if entry.out.frame.command == command::SET_TEMPERATURE {
            let dst = entry.out.frame.dst;
            let first_waiting = self
                .commands
                .iter()
                .position(|e| e.attempts == 0 && e.out.frame.command != command::SET_TEMPERATURE)
                .unwrap_or(self.commands.len());
            let after_own = self.commands.iter().rposition(|e| e.out.frame.dst == dst).map_or(0, |p| p + 1);
            self.commands.insert(first_waiting.max(after_own), entry);
        } else {
            self.commands.push_back(entry);
        }
    }

    /// Queue ahead of other commands (but behind one that is already on air).
    pub fn push_front(&mut self, out: Outgoing) {
        let seq = self.alloc_seq();
        let at = usize::from(self.commands.front().is_some_and(|e| e.attempts > 0));
        self.commands.insert(at, Entry { seq, out, attempts: 0, sent_at: None, park_on_fail: false });
    }

    fn alloc_seq(&mut self) -> u32 {
        self.next_seq = self.next_seq.wrapping_add(1);
        self.next_seq
    }

    fn refill(&mut self, now: Millis) {
        let elapsed = now.saturating_sub(self.credit_updated);
        // 10 ms per second, keep the remainder.
        let gained = (elapsed / 100) as u32;
        self.credit_ms = (self.credit_ms + gained).min(CREDIT_MAX_MS);
        self.credit_updated += gained as Millis * 100;
    }

    /// Spend airtime, or say when there will be enough.
    fn spend(&mut self, ms: u32, now: Millis) -> Result<(), Millis> {
        if !self.duty_cycle_limit {
            return Ok(());
        }
        if self.credit_ms >= ms {
            self.credit_ms -= ms;
            Ok(())
        } else {
            Err(now + (ms - self.credit_ms) as Millis * 100)
        }
    }

    /// Pick what to transmit. Call [`Self::transmitted`] when the radio is done with it.
    pub fn next(&mut self, now: Millis, events: &mut Vec<Event>) -> Next {
        self.refill(now);

        if let Some(out) = self.responses.pop_front() {
            // ACKs are owed to devices; send even when out of credit.
            let cost = out.airtime_ms();
            self.credit_ms = self.credit_ms.saturating_sub(cost);
            return Next::Send { id: TxId::Response, bytes: out.frame.encode(), long_preamble: out.long_preamble };
        }

        loop {
            let Some(head) = self.commands.front_mut() else {
                return Next::Idle;
            };
            if let Some(sent_at) = head.sent_at {
                let deadline = sent_at + ACK_TIMEOUT;
                if now < deadline {
                    return Next::WaitUntil(deadline);
                }
                if head.park_on_fail && head.attempts >= WOKEN_ATTEMPTS {
                    let mut e = self.commands.pop_front().expect("head exists");
                    log::info!(
                        "{} went back to sleep, command {} waits for it",
                        e.out.frame.dst,
                        command::name(e.out.frame.command)
                    );
                    e.out.long_preamble = true;
                    self.park(e.out);
                    continue;
                }
                if head.attempts >= MAX_ATTEMPTS {
                    let e = self.commands.pop_front().expect("head exists");
                    log::warn!(
                        "No ACK from {} for command {} after {} attempts",
                        e.out.frame.dst,
                        command::name(e.out.frame.command),
                        e.attempts
                    );
                    events.push(Event::GaveUp { dst: e.out.frame.dst, command: e.out.frame.command });
                    continue;
                }
            }
            let cost = head.out.airtime_ms();
            return match self.spend(cost, now) {
                Err(when) => Next::WaitUntil(when),
                Ok(()) => {
                    let head = self.commands.front_mut().expect("head exists");
                    // Logged when it goes out, not every time it waits for airtime.
                    if head.attempts > 0 {
                        log::info!(
                            "Retrying command {} to {} (attempt {})",
                            command::name(head.out.frame.command),
                            head.out.frame.dst,
                            head.attempts + 1
                        );
                    }
                    head.attempts += 1;
                    head.sent_at = None;
                    Next::Send {
                        id: TxId::Command(head.seq),
                        bytes: head.out.frame.encode(),
                        long_preamble: head.out.long_preamble,
                    }
                }
            };
        }
    }

    /// The radio finished sending. The ACK timeout starts now, not when sending started:
    /// the long preamble alone takes a second.
    pub fn transmitted(&mut self, id: TxId, now: Millis) {
        let TxId::Command(seq) = id else { return };
        let Some(pos) = self.commands.iter().position(|e| e.seq == seq) else { return };
        if self.commands[pos].out.expect_ack {
            self.commands[pos].sent_at = Some(now);
        } else {
            self.commands.remove(pos);
        }
    }

    /// An ACK from `from` for message `msgcnt`. Only matches a command we sent to that device.
    pub fn ack(&mut self, from: Address, msgcnt: u8, ok: bool, events: &mut Vec<Event>) -> bool {
        let Some(pos) = self.commands.iter().position(|e| {
            e.attempts > 0 && e.out.expect_ack && e.out.frame.dst == from && e.out.frame.msgcnt == msgcnt
        }) else {
            return false;
        };
        let e = self.commands.remove(pos).expect("position is valid");
        let (dst, command, payload) = (e.out.frame.dst, e.out.frame.command, e.out.frame.payload);
        events.push(if ok { Event::Acked { dst, command, payload } } else { Event::Nacked { dst, command, payload } });
        true
    }

    /// Drop everything queued for one device (it was removed or renamed away).
    pub fn forget(&mut self, dst: Address) {
        self.commands.retain(|e| e.out.frame.dst != dst);
        self.parked.retain(|p| p.frame.dst != dst);
    }

    /// Frames waiting to be sent, for patching time payloads right before sending.
    pub fn head_frame_mut(&mut self) -> Option<&mut Frame> {
        if let Some(r) = self.responses.front_mut() {
            return Some(&mut r.frame);
        }
        self.commands.front_mut().map(|e| &mut e.out.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const ME: Address = Address([0x12, 0x34, 0x56]);
    const DEV: Address = Address([0xAA, 0xBB, 0xCC]);

    fn frame(msgcnt: u8, command: u8, payload: Vec<u8>) -> Frame {
        Frame { msgcnt, flags: 0, command, src: ME, dst: DEV, group: 0, payload }
    }

    fn sent_id(n: Next) -> TxId {
        match n {
            Next::Send { id, .. } => id,
            other => panic!("expected send, got {other:?}"),
        }
    }

    #[test]
    fn command_waits_for_ack_then_queue_moves_on() {
        let mut q = TxQueue::new(false, 0);
        let mut ev = vec![];
        q.push(Outgoing::command(frame(1, command::SET_TEMPERATURE, vec![0x40])));
        q.push(Outgoing::command(frame(2, command::SET_GROUP_ID, vec![1])));
        let id = sent_id(q.next(0, &mut ev));
        q.transmitted(id, 1_100);
        assert_eq!(q.next(1_200, &mut ev), Next::WaitUntil(4_100));
        assert!(!q.ack(DEV, 9, true, &mut ev), "wrong msgcnt");
        assert!(!q.ack(Address([1, 1, 1]), 1, true, &mut ev), "wrong device");
        assert!(q.ack(DEV, 1, true, &mut ev));
        assert_eq!(ev, vec![Event::Acked { dst: DEV, command: command::SET_TEMPERATURE, payload: vec![0x40] }]);
        assert!(matches!(q.next(1_300, &mut ev), Next::Send { .. }));
    }

    #[test]
    fn ack_with_empty_queue_is_harmless() {
        // The old firmware called queue.front() on an empty std::queue here.
        let mut q = TxQueue::new(false, 0);
        assert!(!q.ack(DEV, 1, true, &mut vec![]));
    }

    #[test]
    fn retries_then_gives_up() {
        let mut q = TxQueue::new(false, 0);
        let mut ev = vec![];
        q.push(Outgoing::command(frame(1, command::SET_TEMPERATURE, vec![0x40])));
        let mut now = 0;
        for _ in 0..MAX_ATTEMPTS {
            let id = sent_id(q.next(now, &mut ev));
            q.transmitted(id, now + 1_000);
            now += 1_000 + ACK_TIMEOUT;
        }
        assert_eq!(q.next(now, &mut ev), Next::Idle);
        assert_eq!(ev, vec![Event::GaveUp { dst: DEV, command: command::SET_TEMPERATURE }]);
    }

    #[test]
    fn responses_jump_the_queue_even_while_waiting_for_an_ack() {
        let mut q = TxQueue::new(false, 0);
        let mut ev = vec![];
        q.push(Outgoing::command(frame(1, command::SET_TEMPERATURE, vec![0x40])));
        let id = sent_id(q.next(0, &mut ev));
        q.transmitted(id, 1_000);
        q.push_response(Outgoing::response(frame(77, command::ACK, vec![1])));
        match q.next(1_100, &mut ev) {
            Next::Send { id: TxId::Response, long_preamble: false, bytes } => assert_eq!(bytes[1], 77),
            other => panic!("{other:?}"),
        }
        assert_eq!(q.next(1_200, &mut ev), Next::WaitUntil(4_000));
    }

    #[test]
    fn broadcast_and_no_ack_commands_leave_after_sending() {
        let mut q = TxQueue::new(false, 0);
        let mut f = frame(1, command::TIME_INFORMATION, vec![0; 5]);
        f.dst = Address::BROADCAST;
        q.push(Outgoing::command(f));
        let id = sent_id(q.next(0, &mut vec![]));
        q.transmitted(id, 1_000);
        assert!(q.is_empty());
    }

    #[test]
    fn newer_unsent_command_replaces_older() {
        let mut q = TxQueue::new(false, 0);
        q.push(Outgoing::command(frame(1, command::SET_TEMPERATURE, vec![0x40 | 40])));
        q.push(Outgoing::command(frame(2, command::SET_TEMPERATURE, vec![0x40 | 42])));
        assert_eq!(q.len(), 1);
        match q.next(0, &mut vec![]) {
            Next::Send { bytes, .. } => assert_eq!(bytes[11], 0x40 | 42),
            other => panic!("{other:?}"),
        }
        // Different schedule days are different messages.
        q.push(Outgoing::command(frame(3, command::CONFIG_WEEK_PROFILE, vec![0])));
        q.push(Outgoing::command(frame(4, command::CONFIG_WEEK_PROFILE, vec![1])));
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn group_set_and_remove_replace_each_other() {
        let mut q = TxQueue::new(false, 0);
        q.push(Outgoing::command(frame(1, command::SET_GROUP_ID, vec![3])));
        q.push(Outgoing::command(frame(2, command::REMOVE_GROUP_ID, vec![0])));
        q.push(Outgoing::command(frame(3, command::SET_GROUP_ID, vec![1])));
        assert_eq!(q.len(), 1, "only the latest group goes out");
        match q.next(0, &mut vec![]) {
            Next::Send { bytes, .. } => assert_eq!((bytes[3], bytes[11]), (command::SET_GROUP_ID, 1)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn duty_cycle_credit() {
        let mut q = TxQueue::new(true, 0);
        let mut ev = vec![];
        // Each long-preamble command costs 1000 + 12*8 = 1096 ms; 9000 ms buys 8.
        for i in 0..9u8 {
            let mut f = frame(i, command::SET_TEMPERATURE, vec![0x40]);
            f.dst = Address([0, 0, i + 1]);
            let mut out = Outgoing::command(f);
            out.expect_ack = false;
            q.push(out);
        }
        for _ in 0..8 {
            let id = sent_id(q.next(0, &mut ev));
            q.transmitted(id, 0);
        }
        assert_eq!(q.credit_ms(), 9_000 - 8 * 1_096);
        let Next::WaitUntil(when) = q.next(0, &mut ev) else { panic!() };
        assert_eq!(when, (1_096 - 232) * 100);
        assert!(matches!(q.next(when, &mut ev), Next::Send { .. }));
    }

    #[test]
    fn push_front_does_not_preempt_a_command_on_air() {
        let mut q = TxQueue::new(false, 0);
        q.push(Outgoing::command(frame(1, command::SET_TEMPERATURE, vec![0x40])));
        let id = sent_id(q.next(0, &mut vec![]));
        q.transmitted(id, 0);
        q.push_front(Outgoing::command(frame(2, command::PAIR_PONG, vec![0])));
        assert!(q.ack(DEV, 1, true, &mut vec![]));
        match q.next(10, &mut vec![]) {
            Next::Send { bytes, .. } => assert_eq!(bytes[3], command::PAIR_PONG),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parked_until_the_device_wakes_and_parked_again_if_missed() {
        let mut q = TxQueue::new(false, 0);
        let mut ev = vec![];
        q.park(Outgoing::command(frame(1, command::ADD_LINK_PARTNER, vec![1, 2, 3, 1])));
        assert_eq!(q.next(0, &mut ev), Next::Idle);
        assert_eq!(q.parked(), 1);
        q.wake(Address([9, 9, 9]));
        assert_eq!(q.next(0, &mut ev), Next::Idle, "another device woke");
        q.wake(DEV);
        let Next::Send { id, long_preamble, .. } = q.next(0, &mut ev) else { panic!() };
        assert!(!long_preamble, "the device is awake");
        q.transmitted(id, 100);
        // No ACK: one retry, then parked again instead of given up.
        let id = sent_id(q.next(3_200, &mut ev));
        q.transmitted(id, 3_300);
        assert_eq!(q.next(6_400, &mut ev), Next::Idle);
        assert_eq!(q.parked(), 1);
        assert!(ev.is_empty());
        q.wake(DEV);
        let id = sent_id(q.next(7_000, &mut ev));
        q.transmitted(id, 7_100);
        assert!(q.ack(DEV, 1, true, &mut ev));
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn temperature_changes_jump_the_queue() {
        let mut q = TxQueue::new(false, 0);
        let mut ev = vec![];
        let other = Address([1, 2, 3]);
        let to = |dst, msgcnt, command| Outgoing::command(Frame { dst, ..frame(msgcnt, command, vec![0x02]) });
        q.push(to(other, 1, command::CONFIG_WEEK_PROFILE));
        q.push(to(other, 2, command::CONFIG_WEEK_PROFILE));
        q.push(to(DEV, 3, command::SET_TEMPERATURE));
        let Next::Send { bytes, .. } = q.next(0, &mut ev) else { panic!() };
        assert_eq!(bytes[3], command::SET_TEMPERATURE, "ahead of another device's program");
        // Behind its own device's earlier commands.
        let mut q = TxQueue::new(false, 0);
        q.push(to(DEV, 1, command::SET_GROUP_ID));
        q.push(to(DEV, 2, command::SET_TEMPERATURE));
        let Next::Send { bytes, .. } = q.next(0, &mut ev) else { panic!() };
        assert_eq!(bytes[3], command::SET_GROUP_ID);
    }
}

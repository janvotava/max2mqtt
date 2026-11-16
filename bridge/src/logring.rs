//! The last lines of the log, kept in RAM for the web UI.
//!
//! The lines are text in one buffer of a fixed size, each ended by a line feed, and are
//! numbered from 1 so a reader can ask for what came after the last line it has. One
//! allocation for good: a `String` per line took half as much again (spare capacity, the
//! allocator's bookkeeping) and left holes in the heap as lines came and went.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub struct LogRing {
    text: VecDeque<u8>,
    capacity: usize,
    /// Sequence number of the oldest line in `text`.
    first: u32,
    /// Sequence number the next line gets.
    next: u32,
}

/// What [`LogRing::copy_after`] put into the buffer.
#[derive(Debug, PartialEq, Eq)]
pub struct Copied {
    /// Sequence number of the first line copied.
    pub first: u32,
    pub lines: u32,
    /// The newest line the log has: the number to ask after next time.
    pub last: u32,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    /// Keeps nothing until [`LogRing::allocate`] gave it room.
    pub const fn new() -> Self {
        LogRing { text: VecDeque::new(), capacity: 0, first: 1, next: 1 }
    }

    /// Take the memory: `bytes` of log from now on.
    pub fn allocate(&mut self, bytes: usize) {
        self.text.reserve_exact(bytes.saturating_sub(self.text.len()));
        self.capacity = bytes;
    }

    /// Add a line, dropping the oldest ones to make room.
    pub fn push(&mut self, line: &str) {
        if self.capacity == 0 {
            return;
        }
        // Cut what could never fit, at a character boundary.
        let mut len = line.len().min(self.capacity - 1);
        while !line.is_char_boundary(len) {
            len -= 1;
        }
        let line = &line.as_bytes()[..len];
        while self.text.len() + line.len() + 1 > self.capacity {
            while let Some(c) = self.text.pop_front() {
                if c == b'\n' {
                    break;
                }
            }
            self.first += 1;
        }
        // One line per entry, whatever the message contains.
        self.text.extend(line.iter().map(|&c| if c == b'\n' { b' ' } else { c }));
        self.text.push_back(b'\n');
        self.next += 1;
    }

    /// Copy whole lines with a sequence number from `after + 1` on into `out` (emptied
    /// first), each ended by a line feed: as many as fit in `max` bytes, at least one if
    /// there is one. For reading the log piece by piece, without holding all of it twice.
    pub fn copy_after(&self, after: u32, out: &mut Vec<u8>, max: usize) -> Copied {
        out.clear();
        let last = self.next - 1;
        // Lines the reader already has. A reader ahead of us (we restarted) gets nothing.
        let skip = after.saturating_add(1).saturating_sub(self.first).min(self.next - self.first);
        let first = self.first + skip;
        let mut lines = 0;
        let mut text = self.text.iter().copied();
        for _ in 0..skip {
            text.by_ref().find(|&c| c == b'\n');
        }
        let mut text = text.peekable();
        while text.peek().is_some() {
            let line = text.clone().position(|c| c == b'\n').map_or(0, |i| i + 1);
            if lines > 0 && out.len() + line > max {
                break;
            }
            out.extend(text.by_ref().take(line));
            lines += 1;
        }
        Copied { first, lines, last }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(ring: &LogRing, after: u32, max: usize) -> (Vec<(u32, String)>, u32) {
        let mut out = Vec::new();
        let c = ring.copy_after(after, &mut out, max);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count() as u32, c.lines, "whole lines only");
        ((c.first..).zip(text.lines().map(String::from)).collect(), c.last)
    }

    /// Everything after `after`, read in pieces of `max` bytes as the web server does.
    fn read_all(ring: &LogRing, mut after: u32, max: usize) -> (Vec<(u32, String)>, u32) {
        let mut all = Vec::new();
        loop {
            let (lines, last) = read(ring, after, max);
            let Some((n, _)) = lines.last() else { return (all, last) };
            after = *n;
            all.extend(lines);
        }
    }

    #[test]
    fn keeps_nothing_before_it_has_memory() {
        let mut ring = LogRing::new();
        ring.push("too early");
        assert_eq!(read(&ring, 0, 100), (vec![], 0));
    }

    #[test]
    fn numbers_lines_and_reads_after_a_number() {
        let mut ring = LogRing::new();
        ring.allocate(64);
        ring.push("one");
        ring.push("two");
        ring.push("three");
        let line = |n: u32, s: &str| (n, String::from(s));
        assert_eq!(read(&ring, 0, 100), (vec![line(1, "one"), line(2, "two"), line(3, "three")], 3));
        assert_eq!(read(&ring, 2, 100), (vec![line(3, "three")], 3));
        assert_eq!(read(&ring, 3, 100), (vec![], 3));
        // A page that followed the log before a restart: `last` below its number resets it.
        assert_eq!(read(&ring, 500, 100), (vec![], 3));
        assert_eq!(read(&ring, u32::MAX, 100), (vec![], 3));
    }

    #[test]
    fn reads_in_pieces_of_whole_lines() {
        let mut ring = LogRing::new();
        ring.allocate(64);
        ring.push("one");
        ring.push("two");
        ring.push("three");
        let line = |n: u32, s: &str| (n, String::from(s));
        // "one\ntwo\n" is 8 bytes.
        assert_eq!(read(&ring, 0, 8).0, vec![line(1, "one"), line(2, "two")]);
        assert_eq!(read(&ring, 0, 7).0, vec![line(1, "one")]);
        // A line longer than the piece still comes, alone.
        assert_eq!(read(&ring, 2, 1).0, vec![line(3, "three")]);
        assert_eq!(read_all(&ring, 0, 1).0.len(), 3);
    }

    #[test]
    fn drops_the_oldest_lines_and_never_grows() {
        let mut ring = LogRing::new();
        ring.allocate(100);
        let capacity = ring.text.capacity();
        let mut model: Vec<(u32, String)> = Vec::new();
        for n in 1..=500u32 {
            // Lengths that wrap around the buffer at every possible offset.
            let line = format!("{n} {}", "x".repeat((n as usize * 7) % 23));
            ring.push(&line);
            model.push((n, line));
            while model.iter().map(|(_, l)| l.len() + 1).sum::<usize>() > 100 {
                model.remove(0);
            }
            for max in [1, 16, 1000] {
                assert_eq!(read_all(&ring, 0, max), (model.clone(), n));
            }
            let after = n.saturating_sub(2);
            let recent: Vec<_> = model.iter().filter(|(k, _)| *k > after).cloned().collect();
            assert_eq!(read_all(&ring, after, 16), (recent, n));
        }
        assert_eq!(ring.text.capacity(), capacity, "one allocation for good");
        assert!(capacity < 110);
    }

    #[test]
    fn long_and_multi_line_messages_stay_one_line() {
        let mut ring = LogRing::new();
        ring.allocate(16);
        ring.push("kept");
        ring.push("a\nb");
        assert_eq!(read(&ring, 1, 100).0, vec![(2, String::from("a b"))]);
        // Cut to what fits, not in the middle of a character (each of these is 2 bytes).
        ring.push(&"é".repeat(20));
        assert_eq!(read(&ring, 0, 100), (vec![(3, "é".repeat(7))], 3));
    }
}

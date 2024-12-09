//! CC1101 driver with the MAX! register set (from culfw / the old `MaxCC1101.cpp`).
//!
//! Differences from the C++ driver:
//! - Nothing runs in an interrupt. The old firmware read the FIFO over SPI inside the GDO2
//!   ISR, racing with transmissions from the main loop on the same bus.
//! - Every wait has a timeout. A missing or dead module used to hang the firmware in
//!   `while (MARCSTATE != IDLE)` loops.
//! - The RX overflow check had an operator precedence bug (`x & 0x1F != 0x11`, which is
//!   `x & 1`), so it rarely detected an overflow.
//! - Reads the length byte first and then exactly one frame, instead of the whole FIFO.

use alloc::vec::Vec;

use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::spi::SpiBus;
use esp_hal::gpio::Output;

const WRITE_BURST: u8 = 0x40;
const READ_SINGLE: u8 = 0x80;
const READ_BURST: u8 = 0xC0;

mod reg {
    pub const IOCFG2: u8 = 0x00;
    pub const IOCFG0: u8 = 0x02;
    pub const SYNC1: u8 = 0x04;
    pub const SYNC0: u8 = 0x05;
    pub const PKTLEN: u8 = 0x06;
    pub const PKTCTRL1: u8 = 0x07;
    pub const FSCTRL1: u8 = 0x0B;
    pub const FSCTRL0: u8 = 0x0C;
    pub const FREQ2: u8 = 0x0D;
    pub const FREQ1: u8 = 0x0E;
    pub const FREQ0: u8 = 0x0F;
    pub const MDMCFG4: u8 = 0x10;
    pub const MDMCFG3: u8 = 0x11;
    pub const MDMCFG2: u8 = 0x12;
    pub const MDMCFG1: u8 = 0x13;
    pub const DEVIATN: u8 = 0x15;
    pub const MCSM2: u8 = 0x16;
    pub const MCSM1: u8 = 0x17;
    pub const MCSM0: u8 = 0x18;
    pub const FOCCFG: u8 = 0x19;
    pub const AGCCTRL2: u8 = 0x1B;
    pub const WOREVT1: u8 = 0x1E;
    pub const WOREVT0: u8 = 0x1F;
    pub const WORCTRL: u8 = 0x20;
    pub const FREND1: u8 = 0x21;
    pub const FSCAL1: u8 = 0x25;
    pub const FSCAL0: u8 = 0x26;
    pub const FSTEST: u8 = 0x29;
    pub const TEST2: u8 = 0x2C;
    pub const TEST1: u8 = 0x2D;
    pub const PATABLE: u8 = 0x3E;
    pub const FIFO: u8 = 0x3F;
    // Status registers (read with READ_BURST).
    pub const PARTNUM: u8 = 0x30;
    pub const FREQEST: u8 = 0x32;
    pub const PKTSTATUS: u8 = 0x38;
    pub const VERSION: u8 = 0x31;
    pub const RSSI: u8 = 0x34;
    pub const MARCSTATE: u8 = 0x35;
    pub const RXBYTES: u8 = 0x3B;
}

mod strobe {
    pub const SRES: u8 = 0x30;
    pub const SCAL: u8 = 0x33;
    pub const SRX: u8 = 0x34;
    pub const STX: u8 = 0x35;
    pub const SIDLE: u8 = 0x36;
    pub const SFRX: u8 = 0x3A;
    pub const SFTX: u8 = 0x3B;
}

mod marc {
    pub const IDLE: u8 = 0x01;
    pub const RX: u8 = 0x0D;
    pub const RXFIFO_OVERFLOW: u8 = 0x11;
    pub const TXFIFO_UNDERFLOW: u8 = 0x16;
}

/// The MAX! register set: 868.3 MHz, 2-FSK, ~10 kbit/s, sync word C626, variable length,
/// CRC with auto flush, status bytes appended. GDO2 goes high when a frame with a good CRC
/// is in the FIFO and low again when the first byte is read.
const MAX_REGISTERS: &[(u8, u8)] = &[
    (reg::IOCFG2, 0x07),
    (reg::IOCFG0, 0x46),
    (reg::SYNC1, 0xC6),
    (reg::SYNC0, 0x26),
    (reg::FSCTRL1, 0x06),
    (reg::MDMCFG4, 0xC8),
    (reg::MDMCFG3, 0x93),
    (reg::MDMCFG2, 0x03),
    (reg::MDMCFG1, 0x22),
    (reg::DEVIATN, 0x34),
    (reg::MCSM1, 0x3F),
    (reg::MCSM0, 0x28),
    (reg::FOCCFG, 0x16),
    (reg::AGCCTRL2, 0x43),
    (reg::FREND1, 0x56),
    (reg::FSCAL1, 0x00),
    (reg::FSCAL0, 0x11),
    (reg::FREQ2, 0x21),
    (reg::FREQ1, 0x65),
    (reg::FREQ0, 0x6A),
    (reg::PKTCTRL1, 0x0C),
    (reg::MCSM2, 0x07),
    (reg::WORCTRL, 0xF8),
    (reg::WOREVT1, 0x87),
    (reg::WOREVT0, 0x6B),
    (reg::FSTEST, 0x59),
    (reg::TEST2, 0x81),
    (reg::TEST1, 0x35),
    (reg::PATABLE, 0xC3),
    (reg::PKTLEN, 30),
];

/// Largest frame we accept or send, including the length byte (the FIFO holds 64 bytes,
/// two of which are the appended status).
const MAX_FRAME: usize = 62;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Spi,
    /// The chip did not reach the expected state in time.
    Timeout(u8),
    /// PARTNUM/VERSION do not look like a CC1101: nothing on the bus.
    NotFound(u8, u8),
}

pub struct Cc1101<'d, S> {
    spi: S,
    cs: Output<'d>,
    freq_offset: i8,
}

impl<'d, S: SpiBus> Cc1101<'d, S> {
    pub fn new(spi: S, cs: Output<'d>, freq_offset: i8) -> Self {
        Cc1101 { spi, cs, freq_offset }
    }

    async fn transfer(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.cs.set_low();
        let r = self.spi.transfer_in_place(buf).await;
        let r = match r {
            Ok(()) => self.spi.flush().await,
            Err(e) => Err(e),
        };
        self.cs.set_high();
        r.map_err(|_| Error::Spi)
    }

    async fn strobe(&mut self, cmd: u8) -> Result<u8, Error> {
        let mut b = [cmd];
        self.transfer(&mut b).await?;
        Ok(b[0])
    }

    async fn write_reg(&mut self, addr: u8, value: u8) -> Result<(), Error> {
        self.transfer(&mut [addr, value]).await
    }

    async fn read_status(&mut self, addr: u8) -> Result<u8, Error> {
        let mut b = [addr | READ_BURST, 0];
        self.transfer(&mut b).await?;
        Ok(b[1])
    }

    /// Errata: status registers that change while being read can return garbage. Read
    /// until two consecutive reads agree.
    async fn read_status_stable(&mut self, addr: u8) -> Result<u8, Error> {
        let mut last = self.read_status(addr).await?;
        for _ in 0..16 {
            let v = self.read_status(addr).await?;
            if v == last {
                return Ok(v);
            }
            last = v;
        }
        Ok(last)
    }

    async fn marcstate(&mut self) -> Result<u8, Error> {
        Ok(self.read_status_stable(reg::MARCSTATE).await? & 0x1F)
    }

    async fn wait_state(&mut self, want: u8, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.marcstate().await?;
            if s == want {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(Error::Timeout(s));
            }
            Timer::after_millis(1).await;
        }
    }

    async fn reset(&mut self) -> Result<(), Error> {
        // Manual power-on reset sequence from the datasheet (19.1.2).
        self.cs.set_high();
        Timer::after_micros(5).await;
        self.cs.set_low();
        Timer::after_micros(10).await;
        self.cs.set_high();
        Timer::after_micros(45).await;
        self.cs.set_low();
        // Wait for SO to go low (crystal running). We cannot sample MISO while the SPI
        // peripheral owns it, so wait generously instead.
        Timer::after_micros(500).await;
        let mut b = [strobe::SRES];
        let r = self.spi.transfer_in_place(&mut b).await;
        Timer::after_millis(5).await;
        self.cs.set_high();
        r.map_err(|_| Error::Spi)
    }

    /// The configured crystal correction (FSCTRL0).
    pub fn freq_offset(&self) -> i8 {
        self.freq_offset
    }

    /// Reset and configure for MAX!, then enter RX.
    pub async fn init(&mut self) -> Result<(), Error> {
        self.configure(self.freq_offset, false).await
    }

    /// For measuring the module's frequency error: a 203 kHz receive filter (instead of
    /// 101 kHz) and offset compensation over half of it, so packets still decode from a
    /// module that is far off, with `fsctrl0` as the correction. Read [`Self::freq_estimate`]
    /// while a packet comes in.
    pub async fn init_wide(&mut self, fsctrl0: i8) -> Result<(), Error> {
        self.configure(fsctrl0, true).await
    }

    /// Sync word seen, a packet is coming in (PKTSTATUS bit 3).
    pub async fn in_packet(&mut self) -> Result<bool, Error> {
        Ok(self.read_status(reg::PKTSTATUS).await? & 0x08 != 0)
    }

    /// The demodulator's estimate of the carrier offset, in FSCTRL0 steps (1.587 kHz).
    pub async fn freq_estimate(&mut self) -> Result<i8, Error> {
        Ok(self.read_status(reg::FREQEST).await? as i8)
    }

    async fn configure(&mut self, fsctrl0: i8, wide: bool) -> Result<(), Error> {
        self.reset().await?;
        let (part, version) = (self.read_status(reg::PARTNUM).await?, self.read_status(reg::VERSION).await?);
        // CC1101 reports PARTNUM 0 and VERSION 0x04 or 0x14; an empty bus reads 0x00 or 0xFF.
        if version == 0x00 || version == 0xFF {
            return Err(Error::NotFound(part, version));
        }
        log::info!("CC1101 partnum {part:#04x} version {version:#04x}");
        for &(addr, value) in MAX_REGISTERS {
            self.write_reg(addr, value).await?;
        }
        if wide {
            self.write_reg(reg::MDMCFG4, 0x88).await?;
            self.write_reg(reg::FOCCFG, 0x17).await?;
        }
        if fsctrl0 != 0 {
            log::info!("CC1101 frequency correction {fsctrl0} steps");
            self.write_reg(reg::FSCTRL0, fsctrl0 as u8).await?;
        }
        self.strobe(strobe::SIDLE).await?;
        self.strobe(strobe::SCAL).await?;
        self.wait_state(marc::IDLE, Duration::from_millis(100)).await?;
        self.strobe(strobe::SFRX).await?;
        self.strobe(strobe::SRX).await?;
        self.wait_state(marc::RX, Duration::from_millis(100)).await
    }

    async fn recover_rx(&mut self) -> Result<(), Error> {
        self.strobe(strobe::SIDLE).await?;
        self.strobe(strobe::SFRX).await?;
        self.strobe(strobe::SFTX).await?;
        self.strobe(strobe::SRX).await?;
        Ok(())
    }

    /// Radio state for diagnostics: MARCSTATE, bytes in the RX FIFO (bit 7: overflow), and
    /// the current signal strength in dBm (the noise floor when nothing is transmitting).
    pub async fn status(&mut self) -> Result<(u8, u8, i16), Error> {
        let marc = self.marcstate().await?;
        let rxbytes = self.read_status_stable(reg::RXBYTES).await?;
        let rssi = self.read_status_stable(reg::RSSI).await?;
        Ok((marc, rxbytes, max2mqtt_bridge::protocol::rssi_dbm(rssi)))
    }

    /// Read one frame from the RX FIFO: length byte, frame, RSSI, LQI. `None` if the FIFO
    /// held nothing usable (it is flushed in that case).
    pub async fn receive(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let rxbytes = self.read_status_stable(reg::RXBYTES).await?;
        let available = (rxbytes & 0x7F) as usize;
        if rxbytes & 0x80 != 0 || self.marcstate().await? == marc::RXFIFO_OVERFLOW {
            log::warn!("CC1101 RX FIFO overflow");
            self.recover_rx().await?;
            return Ok(None);
        }
        if available == 0 {
            return Ok(None);
        }
        let mut len = [reg::FIFO | READ_SINGLE, 0];
        self.transfer(&mut len).await?;
        let frame_len = len[1] as usize + 3;
        if frame_len > MAX_FRAME + 2 || frame_len > available {
            log::warn!("CC1101 bad frame length {} with {available} bytes in FIFO", len[1]);
            self.recover_rx().await?;
            return Ok(None);
        }
        // Burst-read the remaining len + 2 bytes. Slot 0 carries the command byte and
        // receives the status byte, so it is replaced by the length byte afterwards.
        let mut buf = alloc::vec![0u8; frame_len];
        buf[0] = reg::FIFO | READ_BURST;
        self.transfer(&mut buf).await?;
        buf[0] = len[1];
        Ok(Some(buf))
    }

    /// Send a frame (starting with its length byte). A long preamble (1 s) wakes devices
    /// that only listen periodically. Returns to RX when done.
    pub async fn transmit(&mut self, frame: &[u8], long_preamble: bool) -> Result<(), Error> {
        let frame = &frame[..frame.len().min(MAX_FRAME)];
        self.strobe(strobe::SIDLE).await?;
        self.strobe(strobe::SFTX).await?;

        let mut burst = Vec::with_capacity(frame.len() + 1);
        burst.push(reg::FIFO | WRITE_BURST);
        burst.extend_from_slice(frame);

        if long_preamble {
            // With an empty FIFO the CC1101 keeps sending preamble until data arrives.
            self.strobe(strobe::STX).await?;
            Timer::after_millis(1000).await;
            self.transfer(&mut burst).await?;
        } else {
            self.transfer(&mut burst).await?;
            self.strobe(strobe::STX).await?;
        }

        // ~10 kbit/s: 62 bytes take about 50 ms. TXOFF_MODE switches back to RX.
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            Timer::after_millis(5).await;
            match self.marcstate().await? {
                marc::RX | marc::IDLE => break,
                marc::TXFIFO_UNDERFLOW => {
                    log::warn!("CC1101 TX FIFO underflow");
                    break;
                }
                s if Instant::now() > deadline => {
                    self.recover_rx().await?;
                    return Err(Error::Timeout(s));
                }
                _ => {}
            }
        }
        if self.marcstate().await? != marc::RX {
            self.recover_rx().await?;
        }
        Ok(())
    }
}

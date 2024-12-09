//! Radio task: owns the CC1101. Receives when GDO2 says a frame is waiting, transmits what
//! the bridge queues, right away for ACKs.

use embassy_futures::select::{Either, Either4, select, select4};
use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::Async;
use esp_hal::gpio::Input;
use esp_hal::spi::master::Spi;
use max2mqtt_bridge::tx::Next;

use crate::cc1101::Cc1101;
use crate::shared::{CALIBRATE, CALIBRATION, RADIO_REINIT, TX_KICK, ctx, dispatch, with_bridge};

pub type Radio = Cc1101<'static, Spi<'static, Async>>;

async fn init(radio: &mut Radio) {
    loop {
        match radio.init().await {
            Ok(()) => {
                log::info!("CC1101 ready, listening");
                return;
            }
            Err(e) => {
                log::error!("CC1101 init failed: {e:?}. Check the module and wiring; retrying in 10 s");
                Timer::after_secs(10).await;
            }
        }
    }
}

/// Send everything that is due. Returns when the next frame becomes due, if any.
async fn transmit_due(radio: &mut Radio) -> Option<Instant> {
    loop {
        match with_bridge(|b| b.next_tx(ctx())).await {
            Next::Send { id, bytes, long_preamble } => {
                if let Err(e) = radio.transmit(&bytes, long_preamble).await {
                    log::warn!("CC1101 transmit failed: {e:?}");
                }
                let now = ctx().now;
                with_bridge(|b| b.transmitted(id, now)).await;
            }
            Next::WaitUntil(ms) => return Some(Instant::from_millis(ms)),
            Next::Idle => return None,
        }
    }
}

#[embassy_executor::task]
pub async fn radio_task(mut radio: Radio, mut gdo2: Input<'static>) {
    init(&mut radio).await;
    let mut next_tx = transmit_due(&mut radio).await;
    let mut status = Ticker::every(Duration::from_secs(60));
    loop {
        let tx_due = async {
            match next_tx {
                Some(at) => Timer::at(at).await,
                None => core::future::pending().await,
            }
        };
        // GDO2 is level-triggered on purpose: a frame that arrived while we were busy
        // transmitting is still flagged, where an edge would have been missed.
        let event = select4(gdo2.wait_for_high(), TX_KICK.wait(), tx_due, RADIO_REINIT.wait());
        let event = match select(event, select(status.next(), CALIBRATE.wait())).await {
            Either::First(event) => event,
            Either::Second(Either::Second(start)) => {
                if start {
                    calibrate(&mut radio, &mut gdo2).await;
                    init(&mut radio).await;
                }
                next_tx = transmit_due(&mut radio).await;
                continue;
            }
            Either::Second(Either::First(())) => {
                match radio.status().await {
                    Ok((marc, rxbytes, rssi)) => {
                        // Normal: listening (RX) with GDO2 low. Anything else is worth a warning.
                        let healthy = marc == 0x0D && rxbytes & 0x80 == 0 && !gdo2.is_high();
                        let level = if healthy { log::Level::Debug } else { log::Level::Warn };
                        log::log!(
                            level,
                            "Radio: state {marc:#04x}{}, {} bytes in FIFO{}, signal {rssi} dBm, GDO2 {}",
                            if marc == 0x0D { " (RX)" } else { "" },
                            rxbytes & 0x7F,
                            if rxbytes & 0x80 != 0 { " (overflow)" } else { "" },
                            if gdo2.is_high() { "high" } else { "low" },
                        );
                    }
                    Err(e) => log::warn!("Radio status failed: {e:?}"),
                }
                continue;
            }
        };
        match event {
            Either4::First(()) => match radio.receive().await {
                Ok(Some(raw)) => {
                    let effects = with_bridge(|b| b.handle_radio(&raw, ctx())).await;
                    dispatch(effects);
                }
                Ok(None) => {
                    if gdo2.is_high() {
                        // GDO2 stuck high without data (module missing or misconfigured):
                        // do not spin.
                        Timer::after_millis(50).await;
                    }
                }
                Err(e) => log::warn!("CC1101 receive failed: {e:?}"),
            },
            Either4::Second(()) | Either4::Third(()) => {}
            Either4::Fourth(()) => init(&mut radio).await,
        }
        next_tx = transmit_due(&mut radio).await;
    }
}

/// Corrections tried in turn while nothing decodes, after the one in use: a module more
/// than ~60 kHz off may need a head start even with the wide filter.
const SEARCH: [i8; 3] = [0, 50, -50];
/// Listen again centred on the measurement once it is this far from where we listen:
/// signals near the filter edge decode only when strong, and estimate worse.
const RECENTRE: i32 = 8;
const SEARCH_STEP: Duration = Duration::from_secs(90);
const CALIBRATION_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Measure the module's frequency error from real MAX! packets, like the old C++ check tool:
/// listen with a wide filter, average the demodulator's offset estimate over each packet,
/// and add the correction listened with. Frames still reach the bridge; sending waits.
async fn calibrate(radio: &mut Radio, gdo2: &mut Input<'static>) {
    // Start where the module is believed to be; then a well-set board hears everything.
    let mut search: heapless::Vec<i8, 4> = heapless::Vec::new();
    let _ = search.push(radio.freq_offset());
    for s in SEARCH {
        if !search.contains(&s) {
            let _ = search.push(s);
        }
    }
    log::info!("Measuring the CC1101 frequency error: waiting for MAX! packets");
    CALIBRATION.lock(|c| {
        let mut c = c.borrow_mut();
        c.running = true;
        c.packets.clear();
        c.started_at = Instant::now().as_secs();
    });
    let started = Instant::now();
    let mut step = 0;
    let mut listening_at = search[0];
    let mut locked = false;
    let mut since = Instant::now();
    let (mut in_packet, mut sync_at, mut sum, mut samples) = (false, Instant::now(), 0i32, 0i32);
    if let Err(e) = radio.init_wide(listening_at).await {
        log::warn!("CC1101: {e:?}");
    }
    CALIBRATION.lock(|c| c.borrow_mut().listening_at = listening_at);

    while started.elapsed() < CALIBRATION_LIMIT {
        if CALIBRATE.signaled() && !CALIBRATE.wait().await {
            break;
        }
        match radio.in_packet().await {
            Ok(true) => {
                if !in_packet {
                    (in_packet, sync_at, sum, samples) = (true, Instant::now(), 0, 0);
                }
                // Let the estimate settle after the sync word.
                if sync_at.elapsed() >= Duration::from_millis(2)
                    && let Ok(f) = radio.freq_estimate().await
                {
                    sum += f as i32;
                    samples += 1;
                }
            }
            Ok(false) => in_packet = false,
            Err(_) => {}
        }
        if gdo2.is_high() {
            if let Ok(Some(raw)) = radio.receive().await {
                if samples > 0
                    && let Ok(rx) = max2mqtt_bridge::protocol::decode_rx(&raw)
                {
                    // Integer division rounded to the nearest step.
                    let mean = (2 * sum + samples * sum.signum()) / (2 * samples);
                    let total = (listening_at as i32 + mean).clamp(-127, 127) as i8;
                    let src = alloc::format!("{}", rx.frame.src);
                    let suggested = CALIBRATION.lock(|c| {
                        let mut c = c.borrow_mut();
                        c.packets.push((src.clone(), rx.rssi_dbm, total));
                        c.suggested()
                    });
                    log::info!(
                        "Frequency: packet from {src} ({} dBm) is {total:+} steps off; correction so far {:?}",
                        rx.rssi_dbm,
                        suggested
                    );
                    locked = true;
                    since = Instant::now();
                    if let Some(s) = suggested
                        && (s as i32 - listening_at as i32).abs() >= RECENTRE
                    {
                        log::info!("Listening again at {s:+}");
                        listening_at = s;
                        if let Err(e) = radio.init_wide(s).await {
                            log::warn!("CC1101: {e:?}");
                        }
                        CALIBRATION.lock(|c| c.borrow_mut().listening_at = s);
                    }
                }
                let effects = with_bridge(|b| b.handle_radio(&raw, ctx())).await;
                dispatch(effects);
            }
            samples = 0;
        }
        if !locked && since.elapsed() >= SEARCH_STEP {
            step = (step + 1) % search.len();
            listening_at = search[step];
            log::info!("Nothing decoded, listening with correction {listening_at:+}");
            if let Err(e) = radio.init_wide(listening_at).await {
                log::warn!("CC1101: {e:?}");
            }
            CALIBRATION.lock(|c| c.borrow_mut().listening_at = listening_at);
            since = Instant::now();
        }
        Timer::after_micros(300).await;
    }
    CALIBRATION.lock(|c| c.borrow_mut().running = false);
    log::info!("Frequency measurement stopped");
}

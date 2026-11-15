# CC1101 shield: radio stays silent

Notes from debugging the CC1101 shield on the Wemos D1 mini (November 2026), with the earlier C++ firmware. The
board booted, connected to WiFi and MQTT, but never received a packet, and every command was resent with a long
preamble without ever being ACKed. The same firmware worked with a hand-wired module.

Two independent faults were behind it. Either one alone gives the same symptom.

| Fault | How it shows | Fix |
|---|---|---|
| Radio VCC and GND pins not conducting | The chip still answers over SPI, because it powers itself through its signal pins, but it resets or loses its settings as soon as it starts receiving | Reheat module pins 1 and 2 and the 3V3 and G socket pins, 3-4 s each with fresh solder |
| Module crystal off frequency | The radio hears the thermostats (signal strength jumps) but never finds the sync word | Set `MAX_CC1101_FREQ_OFFSET` for that module |

## Checking a board

The C++ firmware had a check sketch (`src/cc1101_check.cpp`, in the git history) that tested every joint and
measured the frequency offset. It has not been ported to the Rust firmware yet. What the Rust firmware shows on
the serial monitor (`fw-monitor`):

| Log line | Meaning |
|---|---|
| `CC1101 init failed: NotFound(..)` | Nothing on SPI: module missing, wrong pins, or no power even through the signal pins |
| `CC1101 init failed: Timeout(..)` | The chip answers but does not calibrate or enter RX: suspect power or ground joints |
| `CC1101 ready, listening` and then `RX ...` lines | Receiving works |
| `... acknowledged command ...` after an MQTT command | Sending works too: that line only appears for an ACK to a packet this board sent |
| `Nothing received for 5 minutes, re-initialising the radio` | Deaf: frequency offset, antenna, or the GDO2 joint (pin 6) |
| `Radio: state ..., signal ... dBm, GDO2 ...` (warning) | Once a minute when the radio is not listening normally. At debug level it also shows the noise floor (about -95 dBm) |

## Frequency correction

The web UI measures it: System → Frequency correction → Measure. It listens with a wide filter, reads the
CC1101's offset estimate (FREQEST) during each MAX! packet, and suggests the median; turn a thermostat wheel to
get packets sooner. "Use … and restart" saves it.

The value belongs to one physical module. Set it in the web UI (Settings, Device), or as the default for
that board's first boot in `esp/secrets.toml`:

```toml
[env]
MAX_CC1101_FREQ_OFFSET = "45"
```

45 is what the module on the first shield measured (+71.4 kHz), confirmed again with the Rust firmware on the
ESP32 board: without it no frame arrived, with it every device was heard. A module within about ±6 steps
(±10 kHz) needs no correction. The stored setting survives reflashing; a factory reset brings back the build default.

## Why this happens

The MAX! register set (from culfw, in `esp/src/cc1101.rs`) receives through a 101 kHz filter and
corrects at most ±25 kHz of frequency error, which allows roughly 30 ppm between the bridge and a thermostat.
The module on the first shield was 72 kHz (83 ppm) high. Four different devices all measured 66-78 kHz away
from it, so the module was the outlier, not the devices.

`MAX_CC1101_FREQ_OFFSET` is written to the CC1101 `FSCTRL0` register. One step is f_xosc / 2^14 = 1.587 kHz and
the range is ±127 steps (±200 kHz). It shifts the synthesizer, so it corrects sending as well as receiving.
Only widening the receive filter would let the board hear, but the thermostats still would not hear it.

## For the next PCB revision

Done in v2 (see [README.md](README.md)), except where noted there.

- Route 3V3 and GND to the module as tracks. They currently exist only as copper fills, which pull heat
  away from the iron; those four joints are the hard ones to solder.
- Use bigger pads on the 2 mm header: a 1.2 mm pad around a 0.81 mm hole leaves a 0.19 mm ring.
- Ground reaches module pin 2 only through three 11.5 mil strips between pins 5-8, because the D6 and D8
  tracks on the bottom fence that area off.
- Leave module pin 7 (GDO0) unconnected. The firmware does not use it, it is wired to D3, and the CC1101
  outputs a 135 kHz clock on it after reset. On the D1 mini D3 is GPIO0, which must be high at boot.
- GDO2 (D2) is GPIO8 on the C3 Mini, a strapping pin that must be high for USB download mode. The CC1101
  drives it low after power-up, so flash the first time with the shield unplugged, or route GDO2 elsewhere.
- Keep the coil antenna away from the board's WiFi antenna. On the D1 mini, WiFi activity raised the noise the
  radio sees from about -98 to -79 dBm.

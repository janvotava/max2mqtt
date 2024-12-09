# CC1101 shield: radio stays silent

Notes from debugging the Wemos D1 mini CC1101 shield (November 2026). The board booted, connected to WiFi
and MQTT, but never printed a `Received message` line, and every command was repeated with
`Sending with long preamble` without ever being ACKed. The same firmware worked with a hand-wired module.

Two independent faults were behind it. Either one alone gives the same symptom.

| Fault | How it shows | Fix |
|---|---|---|
| Radio VCC and GND pins not conducting | The chip still answers over SPI, because it powers itself through its signal pins, but it resets or loses its settings as soon as it starts receiving | Reheat module pins 1 and 2 and the 3V3 and G socket pins, 3-4 s each with fresh solder |
| Module crystal off frequency | The radio hears the thermostats (signal strength jumps) but never finds the sync word | Measure the offset and build with `CC1101_FREQ_OFFSET` |

## 1. Run the check

`src/cc1101_check.cpp` is a small sketch that replaces the firmware. It never transmits and uses no WiFi.
Close any serial monitor first, otherwise the upload fails with `Resource busy`.

```bash
pio run -e cc1101_check -t upload --upload-port /dev/tty.usbserial-XXX
pio device monitor -e cc1101_check --port /dev/tty.usbserial-XXX   # or: screen /dev/tty.usbserial-XXX 115200
```

Without PlatformIO installed, prefix with `nix run nixpkgs#platformio --` instead of `pio`.

It prints one line per check and then listens for MAX! packets:

| Line | If it says FAIL |
|---|---|
| `chip answers` | Nothing on SPI at all: module missing, wrong pins, or no power even through the signal pins |
| `SPI (pins 3 MOSI, 4 SCK, 5 MISO, 8 CSN)` | One of those four joints is open or intermittent |
| `interrupt line (pin 6 GDO2)` | GDO2 does not reach `CC1101_IRQ_PIN`; the firmware would never notice a packet |
| `power (pin 1 VCC)` | VCC is open somewhere between the 3V3 socket and module pin 1 |
| `ground (pin 2 GND)` | GND is open somewhere between the G socket and module pin 2 |

These catch open and intermittent joints. A joint that conducts but is merely weak passes.

## 2. Read the frequency offset

After the checks the sketch listens with a 203 kHz wide receiver (the firmware uses 101 kHz), so it can
decode packets from a module that is far off. Turn a thermostat wheel to produce traffic. Each packet prints:

```
packet 4 from 15A52A, -68 dBm, off by +71.4 kHz  =>  -DCC1101_FREQ_OFFSET=45 (median of 4, +71.4 kHz)
```

Wait for five packets or so, ideally from more than one device, and take the last suggested value.
If it stays within about ±6 steps (±10 kHz) the module is fine and needs no correction.

If nothing is decoded for 90 seconds the sketch retries 79 kHz higher, then 79 kHz lower. If that finds
nothing either, the problem is not the frequency: check the antenna joint.

## 3. Apply it

The value belongs to one physical module. Give that board its own environment in `platformio.ini`:

```ini
[env:d1_mini_shield]
extends = env:d1_mini
build_flags = -DCC1101_FREQ_OFFSET=45
```

`pio run -e d1_mini_shield -t upload` then uploads over the air like `d1_mini` does. To flash over USB
instead, add `upload_protocol = esptool` and `upload_speed = 115200` to the environment and pass
`--upload-port`.

Flashing the plain `d1_mini` environment onto a board that needs a correction makes it deaf again.

It worked when the log shows `Received message, ..., CRC OK` lines and, after a command,
`ACKed message in the queue`. That second line only appears for an ACK that carries the message counter of
a packet this board sent, so it proves sending works even while a second bridge is running.

## Why this happens

The register set in `MaxCC1101::initReceive()` is the usual MAX! one (from culfw). It receives through a
101 kHz filter and corrects at most ±25 kHz of frequency error, which allows roughly 30 ppm between the
bridge and a thermostat. The module on the first shield was 72 kHz (83 ppm) high. Four different devices
all measured 66-78 kHz away from it, so the module was the outlier, not the devices.

`CC1101_FREQ_OFFSET` is written to the CC1101 `FSCTRL0` register. One step is f_xosc / 2^14 = 1.587 kHz and
the range is ±127 steps (±200 kHz). It shifts the synthesizer, so it corrects sending as well as receiving.
Only widening the receive filter would let the board hear, but the thermostats still would not hear it.

## For the next PCB revision

- Route 3V3 and GND to the module as tracks. They currently exist only as copper fills, which pull heat
  away from the iron; those four joints are the hard ones to solder.
- Use bigger pads on the 2 mm header: a 1.2 mm pad around a 0.81 mm hole leaves a 0.19 mm ring.
- Ground reaches module pin 2 only through three 11.5 mil strips between pins 5-8, because the D6 and D8
  tracks on the bottom fence that area off.
- Leave module pin 7 (GDO0) unconnected. The firmware does not use it, and it is wired to D3 (GPIO0), which
  must be high at boot while the CC1101 outputs a 135 kHz clock on that pin after reset.
- Keep the coil antenna away from the D1 mini's WiFi antenna. It works where it is, but WiFi activity
  raises the noise the radio sees from about -98 to -79 dBm.

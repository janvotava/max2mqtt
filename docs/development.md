# Development

Rewrite of the C++ firmware for a **Lolin C3 Mini** (ESP32-C3) with the same CC1101 and relay shields,
using Embassy (esp-hal 1.2, esp-rtos, esp-radio).

```
bridge/     protocol, heating control, TX queue, MQTT codec, Home Assistant discovery, settings,
            bridge logic. no_std, tested on the host.
esp/        Embassy firmware: CC1101 driver, Wi-Fi with setup hotspot, web server, MQTT, SNTP,
            storage in flash, relay.
web/        the web UI: SvelteKit (TypeScript, pnpm), built into one HTML file the firmware embeds.
```

The earlier C++ firmware for the Wemos D1 mini is in the git history.

## Hardware

The ESP8266 is not supported by modern Embassy. Two boards with the D1 mini footprint are supported, so both
shields plug in. The board is a cargo feature; pins are in `esp/src/board.rs`.

| Shield       | Header | D1 mini (old) | `c3-mini` (default) | `d1-mini-esp32` |
|--------------|--------|---------------|---------------------|-----------------|
| CC1101 SCK   | D5     | GPIO14        | GPIO2               | GPIO18          |
| CC1101 MISO  | D6     | GPIO12        | GPIO3               | GPIO19          |
| CC1101 MOSI  | D7     | GPIO13        | GPIO4               | GPIO23          |
| CC1101 CSN   | D8     | GPIO15        | GPIO5               | GPIO5           |
| CC1101 GDO2  | D2     | GPIO4         | GPIO8               | GPIO21          |
| Burner relay | D1     | GPIO5         | GPIO10              | GPIO22          |

- **`c3-mini`: Lolin C3 Mini (ESP32-C3, RISC-V).** Standard Rust toolchain. The mapping comes from Lolin's Arduino
  pin definitions (SPI on 2/3/4/5, I2C SDA/SCL on 8/10); check it against the pinout printed on the board before
  the first power-up. GPIO8 is a boot strapping pin and the CC1101 drives GDO2 low after power-up: if flashing
  over USB cannot enter download mode, unplug the CC1101 shield for the first flash.
- **`d1-mini-esp32`: ESP32-WROOM-32 boards in D1 mini size** (MH-ET Live MiniKit and clones such as the HW-665).
  The shields go on the inner header rows. The mapping is read from the HW-665 silkscreen. The original ESP32 is
  Xtensa, so it needs Espressif's Rust fork: run `esp-toolchain-install` once (devenv), then `fw-esp32-build` /
  `fw-esp32-flash`. Without devenv: `espup install --targets esp32`, source the export file it writes, then
  `cargo +esp build-esp32` / `cargo +esp flash-esp32` in `esp/`.

## Build and flash

The devenv shell in the repository root provides the toolchain, `espflash`, Node and pnpm (`direnv allow` or
`devenv shell`), with the scripts `core-test`, `web-build`, `web-dev`, `fw-build`, `fw-flash` and `fw-monitor`.
`devenv test` runs tests, clippy, rustfmt and the TypeScript check. Without devenv, `rustup`, `espflash` and
pnpm work too; `rust-toolchain.toml` selects the toolchain.

```bash
(cd web && pnpm install && pnpm build)   # = web-build; without it the firmware embeds a placeholder page
cd esp
cargo run --release                       # build, flash over USB, open the serial monitor (= fw-flash)
```

Runtime settings (Wi-Fi, MQTT, hostname, password, CC1101 frequency correction) are edited in the web UI and
stored on the device. `esp/.cargo/config.toml` and the optional `secrets.toml` only give the defaults for
the first boot; all of them are optional. Without a password one is generated on the first boot; the serial
console prints it at every boot.

`web-dev` runs the UI from Vite with hot reload, sending API calls to a real board
(`DEVICE=http://10.0.0.5 web-dev`, default `http://max.lan`).

### Storage

The `config` partition from `partitions.csv` holds a key-value map
([sequential-storage](https://crates.io/crates/sequential-storage): wear-levelled, power-fail safe, CRC-checked):
the settings, the bridge config, and one entry per device, each as JSON. A save writes only what changed.
Flash from the earlier two-slot format is erased on the first boot.

### Web UI and API

`esp/src/web.rs` serves the UI at `/` and a JSON API under `/api/` (documented at the top of the file),
on the home network and on the setup hotspot. The API needs HTTP Basic auth with the device password (any user
name); the UI keeps it in session storage. It is plain HTTP, so the password is readable on the local network.

### Firmware updates

`partitions.csv` has two app slots. `fw-image` (C3 Mini) or `fw-esp32-image` (ESP32) builds an app image
(`espflash save-image`) to upload on the System page; the board writes it to the slot it is not running from,
after checking the ESP image header, the chip it was built for, the size and the appended SHA-256, and restarts
into it. A new image is on trial: it is confirmed once it has run for a minute and is on the Wi-Fi (or has no
network configured). If it crashes three times, or is not confirmed within 10 minutes, the previous image boots
again (`esp/src/ota.rs`). Tested on the HW-665, including the rollback from a panicking image.

A panic restarts the board after printing it (`custom_halt` in `main.rs`) instead of hanging, which would leave
the relay as it was.

Flashing over USB (`fw-flash`) erases `otadata`, so the USB image always boots. A board flashed before the two
slots existed needs one USB flash to get the new partition table; its settings and devices start empty.

### Lost password, factory reset

Over USB, from `esp` (devenv: also `fw-monitor`):

```bash
espflash monitor                 # shows the password at every boot; press the board's reset button
espflash erase-parts --partition-table partitions.csv config   # erase settings and devices
```

After the erase the board boots with the defaults it was built with (`secrets.toml`): with `MAX_PASSWORD` set
that is your password again, otherwise a new one is generated and printed on the console. Add `--port` when
several boards are connected. `espflash erase-flash` wipes everything, firmware included; flash again afterwards.

### Setup hotspot

`max2mqtt-XXXX`, WPA2 with the device password, at 192.168.4.1 with its own DHCP server and a DNS server that
answers every name with that address, so phones show the captive portal. It comes on when no network is
configured, or when the board has not been connected for a minute; it goes off again once the board is connected
and nobody has used the hotspot for two minutes. While someone is using it, the board does not try to reconnect,
since every attempt scans all channels and interrupts the hotspot.

Tests: `cd bridge && cargo test`.

## MQTT

Topics and payloads stay as in the main README, with these changes:

- **Configurable prefix** (`max` by default), so a second board can run next to the first.
- **Home Assistant discovery** (on by default), in the device-based format of Home Assistant 2024.11+: one
  retained `homeassistant/device/max2mqtt_<address>/config` per device with a room, and one for the bridge.
  Thermostats are `climate` entities (modes `auto` = week program, `heat` = manual at the comfort temperature,
  `off` = 4.5 °C; presets `boost`, `eco`, `comfort`) with temperature, valve, battery and signal sensors; window
  sensors are `binary_sensor`s. The bridge has the furnace, heat demand, a pairing switch and a restart button.
  Removing a device's room removes it from Home Assistant. Everything is announced again when Home Assistant
  publishes `online` on `homeassistant/status`. See `bridge/src/homeassistant.rs`.

- **Schedules use start times.** `{"day":"monday","schedule":{"6:00":21.5,"22:30":4.5}}` means 21.5 °C from 6:00
  and 4.5 °C from 22:30, with the evening temperature continuing until 6:00. The old firmware sent the keys as
  "until" times and left out the closing 24:00 point. Several days can go in one message:
  `{"schedule":{"monday":{...},"tuesday":{...}}}`. Times must be in 5 minute steps, at most 13 changes per day.
- **`temperature` keeps auto mode.** A device in auto mode stays in auto (the device treats it as an override
  until the next switch point). Send `"mode":"manual"` to leave auto mode. `temperature` also accepts `"eco"`,
  `"comfort"`, `"on"` and `"off"`.
- **`mode` without `temperature`.** `auto` and `boost` follow the device's own settings; `manual` reuses the
  last known setpoint. `temporary` needs `"until":"YYYY-MM-DD HH:MM"` (minutes `00` or `30`).
- **`boost_duration` is in minutes** (0, 5, ..., 30 or 60; other values round down).
- **Only devices with a `room` heat.** They are also the only ones that get the time and are re-paired
  automatically. Devices heard but not configured (for example a neighbour's) are still listed and published.
- **Rooms link their devices**, as a MAX! Cube does: the thermostats in a room get the same group id, and the
  thermostats and window sensors in it are linked to each other, both ways. A radiator thermostat linked to a wall
  thermostat follows its setpoint and regulates on its temperature; linked to a window sensor it drops to the
  window-open temperature. Leaving a room removes the links and the group id. `{"relink":true}` on a device sends
  its room's group id and links again, e.g. after a device was reset or replaced. Commands for window sensors and
  eco switches wait until the device sends something (it only listens then), so open or close the window once.
  Devices report `rf_error` when a link partner did not answer.
- **The burner follows the valves.** The old PID on the room temperature is gone: each valve already regulates
  its room, so the most-open valve in a room with a device is the heat demand, in any mode (auto included; the
  old firmware ignored valves in auto). A 20 minute time-proportional cycle runs the burner for that share of the
  time, with minimum on and off times. Valves open less than `min_valve_position` (30 %) do not count, and a
  valve during its weekly descaling hour is ignored. `quiet_mode_offset` and `kp`/`ki`/`kd` are gone.
- **Week programs per room.** `{"room_schedule":{"monday":{"6:00":21.5,"22:30":17},...}}` on any device sets
  that program on every thermostat in its room (each device runs its own program; links do not pass on switch
  points), the same as the web UI's week editor. A thermostat joining a room takes over the room's program.
  Devices keep their program through battery changes; after a factory reset, pairing restores it.
- **Pairing switches itself off** after 5 minutes (`{"pair":true}` on `max/set`, the Devices page, or the
  Home Assistant switch), so a stranger's device in pairing mode later is not taken over.
- **New on `max/set`:** `"heating":{...}` changes the burner cycle (`cycle_minutes`, `min_on_minutes`,
  `min_off_minutes`, `min_valve_position`, `stale_minutes`), `"duty_cycle_limit":false` turns off the 1 % airtime limit, `"timezone":{...}` sets the
  offset and EU daylight saving.
- **`max/save-config`** now saves (the old firmware subscribed to it but did nothing). `max/format` erases the
  config (devices, rooms, tuning) and restarts; the Wi-Fi and MQTT settings stay, so the board stays reachable.
  A full factory reset is in the web UI. Retained messages on `max/reset` and `max/format` are ignored.
- Device state includes `address`; bridge state includes `heat_demand` (the burner's share of the cycle, %),
  `tx_queue` and `address`.

## Fixed bugs

From the C++ firmware, each covered by a test in `bridge`:

**Radio**
- Read the SPI bus from the GDO2 interrupt handler while the main loop could be transmitting on it.
- Handled one received frame per second (`delay(1000)` in the loop), so ACKs arrived late and commands were resent.
- Queued our ACKs to devices behind commands still waiting for their own ACK. Devices only listen briefly.
- `ackMessageInQueue` called `front()` on an empty queue when an unexpected ACK arrived.
- Matched ACKs by message counter alone, from any device. Resent after about 1.2 s regardless.
- Checked a length byte and called it "CRC OK"; the real CRC flag was never read.
- RX overflow check had a precedence bug (`x & 0x1F != 0x11` is `x & 1`).
- Waits for the radio state had no timeout and could hang the firmware.

**Protocol**
- Read the wall thermostat temperature from the wrong bytes, which hold the paired valve's reading (or an
  "until" date). That is the temperature the burner control used.
- Read the valve temperature in temporary mode, where those bytes are a date.
- Parsed every ACK as a full state, so short ACKs set devices to auto mode.
- Answered pairing requests from devices paired to another cube when pairing was on, and pushed its whole
  config to them.
- `mode || MODE_MANUAL` is always 1, so every temperature change switched the device to manual.
- Mode without temperature sent -1 °C.
- `decalc_weekday` became 0 or 1 (`weekday || current`), so Saturday or Sunday.
- Boost duration mixed minutes and protocol codes; the default meant a 150 minute boost window.
- Unknown schedule day indexed `schedule[255]`.
- The topic was lowercased before matching device names, so default names (uppercase hex) could not be controlled.
- Newly discovered devices were not subscribed until the next reconnect.

**Heating**
- A second controller (PID on the room temperature) on top of the valves' own, with its bugs: 0 % right after
  every setpoint change, and a derivative over a fixed 15 minute divisor. Replaced by the valve positions.
- Valves in auto mode never called for heat.
- Boost shorter than 2 minutes underflowed and kept the burner on.
- Any neighbour's valve in manual mode could switch the burner on (autocreate is on by default).
- `millis()` wrapped after 49 days; timestamps are now 64-bit.

**Other**
- Config was rewritten in place; now a wear-levelled, power-fail safe key-value store.
- Device state was not republished after an MQTT reconnect.

## Not ported (yet)

- The `cc1101_check` radio test sketch.
- mDNS. The router learns the hostname from DHCP, as before (`max.lan`).

Tested on the HW-665 (ESP32): radio reception, Wi-Fi, time sync, storage, the web API, firmware updates and
rollback. Not yet tested on hardware: transmitting to devices, pairing, MQTT and Home Assistant, the relay, the
setup hotspot, and the C3 Mini.

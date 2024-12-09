# MAX2MQTT

eQ-3 MAX! to MQTT bridge with burner control. Runs on a Lolin C3 Mini (ESP32-C3) with a CC1101 shield and a relay
shield that switches the gas furnace when heating is needed.

The firmware is Rust/Embassy, in [`docs/development.md`](docs/development.md): hardware, pin mapping, build and flash instructions,
and how it differs from the earlier C++ firmware for the Wemos D1 mini (in the git history).

Radio silent on a new board? See [docs/cc1101-shield.md](docs/cc1101-shield.md) for soldering faults and the
per-module frequency correction.

## Usage

```bash
git clone git@github.com:janvotava/max2mqtt.git
cd max2mqtt
direnv allow        # or: devenv shell
fw-flash            # build the web UI and the firmware, flash over USB, monitor
```

## Setup

The serial console prints the device password at every boot (lost it, or want a factory reset over USB? see
[docs/development.md](docs/development.md#lost-password-factory-reset)). Then:

1. **Wi-Fi.** Without a configured network (or when the board cannot join it for a minute) it opens a hotspot
   `max2mqtt-XXXX` with that password. Phones open the setup page by themselves; otherwise go to
   http://192.168.4.1/. Pick the network under Settings and save; the board restarts and joins it.
2. **Web UI.** On the home network, at the address printed on the console (or `http://max/`). Settings for Wi-Fi,
   MQTT (broker, topic prefix, Home Assistant discovery), hostname, password, and the CC1101 frequency
   correction; an overview of the furnace and rooms; the devices with pairing, names, rooms and temperatures;
   the radio address, time zone and frequency correction (measured from the thermostats' packets); a live log;
   firmware updates (`fw-image` builds the file to upload).
3. **Home Assistant.** With an MQTT broker set and discovery on (the default), every device with a room shows up
   as a thermostat, window sensor and so on, with the bridge's furnace and pairing switch.

Optional build-time defaults for the first boot (Wi-Fi, MQTT, password) can go in
`esp/secrets.toml`, see `secrets.example.toml`.

## MQTT configuration

Everything in the web UI can also be done over MQTT, stored on the device.

```bash
HOSTNAME="mqtt.lan"

mosquitto_pub -h $HOSTNAME -t max/set -m '{"address":"DEDABA"}'
mosquitto_pub -h $HOSTNAME -t max/rename -m '{"address":"123456","to":"living-room/wall-thermostat"}'
mosquitto_pub -h $HOSTNAME -t max/rename -m '{"address":"234567","to":"living-room/heater"}'
mosquitto_pub -h $HOSTNAME -t max/living-room/wall-thermostat/set -m '{"group":2,"room":"living-room","eco_temperature":4.5,"comfort_temperature":21.5,"display_actual_temperature":true,"boost_duration":30}'
mosquitto_pub -h $HOSTNAME -t max/living-room/heater/set -m '{"group":2,"room":"living-room","associate":"living-room/wall-thermostat","eco_temperature":4.5,"comfort_temperature":21.5,"decalc_weekday":"saturday","decalc_hour":12,"boost_duration":30}'
# 21.5 °C from 6:00, 4.5 °C from 22:30 (until 6:00 the next morning)
mosquitto_pub -h $HOSTNAME -t max/living-room/wall-thermostat/set -m '{"day":"monday","schedule":{"6:00":21.5,"22:30":4.5}}'
```

Only devices with a `room` take part in heating. All topics and options are listed in
[docs/development.md](docs/development.md#mqtt) and in the doc comments of `bridge/src/bridge.rs`.

## TODO
- schedules and the heating tuning in the web UI (MQTT only for now)
- radio check tool (the C++ `cc1101_check` sketch was not ported)
- push button
- CC1101 shield PCB
- 3D printed case

# MAX2MQTT

Replaces the eQ-3 MAX! Cube: an ESP32 with a CC1101 runs your MAX! thermostats, fires the gas furnace when a room
needs heat, and talks to Home Assistant over MQTT. Everything is set up in its web UI.

Runs on D1-mini-sized ESP32 boards (HW-665 / ESP32-WROOM-32, Lolin C3 Mini) with a CC1101 shield
([`hardware/cc1101-shield`](hardware/cc1101-shield)) and a relay shield that switches the furnace.

The firmware is Rust/Embassy, in [`docs/development.md`](docs/development.md): hardware, pin mapping, build and flash instructions,
and how it differs from the earlier C++ firmware for the Wemos D1 mini (in the git history).

Radio silent on a new board? See [hardware/cc1101-shield/troubleshooting.md](hardware/cc1101-shield/troubleshooting.md) for soldering faults and the
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

## MQTT

MQTT is for Home Assistant: device state, and the commands its thermostat cards send (temperature, mode).
Rooms, week programs, names and heating settings are set in the web UI, so whatever can publish to the broker
cannot reconfigure the bridge. Topics: [docs/development.md](docs/development.md#mqtt-and-home-assistant).

# AGENTS.md

Guidance for coding agents working in this repository.

## What this is

Firmware that replaces the eQ-3 MAX! Cube: an ESP32 with a CC1101 radio runs the MAX! thermostats, switches a
gas furnace relay, and talks to Home Assistant over MQTT. It is configured in its own web UI. `README.md` is the
user's view; `docs/development.md` has hardware, pin mapping, storage, updates, MQTT topics and heating rules.

The board in use is an **HW-665 (ESP32-WROOM-32, silicon v1.0)**, feature `d1-mini-esp32`. The default cargo
target is the Lolin C3 Mini (`c3-mini`), which has not been tested on hardware. It controls real heating: a
restart drops the burner until the valves report again (a minute or two).

## Version control

This is a colocated Jujutsu repository: use `jj` (`jj st`, `jj diff`, `jj log`, `jj describe`), not git. Nothing
needs staging.

## Commands

The toolchain comes from devenv (`devenv.nix`); its scripts are on the PATH inside `devenv shell`. From a plain
shell, prefix with `direnv exec .` (e.g. `direnv exec . core-test`).

| | |
|---|---|
| `core-test` | host tests of `bridge/` (`cd bridge && cargo test`) |
| `cd bridge && cargo test <name>` | one test, or all tests of a module (`cargo test logring`) |
| `devenv test` | everything CI-like: bridge tests, clippy `-D warnings` and `cargo fmt --check` for `bridge` and `esp`, web build and `pnpm check`, and the ESP32 clippy when its toolchain is installed |
| `web-build` | the web UI into `web/build/index.html` |
| `web-dev` | Vite with hot reload, API proxied to a real board (`DEVICE=http://10.0.0.5 web-dev`, default `http://max.lan`) |
| `fw-esp32-build`, `fw-esp32-flash`, `fw-esp32-image` | ESP32: build, flash over USB, or write `esp/target/max2mqtt-d1-mini-esp32.bin` for an update over the network |
| `fw-build`, `fw-flash`, `fw-image` | the same for the C3 Mini |
| `fw-crash [host]` | a board's crash records with source lines |

The `fw-*` scripts run `web-build` first: the firmware embeds the UI at build time (`esp/build.rs`), and without
a built UI it embeds a placeholder page.

In `esp/`, plain `cargo clippy --release` checks the C3 target. The ESP32 is Xtensa and needs Espressif's
toolchain (`esp-toolchain-install` once); outside the `fw-esp32-*` scripts, source `.devenv/export-esp.sh`, put
`~/.rustup/toolchains/esp/bin` first in PATH, set `ESP_HAL_CONFIG_MIN_CHIP_REVISION=100`, and use the aliases
`cargo build-esp32` / `cargo clippy-esp32` (`esp/.cargo/config.toml`).

Updating a board over the network (what the System page does): the image is the request body, and the board
restarts into it. It is on trial until it has run for a minute on Wi-Fi, and rolls back otherwise
(`esp/src/ota.rs`).

```bash
curl -u x:<device password> -X POST -H 'Expect:' -H 'Content-Type: application/octet-stream' \
  --data-binary @esp/target/max2mqtt-d1-mini-esp32.bin http://max/api/firmware
```

`GET /api/status` and `GET /api/logs` (same Basic auth, any user name) show the result: firmware slot and
state, heap use, the log with the `Heap after ...` lines of each startup step.

## Architecture

Three parts, each depending only on the one before it:

- **`bridge/`** (`max2mqtt-bridge`): all logic, `no_std` + `alloc`, no hardware and no clock. The radio
  protocol, the transmit queue, heating control, the MQTT codec, Home Assistant discovery, settings and config.
  `Bridge` (`bridge.rs`) ties them together: every entry point (`handle_radio`, `handle_mqtt`, `tick`,
  `configure_device`, ...) takes a `Ctx` with the current time and returns `Effect`s (publish, subscribe, save
  config, restart, re-initialise the radio) for the caller to carry out. That is what makes it testable on the
  host, and logic belongs here with a test rather than in `esp/`, including small data structures the firmware
  needs (`logring.rs`).
- **`esp/`**: the Embassy firmware that wires the bridge to hardware. One `Bridge` behind an async mutex
  (`shared::BRIDGE`, used through `with_bridge`), and one task per concern: radio (owns the CC1101), MQTT,
  Wi-Fi with the setup hotspot, web server (a pool of listeners), a once-a-second control loop that drives the
  relay and feeds the watchdog, and a system task that does flash writes and restarts in order.
  `shared::dispatch` routes the bridge's effects to those tasks through channels and signals. `web.rs` is a
  hand-written HTTP/1.1 server; its API is documented at the top of the file.
- **`web/`**: SvelteKit (Svelte 5 runes, TypeScript, pnpm) built into a single HTML file with hash routing,
  because the firmware serves exactly one file at `/`.

Two boards are cargo features that select the chip and the pins (`esp/src/board.rs`); exactly one must be on.

Settings (Wi-Fi, MQTT, password) are read once at boot and changing them restarts the board. The bridge config
and one entry per device live in a key-value map in flash (`esp/src/storage.rs`); a save writes only the
entries whose checksum changed. `esp/.cargo/config.toml` and the optional, git-ignored `esp/secrets.toml` only
give first-boot defaults.

## Things to know before changing code

- **Memory is the constraint.** The heap is 177 KiB on the ESP32 and 129 KiB on the C3, and an allocation that
  fails restarts the board. Large responses (`/api/config`, `/api/backup`, `/api/logs`) are written to the
  socket piece by piece instead of built whole, status is serialized straight from borrowed data rather than
  through a `serde_json::Value` tree, and MQTT announcements go out one device at a time. `esp/src/heap.rs`
  logs heap use after each startup step and each new high after a web request.
- **MQTT only carries state and Home Assistant's commands.** Rooms, names, week programs and heating settings
  are changed through the web API only, so whatever can publish to the broker cannot reconfigure the bridge.
- **Device settings are tracked until the device confirms them** (`Pending` in `config.rs`), stored in flash and
  sent again; window sensors only listen right after they send something, so their commands are parked in the
  transmit queue (`tx.rs`).
- **UI texts** in both languages (English, Czech) are one table in `web/messages/build.py`; edit it there and
  run `python3 messages/build.py` in `web/`. The JSON files and `src/lib/paraglide/` are generated.
- Rust is formatted with a 120-column `rustfmt.toml`; clippy warnings are errors.

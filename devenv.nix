{ pkgs, ... }:

let
  # The ESP32 (Xtensa) needs Espressif's Rust fork, installed by espup into
  # ~/.rustup/toolchains/esp. Its PATH settings go to .devenv/export-esp.sh.
  espEnv = ''
    if [ ! -f "$DEVENV_ROOT/.devenv/export-esp.sh" ]; then
      echo "ESP32 toolchain missing: run esp-toolchain-install first" >&2
      exit 1
    fi
    . "$DEVENV_ROOT/.devenv/export-esp.sh"
    export PATH="''${RUSTUP_HOME:-$HOME/.rustup}/toolchains/esp/bin:$PATH"
    # esp-hal requires ESP32 silicon v3.0+ by default (it skips the older chips' errata
    # workarounds). The HW-665 at hand is v1.0, so allow it. Only for ESP32 builds: the
    # setting would break the C3, whose revisions are numbered 0.x.
    export ESP_HAL_CONFIG_MIN_CHIP_REVISION="''${ESP_HAL_CONFIG_MIN_CHIP_REVISION:-100}"
    cd "$DEVENV_ROOT/esp"
  '';
in
{
  # Rust toolchain for the bridge crate and the ESP32-C3 firmware, taken from the firmware's
  # toolchain file so rustup users and devenv get the same thing: stable, rust-src, and
  # riscv32imc-unknown-none-elf. Host std is included for the bridge crate's tests.
  # Do not set languages.rust.rustflags: RUSTFLAGS would override the firmware's
  # per-target rustflags in esp/.cargo/config.toml.
  languages.rust = {
    enable = true;
    toolchainFile = ./esp/rust-toolchain.toml;
  };

  packages = [
    pkgs.espflash # flash and monitor over USB
    pkgs.nodejs # web UI (web/), embedded into the firmware
    pkgs.pnpm
  ];

  scripts = {
    # Web UI: a single gzipped HTML file the firmware embeds (esp/build.rs).
    web-build.exec = ''
      cd "$DEVENV_ROOT/web" && pnpm install --frozen-lockfile && pnpm build
    '';
    # Vite dev server; API calls go to the board at $DEVICE (default http://max.lan).
    web-dev.exec = ''
      cd "$DEVENV_ROOT/web" && pnpm install --frozen-lockfile && pnpm dev "$@"
    '';

    core-test.exec = ''
      cd "$DEVENV_ROOT/bridge" && cargo test "$@"
    '';

    # Lolin C3 Mini (default board)
    fw-build.exec = ''
      web-build && cd "$DEVENV_ROOT/esp" && cargo build --release "$@"
    '';
    fw-flash.exec = ''
      web-build && cd "$DEVENV_ROOT/esp" && cargo run --release "$@"
    '';
    # App image for the web UI's firmware update (System page).
    fw-image.exec = ''
      fw-build && cd "$DEVENV_ROOT/esp" && espflash save-image --chip esp32c3 \
        target/riscv32imc-unknown-none-elf/release/max2mqtt target/max2mqtt-c3-mini.bin \
        && echo "Upload esp/target/max2mqtt-c3-mini.bin in the web UI (System)"
    '';

    # D1-mini-sized ESP32-WROOM-32 boards (MH-ET Live MiniKit, HW-665)
    esp-toolchain-install.exec = ''
      PATH="${pkgs.rustup}/bin:$PATH" ${pkgs.espup}/bin/espup install --targets esp32 \
        --export-file "$DEVENV_ROOT/.devenv/export-esp.sh" "$@"
    '';
    fw-esp32-build.exec = "web-build || exit 1\n" + espEnv + ''
      cargo build-esp32 "$@"
    '';
    fw-esp32-flash.exec = "web-build || exit 1\n" + espEnv + ''
      cargo flash-esp32 "$@"
    '';
    fw-esp32-image.exec = "web-build || exit 1\n" + espEnv + ''
      cargo build-esp32 "$@" && espflash save-image --chip esp32 \
        target/xtensa-esp32-none-elf/release/max2mqtt target/max2mqtt-d1-mini-esp32.bin \
        && echo "Upload esp/target/max2mqtt-d1-mini-esp32.bin in the web UI (System)"
    '';

    # Crash records of a board with source lines: fw-crash [host]
    fw-crash.exec = ''
      if [ -f "$DEVENV_ROOT/.devenv/export-esp.sh" ]; then . "$DEVENV_ROOT/.devenv/export-esp.sh"; fi
      python3 "$DEVENV_ROOT/esp/tools/fw-crash.py" "$@"
    '';

    fw-monitor.exec = ''
      espflash monitor "$@"
    '';
  };

  enterShell = ''
    echo "max2mqtt: core-test, web-build, web-dev, fw-build, fw-flash, fw-image (C3 Mini), fw-esp32-build, fw-esp32-flash, fw-esp32-image (ESP32), fw-monitor, fw-crash"
  '';

  # `devenv test`: host tests, lints, and firmware lints with placeholder Wi-Fi settings.
  # The ESP32 build is checked too when its toolchain is installed.
  enterTest = ''
    set -e
    core-test
    (cd "$DEVENV_ROOT/bridge" && cargo clippy --all-targets -- -D warnings)
    (cd "$DEVENV_ROOT/bridge" && cargo fmt --check)
    web-build
    (cd "$DEVENV_ROOT/web" && pnpm check)
    cd "$DEVENV_ROOT/esp"
    cargo clippy --release -- -D warnings
    cargo fmt --check
    if [ -f "$DEVENV_ROOT/.devenv/export-esp.sh" ]; then
      (
        . "$DEVENV_ROOT/.devenv/export-esp.sh"
        export PATH="''${RUSTUP_HOME:-$HOME/.rustup}/toolchains/esp/bin:$PATH"
        cargo clippy-esp32 -- -D warnings
      )
    else
      echo "Skipping ESP32 build: run esp-toolchain-install to include it"
    fi
  '';
}

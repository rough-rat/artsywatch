# artsywatch firmware

Rust firmware for the artsywatch (CH582). The watch face LED layout is described in JSON;
`build.rs` checks it with [`ledface_layout`](ledface_layout/README.md) and turns it into Rust
constants (nothing is decoded at run time).

## Building and flashing

Requirements: [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`),
[wchisp](https://github.com/ch32-rs/wchisp) for USB flashing, and optionally
[wlink](https://github.com/ch32-rs/wlink) with a WCH-LinkE probe.

```sh
cargo run --release --bin artsywatch_fw     # build and flash (USB if possible, else wlink)
```

The runner ([tools/flash.sh](tools/flash.sh)) picks the method:

1. **Firmware running, watch on USB:** resets it into the WCH ROM bootloader over the CDC port
   ([tools/cdc_reset.py](tools/cdc_reset.py) `--bootloader`), then flashes with `wchisp`.
   No button needed.
2. **Bootloader already on USB** (`4348:55e0`): flashes with `wchisp`.
3. **Otherwise:** `wlink` over the probe.

Force one with `FLASH_WITH=usb` or `FLASH_WITH=wlink`.

Other ways into the bootloader: hold the button (BOOT) for 3 seconds while the firmware runs
(a short press shows the time; the button has to be released first after power-up), or hold
BOOT while plugging in USB.
`tools/cdc_reset.py` without arguments just restarts the firmware.

### Remote reset over CDC (DTR/RTS)

The CDC port's DTR/RTS lines behave like the ESP32 auto-reset circuit
(`ch58x_rs_kit::usb_log::RemoteReset::EspStyle`): RTS acts as EN (reset), DTR as IO0 (boot).
`DTR=0, RTS=1` holds reset; releasing it resets the chip, or enters the bootloader if `DTR=1,
RTS=0` follows (esptool's classic sequence). Terminals set both lines together, so opening or
closing a port never resets the watch.

Known issue: on a board with the WCH-LinkE attached, `wchisp` flashing failed verification
(papajbadge). Use `FLASH_WITH=wlink` there.

## Clock face

The button shows the time for 5 seconds: the wave sweeps across the face while the hour and
minute bits fade in and out (see the project README). The LED layout comes from
[ledface_layout/great_voltage_wave.json](ledface_layout/great_voltage_wave.json), generated from
the ledface PCB by `ledface_layout/tools/gen_great_voltage_wave.py` (LED positions, and which
driver output drives which kanji). Brightness: 16 levels per LED (`ch58x_rs_kit::sr_led_bcm`).

Set the time over Bluetooth (Current Time Service, e.g. `cts_writer.html`). It is written into
the RTC, with a reboot, when the client disconnects.

## Power

After boot or a button press the watch is connectable (advertising) for 20 s. Then, when nothing
keeps it awake, it goes to sleep: only the RTC and the button stay powered
(`ch58x_rs_kit::power::sleep_until_pressed`). A short press wakes it (it reboots, shows the time
and is connectable again); holding the button 3 s still enters the bootloader, also from sleep.

It stays awake while a Bluetooth client is connected, while the time is shown, and while a PC
uses the USB port (so logs and USB flashing keep working during development).

The LED face is powered (4V_ENA, PB13) only while something is shown; its data lines are
released (inputs with pull-down) while it is off. USB_BACKFEED_ENA (PB12, battery to VBUS for
future USB gadgets) is always off: driven low while running, pulled down in sleep. The board has
no pull-down on it, so it floats from reset until the firmware starts (and in the ROM bootloader).

### Battery

VBAT_MEAS (PA4) is 4V_OUT / 2, so it reads only while the LED face is powered, and it measures
the charger's OUT rail: the battery voltage on battery, the charger's ~4.4 V on USB. The firmware
measures it at every boot and at the start of every time display (LEDs still dark):

- below 3.5 V: back to sleep right after waking up;
- below 3.6 V: one LED (the rightmost kanji) blinks 3 times, then sleep, no Bluetooth;
- otherwise normal. The Bluetooth Battery Service (0x180F) reports the level in % (rough LiPo
  curve) and the voltage (Voltage, 0x2B18, little-endian uint16 in 1/64 V: `19 01` = 281/64 =
  4.39 V).

RAM (32 KB) holds the RAM code (`.highcode`, ~7.7 KB: interrupt handlers, parts of the BLE
library, the last instructions of the sleep code), the globals (~11.4 KB, incl. the 6 KB BLE
heap) and the stack (the rest, ~12 KB; peak use measured at ~1.4 KB). A stack overflow silently
corrupts the globals, so `stack-check.x` makes the link fail when less than 4 KB is left for it.

## Logs

Logs (`log!`) go to two places:

- **USB-C** of the watch: a CDC-ACM serial port named "ArtsyWatch" (VID:PID `1209:0001`), e.g.
  `picocom /dev/serial/by-id/usb-ch58x-rs-kit_ArtsyWatch_0001-if00`. Lines are buffered
  (1 KiB) until the port is opened, so the boot log shows up when you connect.
- **UART0 TX (PB7)** at 115200 baud, e.g. via the WCH-LinkE's serial port. Fatal errors/panics
  are reported only here (USB is dead by then).

Shared code comes from [ch58x-rs-kit](https://github.com/cr1tbit/ch58x-rs-playground) and
[ch58x-hal](https://github.com/cr1tbit/ch58x-hal) (git dependencies, pinned in `Cargo.lock`).
To work on them locally, add a temporary override to `Cargo.toml`:

```toml
[patch."https://github.com/cr1tbit/ch58x-rs-playground.git"]
ch58x-rs-kit = { path = "/path/to/ch58x-rs-playground/ch58x-rs-kit" }
```

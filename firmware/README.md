# artsywatch firmware

Rust firmware for the artsywatch (CH582). The watch face LED layout is described in JSON and
compiled at build time by `build.rs` using [`ledface_layout`](ledface_layout/README.md).

## Building and flashing

Requirements: [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`),
[wlink](https://github.com/ch32-rs/wlink) and a WCH-LinkE probe (`cargo install wlink`).

```sh
cargo run --release --bin artsywatch_fw     # build, flash with wlink, reset
```

Logs are printed on UART0 TX (PB7) at 115200 baud, e.g. via the WCH-LinkE's serial port.

Shared code comes from [ch58x-rs-kit](https://github.com/cr1tbit/ch58x-rs-playground) and
[ch58x-hal](https://github.com/cr1tbit/ch58x-hal) (git dependencies, pinned in `Cargo.lock`).
To work on them locally, add a temporary override to `Cargo.toml`:

```toml
[patch."https://github.com/cr1tbit/ch58x-rs-playground.git"]
ch58x-rs-kit = { path = "../../ch58x-rs-playground/ch58x-rs-kit" }
```

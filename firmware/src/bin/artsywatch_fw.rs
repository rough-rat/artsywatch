#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![no_std]
#![no_main]

use ch58x_hal as hal;
use embassy_executor::Spawner;
use hal::gpio::{Input, Level, Output, OutputDrive, Pull};
use hal::peripherals;
use hal::uart::UartTx;
use hal::ble;
use ch58x_rs_kit::logger::init as init_logger;
use ch58x_rs_kit::{get_configured_rtc, log};
use ch58x_rs_kit::sr_led_bcm::Display;
use ch58x_rs_kit::sr_led_driver::SrLedDriver;
use ch58x_rs_kit::usb_log::RemoteReset;
use ch58x_rs_kit::power::{PinSleep, Port};

use ch58x_rs_kit::ble_periph::{common_init_named, devinfo_init, peripheral, set_connection_parameters};
use ch58x_rs_kit::ble_periph::current_time_service::current_time_init;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_time::Duration;
use embassy_time::Timer;

/// Number of LEDs on the Great Voltage Wave ledface.
const LEDS: usize = 48;

/// What the clock needs from the layout (ledface_layout/great_voltage_wave.json). `build.rs`
/// generates [`FACE`] from the JSON: decoding the layout here took a 9.3 KB stack frame.
struct Face {
    /// Minute bits, LSB first: the 7 rightmost kanji, read right to left, top to bottom.
    minutes: &'static [u8],
    /// Hour bits (24 h), LSB first: the 6 leftmost kanji.
    hours: &'static [u8],
    /// The wave LEDs, sorted left to right.
    wave: &'static [u8],
    /// Each LED's position in 0.1 mm, from the top left of the face (y grows downwards).
    x: [i32; LEDS],
    y: [i32; LEDS],
    /// Bounds of the wave LEDs, in 0.1 mm.
    wave_left: i32,
    wave_right: i32,
    wave_top: i32,
    wave_bottom: i32,
}

include!(concat!(env!("OUT_DIR"), "/face.rs"));

/// Set while the time is shown (the LEDs are on).
static SHOWING: AtomicBool = AtomicBool::new(false);
/// Set while the button is held.
static BUTTON_DOWN: AtomicBool = AtomicBool::new(false);

/// Button pressed: show the time.
static SHOW_TIME: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// How long the time is shown after a button press.
const SHOW_MS: u32 = 5000;
/// Fade in / fade out time of the hour and minute bits.
const FADE_MS: u32 = 600;
/// The wave is the area under one sine hump (half a period), as wide as the wave area. It
/// sweeps once across the face during the show: enters on the left, leaves on the right.
/// Hump height in percent of the area's height (100: lights whole columns at the peak).
const WAVE_HEIGHT_PERCENT: i32 = 100;
/// Soft edge of the wave, in 0.1 mm: LEDs this close to the curve are partly lit.
const WAVE_EDGE: i32 = 15;
/// Animation frame time.
const FRAME_MS: u32 = 40;

/// sin(pi * i / 32) for i = 0..=32, scaled to 1024: the hump's shape.
const SIN_HUMP: [i32; 33] = [
    0, 100, 200, 297, 392, 483, 569, 650, 724, 792, 851, 903, 946, 980, 1004, 1019, 1024, 1019,
    1004, 980, 946, 903, 851, 792, 724, 650, 569, 483, 392, 297, 200, 100, 0,
];

/// sin(pi * u / 1024) * 1024 for u = 0..=1024 (linear interpolation in [`SIN_HUMP`]).
fn hump(u: i32) -> i32 {
    let u = u.clamp(0, 1024);
    let (i, frac) = ((u / 32) as usize, u % 32);
    if i >= 32 {
        return SIN_HUMP[32];
    }
    SIN_HUMP[i] + (SIN_HUMP[i + 1] - SIN_HUMP[i]) * frac / 32
}

/// 0..=15 from a 0..=FADE_MS ramp at both ends of the show.
fn envelope(t_ms: u32) -> u32 {
    let edge = t_ms.min(SHOW_MS.saturating_sub(t_ms));
    (edge.min(FADE_MS) * 15 + FADE_MS / 2) / FADE_MS
}

/// Render one frame of the time display, `t_ms` into the show.
fn render(face: &Face, t_ms: u32, levels: &mut [u8; LEDS]) {
    levels.fill(0);
    let env = envelope(t_ms);

    // wave: the area under one sine hump, sweeping once from left to right during the show.
    // Integer math in 0.1 mm: no FPU, and a float version took 8 ms a frame, long enough to
    // starve the BLE stack (advertising failed to start).
    let width = (face.wave_right - face.wave_left).max(1);
    // baseline just below the lowest LEDs (soft edge included): nothing lit outside the hump
    let baseline = face.wave_bottom + WAVE_EDGE;
    let height = (baseline - face.wave_top + WAVE_EDGE) * WAVE_HEIGHT_PERCENT / 100;
    // hump start: from fully left of the area to fully right of it
    let start = face.wave_left - width + (t_ms.min(SHOW_MS) as i32) * 2 * width / SHOW_MS as i32;
    for &index in face.wave.iter() {
        let index = index as usize;
        let (x, y) = (face.x[index], face.y[index]);
        let u = (x - start) * 1024 / width; // 0..1024 across the hump
        // y grows downwards: the hump rises towards smaller y
        let curve = if (0..=1024).contains(&u) { baseline - height * hump(u) / 1024 } else { baseline };
        // 0 above the curve, 1024 below it, a linear ramp across the soft edge
        let under = ((y - curve) * 1024 / WAVE_EDGE + 512).clamp(0, 1024);
        levels[index] = ((under * 15 + 512) / 1024) as u8;
    }

    // time: one LED per bit, fading in and out with the show
    let now = ch58x_rs_kit::clock::now();
    let bits = [(face.minutes, now.minute as u32), (face.hours, now.hour as u32)];
    for (map, value) in bits {
        for (bit, &index) in map.iter().enumerate() {
            if value & (1 << bit) != 0 {
                if let Some(slot) = levels.get_mut(index as usize) {
                    *slot = env as u8;
                }
            }
        }
    }
}

/// The LED face: its supply (U4, enabled by 4V_ENA), the refresh engine, and the battery
/// measurement, which only works while the supply is on (VBAT_MEAS is taken from 4V_OUT).
///
/// The face is powered only while something is shown: the three LED drivers draw current even
/// with all LEDs off, and the refresh interrupt would keep running.
struct LedFace {
    display: Display,
    ena: Output<'static, peripherals::PB13>,
    adc: hal::adc::Adc<'static, peripherals::ADC>,
    vbat: peripherals::PA4,
    on: bool,
}

/// The LED drivers' lines: released (inputs with pull-down) while the face is unpowered, so
/// they don't feed it through the drivers' input protection diodes.
const FACE_LINES: [(Port, u8); 4] = [
    (Port::A, 8),  // OE
    (Port::A, 9),  // LAT
    (Port::A, 13), // SCLK
    (Port::A, 14), // DAT
];

impl LedFace {
    /// Switch the face on (if it isn't) and start refreshing.
    async fn power_on(&mut self) {
        if self.on {
            return;
        }
        self.ena.set_high();
        // supply ramp (C8, 10 uF) and VBAT_MEAS settling (100k || 100k with C9 = 100 nF: 5 ms)
        Timer::after(Duration::from_millis(25)).await;
        for (port, pin) in FACE_LINES {
            ch58x_rs_kit::power::restore_output(port, pin);
        }
        self.display.resume();
        self.on = true;
    }

    /// Stop refreshing and switch the face off.
    fn power_off(&mut self) {
        self.display.pause();
        for (port, pin) in FACE_LINES {
            ch58x_rs_kit::power::release_pin(port, pin, PinSleep::PullDown);
        }
        self.ena.set_low();
        self.on = false;
    }

    /// Battery voltage in mV; only valid while the face is on (and settled, see power_on).
    /// VBAT_MEAS (PA4/AIN0) is 4V_OUT through a 100k/100k divider: the charger's OUT rail, so
    /// the battery voltage on battery, but the charger's regulated ~4.4 V on USB.
    fn battery_mv(&mut self) -> u16 {
        const SAMPLES: i32 = 16;
        let sum: i32 = (0..SAMPLES).map(|_| self.adc.read_as_millivolts(&mut self.vbat)).sum();
        (sum * 2 / SAMPLES).max(0) as u16 // divider: x2
    }

    /// Measure (face on, LEDs dark), log, and update the Bluetooth Battery Service.
    fn measure_battery(&mut self) -> u16 {
        let mv = self.battery_mv();
        let percent = battery_percent(mv);
        log!("Battery: {} mV, {}%", mv, percent);
        ch58x_rs_kit::ble_periph::battery_service::set_battery(mv, percent);
        mv
    }
}

/// Below this, a wake-up only blinks one LED (3 times) and goes back to sleep, no Bluetooth.
const BATTERY_LOW_MV: u16 = 3600;
/// Below this, a wake-up goes straight back to sleep.
const BATTERY_EMPTY_MV: u16 = 3500;
/// The LED blinked when the battery is low: the rightmost kanji (minutes, bit 0).
const LOW_BATTERY_LED: usize = FACE.minutes[0] as usize;

/// Rough LiPo state of charge (resting voltage), in percent.
fn battery_percent(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 9] = [
        (3500, 0), (3600, 5), (3700, 20), (3750, 35), (3800, 50),
        (3900, 65), (4000, 80), (4100, 92), (4200, 100),
    ];
    if mv <= CURVE[0].0 {
        return 0;
    }
    for w in CURVE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv <= v1 {
            return p0 + ((mv - v0) as u32 * (p1 - p0) as u32 / (v1 - v0) as u32) as u8;
        }
    }
    100
}

/// Low battery: blink one LED 3 times, then sleep.
async fn low_battery_blink(face: &mut LedFace) -> ! {
    let mut levels = [0u8; LEDS];
    face.power_on().await;
    for _ in 0..3 {
        levels[LOW_BATTERY_LED] = 10;
        face.display.set_levels(&levels);
        Timer::after(Duration::from_millis(250)).await;
        face.display.set_levels(&[]);
        Timer::after(Duration::from_millis(250)).await;
    }
    face.power_off();
    sleep_now()
}

/// The clock face: dark (and unpowered) until the button is pressed, then the time for SHOW_MS.
#[embassy_executor::task]
async fn led_task(mut led_face: LedFace) -> ! {
    log!("LED task started");
    let face = &FACE;

    let mut levels = [0u8; LEDS];
    // Let BLE start advertising first: starting the show at the same moment made advertising
    // fail (GAP state 6, error) on every boot.
    for _ in 0..50 {
        if ch58x_rs_kit::ble_periph::is_advertising() {
            break;
        }
        Timer::after(Duration::from_millis(20)).await;
    }
    SHOW_TIME.signal(()); // show the time once after boot
    loop {
        SHOW_TIME.wait().await;
        SHOWING.store(true, Ordering::Relaxed);
        led_face.power_on().await;
        led_face.measure_battery(); // LEDs still dark: no load on the rail
        let now = ch58x_rs_kit::clock::now();
        log!("Showing the time: {:02}:{:02}", now.hour, now.minute);
        let mut t_ms = 0;
        while t_ms <= SHOW_MS {
            if SHOW_TIME.try_take().is_some() {
                t_ms = t_ms.min(FADE_MS); // pressed again: keep showing, without re-fading
            }
            render(face, t_ms, &mut levels);
            led_face.display.set_levels(&levels);
            Timer::after(Duration::from_millis(FRAME_MS as u64)).await;
            t_ms += FRAME_MS;
        }
        led_face.display.set_levels(&[]);
        led_face.power_off();
        SHOWING.store(false, Ordering::Relaxed);
    }
}

/// Hold time on the BOOT button (PB22, active low) that reboots into the USB bootloader.
/// Long enough not to happen by accident: a short press shows the time.
const BOOTLOADER_HOLD: Duration = Duration::from_secs(3);

#[embassy_executor::task]
async fn boot_button_task(pin: peripherals::PB22, woken_by_button: bool) -> ! {
    // external pull-up R17 is fitted too
    let button = Input::new(pin, Pull::Up);
    let poll = Duration::from_millis(20);
    if woken_by_button {
        // the press that woke the watch may still be held: it counts (3 s -> bootloader)
    } else {
        // Arm only after the button has been seen released: BOOT is also on the programming
        // header (J5 pin 8) and is held while plugging in for the ROM bootloader, so it can be
        // low from power-up without anyone pressing it.
        if button.is_low() {
            log!("BOOT is low at startup, button disarmed until it is released");
        }
        while button.is_low() {
            Timer::after(poll).await;
        }
    }
    loop {
        while button.is_high() {
            Timer::after(poll).await;
        }
        // pressed: show the time and be connectable; held long enough: bootloader
        BUTTON_DOWN.store(true, Ordering::Relaxed);
        SHOW_TIME.signal(());
        if hal::ble::stack::is_started() {
            ch58x_rs_kit::ble_periph::advertise_now();
        }
        let mut held = Duration::from_ticks(0);
        while button.is_low() && held < BOOTLOADER_HOLD {
            Timer::after(poll).await;
            held += poll;
        }
        if held >= BOOTLOADER_HOLD {
            log!("BOOT held for {} ms: entering USB bootloader (wchisp)", held.as_millis());
            Timer::after(Duration::from_millis(100)).await; // let usb_log send the line
            ch58x_rs_kit::bootloader::enter();
        }
        BUTTON_DOWN.store(false, Ordering::Relaxed);
        Timer::after(poll).await; // debounce the release
    }
}

/// TEST ONLY: sleep even while a PC uses the USB port (to watch it drop off and come back).
const SLEEP_TEST_IGNORE_USB: bool = false;

/// Sleep as soon as nothing keeps the watch awake; the button wakes it (and reboots it).
#[embassy_executor::task]
async fn power_task() -> ! {
    use ch58x_rs_kit::ble_periph::{is_advertising, is_connected};
    // Stay awake a moment after every boot: BLE starts advertising, the time is shown.
    Timer::after(Duration::from_secs(5)).await;
    loop {
        Timer::after(Duration::from_millis(250)).await;
        let busy = is_connected()
            || is_advertising()
            || SHOWING.load(Ordering::Relaxed)
            || BUTTON_DOWN.load(Ordering::Relaxed)
            || (ch58x_rs_kit::usb_log::is_active() && !SLEEP_TEST_IGNORE_USB) // on USB: stay awake for logs and flashing
            || ch58x_rs_kit::clock::needs_commit(); // the reboot on disconnect writes the RTC
        if busy {
            continue;
        }
        log!(
            "Nothing to do (connected={} advertising={} usb={}): sleeping until the button is pressed",
            is_connected(), is_advertising(), ch58x_rs_kit::usb_log::is_active()
        );
        Timer::after(Duration::from_millis(20)).await; // let UART send the line
        sleep_now();
    }
}

/// Sleep until the button is pressed (then reboot). Only the RTC and the button stay powered.
fn sleep_now() -> ! {
    ch58x_rs_kit::power::sleep_until_pressed(
        (Port::B, 22), // the button (BOOT)
        &[
            (Port::B, 13, PinSleep::PullDown), // 4V_ENA: LED face power off
            (Port::B, 12, PinSleep::PullDown), // USB_BACKFEED_ENA: off
            // ledface signals: its drivers are unpowered, don't feed them
            (Port::A, 8, PinSleep::PullDown),  // OE
            (Port::A, 9, PinSleep::PullDown),  // LAT
            (Port::A, 13, PinSleep::PullDown), // SCLK
            (Port::A, 14, PinSleep::PullDown), // DAT
        ],
    )
}

/// Runs before a fatal halt or bootloader jump: LEDs off, LED supply off, USB backfeed off.
fn safe_state() {
    // peripherals are stolen: normal code never runs again after this hook
    let oe = Output::new(unsafe { peripherals::PA8::steal() }, Level::High, OutputDrive::_5mA);
    let ena = Output::new(unsafe { peripherals::PB13::steal() }, Level::Low, OutputDrive::_5mA);
    let backfeed = Output::new(unsafe { peripherals::PB12::steal() }, Level::Low, OutputDrive::_5mA);
    core::mem::forget((oe, ena, backfeed));
}

#[qingke_rt::interrupt]
fn TMR0() {
    unsafe { ch58x_rs_kit::sr_led_bcm::on_interrupt() };
}

#[qingke_rt::interrupt]
fn USB() {
    unsafe { hal::usb::on_interrupt() };
}

#[embassy_executor::task]
async fn usb_log_task(usb: peripherals::USB) -> ! {
    ch58x_rs_kit::usb_log::run(usb, "ArtsyWatch", RemoteReset::EspStyle).await
}

#[qingke_rt::entry]
fn main() -> ! {
    // hal's executor: sleeps without losing wake-ups (embassy's arch-riscv32 one stalls USB)
    hal::embassy::Executor::new().run(|spawner| spawner.must_spawn(app_main(spawner)))
}

#[embassy_executor::task]
async fn app_main(spawner: Spawner) -> ! {
    let mut config = hal::Config::default();
    config.clock.use_pll_60mhz().enable_lse();
    let p = hal::init(config);
    hal::embassy::init();

    // 4V_ENA: LED face supply, on for the start-up battery check (R11 holds it off until here)
    let ena = Output::new(p.PB13, Level::High, OutputDrive::_5mA);

    let serial = UartTx::new(p.UART0, p.PB7, Default::default()).unwrap();
    init_logger(serial);
    // log! also goes to USB CDC (PB10/PB11), buffered until a terminal opens the port
    ch58x_rs_kit::logger::init_usb();
    spawner.must_spawn(usb_log_task(p.USB));
    log!("\n\n\nArtsyWatch Firmware Started!");
    let woken_by_button = ch58x_rs_kit::power::woken_by_button();
    log!("Reset cause: {}{}", hal::wdog::reset_cause().name(), if woken_by_button { ", woken by the button" } else { "" });

    ch58x_rs_kit::fatal::set_safe_state_hook(safe_state);
    spawner.must_spawn(boot_button_task(p.PB22, woken_by_button));
    // USB_BACKFEED_ENA (U5: the battery powering VBUS, for future USB gadgets): always off for
    // now. No pull-down on the board: it floats until here (and in the ROM bootloader).
    core::mem::forget(Output::new(p.PB12, Level::Low, OutputDrive::_5mA));

    // Shift register control pins for LED matrix column driving
    // SCK - PA13 (SPI Clock)
    // DAT - PA14 (SPI MOSI/Data)
    // OE  - PA8  (Output Enable, directly GPIO-controlled)
    // LAT - PA9  (Latch, directly GPIO-controlled)

    // Initialize shift register LED driver from pins
    // 8 MHz: a 6-byte plane shifts in within ~7 us, far below the shortest plane (~130 us)
    let mut spi_config = hal::spi::Config::default();
    spi_config.frequency = hal::prelude::Hertz::from_raw(8_000_000);
    let sr_driver = SrLedDriver::<_, 48>::new_from_pins_with_config::<false, _, _, _, _>(
        p.SPI0, p.PA13, p.PA14, p.PA8, p.PA9, spi_config,
    );

    log!("Shift register driver initialized");
    log!("  SPI: SCK=PA13, DAT=PA14");
    log!("  GPIO: OE=PA8, LAT=PA9");

    log!("Starting shift register background task...");
    // 16 brightness levels per LED: TMR0 interrupt switches bit-planes (binary code modulation)
    let display = Display::start(sr_driver);
    // ADC default config: gain 1/2 (0..3.15 V; VBAT_MEAS is at most ~2.2 V), input buffer on
    let adc = hal::adc::Adc::new(p.ADC, hal::adc::Config::default());
    let mut led_face = LedFace { display, ena, adc, vbat: p.PA4, on: true };

    // Battery check before anything else starts (the face is on, all LEDs dark)
    Timer::after(Duration::from_millis(25)).await; // VBAT_MEAS settling, see LedFace::power_on
    let battery_mv = led_face.measure_battery();
    if battery_mv < BATTERY_EMPTY_MV {
        log!("Battery empty (< {} mV): back to sleep", BATTERY_EMPTY_MV);
        led_face.power_off();
        Timer::after(Duration::from_millis(20)).await; // let UART send the line
        sleep_now();
    }
    if battery_mv < BATTERY_LOW_MV {
        log!("Battery low (< {} mV): blink, then sleep (no Bluetooth)", BATTERY_LOW_MV);
        low_battery_blink(&mut led_face).await;
    }
    led_face.power_off(); // led_task powers it for each show
    spawner.must_spawn(led_task(led_face));

    let rtc = get_configured_rtc();

    log!("System Clocks: {}", hal::sysctl::clocks().hclk);
    log!("ChipID: 0x{:02x}", hal::signature::get_chip_id());
    log!("RTC datetime: {}", rtc.now());

    log!("BLE Lib Version: {}", ble::lib_version());

    let mut ble_config = ble::Config::default();
    ble_config.pa_config = None;
    ble_config.mac_addr = [0x21, 0x37, 0x04, 0x20, 0x69, 0x96].into();
    let (task_id, sub) = hal::ble::init(ble_config).unwrap();
    log!("BLE hal task id: {}", task_id);

    // Connections are short (set the time, disconnect), so ask for a fast link: 15..30 ms,
    // latency 0 (units 1.25 ms). peripheral()'s battery default (7.5 ms..4 s, latency 1) lets a
    // PC pick multi-second intervals, and GATT discovery then times out in Chrome
    // ("No Services matching UUID ...").
    set_connection_parameters(12, 24, 0);
    // connectable for 20 s after boot (and after a disconnect / button press), then sleep
    ch58x_rs_kit::ble_periph::set_advertising_duration(20_000);

    unsafe {
        common_init_named("ArtsyWatch");
        devinfo_init();
        current_time_init();
        ch58x_rs_kit::ble_periph::battery_service::battery_init();

        // Stay on LE 1M. With a Linux PC (Intel AX211) the link switched to 2M right after
        // connecting, and the WCH link layer then sent the ATT MTU response with LLID
        // "continuation" (btmon: "unexpected continuation"). BlueZ dropped it, GATT discovery
        // never finished, and Chrome reported "No Services matching UUID". Also happens with
        // the original firmware, so it's the BLE library, not this code.
        let phy_1m: u8 = hal::ble::ffi::GAP_PHY_BIT_LE_1M as u8;
        let _ = hal::ble::gap::GAPRole_SetParameter(
            hal::ble::ffi::GAPROLE_PHY_TX_SUPPORTED,
            1,
            &phy_1m as *const _ as _,
        );
        let _ = hal::ble::gap::GAPRole_SetParameter(
            hal::ble::ffi::GAPROLE_PHY_RX_SUPPORTED,
            1,
            &phy_1m as *const _ as _,
        );
    }

    // low-power sleep when idle (see power_task)
    spawner.must_spawn(power_task());

    // Application code
    peripheral(spawner, task_id, sub, rtc, None).await
}

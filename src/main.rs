#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![allow(dead_code)]

extern crate alloc;

use alloc::format;
use alloc_cortex_m::CortexMHeap;
use core::alloc::Layout;

#[global_allocator]
static ALLOCATOR: CortexMHeap = CortexMHeap::empty();

use rtt_target::{rprintln, rtt_init_print};

use core::panic::PanicInfo;

use rtic::app;

use stm32f7xx_hal::rcc::{HSEClock, HSEClockMode};
use stm32f7xx_hal::{
    delay::Delay,
    fmc_lcd::{ChipSelect1, LcdPins},
    gpio::GpioExt,
    pac,
    prelude::*,
};

mod display;
mod external_flash;
mod keypad;
mod led;

use keypad::{Key, KeyMatrix, KeyPad};
use led::Led;

use crate::display::Display;

const HCLK: u32 = 216_000_000;

const HEAP: usize = 32768;

#[app(device = stm32f7xx_hal::pac, peripherals = true)]
const APP: () = {
    #[init]
    fn init(cx: init::Context) {
        // Initialize RTT printing (for debugging).
        rtt_init_print!(BlockIfFull, 4096);
        // Initialize the heap.
        let start = cortex_m_rt::heap_start() as usize;
        unsafe { ALLOCATOR.init(start, HEAP) }

        let mut cp: cortex_m::Peripherals = cx.core;

        let mut dp: pac::Peripherals = cx.device;

        init_mpu(&mut cp.MPU);

        let gpioa = dp.GPIOA.split();
        let gpioc = dp.GPIOC.split();
        let gpiob = dp.GPIOB.split();
        let gpiod = dp.GPIOD.split();
        let gpioe = dp.GPIOE.split();

        // Take ownership of the QSPI pins (to prevent them from being messed with later) and set
        // them to the correct modes.
        let qspi_pins = (
            gpiob.pb2.into_alternate_af9(),
            gpiob.pb6.into_alternate_af10(),
            gpioc.pc9.into_alternate_af9(),
            gpiod.pd12.into_alternate_af9(),
            gpiod.pd13.into_alternate_af9(),
            gpioe.pe2.into_alternate_af9(),
        );

        // Setup external flash over QSPI.
        let mut external_flash =
            external_flash::ExternalFlash::new(&mut dp.RCC, dp.QUADSPI, qspi_pins);

        // Configure the system clocks.
        let rcc = dp.RCC.constrain();
        let clocks = rcc
            .cfgr
            .hse(HSEClock::new(8.mhz(), HSEClockMode::Oscillator))
            .use_pll()
            .sysclk(HCLK.hz())
            .freeze();
        let mut delay = Delay::new(cp.SYST, clocks);

        // Initialize the external flash chip.
        external_flash.init(&mut delay);

        external_flash.mass_erase();

        delay.delay_ms(100_u8);

        // Setup the keypad for reading.
        let keymatrix = KeyMatrix::new(
            gpioa.pa0, gpioa.pa1, gpioa.pa2, gpioa.pa3, gpioa.pa4, gpioa.pa5, gpioa.pa6, gpioa.pa7,
            gpioa.pa8, gpioc.pc0, gpioc.pc1, gpioc.pc2, gpioc.pc3, gpioc.pc4, gpioc.pc5,
        );

        let mut keypad = KeyPad::new(keymatrix);

        // Setup the LED (currently just using it with 7 colours or off).
        let mut led = Led::new(
            gpiob.pb4.into_push_pull_output(),
            gpiob.pb5.into_push_pull_output(),
            gpiob.pb0.into_push_pull_output(),
        );

        led.blue();

        let mut power_state = true;

        // Take onwership of the LCD pins and set them to the correct modes.
        let lcd_pins = LcdPins {
            data: (
                gpiod.pd14.into_alternate_af12(),
                gpiod.pd15.into_alternate_af12(),
                gpiod.pd0.into_alternate_af12(),
                gpiod.pd1.into_alternate_af12(),
                gpioe.pe7.into_alternate_af12(),
                gpioe.pe8.into_alternate_af12(),
                gpioe.pe9.into_alternate_af12(),
                gpioe.pe10.into_alternate_af12(),
                gpioe.pe11.into_alternate_af12(),
                gpioe.pe12.into_alternate_af12(),
                gpioe.pe13.into_alternate_af12(),
                gpioe.pe14.into_alternate_af12(),
                gpioe.pe15.into_alternate_af12(),
                gpiod.pd8.into_alternate_af12(),
                gpiod.pd9.into_alternate_af12(),
                gpiod.pd10.into_alternate_af12(),
            ),
            address: gpiod.pd11.into_alternate_af12(),
            read_enable: gpiod.pd4.into_alternate_af12(),
            write_enable: gpiod.pd5.into_alternate_af12(),
            chip_select: ChipSelect1(gpiod.pd7.into_alternate_af12()),
        };

        // Setup the display.
        let mut display = Display::new(
            lcd_pins,
            dp.FMC,
            gpioe.pe1.into_push_pull_output(),
            gpioc.pc8.into_push_pull_output(),
            gpioe.pe0.into_push_pull_output(),
            gpiob.pb11.into_floating_input(),
            gpiod.pd6.into_push_pull_output(),
            &mut delay,
        );

        // Holds the keys pressed on the previous scan.
        let mut last_pressed: heapless::Vec<Key, 46> = heapless::Vec::new();

        // Whether the calculator is on or off, currently just disables the backlight, clears the
        // screen and stops any key presses except for `Key::Power` from being evaluated.
        let mut off = false;

        led.green();

        // Total number of keypresses.
        let mut key_count = 0usize;

        loop {
            // Read the keys currently pressed.
            let keys = keypad.read(&mut delay);
            // Make sure that the keys currently pressed are not the same as the last scan (done to
            // ensure that keys are not repeated unintentionally).
            if keys != last_pressed {
                // If no keys are pressed there is no need to check for specific keys.
                if !keys.is_empty() {
                    // Check if the power keys is pressed.
                    if keys.contains(&Key::Power) {
                        // If the calculator is currently "on" (meaning the backlight is on and all
                        // keys are being scanned) turn it "off", otherwise turn it back "on".
                        if power_state {
                            // Disable the backlight and clear the screen to avoid burn in.
                            display.set_backlight(0);
                            led.off();
                            display.clear(display::BG_COLOUR);
                            off = true;
                            power_state = false;
                        } else {
                            // re-enable backlight
                            display.set_backlight(1);
                            led.green();
                            off = false;
                            power_state = true;
                        }
                    }

                    // Do not evaluate anything or draw anything to display if the calulator is
                    // "off".
                    if !off {
                        // If `Key::EXE` is pressed create a new line and do not do anything else.
                        if keys.contains(&Key::EXE) {
                            // Push the text in the input bar into the output display.
                            display.write_bottom_to_top();
                            // Write the key count (with padding so that it appears left alligned)
                            // to the output section of the display.
                            display.write_top(&format!("\n{: >52}", key_count));
                            // Draw both sections of the display.
                            display.draw_all();
                        } else {
                            // Set `shift` to `true` if `Key::Shift` is pressed.
                            let shift = keys.contains(&Key::Shift);
                            // Evaluate all the keys pressed on the keypad.
                            for key in keys.iter() {
                                // Get the pressed key's corresponding character, will be `\0` if
                                // the key does not have a character, will probably change this in
                                // the future to be strings, or completely redesign the console...
                                let mut key_char = char::from(*key);
                                if key_char != '\0' {
                                    if shift {
                                        key_char = key_char.to_ascii_uppercase();
                                    }
                                    let mut tmp = [0u8; 4];
                                    if display.write_bottom(key_char.encode_utf8(&mut tmp), true) {
                                        key_count += 1;
                                    }
                                // If `Key::Delete` is pressed, remove the last character from the
                                // input display box
                                } else if key == &Key::Delete {
                                    display.pop_bottom(true);
                                // If `Key::Clear` is pressed (`Key::Delete` and `Key::Shift`)
                                // remove all text from the input display box.
                                } else if key == &Key::Clear {
                                    display.clear_bottom(true);
                                }
                            }
                        }
                    }
                }
                last_pressed = keys;
            }
        }
    }
};

#[inline(never)]
#[alloc_error_handler]
fn oom(layout: Layout) -> ! {
    panic!("OOM: {:?}", layout);
}

#[inline(never)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rprintln!("{}", info);
    cortex_m::peripheral::SCB::sys_reset();
}

fn init_mpu(mpu: &mut cortex_m::peripheral::MPU) {
    unsafe {
        const FULL_ACCESS: u32 = 0b011 << 24;
        const SIZE_512MB: u32 = (29 - 1) << 1;
        const DEVICE_SHARED: u32 = 0b000001 << 16;
        const NORMAL_SHARED: u32 = 0b000110 << 16;

        // Flash
        mpu.rnr.write(0);
        mpu.rbar.write(0x0000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | 1);

        // SRAM
        mpu.rnr.write(1);
        mpu.rbar.write(0x2000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | NORMAL_SHARED | 1);

        // Peripherals
        mpu.rnr.write(2);
        mpu.rbar.write(0x4000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | DEVICE_SHARED | 1);

        // FSMC
        mpu.rnr.write(3);
        mpu.rbar.write(0x6000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | DEVICE_SHARED | 1);

        // FSMC
        mpu.rnr.write(4);
        mpu.rbar.write(0xA000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | DEVICE_SHARED | 1);

        // Core peripherals
        mpu.rnr.write(5);
        mpu.rbar.write(0xE000_0000);
        mpu.rasr.write(FULL_ACCESS | SIZE_512MB | 1);

        // Enable MPU
        mpu.ctrl.write(1);
    }
}

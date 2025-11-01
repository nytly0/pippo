use anyhow::{self};
use embedded_graphics::{
  mono_font::{ascii::FONT_5X8, iso_8859_10::FONT_10X20, MonoTextStyleBuilder},
  pixelcolor::BinaryColor,
  prelude::*,
  text::{Baseline, Text},
};
use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};
use esp_idf_hal::units::*;
use esp_idf_hal::{
  delay::FreeRtos,
  ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution},
  peripherals::Peripherals,
};
use esp_idf_hal::{gpio::PinDriver, i2c::*};
use esp_idf_svc::eventloop::EspSystemEventLoop;

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use rand::Rng;
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
// use std::cell::UnsafeCell;
use std::{time::Duration, time::Instant};
mod constants;
mod utils;
use constants::{PASSWORD, SSID};
use utils::map; // include your API key

fn main() -> anyhow::Result<()> {
  initialize();

  let peripherals = Peripherals::take().unwrap();
  let sysloop = EspSystemEventLoop::take()?;
  let nvs = EspDefaultNvsPartition::take()?;

  let button = PinDriver::input(peripherals.pins.gpio33)?;
  // Initialize I2C SSD1306 Display
  let mut display = {
    let config = I2cConfig::new().baudrate(100.kHz().into());
    let sda = peripherals.pins.gpio26;
    let scl = peripherals.pins.gpio25;
    let i2c =
      esp_idf_hal::i2c::I2cDriver::new(peripherals.i2c0, sda, scl, &config)?;
    let interface = I2CDisplayInterface::new(i2c);
    Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
      .into_buffered_graphics_mode()
  };
  let mut wifi = BlockingWifi::wrap(
    EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs))?,
    sysloop,
  )?;
  let mut led = PinDriver::output(peripherals.pins.gpio27)?;
  let timer_driver = LedcTimerDriver::new(
    peripherals.ledc.timer0,
    &TimerConfig::default()
      .frequency(50.Hz())
      .resolution(Resolution::Bits14),
  )
  .unwrap();

  // Configure and Initialize LEDC Driver
  let mut driver = LedcDriver::new(
    peripherals.ledc.channel0,
    timer_driver,
    peripherals.pins.gpio32,
  )
  .unwrap();
  let text_style_face = MonoTextStyleBuilder::new()
    .font(&FONT_10X20)
    .text_color(BinaryColor::On)
    .build();
  let text_style_settings = MonoTextStyleBuilder::new()
    .font(&FONT_5X8)
    .text_color(BinaryColor::On)
    .build();

  wifi.set_configuration(&Configuration::Client(ClientConfiguration {
    ssid: SSID.try_into().unwrap(),
    bssid: None,
    auth_method: AuthMethod::None,
    password: PASSWORD.try_into().unwrap(),
    channel: None,
    ..Default::default()
  }))?;

  wifi.start()?;
  display.init().unwrap();

  wifi.connect()?;

  wifi.wait_netif_up()?;

  log::info!("Connected to WiFi!");

  // Get Max Duty and Calculate Upper and Lower Limits for Servo
  let max_duty = driver.get_max_duty();
  println!("Max Duty {}", max_duty);
  let min_limit = max_duty * 25 / 1000;
  println!("Min Limit {}", min_limit);
  let max_limit = max_duty * 125 / 1000;
  println!("Max Limit {}", max_limit);

  // Define Starting Position
  driver
    .set_duty(map(0, 0, 180, min_limit, max_limit))
    .unwrap();
  // Give servo some time to update
  FreeRtos::delay_ms(500);
  // Loop to Avoid Program Termination
  let mut last = Instant::now();
  let mut blinking = false;
  let mut blink_delay =
    Duration::from_millis(rand::rng().random_range(4000..7000));
  let mut in_main_screen = false;
  let mut button_interaction = false;
  loop {
    led.set_high().unwrap();
    
    // Check Button State (Debug)
    if button.is_high() {
      log::info!("Button Pressed");
      button_interaction = true;
    }
    if button_interaction {
      log::info!("In Main Screen");
      in_main_screen = true;
      display.clear(BinaryColor::Off).unwrap();
      main_screen(
        &mut display,
        text_style_settings,
        true,
        false,
        false,
      );
      display.flush().unwrap();
    }
    if button.is_low() && button_interaction {
      log::info!("Button Released");
      button_interaction = false;
      in_main_screen = false;
      display.clear(BinaryColor::Off).unwrap();
    }
    
    draw_neutral_face(
      &mut display,
      text_style_face,
      &mut last,
      &mut blinking,
      &mut blink_delay,
    );

    std::thread::sleep(Duration::from_millis(10));
  }
}

fn initialize() {
  esp_idf_svc::sys::link_patches();
  esp_idf_svc::log::EspLogger::initialize_default();
  log::info!("Initialization complete!");
}

fn main_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
  settings_selected: bool,
  status_selected: bool,
  exit_selected: bool,
) {
  let settings_indicator = if settings_selected { "> " } else { " " };
  let status_indicator = if status_selected { "> " } else { " " };
  let exit_indicator = if exit_selected { "> " } else { " " };
  let y_level = 15;
  Text::with_baseline(
    format!("{settings_indicator}Settings").as_str(),
    Point::new(10, y_level),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    format!("{status_indicator}Status").as_str(),
    Point::new(10, y_level + 8),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    format!("{exit_indicator}Exit").as_str(),
    Point::new(10, y_level + 16),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}
fn draw_neutral_face(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
  last: &mut Instant,
  blinking: &mut bool,
  blink_delay: &mut Duration,
) {
  let elapsed = last.elapsed();

  if !*blinking && elapsed >= *blink_delay {
    // close eyes
    display.clear(BinaryColor::Off).unwrap();
    Text::with_baseline(
      "-      -",
      Point::new(20, 14),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    Text::with_baseline(
      "   --   ",
      Point::new(20, 34),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    display.flush().unwrap();
    *blinking = true;
    *last = Instant::now();
  } else if *blinking && elapsed >= Duration::from_millis(100) {
    // open eyes
    display.clear(BinaryColor::Off).unwrap();
    Text::with_baseline(
      ".      .",
      Point::new(20, 14),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    Text::with_baseline(
      "   --   ",
      Point::new(20, 34),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    display.flush().unwrap();
    *blinking = false;
    *blink_delay = Duration::from_millis(rand::rng().random_range(4000..7000));
    *last = Instant::now();
  }
}

#[allow(dead_code)]
fn draw_happy_face(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
) {
  display.clear(BinaryColor::Off).unwrap();

  Text::with_baseline(
    "^      ^",
    Point::new(20, 14),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    "   --   ",
    Point::new(20, 34),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}

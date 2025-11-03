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
mod utils;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UiState {
  Face,
  Menu,
  Settings,
  Status,
  Exit,
}

// PINS
// LED: GPIO2
// BUTTON: GPIO23
// I2C SDA: GPIO21
// I2C SCL: GPIO22
fn main() -> anyhow::Result<()> {
  initialize();

  let peripherals = Peripherals::take().unwrap();
  let system_event_loop = EspSystemEventLoop::take()?;
  let non_volatile_storage = EspDefaultNvsPartition::take()?;

  let mut button = PinDriver::input(peripherals.pins.gpio23)?;

  // Enable internal pull-up resistor on button pin (Thanks Google)
  button.set_pull(esp_idf_hal::gpio::Pull::Up)?;
  // Initialize I2C SSD1306 Display
  let mut display = {
    let config = I2cConfig::new().baudrate(100.kHz().into());
    let sda = peripherals.pins.gpio21;
    let scl = peripherals.pins.gpio22;
    let i2c =
      esp_idf_hal::i2c::I2cDriver::new(peripherals.i2c0, sda, scl, &config)?;
    let interface = I2CDisplayInterface::new(i2c);
    Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
      .into_buffered_graphics_mode()
  };
  // let mut wifi = BlockingWifi::wrap(
  //   EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs))?,
  //   sysloop,   
  // )?;
  let mut led = PinDriver::output(peripherals.pins.gpio2)?;
  // let timer_driver = LedcTimerDriver::new(
  //   peripherals.ledc.timer0,
  //   &TimerConfig::default()
  //     .frequency(50.Hz())
  //     .resolution(Resolution::Bits14),
  // )
  // .unwrap();

  // Configure and Initialize LEDC Driver
  // let mut driver = LedcDriver::new(
  //   peripherals.ledc.channel0,
  //   timer_driver,
  //   peripherals.pins.gpio32,
  // )
  // .unwrap();
  let text_style_face = MonoTextStyleBuilder::new()
    .font(&FONT_10X20)
    .text_color(BinaryColor::On)
    .build();
  let text_style_settings = MonoTextStyleBuilder::new()
    .font(&FONT_5X8)
    .text_color(BinaryColor::On)
    .build();

  // wifi.set_configuration(&Configuration::Client(ClientConfiguration {
  //   ssid: SSID.try_into().unwrap(),
  //   bssid: None,
  //   auth_method: AuthMethod::None,
  //   password: PASSWORD.try_into().unwrap(),
  //   channel: None,
  //   ..Default::default()
  // }))?;

  // wifi.start()?;
  display.init().unwrap();

  // wifi.connect()?;

  // wifi.wait_netif_up()?;

  // log::info!("Connected to WiFi!");

  // Get Max Duty and Calculate Upper and Lower Limits for Servo
  // let max_duty = driver.get_max_duty();
  // println!("Max Duty {}", max_duty);
  // let min_limit = max_duty * 25 / 1000;
  // println!("Min Limit {}", min_limit);
  // let max_limit = max_duty * 125 / 1000;
  // println!("Max Limit {}", max_limit);

  // Define Starting Position
  // driver
  //   .set_duty(map(0, 0, 180, min_limit, max_limit))
  //   .unwrap();
  // Give servo some time to update
  FreeRtos::delay_ms(500);
  // Loop to Avoid Program Termination
  let mut last = Instant::now();
  let mut blinking = false;
  let mut blink_delay =
    Duration::from_millis(rand::rng().random_range(3000..7000));
  let mut idle_delay =
    Duration::from_millis(rand::rng().random_range(7000..10000));
  let mut idle = false;
  let mut ui_state = UiState::Face;

  // Button handling states
  let mut option_index: u8 = 0;
  let mut btn_down = false; // debounced current state
  let mut btn_raw_last = false; // last raw read
  let mut btn_changed_at = Instant::now(); // debounce timer
  let mut btn_pressed_at = Instant::now(); // press start time
  let mut long_fired = false; // long press fired once

  const DEBOUNCE_MS: u64 = 30;
  const LONG_PRESS_MS: u64 = 1600;

  loop {
    // Read raw button
    let raw = button.is_low();
    let now = Instant::now();

    // Debounce
    if raw != btn_raw_last {
      btn_raw_last = raw;
      btn_changed_at = now;
    }
    let stable =
      now.duration_since(btn_changed_at) >= Duration::from_millis(DEBOUNCE_MS);

    // Edge detection on stable transitions
    if stable {
      // Rising edge (pressed)
      if raw && !btn_down {
        btn_down = true;
        btn_pressed_at = now;
        long_fired = false;
      }

      // Long press while held
      if btn_down
        && !long_fired
        && now.duration_since(btn_pressed_at)
          >= Duration::from_millis(LONG_PRESS_MS)
      {
        long_fired = true;
        // Selection or navigation on long press
        handle_long_press(&mut ui_state, option_index);
      }

      // Falling edge (released)
      if !raw && btn_down {
        btn_down = false;
        // Short press actions (only if long didn't fire)
        if !long_fired {
          handle_short_press(ui_state, &mut option_index);
        }
      }
    }

    // LED reflects button state (pressed -> low)
    handle_led(&mut led, btn_down);

    // Render by state
    match ui_state {
      UiState::Face => {
        display.clear(BinaryColor::Off).unwrap();
        draw_neutral_face(
          &mut display,
          text_style_face,
          &mut last,
          &mut blinking,
          &mut idle,
          &mut blink_delay,
          &mut idle_delay,
        );
      }
      UiState::Menu => {
        // Avoid flicker: only redraw when not holding the button
        if !btn_down {
          display.clear(BinaryColor::Off).unwrap();
          match option_index {
            0 => {
              main_screen(&mut display, text_style_settings, true, false, false)
            }
            1 => {
              main_screen(&mut display, text_style_settings, false, true, false)
            }
            2 => {
              main_screen(&mut display, text_style_settings, false, false, true)
            }
            _ => unreachable!(),
          }
          display.flush().unwrap();
        }
      }
      UiState::Settings => {
        display.clear(BinaryColor::Off).unwrap();
        draw_settings_screen(&mut display, text_style_settings);
      }
      UiState::Status => {
        display.clear(BinaryColor::Off).unwrap();
        draw_status_screen(&mut display, text_style_settings);
      }
      UiState::Exit => {
        display.clear(BinaryColor::Off).unwrap();
        draw_exit_screen(&mut display, text_style_settings);
      }
    }

    FreeRtos::delay_ms(20);
  }
}

fn handle_long_press(ui_state: &mut UiState, option_index: u8) {
    match *ui_state {
      UiState::Face => *ui_state = UiState::Menu, // long press from face opens menu
      UiState::Menu => match option_index {
        0 => *ui_state = UiState::Settings,
        1 => *ui_state = UiState::Status,
        2 => *ui_state = UiState::Exit,
        _ => *ui_state = UiState::Menu,
      },
      // long press on any sub-screen returns to face
      _ => *ui_state = UiState::Face,
    };
}

fn handle_short_press(ui_state: UiState, option_index: &mut u8) {
    match ui_state {
        UiState::Menu => {
          *option_index = (*option_index + 1) % 3; // cycle options
          UiState::Menu
        }
        // short press on sub-screen goes back to Menu
        UiState::Settings | UiState::Status | UiState::Exit => {
          UiState::Menu
        }
        // short press on face does nothing
        UiState::Face => UiState::Face,
      };
}

fn handle_led(
  led: &mut PinDriver<'_, esp_idf_hal::gpio::Gpio2, esp_idf_hal::gpio::Output>,
  btn_down: bool,
) {
  if btn_down {
    led.set_high().unwrap();
  } else {
    led.set_low().unwrap();
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
  idle: &mut bool,
  blink_delay: &mut Duration,
  idle_delay: &mut Duration,
) {
  let elapsed = last.elapsed();

  const BLINK_EYES: &str = "-      -";
  const NORMAL_EYES: &str = ".      .";
  const MOUTH: &str = "   --   ";
  // add idle expression

  if !*blinking && elapsed >= *blink_delay {
    // close eyes
    display.clear(BinaryColor::Off).unwrap();
    Text::with_baseline(
      BLINK_EYES,
      Point::new(20, 14),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    Text::with_baseline(MOUTH, Point::new(20, 34), text_style, Baseline::Top)
      .draw(display)
      .unwrap();
    display.flush().unwrap();
    *blinking = true;
    *last = Instant::now();
  } else if *blinking && elapsed >= Duration::from_millis(100) {
    // open eyes
    display.clear(BinaryColor::Off).unwrap();
    Text::with_baseline(
      NORMAL_EYES,
      Point::new(20, 14),
      text_style,
      Baseline::Top,
    )
    .draw(display)
    .unwrap();
    Text::with_baseline(MOUTH, Point::new(20, 34), text_style, Baseline::Top)
      .draw(display)
      .unwrap();
    display.flush().unwrap();
    *blinking = false;
    *blink_delay = Duration::from_millis(rand::rng().random_range(4000..7000));
    *last = Instant::now();
    // add idle
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

fn draw_settings_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
) {
  Text::with_baseline(
    "Settings",
    Point::new(10, 10),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    "Short: Back",
    Point::new(10, 26),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    "Long: Face",
    Point::new(10, 34),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}

fn draw_status_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
) {
  Text::with_baseline("Status", Point::new(10, 10), text_style, Baseline::Top)
    .draw(display)
    .unwrap();
  Text::with_baseline(
    "Short: Back",
    Point::new(10, 26),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    "Long: Face",
    Point::new(10, 34),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}

fn draw_exit_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
) {
  Text::with_baseline("Exit", Point::new(10, 10), text_style, Baseline::Top)
    .draw(display)
    .unwrap();
  Text::with_baseline(
    "Short: Back",
    Point::new(10, 26),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    "Long: Face",
    Point::new(10, 34),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}

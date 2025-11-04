use anyhow::{self};
use chrono::{DateTime, Local, Utc};
use embedded_graphics::{
  mono_font::{
    MonoTextStyleBuilder,
  },
  pixelcolor::BinaryColor,
  prelude::*,
  primitives::{
    Arc as GraphicsArc, CornerRadii, Line, PrimitiveStyle, Rectangle,
    RoundedRectangle,
  },
  text::{Baseline, Text},
};
use embedded_svc::{
  http::client::Client,
  wifi::{AuthMethod, ClientConfiguration, Configuration},
};
use esp_idf_hal::{
  delay::FreeRtos,
  ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution},
  peripherals::Peripherals,
};
use esp_idf_hal::{gpio::PinDriver, i2c::*};
use esp_idf_hal::{io::Read, units::*};
use esp_idf_svc::http::server::{
  Configuration as HttpServerConfig, EspHttpServer,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use esp_idf_svc::{
  eventloop::EspSystemEventLoop, http::client::EspHttpConnection,
};
use esp_idf_svc::{
  http::{client::Configuration as HttpClientConfiguration, Method},
  sntp::EspSntp,
};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use std::sync::{Arc, Mutex};
use std::{time::Duration, time::Instant};
mod utils;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UiState {
  Home,
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
  // Initialize I2C SSD1306 Display (Yellow and Blue Pixels)
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

  let mut led = PinDriver::output(peripherals.pins.gpio2)?;
  let buzzer = Arc::new(Mutex::new(PinDriver::output(peripherals.pins.gpio5)?));

  let mut motion_sensor = PinDriver::input(peripherals.pins.gpio15)?;
  motion_sensor
    .set_interrupt_type(esp_idf_hal::gpio::InterruptType::AnyEdge)?;
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
    peripherals.pins.gpio4,
  )
  .unwrap();
  let text_style_settings = MonoTextStyleBuilder::new()
    .font(&embedded_graphics::mono_font::ascii::FONT_7X13)
    .text_color(BinaryColor::On)
    .build();

  display.init().unwrap();
  boot_screen(&mut display, text_style_settings);
  let mut wifi = BlockingWifi::wrap(
    EspWifi::new(
      peripherals.modem,
      system_event_loop.clone(),
      Some(non_volatile_storage),
    )?,
    system_event_loop,
  )?;
  wifi.set_configuration(&Configuration::Client(ClientConfiguration {
    ssid: "A 403".try_into().unwrap(),
    bssid: None,
    auth_method: AuthMethod::None,
    password: "38YZ5VQF".try_into().unwrap(),
    channel: None,
    ..Default::default()
  }))?;

  wifi.start()?;
  wifi.connect()?;

  wifi.wait_netif_up()?;

  log::info!("Connected to WiFi!");

  // get weather from API
  let weather_json = get_weather("https://api.weatherapi.com/v1/current.json?key=2b6e79acb58f407bba4125239250411&q=18.555917,73.764256")?;
  let parsed: serde_json::Value = serde_json::from_str(&weather_json)?;
  let temp = parsed["current"]["temp_c"].as_f64().unwrap();
  let weather_condition = parsed["current"]["condition"]["text"]
    .as_str()
    .unwrap_or("Unknown");
  let humidity = parsed["current"]["humidity"].as_u64().unwrap_or(0);

  let ntp = EspSntp::new_default().unwrap();

  println!("Synchronizing with NTP Server");
  while ntp.get_sync_status() != esp_idf_svc::sntp::SyncStatus::Completed {}

  let mut http_server = EspHttpServer::new(&HttpServerConfig::default())?;
  http_server.fn_handler(
    "/",
    Method::Get,
    |request| -> Result<(), anyhow::Error> {
      let html = index_html();
      let mut response = request.into_ok_response()?;
      response.write(html.as_bytes())?;
      Ok(())
    },
  )?;
  let buzzer_clone = Arc::clone(&buzzer);
  http_server.fn_handler(
    "/buzz",
    Method::Get,
    move |request| -> Result<(), anyhow::Error> {
      let html = buzz_html();
      let mut response = request.into_ok_response()?;
      {
        let mut buzzer_lock = buzzer_clone.lock().unwrap();
        buzzer_lock.set_high().unwrap();
      }
      FreeRtos::delay_ms(200);
      {
        let mut buzzer_lock = buzzer_clone.lock().unwrap();
        buzzer_lock.set_low().unwrap();
      }
      response.write(html.as_bytes())?;
      Ok(())
    },
  )?;
  // Give servo some time to update
  FreeRtos::delay_ms(500);
  // Loop to Avoid Program Termination
  let mut ui_state = UiState::Home;

  // Button handling states
  let mut option_index: u8 = 0;
  let mut btn_down = false; // debounced current state
  let mut btn_raw_last = false; // last raw read
  let mut btn_changed_at = Instant::now(); // debounce timer
  let mut btn_pressed_at = Instant::now(); // press start time
  let mut long_fired = false; // long press fired once
  let mut motion_detected = false;

  const DEBOUNCE_MS: u64 = 30;
  const LONG_PRESS_MS: u64 = 1600;

  loop {
    let st_now = std::time::SystemTime::now();
    // Convert to IST
    let local_date_now: DateTime<Local> = st_now.into();
    // Format Time String having date and time
    let formatted_time = local_date_now.format("%d/%m %H:%M").to_string();

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
          handle_short_press(&mut ui_state, &mut option_index);
        }
      }
    }

    // LED reflects button state (pressed -> low)
    handle_led(&mut led, btn_down);
    // Render by state

    match ui_state {
      UiState::Home => {
        display.clear(BinaryColor::Off).unwrap();
        home_screen(&mut display, text_style_settings, formatted_time.as_str());
      }
      UiState::Menu => {
        // Avoid flicker: only redraw when not holding the button
        if !btn_down {
          display.clear(BinaryColor::Off).unwrap();
          match option_index {
            0 => {
              menu_screen(&mut display, text_style_settings, true, false, false)
            }
            1 => {
              menu_screen(&mut display, text_style_settings, false, true, false)
            }
            2 => {
              menu_screen(&mut display, text_style_settings, false, false, true)
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
        draw_status_screen(
          &mut display,
          text_style_settings,
          temp,
          weather_condition,
          humidity,
          formatted_time.as_str(),
        );
      }
      UiState::Exit => {
        display.clear(BinaryColor::Off).unwrap();
        draw_exit_screen(&mut display, text_style_settings);
      }
    }

    FreeRtos::delay_ms(20);
  }
}

fn boot_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style_settings: embedded_graphics::mono_font::MonoTextStyle<
    '_,
    BinaryColor,
  >,
) {
  display.clear(BinaryColor::Off).unwrap();

  Text::with_baseline(
    "pippo is booting...",
    Point::new(30, 3),
    text_style_settings,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();

  display.flush().unwrap();
}

fn handle_long_press(ui_state: &mut UiState, option_index: u8) {
  match *ui_state {
    UiState::Home => *ui_state = UiState::Menu, // long press from home opens menu
    UiState::Menu => match option_index {
      0 => *ui_state = UiState::Settings,
      1 => *ui_state = UiState::Status,
      2 => *ui_state = UiState::Exit,
      _ => *ui_state = UiState::Menu,
    },
    // long press on any sub-screen returns to home
    _ => *ui_state = UiState::Home,
  };
}

fn handle_short_press(ui_state: &mut UiState, option_index: &mut u8) {
  match *ui_state {
    UiState::Menu => {
      *option_index = (*option_index + 1) % 3;
    }
    UiState::Settings | UiState::Status | UiState::Exit => {
      *option_index = 0;
      *ui_state = UiState::Menu; // now actually updates
    }
    UiState::Home => {}
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
fn home_screen(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
  text_style: embedded_graphics::mono_font::MonoTextStyle<'_, BinaryColor>,
  formatted_time: &str,
) {
  Text::with_baseline(
    formatted_time,
    Point::new(1, 1),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  draw_wifi_icon(display);

  // centered "Welcome!" text
  let welcome_text = "Welcome!";
  let text_width = welcome_text.len() as i32 * 6; // Approximate width per character
  let x_position = (128 - text_width) / 2; // Center horizontally
  let y_position = (64 - 8) / 2; // Center vertically (assuming 8px height)
  Text::with_baseline(
    welcome_text,
    Point::new(x_position, y_position),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  display.flush().unwrap();
}
fn menu_screen(
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
  temp: f64,
  weather_condition: &str,
  humidity: u64,
  formatted: &str,
) {
  Text::with_baseline("Status", Point::new(10, 7), text_style, Baseline::Top)
    .draw(display)
    .unwrap();

  Text::with_baseline(
    format!("Temperature: {}°C", temp).as_str(),
    Point::new(10, 26),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    format!("Condition: {}", weather_condition).as_str(),
    Point::new(10, 34),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();

  Text::with_baseline(
    format!("Humidity: {}%", humidity).as_str(),
    Point::new(10, 42),
    text_style,
    Baseline::Top,
  )
  .draw(display)
  .unwrap();
  Text::with_baseline(
    format!("Time: {}", formatted).as_str(),
    Point::new(10, 50),
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

fn get_weather(api_url: &str) -> anyhow::Result<String> {
  log::info!("Fetching weather data from API: {}", api_url);

  let connection = EspHttpConnection::new(&HttpClientConfiguration {
    use_global_ca_store: true,
    crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
    ..Default::default()
  })?;
  let mut client = Client::wrap(connection);

  let headers = [("accept", "application/json")];
  let request = client.request(Method::Get, api_url.as_ref(), &headers)?;

  let response = request.submit()?;
  let status = response.status();

  println!("Response code: {}\n", status);
  match status {
    200..=299 => {
      let mut buf = [0_u8; 512]; // Increased for larger JSON
      let mut offset = 0;
      let mut total = 0;
      let mut reader = response;
      let mut json_response = String::new(); // Accumulate response here

      loop {
        if let Ok(size) = Read::read(&mut reader, &mut buf[offset..]) {
          if size == 0 {
            break;
          }
          total += size;
          let size_plus_offset = size + offset;
          match str::from_utf8(&buf[..size_plus_offset]) {
            Ok(text) => {
              json_response.push_str(text); // Append to string
              offset = 0;
            }
            Err(error) => {
              let valid_up_to = error.valid_up_to();
              unsafe {
                json_response
                  .push_str(str::from_utf8_unchecked(&buf[..valid_up_to]));
              }
              buf.copy_within(valid_up_to.., 0);
              offset = size_plus_offset - valid_up_to;
            }
          }
        }
      }
      log::info!("Total: {} bytes", total);
      Ok(json_response) // Return the accumulated JSON
    }
    _ => {
      anyhow::bail!("Request failed with status: {}", status)
    }
  }
}

fn draw_wifi_icon(
  display: &mut Ssd1306<
    I2CInterface<I2cDriver<'_>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
  >,
) {
  let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

  // First line: (125, 0) to (120, 5)
  Line::new(Point::new(125, 0), Point::new(120, 5))
    .into_styled(style)
    .draw(display)
    .unwrap();

  // Second line: (120, 5) to (125, 10)
  Line::new(Point::new(120, 5), Point::new(125, 10))
    .into_styled(style)
    .draw(display)
    .unwrap();

  // Third line: (122, 0) to (122, 10)
  Line::new(Point::new(122, 0), Point::new(122, 10))
    .into_styled(style)
    .draw(display)
    .unwrap();
}

fn index_html() -> String {
  include_str!("../web/index.html").to_string()
}
fn buzz_html() -> String {
  include_str!("../web/buzz.html").to_string()
}

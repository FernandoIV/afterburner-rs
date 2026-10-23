//! Minimal ANSI dashboard (TUI) — replaces the skinned Windows GUI
//! (MSIWin11Dark.usf skin there, plain terminal here).

use std::io::Write;
use std::time::Duration;

pub fn enter_alt() {
    print!("\x1b[?1049h\x1b[?25l");
}
pub fn leave_alt() {
    print!("\x1b[?25h\x1b[?1049l");
    std::io::stdout().flush().ok();
}

pub fn bar(v: Option<f32>, max: f32, width: usize) -> String {
    let v = v.unwrap_or(0.0);
    let frac = (v / max.clamp(1.0, 1e9)).clamp(0.0, 1.0);
    let filled = (frac * width as f32) as usize;
    let empty = width - filled;
    let color = if frac < 0.5 {
        "\x1b[32m"
    } else if frac < 0.8 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    };
    format!("{color}{}{}\x1b[0m", "█".repeat(filled), "░".repeat(empty))
}

pub fn home_line(row: u16, col: u16, s: &str) {
    print!("\x1b[{row};{col}H\x1b[2K{s}");
    std::io::stdout().flush().ok();
}

pub fn clear() {
    print!("\x1b[2J\x1b[H");
    std::io::stdout().flush().ok();
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

pub fn temps_color(v: f32) -> &'static str {
    if v < 60.0 {
        "\x1b[32m"
    } else if v < 80.0 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    }
}

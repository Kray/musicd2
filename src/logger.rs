use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use chrono::prelude::*;
use log::{Level, Metadata, Record};

use crate::musicd_c::{self, LogLevel};

pub struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.target().starts_with("musicd2")
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let mut target = record.target();
        if !target.starts_with("musicd2") {
            return;
        }

        if target.starts_with("musicd2::") {
            target = target.get(("musicd2::").len()..).unwrap();
        }

        eprintln!(
            "{} {:05} [{}] {}",
            Local::now().format("%F %T"),
            record.level(),
            target,
            record.args()
        );
    }

    fn flush(&self) {}
}

thread_local!(static LOG_C_BUF: RefCell<String> = RefCell::new(String::new()));

extern "C" fn log_c_callback(level: c_int, message: *const c_char) {
    let log_level = if level == LogLevel::LogLevelError as i32 {
        Level::Error
    } else if level == LogLevel::LogLevelWarn as i32 {
        Level::Warn
    } else if level == LogLevel::LogLevelInfo as i32 {
        Level::Info
    } else if level == LogLevel::LogLevelDebug as i32 {
        Level::Debug
    } else if level == LogLevel::LogLevelTrace as i32 {
        Level::Trace
    } else {
        return;
    };

    let c_str: &CStr = unsafe { CStr::from_ptr(message) };
    let string = String::from_utf8_lossy(c_str.to_bytes());

    LOG_C_BUF.with(|buf| {
        let buf = &mut *buf.borrow_mut();

        *buf += &string;

        if buf.ends_with('\n') {
            buf.pop();
            log!(target: "musicd2::c", log_level, "{}", buf);
            buf.clear();
        }
    });
}

static LOGGER: Logger = Logger;

pub fn init(log_level: &str) {
    let level = match log_level {
        "error" => Level::Error,
        "warn" => Level::Warn,
        "info" => Level::Info,
        "debug" => Level::Debug,
        "trace" => Level::Trace,
        _ => unreachable!(),
    };

    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(level.to_level_filter());

    unsafe {
        musicd_c::musicd_log_setup(log_c_callback);
    }
}

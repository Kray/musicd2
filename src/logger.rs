use chrono::prelude::*;
use log::{Level, Metadata, Record};

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
    log::set_max_level(level.to_level_filter())
}

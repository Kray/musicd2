use chrono::prelude::*;
use log::{Level, LevelFilter, Metadata, Record};

pub struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) && record.target().starts_with("musicd2::") {
            eprintln!(
                "{} {:05} [{}] {}",
                Local::now().format("%F %T"),
                record.level(),
                record.target().get(("musicd2::").len()..).unwrap(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn init() {
    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(LevelFilter::Trace))
        .unwrap();
}

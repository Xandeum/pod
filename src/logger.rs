use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record, SetLoggerError};
use std::sync::OnceLock;
struct Logger {}

impl Logger {
    fn new() -> Self {
        Logger {}
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            println!(
                "[{}] {} [{}:{}] - {}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.module_path().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init_logger() -> Result<(), SetLoggerError> {
    let logger = Logger::new();
    LOGGER.set(logger).ok();
    log::set_logger(LOGGER.get().expect("Logger should be initialized"))?;
    log::set_max_level(LevelFilter::Info);
    Ok(())
}

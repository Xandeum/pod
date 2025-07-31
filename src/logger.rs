use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record, SetLoggerError};
use std::sync::OnceLock;

struct Logger {}

impl Logger {
    fn new() -> Self {
        Logger {}
    }
    
    fn format_level(level: Level) -> &'static str {
        match level {
            Level::Error => "ERROR",
            Level::Warn  => "WARN ",
            Level::Info  => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level = Self::format_level(record.level());
            let time = Local::now().format("%H:%M:%S%.3f");
            let module = record.module_path().unwrap_or("unknown");
            
            // Clean up module name to be more concise
            let clean_module = if module.starts_with("pod::") {
                &module[5..] // Remove "pod::" prefix
            } else {
                module
            };
            
            println!("[{}] {} {:<8} - {}", 
                time, 
                level,
                clean_module,
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
    log::set_logger(LOGGER.get().unwrap())?;
    log::set_max_level(LevelFilter::Info);
    Ok(())
}

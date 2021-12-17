//! # buffered_logger
//! 
//! This is a file logger implemetation for crate [log](https://docs.rs/log/latest/log/). It provides a buffer to save
//! the log contents temporarily indeed of writing file every time. When the size of buffer exceeds the max buffer
//! size, it writes the buffer to current log file. When the size of the current log file exceeds the max file size, 
//! the log file is rotated. It uses crate [flate2](https://docs.rs/flate2/latest/flate2/) to compress rotatd file.
//! It uses crate [crossbeam-channel](https://docs.rs/crossbeam-channel/0.5.1/crossbeam_channel/) for multi-threading.
//! 
//! # Usage
//! ```rust
//! use buffered_logger::Logger;
//! 
//! // Initialize the logger and start the service.
//! let logger = Logger::init(log::Level::Trace, "logs/m.log".to_string(), 10, 1024, 1024 * 5, true).unwrap();
//! logger.start();
//! 
//! // Now you can start logging.
//! log::info!("this is an info message");
//! log::debug!("this is a debug message");
//! 
//! // Logger is clonable. This is useful for passing it to a different thread.
//! let logger_clone = logger.clone();
//! 
//! // You can manually write the buffer to current log file. eg. run it every second.
//! logger_clone.flush();
//! 
//! // You can manually rotate the log file. eg. run it every day at 00:00.
//! logger_clone.rotate();
//! ```

use std::{
    fs::{create_dir_all, read_dir, remove_file, rename, File, OpenOptions},
    io::{stdout, Write},
    path::{Path, PathBuf},
    process::exit,
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use flate2::{write::GzEncoder, Compression};
use log::{Level, Metadata, Record, SetLoggerError};
use permissions::{is_readable, is_writable};
use regex::Regex;

#[cfg(windows)]
const LINE_ENDING: &'static str = "\r\n";
#[cfg(not(windows))]
const LINE_ENDING: &'static str = "\n";

enum Message {
    Flush,
    Rotate,
    Msg(String),
}

#[derive(Clone)]
/// The logger struct
pub struct Logger {
    log_path: String,
    retain: usize,
    buffer_size: usize,
    rotate_size: usize,
    stdout: bool,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

struct Log {
    level: Level,
    sender: Sender<Message>,
}

impl Logger {
    /// Initialize a logger
    ///
    /// # Arguments
    /// * `level` - log level. eg. `Level::Info`.
    /// * `log_path` - relative or absolute path.
    /// * `retain` - max number of rotated logs.
    /// * `buffer_size` - When the size of log buffer becomes higher than this value it will write it to log file.
    /// * `rotate_size` - When the size of current log file becomes higher than this value it will rotate it.
    /// * `stdout` - also log to standard output.
    ///
    /// # Example
    /// ```rust
    /// use buffered_logger::Logger;
    ///
    /// let logger = Logger::init(log::Level::Trace, "logs/m.log".to_string(), 10, 1024, 1024 * 5, true).unwrap();
    /// ```
    pub fn init(
        level: Level,
        log_path: String,
        retain: usize,
        buffer_size: usize,
        rotate_size: usize,
        stdout: bool,
    ) -> Result<Logger, SetLoggerError> {
        let (sender, receiver) = unbounded();
        let lf = match level {
            Level::Trace => log::LevelFilter::Trace,
            Level::Debug => log::LevelFilter::Debug,
            Level::Info => log::LevelFilter::Info,
            Level::Warn => log::LevelFilter::Warn,
            Level::Error => log::LevelFilter::Error,
        };
        log::set_boxed_logger(Box::new(Log {
            level,
            sender: sender.clone(),
        }))
        .map(|()| log::set_max_level(lf))?;
        Ok(Logger {
            log_path,
            retain,
            buffer_size,
            rotate_size,
            stdout,
            sender,
            receiver,
        })
    }

    /// start the logger service.
    pub fn start(&self) {
        let this = self.clone();
        std::thread::spawn(move || {
            let mut curr_len: usize = 0;
            let mut bytes_buf = vec![0u8; this.buffer_size];
            let log_path = Path::new(this.log_path.as_str());
            let log_dir = log_path.parent().unwrap();
            let file_stem = log_path.file_stem().unwrap().to_str().unwrap();
            let file_ext = log_path.extension().unwrap().to_str().unwrap();

            match create_dir_all(&log_dir) {
                Err(err) => {
                    eprintln!(
                        "buffered_logger: Failed to created dir {} - {}",
                        log_dir.to_str().unwrap(),
                        err
                    );
                    exit(-1);
                }
                _ => (),
            }
            match is_readable(&log_dir) {
                Ok(readable) => {
                    if !readable {
                        eprintln!(
                            "buffered_logger: Dir {} is not readable.",
                            log_dir.to_str().unwrap()
                        );
                        exit(-1);
                    }
                }
                Err(err) => {
                    eprintln!(
                        "buffered_logger: Dir {} is not readable - {}",
                        log_dir.to_str().unwrap(),
                        err
                    );
                    exit(-1);
                }
            }
            match is_writable(&log_dir) {
                Ok(writable) => {
                    if !writable {
                        eprintln!(
                            "buffered_logger: Dir {} is not writable.",
                            log_dir.to_str().unwrap()
                        );
                        exit(-1);
                    }
                }
                Err(err) => {
                    eprintln!(
                        "buffered_logger: Dir {} is not writable - {}",
                        log_dir.to_str().unwrap(),
                        err
                    );
                    exit(-1);
                }
            }

            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
                .unwrap();
            let mut file_size = file.metadata().unwrap().len() as usize;

            let re = format!(
                "{}\\.\\d{{6}}\\.\\d{{6}}\\.\\d{{3}}\\.{}\\.gz$",
                &file_stem, &file_ext
            );
            let mut rotated_items: Vec<PathBuf> = read_dir(&log_dir)
                .unwrap()
                .filter_map(|res| {
                    let path = res.unwrap().path();
                    let re = Regex::new(&re).unwrap();
                    if re.is_match(path.to_str().unwrap()) {
                        return Some(path);
                    }
                    None
                })
                .collect();
            rotated_items.sort();

            let retain = this.retain;
            let rotate =
                |rotated_items: &mut Vec<PathBuf>, file: &mut File, file_size: &mut usize| {
                    *file_size = 0;

                    let now = chrono::Local::now().naive_local();
                    let rotated_log_base_name =
                        format!("{}.{}", file_stem, now.format("%y%m%d.%H%M%S%.3f"),);
                    let rotated_log_file_name = format!("{}.{}", rotated_log_base_name, file_ext);
                    let rotated_log_path = log_dir.join(&rotated_log_file_name);
                    rename(log_path, &rotated_log_path).unwrap();
                    *file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log_path)
                        .unwrap();
                    let zip_name = format!("{}.gz", rotated_log_file_name);
                    let zip_path = log_dir.join(zip_name);
                    rotated_items.push(zip_path.clone());
                    while rotated_items.len() > retain {
                        match remove_file(&rotated_items[0]) {
                            _ => (),
                        }
                        rotated_items.remove(0);
                    }
                    std::thread::spawn(move || {
                        let zip_file = std::fs::File::create(zip_path).unwrap();
                        let mut rotated_file = OpenOptions::new()
                            .read(true)
                            .open(&rotated_log_path)
                            .unwrap();
                        let mut zip = GzEncoder::new(&zip_file, Compression::default());
                        std::io::copy(&mut rotated_file, &mut zip).unwrap();
                        zip.flush().unwrap();
                        zip.finish().unwrap();
                        remove_file(rotated_log_path).unwrap();
                    });
                };

            loop {
                let data = this.receiver.recv().unwrap();
                match data {
                    Message::Flush => {
                        let s = &bytes_buf[0..curr_len];
                        file.write_all(s).unwrap();
                        if this.stdout {
                            stdout().write_all(s).unwrap();
                        }
                        curr_len = 0;
                    }
                    Message::Rotate => {
                        rotate(&mut rotated_items, &mut file, &mut file_size);
                    }
                    Message::Msg(data) => {
                        let len = data.len();
                        let next_len = curr_len + len;
                        let next_file_size = file_size + len;
                        if next_file_size > this.rotate_size {
                            rotate(&mut rotated_items, &mut file, &mut file_size);
                        } else {
                            file_size = next_file_size;
                        }
                        if next_len > this.buffer_size {
                            let s = &bytes_buf[0..curr_len];
                            file.write_all(s).unwrap();
                            if this.stdout {
                                stdout().write_all(s).unwrap();
                            }
                            bytes_buf[0..len].copy_from_slice(data.as_bytes());
                            curr_len = len;
                        } else {
                            bytes_buf[curr_len..next_len].copy_from_slice(data.as_bytes());
                            curr_len = next_len;
                        }
                    }
                }
            }
        });
    }

    /// manually write the buffer to the current log file.
    pub fn flush(&self) {
        self.sender.send(Message::Flush).unwrap();
    }

    /// manually rotate the current log file.
    pub fn rotate(&self) {
        self.sender.send(Message::Rotate).unwrap();
    }
}

impl log::Log for Log {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let now = chrono::Local::now().naive_local();
            self.sender
                .send(Message::Msg(format!(
                    "[{} {}] {}{}",
                    now.format("%F %H:%M:%S%.3f"),
                    record.level(),
                    record.args(),
                    LINE_ENDING
                )))
                .unwrap();
        }
    }

    fn flush(&self) {
        self.sender.send(Message::Flush).unwrap();
    }
}

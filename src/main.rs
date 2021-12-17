use buffered_logger::Logger;

fn main() {
    let logger = Logger::init(
        log::Level::Info,
        "logs/m.log".to_string(),
        10,
        1024,
        1024 * 5,
        true,
    )
    .unwrap();
    logger.start();
    log::info!("started");
    logger.flush();
    let l = logger.clone();
    std::thread::spawn(move || {
        let mut n = 1;
        loop {
            log::info!("message {}", n);
            l.flush();
            n = n + 1;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }).join().unwrap();
}

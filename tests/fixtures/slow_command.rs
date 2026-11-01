//! Test-only native process for exercising deployment timeouts and cleanup.

fn main() {
    std::thread::sleep(std::time::Duration::from_secs(60));
}

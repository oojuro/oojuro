//! Supported Linux local journal commands.
#[cfg(target_os = "linux")]
#[path = "../native/mod.rs"]
mod native;

fn main() {
    #[cfg(target_os = "linux")]
    match native::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("oojuro-native: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("oojuro-native: unsupported_platform");
        std::process::exit(2);
    }
}

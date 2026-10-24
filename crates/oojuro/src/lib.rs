//! Native journal applications and scoped hosted capture/history clients.
//! Local storage is provided by `oojuro-engine`; no daemon/socket fallback is supported.
pub mod exclude;
pub mod hosted;
pub mod hosted_capture;
pub mod hosted_mcp;
pub mod timearg;
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

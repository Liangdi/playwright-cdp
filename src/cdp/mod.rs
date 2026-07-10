//! Low-level CDP plumbing: WebSocket transport, wire-format messages,
//! the multiplexing connection, and the per-target session.

pub mod connection;
pub mod messages;
pub mod session;
pub mod transport;

pub use connection::CdpConnection;
pub use messages::{CdpEvent, CdpRequest, CdpResponse};
pub use session::CdpSession;
pub use transport::WebSocketWriter;

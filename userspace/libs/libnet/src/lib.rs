#![no_std]
extern crate alloc;

pub mod socket;
pub mod dns;
pub mod http;

pub use socket::*;
pub use dns::*;
pub use http::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NetError {
    NotConnected,
    ConnectionRefused,
    Timeout,
    DnsResolutionFailed,
    NoBuffers,
    InvalidArgument,
    SocketNotFound,
    TcpError,
    IpcError,
    NetdNotFound,
}

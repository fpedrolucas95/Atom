#![no_std]
extern crate alloc;

pub mod addr;
pub mod socket;
pub mod dns;
pub mod http;
pub mod icmp;

pub use addr::*;
pub use socket::*;
pub use dns::*;
pub use http::*;
pub use icmp::*;

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

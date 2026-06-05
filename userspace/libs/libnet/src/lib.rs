#![no_std]
extern crate alloc;

pub mod addr;
pub mod config;
pub mod dns;
pub mod http;
pub mod icmp;
pub mod socket;

pub use addr::*;
pub use config::*;
pub use dns::*;
pub use http::*;
pub use icmp::*;
pub use socket::*;

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

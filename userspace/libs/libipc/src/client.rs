// IPC Client
//
// This module provides a client abstraction for connecting to
// and communicating with IPC services.

extern crate alloc;

use alloc::vec::Vec;
use atom_syscall::ipc::{self, PortId};
use crate::error::{IpcError, IpcResult};
use crate::protocol::{Message, MessageType, MessageHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};
use crate::services::ServicePort;

/// IPC client for connecting to services
pub struct Client {
    /// Local port for receiving responses
    local_port: PortId,
    /// Remote service port
    remote_port: PortId,
    /// Receive buffer
    recv_buffer: [u8; MAX_PAYLOAD_SIZE + HEADER_SIZE],
}

impl Client {
    /// Create a new client and connect to a service
    pub fn connect(service: ServicePort) -> IpcResult<Self> {
        Self::connect_to_port(service.port_id())
    }

    /// Connect to a specific port ID
    pub fn connect_to_port(remote_port: PortId) -> IpcResult<Self> {
        // Create local port for receiving responses
        let local_port = ipc::create_port()
            .map_err(|_| IpcError::ConnectionFailed)?;

        Ok(Self {
            local_port,
            remote_port,
            recv_buffer: [0u8; MAX_PAYLOAD_SIZE + HEADER_SIZE],
        })
    }

    /// Get the local port ID (for receiving)
    pub fn local_port(&self) -> PortId {
        self.local_port
    }

    /// Get the remote port ID (service we're connected to)
    pub fn remote_port(&self) -> PortId {
        self.remote_port
    }

    /// Send a message to the service
    pub fn send(&self, msg: &Message) -> IpcResult<()> {
        let bytes = msg.to_bytes();
        if bytes.len() > MAX_PAYLOAD_SIZE + HEADER_SIZE {
            return Err(IpcError::MessageTooLarge);
        }

        ipc::send(self.remote_port, &bytes)
            .map_err(|_| IpcError::SendFailed)
    }

    /// Send a message and wait for a response
    pub fn send_recv(&mut self, msg: &Message) -> IpcResult<Message> {
        self.send(msg)?;
        self.recv()
    }

    /// Receive a message (blocking)
    pub fn recv(&mut self) -> IpcResult<Message> {
        let len = ipc::recv(self.local_port, &mut self.recv_buffer)
            .map_err(|_| IpcError::ReceiveFailed)?;

        Message::from_bytes(&self.recv_buffer[..len])
            .ok_or(IpcError::InvalidMessage)
    }

    /// Try to receive a message (non-blocking)
    pub fn try_recv(&mut self) -> IpcResult<Option<Message>> {
        match ipc::try_recv(self.local_port, &mut self.recv_buffer) {
            Ok(Some(len)) => {
                let msg = Message::from_bytes(&self.recv_buffer[..len])
                    .ok_or(IpcError::InvalidMessage)?;
                Ok(Some(msg))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(IpcError::ReceiveFailed),
        }
    }

    /// Send a raw message type with payload
    pub fn send_raw(&self, msg_type: MessageType, payload: &[u8]) -> IpcResult<()> {
        let msg = Message::new(msg_type, payload.to_vec());
        self.send(&msg)
    }

    /// Close the client connection
    pub fn close(self) -> IpcResult<()> {
        ipc::close_port(self.local_port)
            .map_err(|_| IpcError::PortClosed)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Best-effort close
        let _ = ipc::close_port(self.local_port);
    }
}

/// Desktop environment client helper
pub struct DesktopClient {
    client: Client,
}

impl DesktopClient {
    /// Connect to the desktop environment
    pub fn connect() -> IpcResult<Self> {
        let client = Client::connect(ServicePort::Desktop)?;
        Ok(Self { client })
    }

    /// Request a new surface for drawing
    pub fn create_surface(&mut self, width: u32, height: u32) -> IpcResult<SurfaceInfo> {
        use crate::protocol::{SurfaceCreatePayload, SurfaceCreatedPayload};

        let payload = SurfaceCreatePayload::new(width, height);
        let msg = Message::new(MessageType::SurfaceCreate, payload.to_bytes().to_vec());

        let response = self.client.send_recv(&msg)?;

        if response.message_type() != MessageType::SurfaceCreated {
            return Err(IpcError::InvalidMessage);
        }

        let created = SurfaceCreatedPayload::from_bytes(&response.payload)
            .ok_or(IpcError::InvalidMessage)?;

        Ok(SurfaceInfo {
            surface_id: created.surface_id,
            width: created.width,
            height: created.height,
            buffer_region_id: created.buffer_region_id,
            buffer_addr: created.buffer_addr,
            stride: created.stride,
        })
    }

    /// Report damage to a surface (tells compositor to re-composite)
    pub fn damage_surface(&self, surface_id: u32, x: u32, y: u32, width: u32, height: u32) -> IpcResult<()> {
        use crate::protocol::DamagePayload;

        let payload = DamagePayload::new(surface_id, x, y, width, height);
        let msg = Message::new(MessageType::SurfaceDamage, payload.to_bytes().to_vec());

        self.client.send(&msg)
    }

    /// Present a surface (request compositor to display it)
    pub fn present_surface(&self, surface_id: u32) -> IpcResult<()> {
        let payload = surface_id.to_le_bytes().to_vec();
        let msg = Message::new(MessageType::SurfacePresent, payload);
        self.client.send(&msg)
    }

    /// Get the underlying client for direct message access
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get mutable access to the underlying client
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }
}

/// Information about a created surface
#[derive(Debug, Clone, Copy)]
pub struct SurfaceInfo {
    /// Surface ID assigned by desktop environment
    pub surface_id: u32,
    /// Actual surface width
    pub width: u32,
    /// Actual surface height
    pub height: u32,
    /// Shared memory region ID for pixel buffer
    pub buffer_region_id: u64,
    /// Buffer address (after mapping)
    pub buffer_addr: u64,
    /// Stride in bytes
    pub stride: u32,
}

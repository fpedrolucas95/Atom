// IPC Server
//
// This module provides a server abstraction for creating IPC services.

extern crate alloc;

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use atom_syscall::ipc::{self, PortId};
use crate::error::{IpcError, IpcResult};
use crate::protocol::{Message, MessageHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};
use crate::services::ServicePort;

/// IPC server for providing services
pub struct Server {
    /// Server port for receiving messages
    port: PortId,
    /// Receive buffer
    recv_buffer: [u8; MAX_PAYLOAD_SIZE + HEADER_SIZE],
    /// Connected clients (client_port -> client_info)
    clients: BTreeMap<PortId, ClientInfo>,
    /// Next client ID
    next_client_id: u32,
}

/// Information about a connected client
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Unique client ID
    pub id: u32,
    /// Client's response port
    pub port: PortId,
}

/// A received request with sender information
#[derive(Debug)]
pub struct Request {
    /// The message received
    pub message: Message,
    /// Port to send response to
    pub reply_port: PortId,
    /// Client ID (if tracked)
    pub client_id: Option<u32>,
}

impl Server {
    /// Create a new server on a well-known service port
    pub fn new(service: ServicePort) -> IpcResult<Self> {
        Self::on_port(service.port_id())
    }

    /// Create a server on a specific port ID
    ///
    /// Note: For well-known services, the port ID should be reserved.
    /// This function assumes the port is already created or will be created
    /// with a specific ID. In practice, the kernel may need to support
    /// creating ports with specific IDs for system services.
    pub fn on_port(port: PortId) -> IpcResult<Self> {
        // For now, create a new port and use its ID
        // In a real implementation, system services would use reserved ports
        let server_port = ipc::create_port()
            .map_err(|_| IpcError::ConnectionFailed)?;

        Ok(Self {
            port: server_port,
            recv_buffer: [0u8; MAX_PAYLOAD_SIZE + HEADER_SIZE],
            clients: BTreeMap::new(),
            next_client_id: 1,
        })
    }

    /// Get the server's port ID
    pub fn port(&self) -> PortId {
        self.port
    }

    /// Receive a request (blocking)
    pub fn recv(&mut self) -> IpcResult<Request> {
        let len = ipc::recv(self.port, &mut self.recv_buffer)
            .map_err(|_| IpcError::ReceiveFailed)?;

        let message = Message::from_bytes(&self.recv_buffer[..len])
            .ok_or(IpcError::InvalidMessage)?;

        // Extract reply port from message or use a default
        // In a real implementation, the kernel IPC would provide sender info
        let reply_port = 0; // Placeholder - would come from kernel

        Ok(Request {
            message,
            reply_port,
            client_id: None,
        })
    }

    /// Try to receive a request (non-blocking)
    pub fn try_recv(&mut self) -> IpcResult<Option<Request>> {
        match ipc::try_recv(self.port, &mut self.recv_buffer) {
            Ok(Some(len)) => {
                let message = Message::from_bytes(&self.recv_buffer[..len])
                    .ok_or(IpcError::InvalidMessage)?;

                Ok(Some(Request {
                    message,
                    reply_port: 0,
                    client_id: None,
                }))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(IpcError::ReceiveFailed),
        }
    }

    /// Send a response to a client
    pub fn reply(&self, request: &Request, response: &Message) -> IpcResult<()> {
        if request.reply_port == 0 {
            // No reply port specified - this is a one-way message
            return Ok(());
        }

        let bytes = response.to_bytes();
        ipc::send(request.reply_port, &bytes)
            .map_err(|_| IpcError::SendFailed)
    }

    /// Send a message to a specific port
    pub fn send_to(&self, port: PortId, msg: &Message) -> IpcResult<()> {
        let bytes = msg.to_bytes();
        ipc::send(port, &bytes)
            .map_err(|_| IpcError::SendFailed)
    }

    /// Register a new client
    pub fn register_client(&mut self, port: PortId) -> u32 {
        let id = self.next_client_id;
        self.next_client_id += 1;

        self.clients.insert(port, ClientInfo { id, port });
        id
    }

    /// Unregister a client
    pub fn unregister_client(&mut self, port: PortId) -> Option<ClientInfo> {
        self.clients.remove(&port)
    }

    /// Get client info by port
    pub fn get_client(&self, port: PortId) -> Option<&ClientInfo> {
        self.clients.get(&port)
    }

    /// Get all connected clients
    pub fn clients(&self) -> impl Iterator<Item = &ClientInfo> {
        self.clients.values()
    }

    /// Broadcast a message to all clients
    pub fn broadcast(&self, msg: &Message) -> usize {
        let bytes = msg.to_bytes();
        let mut sent = 0;

        for client in self.clients.values() {
            if ipc::send(client.port, &bytes).is_ok() {
                sent += 1;
            }
        }

        sent
    }

    /// Close the server
    pub fn close(self) -> IpcResult<()> {
        ipc::close_port(self.port)
            .map_err(|_| IpcError::PortClosed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort close
        let _ = ipc::close_port(self.port);
    }
}

/// Event loop helper for server implementations
pub struct EventLoop<S> {
    server: Server,
    state: S,
}

impl<S> EventLoop<S> {
    /// Create a new event loop with custom state
    pub fn new(server: Server, state: S) -> Self {
        Self { server, state }
    }

    /// Get a reference to the state
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Get a mutable reference to the state
    pub fn state_mut(&mut self) -> &mut S {
        &mut self.state
    }

    /// Get a reference to the server
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Get a mutable reference to the server
    pub fn server_mut(&mut self) -> &mut Server {
        &mut self.server
    }

    /// Run the event loop with a handler function
    ///
    /// The handler receives the state and request, and returns a response.
    /// Return None to not send a response.
    pub fn run<F>(&mut self, mut handler: F) -> !
    where
        F: FnMut(&mut S, &Server, Request) -> Option<Message>,
    {
        loop {
            match self.server.recv() {
                Ok(request) => {
                    if let Some(response) = handler(&mut self.state, &self.server, request) {
                        // Note: In a real implementation, we'd send to the request's reply port
                        let _ = self.server.send_to(0, &response);
                    }
                }
                Err(_e) => {
                    // Log error and continue
                    atom_syscall::debug::log("Server recv error");
                }
            }
        }
    }

    /// Poll for messages without blocking
    pub fn poll<F>(&mut self, mut handler: F)
    where
        F: FnMut(&mut S, &Server, Request) -> Option<Message>,
    {
        while let Ok(Some(request)) = self.server.try_recv() {
            if let Some(response) = handler(&mut self.state, &self.server, request) {
                let _ = self.server.send_to(0, &response);
            }
        }
    }
}

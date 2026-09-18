//! The client half of the daemon protocol.
//!
//! Keeps the socket off the interface's thread. The reader and the writer each
//! get one, so a chatty agent cannot stall a redraw and a redraw cannot stall
//! the socket; what reaches the interface is a queue of messages it drains
//! whenever it likes.
//!
//! Nothing here knows about panes or drawing. It connects, shakes hands, and
//! moves messages.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use dispatch_os::ipc::{Connection, IpcError};
use dispatch_proto::{ClientMessage, Frame, ProtocolError, ServerMessage};

/// Failures attaching to a daemon.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon is listening.
    ///
    /// Kept separate from the other transport failures because it is the one a
    /// user is expected to hit, and the answer to it is to start one.
    #[error("no daemon is listening on {0}")]
    NotRunning(String),

    /// The transport failed.
    #[error(transparent)]
    Ipc(#[from] IpcError),

    /// The handshake could not be sent or read.
    #[error("the handshake failed: {0}")]
    Handshake(String),

    /// The daemon refused the connection.
    #[error(transparent)]
    Refused(ProtocolError),

    /// The daemon answered the handshake with something else.
    #[error("expected a welcome, got {0}")]
    Unexpected(String),
}

/// Sends to a daemon.
///
/// Cheap to clone, so whatever owns a pane can keep one rather than reaching
/// back through the application for every keystroke.
#[derive(Debug, Clone)]
pub struct Handle {
    outbox: Sender<ClientMessage>,
    connected: Arc<AtomicBool>,
}

impl Handle {
    /// Queues a message. Returns whether the connection is still up.
    ///
    /// A dropped connection is not an error here: the interface has already
    /// been told, and failing a keystroke it can do nothing about would only
    /// add noise.
    pub fn send(&self, message: ClientMessage) -> bool {
        if self.outbox.send(message).is_err() {
            self.connected.store(false, Ordering::Relaxed);
            return false;
        }

        self.is_connected()
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

/// An attached daemon connection.
#[derive(Debug)]
pub struct Client {
    handle: Handle,
    inbox: Receiver<ServerMessage>,
    device: String,
}

impl Client {
    /// Connects and shakes hands.
    ///
    /// `name` is what the daemon logs this client as. Attaching does not ask
    /// for pane events; call [`Client::subscribe`] for those.
    pub fn attach(name: &str) -> Result<Self, ClientError> {
        let endpoint = dispatch_os::ipc::endpoint()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "the daemon endpoint".into());

        let connection = match Connection::connect() {
            Ok(connection) => connection,
            Err(IpcError::NotRunning(_)) => return Err(ClientError::NotRunning(endpoint)),
            Err(error) => return Err(error.into()),
        };

        let (mut reader, mut writer) = connection.split()?;

        Frame::write(
            &mut writer,
            &ClientMessage::Hello {
                version: dispatch_proto::VERSION,
                client: name.to_string(),
            },
        )
        .map_err(|e| ClientError::Handshake(e.to_string()))?;

        // Read the answer before starting any thread: a refused connection
        // should fail this call rather than arrive later as a message the
        // caller has to know to look for.
        let device = match Frame::read::<_, ServerMessage>(&mut reader) {
            Ok(ServerMessage::Welcome { device, .. }) => device,
            Ok(ServerMessage::Error { error }) => return Err(ClientError::Refused(error)),
            Ok(other) => return Err(ClientError::Unexpected(format!("{other:?}"))),
            Err(error) => return Err(ClientError::Handshake(error.to_string())),
        };

        let connected = Arc::new(AtomicBool::new(true));
        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        let reading = Arc::clone(&connected);
        std::thread::spawn(move || {
            loop {
                match Frame::read::<_, ServerMessage>(&mut reader) {
                    Ok(message) => {
                        if incoming.send(message).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::info!(%error, "the daemon connection ended");
                        break;
                    }
                }
            }

            reading.store(false, Ordering::Relaxed);
        });

        let writing = Arc::clone(&connected);
        std::thread::spawn(move || {
            while let Ok(message) = outgoing.recv() {
                if let Err(error) = Frame::write(&mut writer, &message) {
                    tracing::info!(%error, "failed to send to the daemon");
                    break;
                }
            }

            writing.store(false, Ordering::Relaxed);
        });

        Ok(Self {
            handle: Handle { outbox, connected },
            inbox,
            device,
        })
    }

    /// Asks for pane events, and for what already exists.
    pub fn subscribe(&self) -> bool {
        self.send(ClientMessage::Subscribe)
    }

    /// What the daemon calls itself.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// A sender for whatever needs to talk to the daemon.
    #[must_use]
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Queues a message. Returns whether the connection is still up.
    pub fn send(&self, message: ClientMessage) -> bool {
        self.handle.send(message)
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.handle.is_connected()
    }

    /// Takes everything that has arrived since the last call.
    ///
    /// Never blocks: the interface calls this once a frame and draws whatever
    /// it got.
    pub fn poll(&self) -> Vec<ServerMessage> {
        let mut messages = Vec::new();
        while let Ok(message) = self.inbox.try_recv() {
            messages.push(message);
        }
        messages
    }
}

#[cfg(test)]
mod tests;

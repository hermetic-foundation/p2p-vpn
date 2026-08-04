use std::{io, net::SocketAddr};

use tokio::net::UdpSocket;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PacketPlaneSnapshot {
    pub listeners: Vec<SocketAddr>,
}

#[derive(Debug, Default)]
pub struct PacketPlaneRuntime {
    sockets: Vec<UdpSocket>,
    listeners: Vec<SocketAddr>,
}

impl PacketPlaneRuntime {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            sockets: Vec::new(),
            listeners: Vec::new(),
        }
    }

    pub async fn bind(listen_addrs: Vec<SocketAddr>) -> Result<Self, io::Error> {
        let mut sockets = Vec::with_capacity(listen_addrs.len());
        let mut listeners = Vec::with_capacity(listen_addrs.len());

        for address in listen_addrs {
            let socket = UdpSocket::bind(address).await?;
            listeners.push(socket.local_addr()?);
            sockets.push(socket);
        }

        Ok(Self { sockets, listeners })
    }

    #[must_use]
    pub fn snapshot(&self) -> PacketPlaneSnapshot {
        PacketPlaneSnapshot {
            listeners: self.listeners.clone(),
        }
    }

    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.sockets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_configured_udp_listeners() {
        let runtime = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("packet plane bind");

        let snapshot = runtime.snapshot();

        assert_eq!(runtime.listener_count(), 1);
        assert_eq!(snapshot.listeners.len(), 1);
        assert!(snapshot.listeners[0].port() > 0);
    }
}

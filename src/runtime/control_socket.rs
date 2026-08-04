use std::{
    io,
    path::{Path, PathBuf},
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

const STATUS_REQUEST: &[u8] = b"status\n";
const STATE_REQUEST: &[u8] = b"state\n";
const PEERS_REQUEST: &[u8] = b"peers\n";
const ROUTES_REQUEST: &[u8] = b"routes\n";
const PATHS_REQUEST: &[u8] = b"paths\n";
const MTU_REQUEST: &[u8] = b"mtu\n";
const CAPABILITIES_REQUEST: &[u8] = b"capabilities\n";
const SHUTDOWN_REQUEST: &[u8] = b"shutdown\n";
const MAX_REQUEST_LEN: usize = 64;
const MAX_RESPONSE_LEN: usize = 256 * 1024;
const REQUEST_CHANNEL: usize = 16;

#[derive(Debug)]
pub enum RuntimeControlRequest {
    Status {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    State {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Peers {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Routes {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Paths {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Mtu {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Capabilities {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Shutdown {
        respond_to: oneshot::Sender<Vec<String>>,
    },
}

pub struct ControlSocket {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl ControlSocket {
    pub fn bind(
        path: impl Into<PathBuf>,
    ) -> io::Result<(Self, mpsc::Receiver<RuntimeControlRequest>)> {
        let path = path.into();
        let listener = UnixListener::bind(&path)?;
        let (tx, rx) = mpsc::channel(REQUEST_CHANNEL);
        let task = tokio::spawn(serve(listener, tx));

        Ok((Self { path, task }, rx))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        self.task.abort();
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "failed to remove control socket {}: {error}",
                self.path.display()
            );
        }
    }
}

async fn serve(listener: UnixListener, tx: mpsc::Sender<RuntimeControlRequest>) {
    loop {
        match listener.accept().await {
            Ok((stream, _address)) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, tx).await {
                        eprintln!("control socket request failed: {error}");
                    }
                });
            }
            Err(error) => {
                eprintln!("control socket accept failed: {error}");
                break;
            }
        }
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    tx: mpsc::Sender<RuntimeControlRequest>,
) -> io::Result<()> {
    let request = read_bounded_request(&mut stream).await?;
    let request = match request.as_slice() {
        STATUS_REQUEST => RequestKind::Status,
        STATE_REQUEST => RequestKind::State,
        PEERS_REQUEST => RequestKind::Peers,
        ROUTES_REQUEST => RequestKind::Routes,
        PATHS_REQUEST => RequestKind::Paths,
        MTU_REQUEST => RequestKind::Mtu,
        CAPABILITIES_REQUEST => RequestKind::Capabilities,
        SHUTDOWN_REQUEST => RequestKind::Shutdown,
        _ => {
            stream.write_all(b"error unsupported request\n").await?;
            return Ok(());
        }
    };

    let (respond_to, response) = oneshot::channel();
    match request {
        RequestKind::Status => {
            tx.send(RuntimeControlRequest::Status { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::State => {
            tx.send(RuntimeControlRequest::State { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Peers => {
            tx.send(RuntimeControlRequest::Peers { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Routes => {
            tx.send(RuntimeControlRequest::Routes { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Paths => {
            tx.send(RuntimeControlRequest::Paths { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Mtu => {
            tx.send(RuntimeControlRequest::Mtu { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Capabilities => {
            tx.send(RuntimeControlRequest::Capabilities { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Shutdown => {
            tx.send(RuntimeControlRequest::Shutdown { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
    }
    let lines = response
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime response dropped"))?;

    stream
        .write_all(encode_line_response(&lines).as_bytes())
        .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestKind {
    Status,
    State,
    Peers,
    Routes,
    Paths,
    Mtu,
    Capabilities,
    Shutdown,
}

async fn read_bounded_request(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut byte = [0; 1];
    while request.len() < MAX_REQUEST_LEN {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            break;
        }
        request.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(request);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "control request is missing newline or exceeds maximum length",
    ))
}

fn encode_line_response(lines: &[String]) -> String {
    let mut response = String::from("ok\n");
    for line in lines {
        response.push_str(line);
        response.push('\n');
    }
    response
}

pub async fn query_status(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, STATUS_REQUEST).await
}

pub async fn query_state(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, STATE_REQUEST).await
}

pub async fn query_peers(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, PEERS_REQUEST).await
}

pub async fn query_routes(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, ROUTES_REQUEST).await
}

pub async fn query_paths(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, PATHS_REQUEST).await
}

pub async fn query_mtu(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, MTU_REQUEST).await
}

pub async fn query_capabilities(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, CAPABILITIES_REQUEST).await
}

pub async fn query_shutdown(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, SHUTDOWN_REQUEST).await
}

async fn query_lines(
    path: &Path,
    timeout: std::time::Duration,
    request: &'static [u8],
) -> Result<Vec<String>, QueryError> {
    let mut stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| QueryError::TimedOut)??;
    tokio::time::timeout(timeout, stream.write_all(request))
        .await
        .map_err(|_| QueryError::TimedOut)??;

    let mut response = String::new();
    tokio::time::timeout(
        timeout,
        stream
            .take(u64::try_from(MAX_RESPONSE_LEN).expect("response limit fits u64"))
            .read_to_string(&mut response),
    )
    .await
    .map_err(|_| QueryError::TimedOut)??;

    decode_line_response(&response)
}

fn decode_line_response(response: &str) -> Result<Vec<String>, QueryError> {
    let mut lines = response.lines();
    match lines.next() {
        Some("ok") => Ok(lines.map(ToOwned::to_owned).collect()),
        Some(error) if error.starts_with("error ") => Err(QueryError::Remote(error.to_owned())),
        Some(other) => Err(QueryError::InvalidResponse(other.to_owned())),
        None => Err(QueryError::InvalidResponse("empty response".to_owned())),
    }
}

#[derive(Debug)]
pub enum QueryError {
    Io(io::Error),
    TimedOut,
    Remote(String),
    InvalidResponse(String),
}

impl From<io::Error> for QueryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_response_round_trips_lines() {
        let response = encode_line_response(&[
            "network lab".to_owned(),
            "outbound_path_probes_sent 1".to_owned(),
        ]);

        assert_eq!(
            decode_line_response(&response).expect("line response"),
            vec![
                "network lab".to_owned(),
                "outbound_path_probes_sent 1".to_owned()
            ]
        );
    }

    #[test]
    fn line_response_rejects_remote_errors() {
        assert!(matches!(
            decode_line_response("error unsupported request\n"),
            Err(QueryError::Remote(_))
        ));
    }

    #[tokio::test]
    async fn control_socket_serves_status_lines_and_cleans_up() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "status"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Status { respond_to }) = rx.recv().await else {
                panic!("expected status request");
            };
            respond_to
                .send(vec!["network lab".to_owned(), "peers 2".to_owned()])
                .expect("status response accepted");
        });

        let lines = query_status(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(lines, vec!["network lab".to_owned(), "peers 2".to_owned()]);
        responder.await.expect("responder");
        drop(socket);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn control_socket_serves_state_lines() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "state"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::State { respond_to }) = rx.recv().await else {
                panic!("expected state request");
            };
            respond_to
                .send(vec![
                    "daemon state: running".to_owned(),
                    "configured peers: 1".to_owned(),
                ])
                .expect("state response accepted");
        });

        let lines = query_state(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(
            lines,
            vec![
                "daemon state: running".to_owned(),
                "configured peers: 1".to_owned()
            ]
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_serves_shutdown_request() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "shutdown"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Shutdown { respond_to }) = rx.recv().await else {
                panic!("expected shutdown request");
            };
            respond_to
                .send(vec!["shutdown accepted".to_owned()])
                .expect("shutdown response accepted");
        });

        let lines = query_shutdown(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(lines, vec!["shutdown accepted".to_owned()]);
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_serves_daemon_view_requests() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "views"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            for expected in ["peers", "routes", "paths", "mtu", "capabilities"] {
                match (expected, rx.recv().await) {
                    ("peers", Some(RuntimeControlRequest::Peers { respond_to })) => {
                        respond_to
                            .send(vec!["peers: 1".to_owned()])
                            .expect("peers response accepted");
                    }
                    ("routes", Some(RuntimeControlRequest::Routes { respond_to })) => {
                        respond_to
                            .send(vec!["remote advertised routes: 1".to_owned()])
                            .expect("routes response accepted");
                    }
                    ("paths", Some(RuntimeControlRequest::Paths { respond_to })) => {
                        respond_to
                            .send(vec!["peer selected path: abc direct_tcp_stream".to_owned()])
                            .expect("paths response accepted");
                    }
                    ("mtu", Some(RuntimeControlRequest::Mtu { respond_to })) => {
                        respond_to
                            .send(vec!["peer mtu: abc selected_path_mtu 1200".to_owned()])
                            .expect("mtu response accepted");
                    }
                    ("capabilities", Some(RuntimeControlRequest::Capabilities { respond_to })) => {
                        respond_to
                            .send(vec!["validated peers: 1".to_owned()])
                            .expect("capabilities response accepted");
                    }
                    (kind, other) => panic!("expected {kind} request, got {other:?}"),
                }
            }
        });

        assert_eq!(
            query_peers(&path, std::time::Duration::from_secs(1))
                .await
                .expect("peers"),
            vec!["peers: 1".to_owned()]
        );
        assert_eq!(
            query_routes(&path, std::time::Duration::from_secs(1))
                .await
                .expect("routes"),
            vec!["remote advertised routes: 1".to_owned()]
        );
        assert_eq!(
            query_paths(&path, std::time::Duration::from_secs(1))
                .await
                .expect("paths"),
            vec!["peer selected path: abc direct_tcp_stream".to_owned()]
        );
        assert_eq!(
            query_mtu(&path, std::time::Duration::from_secs(1))
                .await
                .expect("mtu"),
            vec!["peer mtu: abc selected_path_mtu 1200".to_owned()]
        );
        assert_eq!(
            query_capabilities(&path, std::time::Duration::from_secs(1))
                .await
                .expect("capabilities"),
            vec!["validated peers: 1".to_owned()]
        );
        responder.await.expect("responder");
        drop(socket);
    }
}

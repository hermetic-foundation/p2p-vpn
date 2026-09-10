use std::{
    io,
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use mio::{Events, Interest, Poll, Token, Waker, unix::SourceFd};

const DEVICE: Token = Token(0);
const STOP: Token = Token(1);

struct Cancellation {
    stopped: AtomicBool,
    wake: Waker,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixDatagram, sync::mpsc, time::Duration};

    #[test]
    fn cancellation_wakes_idle_reader_and_remains_terminal() {
        for cancel_before_read in [true, false] {
            let (socket, _peer) = UnixDatagram::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            let mut ready = Readiness::new(&socket, Interest::READABLE, true).unwrap();
            let cancel = ready.cancellation().unwrap();
            let (entered, entering) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            if cancel_before_read {
                cancel();
                assert_eq!(
                    ready
                        .perform(|| panic!("cancelled read attempted I/O"))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
                continue;
            }
            let worker = std::thread::spawn(move || {
                let mut buffer = [0; 8];
                let result = ready.perform(|| {
                    entered.send(()).unwrap();
                    socket.recv(&mut buffer)
                });
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
                assert_eq!(
                    ready
                        .perform(|| panic!("cancellation must be sticky"))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
                done.send(()).unwrap();
            });
            entering.recv_timeout(Duration::from_secs(1)).unwrap();
            cancel();
            completed.recv_timeout(Duration::from_secs(1)).unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn readiness_delivers_packets_after_would_block() {
        let (socket, peer) = UnixDatagram::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut ready = Readiness::new(&socket, Interest::READABLE, true).unwrap();
        let (entered, entering) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut buffer = [0; 8];
            let length = ready
                .perform(|| {
                    let result = socket.recv(&mut buffer);
                    if result
                        .as_ref()
                        .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
                    {
                        entered.send(()).unwrap();
                    }
                    result
                })
                .unwrap();
            assert_eq!(&buffer[..length], b"packet");
            done.send(()).unwrap();
        });
        entering.recv_timeout(Duration::from_secs(1)).unwrap();
        peer.send(b"packet").unwrap();
        completed.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }
}

/// Owns only readiness registration, never the descriptor. The device must
/// remain alive and nonblocking for every operation through this adapter.
pub(super) struct Readiness {
    poll: Poll,
    events: Events,
    cancellation: Option<Arc<Cancellation>>,
}

impl Readiness {
    pub(super) fn new(
        device: &impl AsRawFd,
        interest: Interest,
        cancellable: bool,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut SourceFd(&device.as_raw_fd()), DEVICE, interest)?;
        let cancellation = if cancellable {
            Some(Arc::new(Cancellation {
                stopped: AtomicBool::new(false),
                wake: Waker::new(poll.registry(), STOP)?,
            }))
        } else {
            None
        };
        Ok(Self {
            poll,
            events: Events::with_capacity(2),
            cancellation,
        })
    }

    pub(super) fn cancellation(&self) -> Option<Box<dyn FnOnce() + Send>> {
        self.cancellation.as_ref().map(|state| {
            let state = Arc::clone(state);
            Box::new(move || {
                state.stopped.store(true, Ordering::Release);
                // The poll owner is alive until the reader is joined. A failed
                // wake can only be useful to report, not repaired by closing its fd.
                if let Err(error) = state.wake.wake() {
                    eprintln!("TUN reader cancellation wake failed: {error}");
                }
            }) as Box<dyn FnOnce() + Send>
        })
    }

    pub(super) fn perform(
        &mut self,
        mut operation: impl FnMut() -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            if self
                .cancellation
                .as_ref()
                .is_some_and(|state| state.stopped.load(Ordering::Acquire))
            {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            match operation() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
            loop {
                match self.poll.poll(&mut self.events, None) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => {
                        result?;
                        break;
                    }
                }
            }
        }
    }
}

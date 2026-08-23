//! The event stream, offered to anything else on the machine.
//!
//! A Unix domain socket at `~/.local/share/ironsight/events.sock`. Connect to it
//! and you are written the same JSON lines the journal receives, from the
//! moment you connect. Nothing is read back: this phase publishes, and does not
//! take instructions. A command channel is a different decision, with a
//! different blast radius, and belongs with the layer that first needs to act
//! rather than watch.
//!
//! Everything here is arranged so that a consumer cannot hurt the engine. The
//! fan-out thread owns the sockets; a client that has gone away, or stopped
//! reading, is dropped on the write that fails. The bus behind it is bounded,
//! so even a wedged gateway thread costs a fixed amount of memory and no
//! latency to the sessions being watched.
//!
//! Windows has no Unix socket in the standard library, so there the stream is
//! reached in-process and through `ironsight events`. The socket is absent
//! rather than faked, because a consumer would rather be told than connect to
//! something that never speaks.

use crate::bus::{Event, Subscriber};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(unix)]
use std::time::Duration;

/// How long the accept loop sleeps between polls. Long enough not to spin, and
/// short enough that a consumer's connection feels immediate.
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);

/// A running gateway. Dropping it stops the threads and removes the socket.
pub struct Gateway {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    clients: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
}

impl Gateway {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many consumers are attached right now.
    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    /// How many events have been written to at least one consumer. Used by the
    /// live check to prove the socket is carrying traffic, not just bound.
    pub fn served(&self) -> usize {
        self.served.load(Ordering::Relaxed)
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Leaving the socket behind would make the next run look like something
        // is already listening.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Default location, beside the journal it mirrors.
pub fn default_path() -> PathBuf {
    crate::app::data_dir().join("events.sock")
}

#[cfg(unix)]
pub fn serve(path: PathBuf, sub: Subscriber) -> io::Result<Gateway> {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Mutex;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket file left by a process that did not exit cleanly would otherwise
    // make bind fail for ever. Removing one nobody is listening on is safe;
    // removing one that is live would be caught by the connect below.
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    let stop = Arc::new(AtomicBool::new(false));
    let clients = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    let pool: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

    // Accept: takes connections and hands them to the fan-out.
    {
        let (stop, clients, pool) = (Arc::clone(&stop), Arc::clone(&clients), Arc::clone(&pool));
        std::thread::Builder::new()
            .name("ironsight-gateway-accept".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // Blocking writes on a client that has stopped
                            // reading would stall every other consumer, so a
                            // write that cannot complete fails and drops it.
                            let _ = stream.set_nonblocking(true);
                            if let Ok(mut held) = pool.lock() {
                                held.push(stream);
                                clients.store(held.len(), Ordering::Relaxed);
                            }
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(POLL)
                        }
                        Err(_) => break,
                    }
                }
            })?;
    }

    // Fan-out: one line per event, to everyone still listening.
    {
        let (stop, clients, served, pool) = (
            Arc::clone(&stop),
            Arc::clone(&clients),
            Arc::clone(&served),
            Arc::clone(&pool),
        );
        std::thread::Builder::new()
            .name("ironsight-gateway-fanout".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(ev) = sub.recv_timeout(POLL) else {
                        if !sub.connected() {
                            break;
                        }
                        continue;
                    };
                    let line = format!("{}\n", ev.line());
                    let Ok(mut held) = pool.lock() else { break };
                    let before = held.len();
                    held.retain_mut(|c| match c.write_all(line.as_bytes()) {
                        Ok(()) => {
                            let _ = c.flush();
                            true
                        }
                        // A consumer that is merely behind gets one more chance
                        // on the next event; one that has gone is forgotten.
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => true,
                        Err(_) => false,
                    });
                    if !held.is_empty() {
                        served.fetch_add(1, Ordering::Relaxed);
                    }
                    if held.len() != before {
                        clients.store(held.len(), Ordering::Relaxed);
                    }
                }
            })?;
    }

    Ok(Gateway {
        path,
        stop,
        clients,
        served,
    })
}

#[cfg(not(unix))]
pub fn serve(_path: PathBuf, _sub: Subscriber) -> io::Result<Gateway> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the event socket needs a Unix domain socket; on Windows use `ironsight events`",
    ))
}

/// Read the stream from a socket, one event per line, for a consumer that is
/// not Ironsight. Blocks until the far end closes.
#[cfg(unix)]
pub fn follow(path: &Path, mut on: impl FnMut(Event)) -> io::Result<()> {
    for ev in connect(path)? {
        on(ev);
    }
    Ok(())
}

/// Connect and yield events as they arrive, so a caller can interleave the
/// live stream with a journal top-up and dedupe by sequence — closing the gap
/// between "replayed the journal" and "attached to the socket", in which the
/// publisher can emit events a fresh client never sees.
#[cfg(unix)]
pub fn connect(path: &Path) -> io::Result<impl Iterator<Item = Event>> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(path)?;
    Ok(BufReader::new(stream)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok()))
}

#[cfg(not(unix))]
pub fn follow(_path: &Path, _on: impl FnMut(Event)) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no event socket on this platform",
    ))
}

#[cfg(not(unix))]
pub fn connect(_path: &Path) -> io::Result<impl Iterator<Item = Event>> {
    Err::<std::iter::Empty<Event>, _>(io::Error::new(
        io::ErrorKind::Unsupported,
        "no event socket on this platform",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::bus::{Bus, Kind};
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ironsight-gw-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("events.sock")
    }

    fn ev(n: &str) -> Event {
        Event::new(
            n,
            "claude",
            Kind::ToolCalled {
                tool: "Bash".into(),
                summary: "ls".into(),
            },
        )
    }

    /// Wait for something to become true, rather than sleeping a guessed
    /// interval: the same test then passes on a loaded machine and on a fast
    /// one, which is the failure this codebase has already paid for twice.
    ///
    /// The deadline is generous on purpose. It is not a measurement of how fast
    /// the gateway is — nothing here asserts a duration — it is only a bound so
    /// that a genuine hang fails instead of hanging. A shared CI runner, or a
    /// laptop compiling something else, is allowed to be slow.
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn carries_the_stream_to_another_process() {
        let path = scratch("carry");
        let mut bus = Bus::new();
        let gw = serve(path.clone(), bus.subscribe()).expect("the socket binds");

        let client = UnixStream::connect(&path).expect("a consumer connects");
        until("the gateway to register the client", || gw.clients() == 1);

        bus.publish(ev("session-one"));
        bus.publish(ev("session-two"));

        let mut lines = BufReader::new(client).lines();
        let first: Event = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        let second: Event = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first.session, "session-one");
        assert_eq!(second.session, "session-two");
        assert_eq!(
            (first.seq, second.seq),
            (1, 2),
            "the socket carries the same ordering the journal has"
        );
    }

    #[test]
    fn a_consumer_that_leaves_costs_the_others_nothing() {
        let path = scratch("leaves");
        let mut bus = Bus::new();
        let gw = serve(path.clone(), bus.subscribe()).expect("the socket binds");

        let leaving = UnixStream::connect(&path).unwrap();
        let staying = UnixStream::connect(&path).unwrap();
        until("both consumers to attach", || gw.clients() == 2);

        bus.publish(ev("a"));
        drop(leaving);

        // A client is forgotten on the next write that fails, so noticing takes
        // at least one write — and how many depends on when the kernel gets
        // round to tearing the socket down, which is not this test's to
        // predict. Publishing until it notices asserts the property that
        // matters — a consumer that leaves is forgotten, promptly and without
        // disturbing anyone else — rather than a number of writes that happened
        // to be enough once, on one machine, when it was not busy.
        until("the departed consumer to be forgotten", || {
            bus.publish(ev("tick"));
            gw.clients() == 1
        });

        let mut lines = BufReader::new(staying).lines();
        let first: Event = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(
            first.session, "a",
            "the survivor missed nothing that was published before the other left"
        );
    }

    #[test]
    fn takes_over_a_socket_left_by_a_process_that_died() {
        let path = scratch("stale");
        {
            let _first = serve(path.clone(), Bus::new().subscribe()).expect("binds once");
            // Leak the file the way an unclean exit would, so the next bind
            // meets a socket nobody is listening on.
            std::mem::forget(std::fs::File::create(path.with_extension("marker")).unwrap());
        }
        std::fs::write(&path, "").ok();
        let again = serve(path.clone(), Bus::new().subscribe());
        assert!(
            again.is_ok(),
            "a stale socket is cleared rather than blocking every future run"
        );
    }

    #[test]
    fn removes_its_socket_when_it_stops() {
        let path = scratch("cleanup");
        {
            let _gw = serve(path.clone(), Bus::new().subscribe()).unwrap();
            assert!(path.exists());
        }
        assert!(
            !path.exists(),
            "nothing is left behind to make the next run think it is already running"
        );
    }
}

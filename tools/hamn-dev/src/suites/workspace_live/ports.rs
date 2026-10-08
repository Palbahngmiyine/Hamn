//! Published ports of containers in the owned VM, end to end: the host
//! listener that the port observer creates, Docker's publication in the
//! guest, and the container.
//!
//! - A UDP port published on all addresses carries a datagram from the Mac
//!   to the container and its reply back, through the host relay.
//! - A UDP port published on 127.0.0.1 is published on the guest's own
//!   loopback, where the relay cannot send. It gets no record and no host
//!   listener, and VM status reports it as `udpAddressUnsupported` until its
//!   container is removed.
//! - A TCP port published on 127.0.0.1 carries a connection: the SSH forward
//!   reaches the guest's loopback.
//!
//! The containers echo with busybox `nc -e cat`, from the image the suite
//! already holds. Its UDP mode answers only its first peer, so one socket
//! sends every datagram of the check. The host ports are fixed, below the
//! ephemeral ranges of macOS and Linux, and must be free when the check
//! starts. The observer acts on Docker events, so every expectation is
//! awaited, within a deadline, on the state that the observer changes:
//! `port-forwards.tsv` and the VM status.
use super::{Live, Must, PROFILE, check_interrupt, finally, random_hex};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

const IMAGE: &str = "busybox:1.37";
const UDP_ALL: (&str, u16) = ("hamn-workspace-udp-all", 24453);
const UDP_LOOPBACK: (&str, u16) = ("hamn-workspace-udp-loopback", 24454);
const TCP_LOOPBACK: (&str, u16) = ("hamn-workspace-tcp-loopback", 24480);
/// The port every echo container listens on.
const CONTAINER_PORT: u16 = 5353;
/// How long the observer may take to act on a change of the containers.
const OBSERVED: Duration = Duration::from_secs(30);
/// How long an echo may take once its forward is recorded: the container's
/// listener can start after the forward.
const ECHOED: Duration = Duration::from_secs(15);

/// One `port-forwards.tsv` record, as host/fwd/ports.c writes it.
#[derive(Debug, PartialEq)]
pub(crate) struct Forward {
    pub protocol: String,
    pub address: String,
    pub host_port: u16,
    pub container_port: u16,
    /// The relay's PID; 0 for TCP, which the SSH master carries.
    pub relay: i32,
    pub ownership: String,
}

/// The records of `state`, the text of `port-forwards.tsv` (empty when the
/// file is absent). A line that is not an 11-field record is an error.
pub(crate) fn forwards(state: &str) -> Result<Vec<Forward>, String> {
    state
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let invalid = || format!("not a port forward record: {line:?}");
            if fields.len() != 11 {
                return Err(invalid());
            }
            Ok(Forward {
                protocol: fields[0].to_owned(),
                address: fields[1].to_owned(),
                host_port: fields[2].parse().map_err(|_| invalid())?,
                container_port: fields[3].parse().map_err(|_| invalid())?,
                relay: fields[4].parse().map_err(|_| invalid())?,
                ownership: fields[7].to_owned(),
            })
        })
        .collect()
}

/// Accepts `state` only when its one record for `protocol` and `host_port`
/// is committed for `address` and the container port, and a UDP record
/// names its relay.
pub(crate) fn assert_forwarded(state: &str, protocol: &str, address: &str, host_port: u16) -> Result<(), String> {
    let records = forwards(state)?;
    let matching: Vec<&Forward> =
        records.iter().filter(|record| record.protocol == protocol && record.host_port == host_port).collect();
    let [record] = matching[..] else {
        return Err(format!("{} records for {protocol} port {host_port}: {records:?}", matching.len()));
    };
    let relay = if protocol == "udp" { record.relay > 1 } else { record.relay == 0 };
    if record.address != address || record.container_port != CONTAINER_PORT || record.ownership != "committed" || !relay {
        return Err(format!("{protocol} port {host_port} is not forwarded from {address}: {record:?}"));
    }
    Ok(())
}

/// Accepts `state` only when it holds no record for `protocol` and
/// `host_port`.
pub(crate) fn assert_unrecorded(state: &str, protocol: &str, host_port: u16) -> Result<(), String> {
    match forwards(state)?.into_iter().find(|record| record.protocol == protocol && record.host_port == host_port) {
        Some(record) => Err(format!("{protocol} port {host_port} is recorded: {record:?}")),
        None => Ok(()),
    }
}

/// Accepts a VM status only when the published ports it reports as not
/// forwarded are exactly the UDP `ports` (address and host port), each for
/// its address.
pub(crate) fn assert_reported(status: &Value, ports: &[(&str, u16)]) -> Result<(), String> {
    let expected: Vec<Value> = ports
        .iter()
        .map(|(address, port)| {
            json!({"hostIp": address, "hostPort": port, "protocol": "udp", "reason": "udpAddressUnsupported"})
        })
        .collect();
    let reported = &status["portForwardFailures"];
    if *reported == Value::Array(expected) { Ok(()) } else { Err(format!("portForwardFailures is {reported}")) }
}

/// One UDP client of 127.0.0.1:`port`.
pub(crate) struct UdpProbe {
    socket: UdpSocket,
    port: u16,
}

impl UdpProbe {
    pub(crate) fn new(port: u16) -> Result<Self, String> {
        let open = || -> io::Result<UdpSocket> {
            let socket = UdpSocket::bind(("127.0.0.1", 0))?;
            socket.connect(("127.0.0.1", port))?;
            socket.set_read_timeout(Some(Duration::from_millis(250)))?;
            Ok(socket)
        };
        Ok(Self { socket: open().map_err(|error| format!("UDP client of port {port}: {error}"))?, port })
    }

    /// Sends `payload` until the same bytes come back or `timeout` has
    /// passed. A datagram is lost while nothing listens yet, so it is sent
    /// again every 250 ms. Other replies are passed over: an echo of an
    /// earlier payload can still arrive.
    pub(crate) fn echo(&self, payload: &[u8], timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 2048];
        let mut last = "no reply".to_owned();
        loop {
            // A refused datagram is reported by a later call on this socket.
            if let Err(error) = self.socket.send(payload) {
                last = format!("send: {error}");
            }
            match self.socket.recv(&mut buffer) {
                Ok(length) if &buffer[..length] == payload => return Ok(()),
                Ok(length) => last = format!("another reply: {:?}", String::from_utf8_lossy(&buffer[..length])),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(error) => {
                    last = format!("receive: {error}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if Instant::now() >= deadline {
                return Err(format!("no echo from UDP 127.0.0.1:{} within {timeout:?}: {last}", self.port));
            }
        }
    }
}

/// Connects to 127.0.0.1:`port`, writes `payload` and reads the same bytes
/// back. A connection that fails or ends early is tried again until
/// `timeout` has passed: the forward accepts connections before the
/// container listens. A reply of other bytes fails at once.
pub(crate) fn tcp_echo(port: u16, payload: &[u8], timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        let exchange = || -> io::Result<Vec<u8>> {
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
            stream.set_read_timeout(Some(Duration::from_secs(1)))?;
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
            stream.write_all(payload)?;
            let mut reply = vec![0; payload.len()];
            stream.read_exact(&mut reply)?;
            Ok(reply)
        };
        match exchange() {
            Ok(reply) if reply == payload => return Ok(()),
            Ok(reply) => {
                return Err(format!("TCP {address} replied {:?}, not the payload", String::from_utf8_lossy(&reply)));
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("no echo from TCP {address} within {timeout:?}: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Evaluates `oracle` until it accepts or `timeout` has passed, and then
/// fails with its last refusal.
#[track_caller]
fn await_accepted(what: &str, timeout: Duration, mut oracle: impl FnMut() -> Result<(), String>) {
    let deadline = Instant::now() + timeout;
    loop {
        check_interrupt();
        match oracle() {
            Ok(()) => return,
            Err(refusal) => assert!(Instant::now() < deadline, "{what} within {timeout:?}: {refusal}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Accepts only when nothing on the Mac holds UDP `port` on any address.
fn udp_free(port: u16) -> Result<(), String> {
    UdpSocket::bind(("0.0.0.0", port)).map(drop).map_err(|error| format!("UDP host port {port} is held: {error}"))
}

/// Accepts only when nothing on the Mac listens on TCP 127.0.0.1:`port`.
fn tcp_free(port: u16) -> Result<(), String> {
    TcpListener::bind(("127.0.0.1", port)).map(drop).map_err(|error| format!("TCP host port {port} is held: {error}"))
}

/// The record of `state` for `protocol` and `host_port`, when it has one.
fn forward(state: &str, protocol: &str, host_port: u16) -> Option<Forward> {
    forwards(state).must().into_iter().find(|record| record.protocol == protocol && record.host_port == host_port)
}

/// Runs an echo container that publishes `publication`.
fn echo_container(live: &Live, name: &str, publication: &str, command: &[&str]) {
    let run = ["run", "-d", "--name", name, "--label", "io.hamn.test=workspace", "-p", publication, IMAGE];
    live.docker(&[&run[..], command].concat());
}

/// Publishes the three ports of the module documentation from echo
/// containers and removes them again.
pub(crate) fn published_ports(live: &Live) {
    live.assert_owned();
    let state_path = live.profile().join("port-forwards.tsv");
    let state = || match fs::read_to_string(&state_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("{}: {error}", state_path.display()),
    };
    let status = || live.call(&["vm", "status"], &[]);
    let (udp_all, udp_loopback, tcp_loopback) = (UDP_ALL.1, UDP_LOOPBACK.1, TCP_LOOPBACK.1);
    assert_reported(&status(), &[]).must();
    udp_free(udp_all).must();
    udp_free(udp_loopback).must();
    tcp_free(tcp_loopback).must();
    let token = random_hex(8);
    let payload = |step: &str| format!("hamn-{step}-{token}").into_bytes();
    let listen = CONTAINER_PORT.to_string();
    let udp_listener = ["nc", "-u", "-l", "-p", listen.as_str(), "-e", "cat"];
    let tcp_listener = ["nc", "-lk", "-p", listen.as_str(), "-e", "cat"];
    let mut nothing = ();
    finally(
        &mut nothing,
        |_| {
            let publication = format!("127.0.0.1:{udp_loopback}:{CONTAINER_PORT}/udp");
            echo_container(live, UDP_LOOPBACK.0, &publication, &udp_listener);
            await_accepted("the report of the UDP port published on 127.0.0.1", OBSERVED, || {
                assert_reported(&status(), &[("127.0.0.1", udp_loopback)])
            });
            let reported = status()["portForwardFailures"].clone();
            assert_unrecorded(&state(), "udp", udp_loopback).must();
            udp_free(udp_loopback).must();
            println!("PASS: a UDP port published on 127.0.0.1 gets no host listener and is reported");

            // The ports beside the reported one are forwarded.
            echo_container(live, UDP_ALL.0, &format!("{udp_all}:{CONTAINER_PORT}/udp"), &udp_listener);
            await_accepted("a relay for the UDP port published on all addresses", OBSERVED, || {
                assert_forwarded(&state(), "udp", "0.0.0.0", udp_all)
            });
            UdpProbe::new(udp_all).must().echo(&payload("udp"), ECHOED).must();
            println!("PASS: a UDP port published on all addresses carries a datagram and its reply");

            let publication = format!("127.0.0.1:{tcp_loopback}:{CONTAINER_PORT}");
            echo_container(live, TCP_LOOPBACK.0, &publication, &tcp_listener);
            await_accepted("a forward for the TCP port published on 127.0.0.1", OBSERVED, || {
                assert_forwarded(&state(), "tcp", "127.0.0.1", tcp_loopback)
            });
            tcp_echo(tcp_loopback, &payload("tcp"), ECHOED).must();
            println!("PASS: a TCP port published on 127.0.0.1 carries a connection");
            assert_reported(&status(), &[("127.0.0.1", udp_loopback)]).must();
            let forwarded = state();
            let relay = forward(&forwarded, "udp", udp_all);

            // Removing the reported port ends the report and leaves the
            // forwards as they are: the relay is the same process.
            live.docker(&["rm", "-f", UDP_LOOPBACK.0]);
            await_accepted("an empty report once its container is removed", OBSERVED, || assert_reported(&status(), &[]));
            assert_forwarded(&state(), "tcp", "127.0.0.1", tcp_loopback).must();
            assert_eq!(forward(&state(), "udp", udp_all), relay, "the relay beside the reported port was replaced");
            tcp_echo(tcp_loopback, &payload("tcp-again"), ECHOED).must();

            live.docker(&["rm", "-f", UDP_ALL.0, TCP_LOOPBACK.0]);
            await_accepted("no forward once the containers are removed", OBSERVED, || {
                let state = state();
                assert_unrecorded(&state, "udp", udp_all).and_then(|()| assert_unrecorded(&state, "tcp", tcp_loopback))
            });
            // A relay that has ended can hold its socket for a moment longer.
            await_accepted("the released host ports", Duration::from_secs(5), || {
                udp_free(udp_all).and_then(|()| tcp_free(tcp_loopback))
            });
            live.write_json(
                "published-ports-results.json",
                &json!({"udpLoopback": {"hostPort": udp_loopback, "reported": reported},
                    "udpAllAddresses": {"hostPort": udp_all, "echoed": true},
                    "tcpLoopback": {"hostPort": tcp_loopback, "echoed": true},
                    "forwards": forwarded, "afterRemoval": {"forwards": state(), "reported": []}}),
            );
            println!("PASS: removing the containers ends the report and every forward");
        },
        |_| {
            // The body removed them unless it failed.
            for name in [UDP_ALL.0, UDP_LOOPBACK.0, TCP_LOOPBACK.0] {
                let _ = live.runtime.engine(&["rm", "-f", name], PROFILE);
            }
        },
    );
}

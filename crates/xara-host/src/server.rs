use crate::command::{Command, parse};
use crate::writer::Writer;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

pub const MAX_LINE: usize = 1024;
pub const DATA_CHUNK: usize = 65536;
pub const INPUT_QUEUE: usize = 64;
pub(crate) const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// `--local-only`: the host and KISS ports take connections from local
/// addresses only (private IPv4 ranges, link-local, loopback; IPv6 unique
/// local and link-local). The VARA host protocol has no password, so a
/// client from anywhere else could make the radio connect and transmit.
pub static LOCAL_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// May this peer connect (see [`LOCAL_ONLY`])?
pub fn peer_allowed(peer: &SocketAddr) -> bool {
    if !LOCAL_ONLY.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    match peer.ip() {
        std::net::IpAddr::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        std::net::IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return v4.is_private() || v4.is_loopback() || v4.is_link_local();
            }
            let s0 = a.segments()[0];
            a.is_loopback() || (s0 & 0xfe00) == 0xfc00 || (s0 & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Debug)]
pub enum Input {
    Command(Command),
    Wrong(String),
    Data(Vec<u8>),
    Connected,
    Closed,
}

#[derive(Clone, Copy)]
enum Port {
    Cmd,
    Data,
}

#[derive(Default)]
struct Clients {
    cmd: Option<Writer>,
    data: Option<Writer>,
}

impl Clients {
    fn slot(&mut self, p: Port) -> &mut Option<Writer> {
        match p {
            Port::Cmd => &mut self.cmd,
            Port::Data => &mut self.data,
        }
    }
}

type Shared = Arc<Mutex<Clients>>;

#[derive(Default)]
struct Hold {
    on: Mutex<bool>,
    cv: Condvar,
}

impl Hold {
    fn set(&self, on: bool) {
        *self.on.lock().unwrap() = on;
        self.cv.notify_all();
    }

    fn wait(&self) {
        drop(self.cv.wait_while(self.on.lock().unwrap(), |on| *on).unwrap());
    }
}

pub struct Host {
    pub input: Receiver<Input>,
    clients: Shared,
    hold: Arc<Hold>,
    pub cmd_addr: SocketAddr,
    pub data_addr: SocketAddr,
}

impl Host {
    pub fn bind(addr: &str, port: u16) -> io::Result<Self> {
        let cl = bind_fixed(addr, port)?;
        let data_port = match port {
            0 => 0,
            p => p.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "port must be < 65535"))?,
        };
        let dl = bind_fixed(addr, data_port)?;
        let (tx, input) = sync_channel(INPUT_QUEUE);
        let clients = Shared::default();
        let hold = Arc::<Hold>::default();
        let h = Self {
            input,
            clients: clients.clone(),
            hold: hold.clone(),
            cmd_addr: cl.local_addr()?,
            data_addr: dl.local_addr()?,
        };
        serve(cl, Port::Cmd, clients.clone(), tx.clone(), hold.clone())?;
        serve(dl, Port::Data, clients, tx, hold)?;
        Ok(h)
    }

    pub fn hold_data(&self, on: bool) {
        self.hold.set(on);
    }

    pub fn line(&self, s: &str) {
        self.send(Port::Cmd, format!("{s}\r").as_bytes());
    }

    pub fn data(&self, b: &[u8]) {
        self.send(Port::Data, b);
    }

    fn send(&self, p: Port, b: &[u8]) {
        if let Some(w) = self.clients.lock().unwrap().slot(p) {
            w.send(b);
        }
    }

    pub fn has_cmd_client(&self) -> bool {
        self.clients.lock().unwrap().cmd.is_some()
    }

    pub fn has_data_client(&self) -> bool {
        self.clients.lock().unwrap().data.is_some()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.hold.set(false);
    }
}

fn set_reuse(l: &TcpListener, on: bool) -> io::Result<()> {
    let v: libc::c_int = on.into();
    let len = std::mem::size_of_val(&v) as libc::socklen_t;
    let r =
        unsafe { libc::setsockopt(l.as_raw_fd(), libc::SOL_SOCKET, libc::SO_REUSEADDR, (&raw const v).cast(), len) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pause(l: &TcpListener) -> io::Result<()> {
    if unsafe { libc::shutdown(l.as_raw_fd(), libc::SHUT_RD) } != 0 {
        return Err(io::Error::last_os_error());
    }
    set_reuse(l, false)
}

fn resume(l: &TcpListener) -> io::Result<()> {
    set_reuse(l, true)?;
    if unsafe { libc::listen(l.as_raw_fd(), 128) } != 0 {
        let e = io::Error::last_os_error();
        let _ = set_reuse(l, false);
        return Err(e);
    }
    Ok(())
}

fn bind_fixed(addr: &str, port: u16) -> io::Result<TcpListener> {
    if port != 0 {
        return TcpListener::bind((addr, port));
    }
    for _ in 0..16 {
        let p = TcpListener::bind((addr, 0))?.local_addr()?.port();
        if let Ok(l) = TcpListener::bind((addr, p)) {
            return Ok(l);
        }
    }
    Err(io::Error::new(io::ErrorKind::AddrInUse, "no free port"))
}

fn serve(l: TcpListener, port: Port, clients: Shared, tx: SyncSender<Input>, hold: Arc<Hold>) -> io::Result<()> {
    let name = match port {
        Port::Cmd => "host-cmd-accept",
        Port::Data => "host-data-accept",
    };
    thread::Builder::new().name(name.into()).spawn(move || {
        loop {
            let s = match l.accept() {
                Ok((s, peer)) if !peer_allowed(&peer) => {
                    eprintln!("xara: {name} refused {peer}: not a local address (--local-only)");
                    drop(s);
                    continue;
                }
                Ok((s, _)) => s,
                Err(_) => {
                    thread::sleep(ACCEPT_BACKOFF);
                    continue;
                }
            };
            if let Err(e) = pause(&l) {
                eprintln!("xara: {name} cannot refuse a second client: {e}");
            }
            if attach(s, port, &clients, &tx, &hold).is_err() {
                return;
            }
            if let Err(e) = resume(&l) {
                eprintln!("xara: {name} cannot listen again, retrying: {e}");
                while resume(&l).is_err() {
                    thread::sleep(ACCEPT_BACKOFF);
                }
            }
        }
    })?;
    Ok(())
}

fn attach(s: TcpStream, port: Port, clients: &Shared, tx: &SyncSender<Input>, hold: &Hold) -> Result<(), ()> {
    let _ = s.set_nodelay(true);
    let Ok(w) = Writer::spawn(&s) else { return Ok(()) };
    *clients.lock().unwrap().slot(port) = Some(w);
    tx.send(Input::Connected).map_err(drop)?;
    match port {
        Port::Cmd => read_commands(s, tx),
        Port::Data => read_data(s, tx, hold),
    }
    let mut c = clients.lock().unwrap();
    if c.slot(port).is_some() {
        for w in [c.cmd.take(), c.data.take()].into_iter().flatten() {
            w.close();
        }
        drop(c);
        tx.send(Input::Closed).map_err(drop)?;
    }
    Ok(())
}

fn read_commands(mut s: TcpStream, tx: &SyncSender<Input>) {
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut long = false;
    while let Ok(n @ 1..) = s.read(&mut buf) {
        for &b in &buf[..n] {
            match b {
                b'\r' => {
                    let l = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    if std::mem::take(&mut long) {
                        continue;
                    }
                    let msg = parse(&l).map_or(Input::Wrong(l), Input::Command);
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                b'\n' => {}
                _ if line.len() >= MAX_LINE => {
                    line.clear();
                    long = true;
                }
                _ if long => {}
                _ => line.push(b),
            }
        }
    }
}

fn read_data(mut s: TcpStream, tx: &SyncSender<Input>, hold: &Hold) {
    let mut buf = vec![0; DATA_CHUNK];
    loop {
        hold.wait();
        let Ok(n @ 1..) = s.read(&mut buf) else { return };
        if tx.send(Input::Data(buf[..n].to_vec())).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod local_only_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn local_only_takes_local_peers_and_refuses_the_rest() {
        let ok = ["192.168.1.20:5000", "10.0.0.7:1", "172.16.5.5:1", "127.0.0.1:1", "169.254.3.3:1", "[fe80::1]:1", "[fd00::5]:1", "[::1]:1", "[::ffff:192.168.0.2]:1"];
        let no = ["8.8.8.8:1", "100.64.0.1:1", "172.32.0.1:1", "[2001:db8::1]:1", "[::ffff:1.2.3.4]:1"];
        LOCAL_ONLY.store(false, Ordering::Relaxed);
        for a in ok.iter().chain(no.iter()) {
            assert!(peer_allowed(&a.parse().unwrap()), "{a} off: all allowed");
        }
        LOCAL_ONLY.store(true, Ordering::Relaxed);
        for a in ok {
            assert!(peer_allowed(&a.parse().unwrap()), "{a} should be allowed");
        }
        for a in no {
            assert!(!peer_allowed(&a.parse().unwrap()), "{a} should be refused");
        }
        LOCAL_ONLY.store(false, Ordering::Relaxed);
    }
}

//! The host supplies the physical LAN. The agent in Docker remains the LXMF endpoint.
use super::Engine;
use rns_interface::{
    auto::{spawn_auto_interface, AutoInterfaceConfig, DATA_PORT},
    hdlc::{frame, HdlcDeframer},
    local::FALLBACK_TCP_PORT,
    traits::InterfaceHandle,
};
use rns_transport::messages::TransportMessage;
use std::{
    fs, io,
    net::Ipv6Addr,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{broadcast, mpsc},
};

const RETRY: Duration = Duration::from_secs(5);

pub(super) fn locked(dir: &Path) -> io::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("worker.lock"))?;
    file.try_lock()?;
    Ok(file)
}

pub(super) fn write_changed(path: &Path, value: &[u8]) -> io::Result<()> {
    use std::io::Write;
    if fs::read(path).ok().as_deref() == Some(value) {
        return Ok(());
    }
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    tmp.write_all(value)?;
    tmp.persist(path)?;
    Ok(())
}

fn state(dir: &Path, value: &str) {
    let _ = write_changed(&dir.join("state"), value.as_bytes());
}

pub fn supervise(dir: &Path) -> io::Result<()> {
    let _guard = match locked(dir) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut tray_started = false;
    let mut hermes_started = false;
    loop {
        if fs::read(dir.join("enabled")).ok().as_deref() == Some(b"no") {
            return Ok(());
        }
        let endpoint = fs::read_to_string(dir.join("engine"))?;
        let engine = Engine::from_endpoint(endpoint.clone()).map_err(io::Error::other)?;
        let status = if any_running(&engine) {
            if !hermes_started {
                let folder = dir.to_owned();
                thread::spawn(move || super::hermes::supervise(&folder));
                hermes_started = true;
            }
            if !tray_started {
                let exe = std::env::var_os("APPIMAGE")
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::env::current_exe().ok());
                if let Some(mut child) = exe.and_then(|exe| {
                    std::process::Command::new(exe)
                        .arg("--agent-tray")
                        .spawn()
                        .ok()
                }) {
                    tray_started = true;
                    thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
            }
            // Dropping this runtime also joins all UDP tasks when Docker stops or is replaced.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(session(dir, &engine))
                .err()
                .map_or("starting".into(), |e| e.to_string())
        } else {
            "waiting".into()
        };
        state(dir, &status);
        thread::sleep(if status == "waiting" {
            Duration::from_secs(10)
        } else {
            RETRY
        });
    }
}

fn any_running(engine: &Engine) -> bool {
    engine.installed_slots().unwrap_or_default().into_iter()
        .any(|slot| matches!(engine.in_slot(slot).lan_container(), Ok(Some(_))))
}

// One physical AutoInterface serves both receivers. Each agent owns a separate
// HDLC decoder and bounded subscription, so a partial frame or slow receiver
// cannot mix the agents' outbound bytes or stall inbound delivery to its peer.
async fn session(dir: &Path, engine: &Engine) -> io::Result<()> {
    let (network, mut bus) = tokio::io::duplex(64 * 1024);
    let (incoming, _) = broadcast::channel::<Arc<[u8]>>(64);
    let (outgoing, mut packets) = mpsc::channel::<Vec<u8>>(64);
    let (mut read, mut write) = tokio::io::split(&mut bus);
    let from_lan = async {
        let mut buf = [0; 8192];
        loop {
            let n = read.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            let _ = incoming.send(Arc::from(&buf[..n]));
        }
    };
    let to_lan = async {
        while let Some(packet) = packets.recv().await {
            write.write_all(&packet).await?;
        }
        Ok(())
    };
    let targets = async {
        let mut workers = tokio::task::JoinSet::new();
        let mut active = std::collections::BTreeMap::new();
        loop {
            let check = engine.clone();
            let slots = tokio::task::spawn_blocking(move || check.installed_slots()).await
                .map_err(io::Error::other)?.map_err(io::Error::other)?;
            if slots.is_empty() || fs::read(dir.join("enabled")).ok().as_deref() == Some(b"no")
                || fs::read_to_string(dir.join("engine")).ok().as_ref() != Some(&engine.endpoint) {
                return Ok(());
            }
            active.retain(|port, task: &mut tokio::task::AbortHandle| {
                let keep = slots.iter().any(|s| s.port == *port);
                if !keep { task.abort(); }
                keep
            });
            while workers.try_join_next().is_some() {}
            for slot in slots {
                active.entry(slot.port).or_insert_with(|| {
                    let target = engine.in_slot(slot);
                    let (incoming, outgoing) = (incoming.clone(), outgoing.clone());
                    workers.spawn(async move { attach_slot(target, &incoming, &outgoing).await })
                });
            }
            tokio::time::sleep(RETRY).await;
            let check = engine.clone();
            if !tokio::task::spawn_blocking(move || any_running(&check)).await.unwrap_or(false) { return Ok(()); }
        }
    };
    tokio::select! {
        result = connect_lan(dir, network) => result,
        result = from_lan => result,
        result = to_lan => result,
        result = targets => result,
    }
}

async fn running_id(engine: &Engine) -> Option<String> {
    let e = engine.clone();
    tokio::task::spawn_blocking(move || e.lan_container())
        .await
        .ok()?
        .ok()?
}

async fn attach_slot(
    engine: Engine,
    incoming: &broadcast::Sender<Arc<[u8]>>,
    outgoing: &mpsc::Sender<Vec<u8>>,
) {
    loop {
        if let Some(id) = running_id(&engine).await {
            if let Ok(agent) = TcpStream::connect(("127.0.0.1", engine.slot.port)).await {
                // Recheck ownership after connecting to the published port.
                if running_id(&engine).await.as_ref() == Some(&id) {
                    let _ = agent.set_nodelay(true);
                    let announce = async {
                        tokio::time::sleep(Duration::from_secs(4)).await;
                        let e = engine.clone();
                        let _ = tokio::task::spawn_blocking(move || e.announce_on_lan(&id)).await;
                        std::future::pending::<()>().await;
                    };
                    tokio::select! {
                        _ = relay_agent(agent, incoming.subscribe(), outgoing) => (),
                        _ = announce => (),
                    }
                }
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn relay_agent<S: AsyncRead + AsyncWrite + Unpin>(
    agent: S,
    mut incoming: broadcast::Receiver<Arc<[u8]>>,
    outgoing: &mpsc::Sender<Vec<u8>>,
) -> io::Result<()> {
    let (mut read, mut write) = tokio::io::split(agent);
    let receive = async {
        loop {
            match incoming.recv().await {
                Ok(bytes) => write.write_all(&bytes).await?,
                // A slow agent misses some bytes; HDLC resyncs at the next flag, so keep the connection.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    };
    let send = async {
        let mut decoder = HdlcDeframer::new();
        let mut buf = [0; 8192];
        loop {
            let n = read.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            for packet in decoder.feed(&buf[..n]) {
                outgoing
                    .send(frame(&packet))
                    .await
                    .map_err(io::Error::other)?;
            }
        }
    };
    tokio::select! { result = receive => result, result = send => result }
}

async fn connect_lan<S: AsyncRead + AsyncWrite + Unpin>(dir: &Path, agent: S) -> io::Result<()> {
    // Two AutoInterfaces on one host have the same link-local address. Incoming unicast
    // would go to only one of them even if both successfully joined the multicast group.
    match std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, DATA_PORT)) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            return shared(dir, agent)
                .await
                .map_err(|_| io::Error::other("in-use"));
        }
        Err(_) => return Err(io::Error::other("unavailable")),
        Ok(probe) => drop(probe),
    }
    native(dir, agent, AutoInterfaceConfig::default()).await
}

async fn native<S: AsyncRead + AsyncWrite + Unpin>(
    dir: &Path,
    agent: S,
    config: AutoInterfaceConfig,
) -> io::Result<()> {
    let (tx, mut incoming) = mpsc::channel(128);
    let (spawned, mut children) = mpsc::channel::<InterfaceHandle>(16);
    let ids = Arc::new(AtomicU64::new(2));
    let mut lan = spawn_auto_interface(config, 1, ids, tx, spawned, Arc::new(AtomicBool::new(true)))
        .await
        .map_err(|_| io::Error::other("unavailable"))?;
    lan.owner.mark_registered();
    state(dir, "native");
    let (packets, inbound) = mpsc::channel(128);
    let (peers, found) = mpsc::channel(16);
    let receive = async {
        while let Some(message) = incoming.recv().await {
            if let TransportMessage::InterfaceEvent { message, .. } = message {
                if let TransportMessage::Inbound(packet) = *message {
                    if packets.send(packet.raw.to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
        Err(io::Error::other("unavailable"))
    };
    // The parent only discovers; each peer is a child interface that carries its traffic.
    let adopt = async {
        while let Some(child) = children.recv().await {
            child.owner.mark_registered();
            if peers.send(Peer { tx: child.tx, mtu: child.mtu as usize }).await.is_err() {
                break;
            }
        }
        Err(io::Error::other("unavailable"))
    };
    tokio::select! {
        result = relay_lan(agent, inbound, found) => result,
        result = receive => result,
        result = adopt => result,
        _ = &mut lan.read_task => Err(io::Error::other("unavailable")),
    }
}

struct Peer<B> {
    tx: mpsc::Sender<B>,
    mtu: usize,
}

async fn relay_lan<S: AsyncRead + AsyncWrite + Unpin, B: From<Vec<u8>>>(
    agent: S,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    mut found: mpsc::Receiver<Peer<B>>,
) -> io::Result<()> {
    let peers = Mutex::new(Vec::new());
    let (mut reader, mut writer) = tokio::io::split(agent);
    let adopt = async {
        while let Some(peer) = found.recv().await {
            peers.lock().unwrap_or_else(|e| e.into_inner()).push(peer);
        }
        std::future::pending::<io::Result<()>>().await
    };
    let to_agent = async {
        while let Some(packet) = inbound.recv().await {
            writer.write_all(&frame(&packet)).await?;
        }
        Ok(())
    };
    let to_lan = async {
        let mut decoder = HdlcDeframer::new();
        let mut buf = [0; 8192];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            for packet in decoder.feed(&buf[..n]) {
                if packet.is_empty() {
                    continue;
                }
                // UDP semantics: a full peer queue drops the packet; a closed one is a departed peer.
                peers.lock().unwrap_or_else(|e| e.into_inner()).retain(|peer: &Peer<B>| {
                    packet.len() > peer.mtu
                        || !matches!(peer.tx.try_send(packet.clone().into()), Err(mpsc::error::TrySendError::Closed(_)))
                });
            }
        }
    };
    tokio::select! {
        biased;
        result = adopt => result,
        result = to_agent => result,
        result = to_lan => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::duplex, time::timeout};

    #[tokio::test]
    async fn two_agents_share_lan_without_mixing_frames_or_disconnecting_each_other() {
        let (inbound, _) = broadcast::channel(8);
        let (outbound, mut packets) = mpsc::channel(8);
        let (a, mut desktop) = duplex(256);
        let (b, mut hermes) = duplex(256);
        let attach = |stream| {
            let rx = inbound.subscribe();
            let tx = outbound.clone();
            tokio::spawn(async move { relay_agent(stream, rx, &tx).await })
        };
        let (one, two) = (attach(a), attach(b));
        let exercise = async {
            let first = frame(&[1, 0x7e, 0x7d, 2]);
            let second = frame(&[3, 4, 5, 6]);
            desktop.write_all(&first[..3]).await.unwrap();
            hermes.write_all(&second).await.unwrap();
            assert_eq!(packets.recv().await.unwrap(), second);
            desktop.write_all(&first[3..]).await.unwrap();
            assert_eq!(packets.recv().await.unwrap(), first);
            inbound.send(Arc::from(&first[..])).unwrap();
            let mut buf = vec![0; first.len()];
            desktop.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, first);
            hermes.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, first);
            drop(desktop);
            one.await.unwrap().unwrap();
            hermes.write_all(&second).await.unwrap();
            assert_eq!(packets.recv().await.unwrap(), second);
            drop(hermes);
            two.await.unwrap().unwrap();
        };
        timeout(Duration::from_secs(3), exercise).await.unwrap();
    }

    #[tokio::test]
    async fn a_lagging_agent_stays_connected() {
        let (inbound, _) = broadcast::channel(2);
        let (outbound, _packets) = mpsc::channel(8);
        let (agent, mut far) = duplex(1 << 16);
        let rx = inbound.subscribe();
        for n in 0..5u8 {
            inbound.send(Arc::from(&[n][..])).unwrap(); // three are dropped before the relay reads
        }
        let relay = tokio::spawn(async move { relay_agent(agent, rx, &outbound).await });
        timeout(Duration::from_secs(3), async {
            let mut buf = [0; 2];
            far.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, [3, 4]);
            inbound.send(Arc::from(&[9][..])).unwrap();
            far.read_exact(&mut buf[..1]).await.unwrap();
            assert_eq!(buf[0], 9);
            assert!(!relay.is_finished());
            drop(far);
            relay.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_shared_owner_carries_both_directions_and_detaches_on_container_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut agent) = duplex(64);
        let (server, mut owner) = duplex(64);
        let transfer = async {
            let packet = frame(&[0x01, 0x7e, 0x7d, 0x02]);
            agent.write_all(&packet[..2]).await.unwrap();
            agent.write_all(&packet[2..]).await.unwrap();
            let mut received = vec![0; packet.len()];
            owner.read_exact(&mut received).await.unwrap();
            assert_eq!(received, packet);
            owner.write_all(&packet).await.unwrap();
            agent.read_exact(&mut received).await.unwrap();
            assert_eq!(received, packet);
            drop(agent);
            assert_eq!(owner.read(&mut received).await.unwrap(), 0);
        };
        timeout(Duration::from_secs(3), async {
            let (result, ()) = tokio::join!(relay_shared(dir.path(), client, server), transfer);
            result.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn lan_peers_get_agent_packets_within_their_mtu_and_peer_packets_reach_the_agent() {
        let (agent, mut far) = duplex(1024);
        let (packets, inbound) = mpsc::channel(8);
        let (peers, found) = mpsc::channel(8);
        let (one, mut first) = mpsc::channel::<Vec<u8>>(8);
        let (gone, departed) = mpsc::channel::<Vec<u8>>(8);
        let (small, mut tiny) = mpsc::channel::<Vec<u8>>(8);
        drop(departed);
        for (tx, mtu) in [(one, 500), (gone, 500), (small, 2)] {
            peers.send(Peer { tx, mtu }).await.unwrap();
        }
        let relay = tokio::spawn(relay_lan(agent, inbound, found));
        timeout(Duration::from_secs(3), async {
            let payload = vec![0x42, 0x7e, 0x7d, 0x21];
            far.write_all(&frame(&payload)).await.unwrap();
            assert_eq!(first.recv().await.unwrap(), payload);
            assert!(tiny.try_recv().is_err());
            packets.send(vec![0x43, 0x7d]).await.unwrap();
            let encoded = frame(&[0x43, 0x7d]);
            let mut received = vec![0; encoded.len()];
            far.read_exact(&mut received).await.unwrap();
            assert_eq!(received, encoded);
            drop(far);
            relay.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}

async fn shared<S: AsyncRead + AsyncWrite + Unpin>(dir: &Path, agent: S) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::{
            linux::net::SocketAddrExt,
            unix::net::{SocketAddr, UnixStream},
        };
        let addr = SocketAddr::from_abstract_name(b"rns/default")?;
        if let Ok(socket) = UnixStream::connect_addr(&addr) {
            socket.set_nonblocking(true)?;
            return relay_shared(dir, agent, tokio::net::UnixStream::from_std(socket)?).await;
        }
    }
    let peer = TcpStream::connect(("127.0.0.1", FALLBACK_TCP_PORT)).await?;
    peer.set_nodelay(true)?;
    relay_shared(dir, agent, peer).await
}

async fn relay_shared<A, B>(dir: &Path, agent: A, owner: B) -> io::Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    // Reticulum's shared packet socket and the agent's TCP listener use identical HDLC framing.
    let (mut ar, mut aw) = tokio::io::split(agent);
    let (mut lr, mut lw) = tokio::io::split(owner);
    state(dir, "shared");
    tokio::select! {
        result = tokio::io::copy(&mut ar, &mut lw) => result?,
        result = tokio::io::copy(&mut lr, &mut aw) => result?,
    };
    Ok(())
}

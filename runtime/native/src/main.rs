//! Private NDJSON bridge. Reticulum and LXMF own all cryptography and wire formats.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use lxmf_core::message_api::{DeliveryMethod, LxMessage, LxMessageDraft, MessageDecodeLimits};
use rns_runtime::{
    config::Config, lifecycle::install_signal_handlers, link_session::LinkSessionConfig, prelude::*,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs::{File, OpenOptions},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::mpsc, task::JoinSet, time::timeout};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
type Peers = Arc<RwLock<HashMap<[u8; 16], [u8; 64]>>>;
mod voice;

const MAX_LINE: usize = 64 * 1024;
// One message, attachment included. Ratspeak and Sideband accept 1 MB by default.
const MAX_MESSAGE: usize = 1_000_000;
const MAX_RAW_EVENT: usize = 48 * 1024;
const MAX_TEXT: usize = 16 * 1024;
const MAX_TITLE: usize = 1024;
const MAX_PEERS: usize = 256;
const SEND_TIMEOUT: Duration = Duration::from_secs(90);
const KEY_DISCOVERY: Duration = Duration::from_secs(12);
const ASPECT: &str = "lxmf.delivery";

#[derive(Clone)]
struct Output(mpsc::Sender<Value>);
impl Output {
    async fn emit(&self, value: Value) {
        let _ = self.0.send(value).await;
    }
    async fn result(&self, id: Value, result: Result<Value>) {
        self.emit(match result {
            Ok(mut value) => {
                value["event"] = json!("result");
                value["id"] = id;
                value["ok"] = json!(value.get("transport_delivered") != Some(&json!(false)));
                value
            }
            Err(error) => json!({"event":"result","id":id,"ok":false,"error":error.to_string()}),
        })
        .await;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    address: String,
    public_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    /// A file in the outbox folder given with --files.
    path: PathBuf,
    name: String,
    kind: AttachmentKind,
    format: Option<String>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum AttachmentKind {
    File,
    Image,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Send {
        destination: String,
        text: String,
        #[serde(default)]
        title: String,
        public_key: Option<String>,
        #[serde(default)]
        best_effort: bool,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    Access {
        senders: Vec<String>,
        mode: InboxMode,
    },
    Peers {
        peers: Vec<Peer>,
    },
    ValidateKey {
        address: String,
        public_key: String,
    },
    Status,
    Announce { label: Option<String> },
    Shutdown,
}

struct Args {
    command: String,
    identity: PathBuf,
    config: Option<PathBuf>,
    create: bool,
    label: String,
    files: Option<PathBuf>,
}
fn args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or("use identity --identity PATH [--create] or serve --identity PATH --config DIR [--label LABEL]")?;
    if command != "identity" && command != "serve" {
        return Err("unknown command".into());
    }
    let (mut identity, mut config, mut create, mut label, mut files) =
        (None, None, false, "Agent Rat".to_string(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--identity" if identity.is_none() => {
                identity = Some(PathBuf::from(args.next().ok_or("--identity needs a path")?))
            }
            "--config" if config.is_none() => {
                config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?))
            }
            "--create" if !create => create = true,
            "--label" => label = args.next().ok_or("--label needs text")?,
            "--files" if files.is_none() => {
                files = Some(PathBuf::from(args.next().ok_or("--files needs a folder")?))
            }
            _ => return Err(format!("unknown or duplicate argument: {arg}").into()),
        }
    }
    if label.len() > 128 {
        return Err("label exceeds 128 UTF-8 bytes".into());
    }
    if command == "serve" && (config.is_none() || create) {
        return Err("serve requires --config and does not accept --create".into());
    }
    Ok(Args {
        command,
        identity: identity.ok_or("--identity is required")?,
        config,
        create,
        label,
        files,
    })
}

fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        return Err(format!("expected {} hexadecimal characters", N * 2).into());
    }
    hex::decode(value)?
        .try_into()
        .map_err(|_| "wrong hex length".into())
}
fn address(identity: &Identity) -> [u8; 16] {
    Destination::hash_from_name_and_identity(ASPECT, Some(&identity.hash))
}
fn bound_key(destination: [u8; 16], key: [u8; 64]) -> Result<[u8; 64]> {
    if address(&Identity::from_public_key(&key)?) != destination {
        return Err("public key does not match LXMF address".into());
    }
    Ok(key)
}
fn limits() -> MessageDecodeLimits {
    MessageDecodeLimits {
        max_wire_bytes: MAX_MESSAGE,
        max_top_level_fields: 64,
    }
}

/// A received file name made safe for this disk: letters, digits, space, dot, dash, and underscore.
fn safe_name(name: &str, fallback: &str) -> String {
    let kept: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
        .take(120)
        .collect();
    let kept = kept.trim().trim_start_matches('.').trim();
    if kept.is_empty() { fallback.to_string() } else { kept.to_string() }
}

fn image_extension(format: &str) -> String {
    let subtype = format.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    match subtype.as_str() {
        "jpeg" | "jpg" => "jpg".into(),
        "png" | "webp" | "gif" | "bmp" | "heic" | "avif" => subtype,
        _ => "img".into(),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

/// Save a verified message's image, file, or voice note under files/incoming/<message id>/.
fn save_attachments(files: Option<&Path>, message: &LxMessage) -> Vec<Value> {
    let mut found = Vec::new();
    if let Ok(Some((format, bytes))) = message.image_attachment() {
        found.push(("image", format!("image.{}", image_extension(&format)), Some(format), None, bytes.to_vec()));
    }
    if let Ok(Some((name, bytes))) = message.first_file_attachment() {
        found.push(("file", safe_name(&name, "file"), None, None, bytes.to_vec()));
    }
    if let Ok(Some(audio)) = message.audio_field() {
        found.push(("audio", "voice.bin".into(), None, Some(audio.mode), audio.bytes.to_vec()));
    }
    let Some(root) = files else {
        return found.iter().map(|(kind, name, ..)| json!({"kind":kind,"name":name,"dropped":true})).collect();
    };
    let folder = root.join("incoming").join(hex::encode(message.message_id()));
    found
        .into_iter()
        .filter_map(|(kind, name, format, mode, bytes)| {
            let path = folder.join(&name);
            let saved = std::fs::create_dir_all(&folder).map_err(Into::into).and_then(|_| write_private(&path, &bytes));
            saved.ok()?;
            let mut entry = json!({"kind":kind,"name":name,"format":format,"mode":mode,"size":bytes.len(),"path":path});
            // Voice notes also get a WAV copy that a transcriber can read without audio libraries.
            if let Some((wav, seconds)) = mode.and_then(|mode| voice::wav(mode, &bytes)) {
                let wav_path = folder.join("voice.wav");
                if write_private(&wav_path, &wav).is_ok() {
                    entry["wav"] = json!(wav_path);
                    entry["seconds"] = json!(seconds);
                }
            }
            Some(entry)
        })
        .collect()
}

/// A file to send must already be a regular file in files/outbox/.
fn outbox_file(files: Option<&Path>, path: &Path) -> Result<Vec<u8>> {
    let outbox = files.ok_or("attachments need --files")?.join("outbox").canonicalize()?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("attachment must not be a symbolic link".into());
    }
    let real = path.canonicalize()?;
    if real.parent() != Some(outbox.as_path()) || !real.is_file() {
        return Err("attachment must be a file in the outbox".into());
    }
    if real.metadata()?.len() as usize > MAX_MESSAGE - 32 * 1024 {
        return Err("attachment exceeds the message size limit".into());
    }
    Ok(std::fs::read(real)?)
}

fn identity(path: &Path, create: bool) -> Result<Identity> {
    if !path.try_exists()? {
        if !create {
            return Err("identity does not exist; explicit --create is required".into());
        }
        let generated = Identity::new();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // create_new prevents accidentally replacing another process's identity.
        let mut file = options.open(path)?;
        file.write_all(
            &*generated
                .get_private_key()
                .ok_or("identity has no private key")?,
        )?;
        file.sync_all()?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        return Ok(generated);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err("identity must be a regular file, not a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("identity must have owner-only permissions (chmod 600)".into());
        }
    }
    Ok(Identity::from_file(path)?)
}

fn read_line_bounded(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::with_capacity(1024);
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if oversized {
                return Err("input line exceeds 64 KiB".into());
            }
            return Ok((!line.is_empty()).then_some(line));
        }
        let count = available
            .iter()
            .position(|&b| b == b'\n')
            .map_or(available.len(), |n| n + 1);
        let ended = available[count - 1] == b'\n';
        if line.len() + count > MAX_LINE {
            oversized = true;
        }
        if !oversized {
            line.extend_from_slice(&available[..count]);
        }
        reader.consume(count);
        if ended {
            if oversized {
                return Err("input line exceeds 64 KiB".into());
            }
            return Ok(Some(line));
        }
    }
}

fn stdin_requests() -> mpsc::Receiver<Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel(16);
    // A detached OS reader lets EOF/signals stop the Tokio runtime even if stdin is idle.
    std::thread::spawn(move || {
        let input = std::io::stdin();
        let mut reader = input.lock();
        loop {
            match read_line_bounded(&mut reader) {
                Ok(Some(line)) => {
                    if tx.blocking_send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    if tx.blocking_send(Err(error)).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn parse_request(raw: &[u8]) -> Result<(Value, Operation)> {
    let mut value: Value = serde_json::from_str(std::str::from_utf8(raw)?)?;
    let object = value.as_object_mut().ok_or("request must be an object")?;
    let id = object.remove("id").ok_or("request id is required")?;
    if !id.is_string() && !id.is_number() {
        return Err("request id must be a string or number".into());
    }
    if id.as_str().is_some_and(|s| s.len() > 128) {
        return Err("request id exceeds 128 bytes".into());
    }
    let allowed: &[&str] = match object.get("op").and_then(Value::as_str) {
        Some("send") => &[
            "op",
            "destination",
            "text",
            "title",
            "public_key",
            "best_effort",
            "attachments",
        ],
        Some("access") => &["op", "senders", "mode"],
        Some("peers") => &["op", "peers"],
        Some("validate_key") => &["op", "address", "public_key"],
        Some("announce") => &["op", "label"],
        Some("status" | "shutdown") => &["op"],
        _ => return Err("unknown operation".into()),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown request field".into());
    }
    Ok((id, serde_json::from_value(value)?))
}

fn replace_peers(peers: &Peers, entries: Vec<Peer>) -> Result<Value> {
    if entries.len() > MAX_PEERS {
        return Err("too many peer keys".into());
    }
    let mut checked = HashMap::new();
    for entry in entries {
        let destination = parse_hex(&entry.address)?;
        let key = bound_key(destination, parse_hex(&entry.public_key)?)?;
        if checked.insert(destination, key).is_some() {
            return Err("duplicate peer address".into());
        }
    }
    let count = checked.len();
    *peers.write().map_err(|_| "peer cache unavailable")? = checked;
    Ok(json!({"peers":count}))
}

fn validate_key(destination: &str, public_key: &str) -> Result<Value> {
    let destination = parse_hex(destination)?;
    let key = bound_key(destination, parse_hex(public_key)?)?;
    Ok(json!({"address":hex::encode(destination),"public_key":hex::encode(key)}))
}

fn decode_message(data: Vec<u8>, recipient: [u8; 16]) -> Result<LxMessage> {
    let message = LxMessage::unpack_owned(data.into(), limits())?;
    if message.destination_hash() != recipient {
        return Err("message addressed to another recipient".into());
    }
    if message.content_bytes().len() > MAX_TEXT || message.title_bytes().len() > MAX_TITLE {
        return Err("message text/title exceeds limit".into());
    }
    std::str::from_utf8(message.content_bytes())?;
    std::str::from_utf8(message.title_bytes())?;
    Ok(message)
}

fn embedded_key(message: &LxMessage) -> Option<[u8; 64]> {
    // This is only a candidate key. No embedded field is trusted before verify().
    let envelope: Value = serde_json::from_slice(message.content_bytes()).ok()?;
    if envelope.get("protocol")?.as_str()? != "agent-rat"
        || envelope.get("version")?.as_u64()? != 1
        || !matches!(envelope.get("type")?.as_str()?, "hello" | "ack")
    {
        return None;
    }
    let key = envelope.get("sender_key")?.as_str()?;
    bound_key(message.source_hash(), parse_hex(key).ok()?).ok()
}

async fn incoming(runtime: &ReticulumHandle, peers: &Peers, files: Option<&Path>, message: LxMessage, output: &Output) {
    let recipient = message.destination_hash();
    let source = message.source_hash();
    let pinned = peers.read().ok().and_then(|p| p.get(&source).copied());
    let mut verified_key = match pinned {
        Some(key) if message.verify(&key).is_ok() => Some(key),
        Some(_) => return, // A different signing key cannot own the pinned address.
        None => embedded_key(&message).filter(|key| message.verify(key).is_ok()),
    };
    // Unpinned senders (ordinary LXMF clients) are verified against their announce;
    // one bounded path request fetches it when it is not cached yet.
    for attempt in 0..2 {
        if verified_key.is_some() {
            break;
        }
        if attempt == 1 && runtime.await_path(source, KEY_DISCOVERY).await.is_err() {
            break;
        }
        if let Ok(Ok(Some(recalled))) =
            timeout(Duration::from_secs(2), runtime.recall(source)).await
        {
            let key = recalled.identity.get_public_key();
            if message.verify(&key).is_ok() {
                verified_key = Some(key);
            }
        }
    }
    let Some(key) = verified_key else { return };
    // Attachments are written only after the signature verified.
    let attachments = save_attachments(files, &message);
    let wire = message.wire_bytes();
    output
        .emit(json!({
            "event":"message", "message_id":hex::encode(message.message_id()),
            "source":hex::encode(source), "destination":hex::encode(recipient),
            "source_key":hex::encode(key), "verified":true,
            "text":std::str::from_utf8(message.content_bytes()).expect("checked UTF-8"),
            "title":std::str::from_utf8(message.title_bytes()).expect("checked UTF-8"),
            "raw_b64":(wire.len() <= MAX_RAW_EVENT).then(|| B64.encode(wire)),
            "attachments":attachments,
            "error":Value::Null
        }))
        .await;
}

// A source address can only exclude traffic here. Authorization still requires LXMF verification.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum InboxMode {
    Owners,
    Anyone,
    Later,
}

// Setup codes avoid I, L, O, and 0, which read alike.
const CLAIM_ALPHABET: &[u8] = b"123456789ABCDEFGHJKMNPQRSTUVWXYZ";

struct InboundAccess {
    senders: HashSet<[u8; 16]>,
    mode: InboxMode,
}

impl InboundAccess {
    fn permits(&self, message: &LxMessage) -> bool {
        if matches!(self.mode, InboxMode::Owners) {
            return self.senders.contains(&message.source_hash());
        }
        if matches!(self.mode, InboxMode::Later) {
            // "6J8E4K" or "6J8 E4K", as the answer loop accepts.
            let text = std::str::from_utf8(message.content_bytes()).unwrap_or("");
            let code = |s: &str, n| {
                s.len() == n && s.bytes().all(|b| CLAIM_ALPHABET.contains(&b.to_ascii_uppercase()))
            };
            return match text.split_ascii_whitespace().collect::<Vec<_>>().as_slice() {
                [whole] => code(whole, 6),
                [first, second] => code(first, 3) && code(second, 3),
                _ => false,
            };
        }
        true
    }
}

async fn drain_events(
    mut events: DestinationEvents,
    recipient: [u8; 16],
    guests: mpsc::Sender<LxMessage>,
    contacts: mpsc::Sender<LxMessage>,
    access: Arc<RwLock<InboundAccess>>,
    shutdown: ShutdownSignal,
) {
    loop {
        let data = tokio::select! {
            _ = shutdown.wait() => break,
            packet = events.link_packets.recv() => match packet { Some((data, _)) => Some(data), None => break },
            resource = events.resource_completions.recv() => match resource { Some(resource) => Some(resource.data), None => break },
            // An opportunistic packet is the Direct image without its destination hash.
            packet = events.packets.recv() => match packet {
                Some(packet) => Some([recipient.as_slice(), packet.data.as_slice()].concat()),
                None => break,
            },
            _ = events.links_established.recv() => None,
            _ = events.links_identified.recv() => None,
            _ = events.link_packet_proofs.recv() => None,
            _ = events.resource_proofs.recv() => None,
            _ = events.resource_events.recv() => None,
            _ = events.channel_messages.recv() => None,
            _ = events.links_closed.recv() => None,
        };
        if let Some(data) = data {
            if let Ok(message) = decode_message(data, recipient)
                && let Ok(rule) = access.read()
                && rule.permits(&message)
            {
                // Transport proofs precede application admission. Never acknowledge a dropped
                // message at the application layer, and keep serving when the inbox drains.
                let channel = if rule.senders.contains(&message.source_hash()) {
                    &contacts
                } else {
                    &guests
                };
                if let Err(mpsc::error::TrySendError::Closed(_)) = channel.try_send(message) {
                    break;
                }
            }
        }
    }
}

async fn status(runtime: &ReticulumHandle) -> Result<Value> {
    let stats = timeout(Duration::from_secs(5), runtime.interface_stats()).await??;
    let online = stats.interfaces.iter().any(|i| i.online);
    let interfaces: Vec<_> = stats
        .interfaces
        .iter()
        .map(|i| {
            json!({
                "name":i.name,"online":i.online,"mode":i.mode,"role":i.role,
                "rx_bytes":i.rx_bytes,"tx_bytes":i.tx_bytes,"rx_rate":i.rx_rate,"tx_rate":i.tx_rate,
                "bitrate":i.bitrate,"mtu":i.mtu,"clients":i.clients
            })
        })
        .collect();
    Ok(
        json!({"network_state":if online {"interface_online"} else {"offline"},"interfaces":interfaces,
        "rx_bytes":stats.rx_bytes,"tx_bytes":stats.tx_bytes,"transport_enabled":runtime.transport_enabled(),
        "startup_interface_failures":runtime.startup_interface_failures()}),
    )
}

struct SessionCloseGuard(Option<LinkSessionHandle>);
impl Drop for SessionCloseGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            tokio::spawn(async move {
                handle.close().await;
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send(
    runtime: ReticulumHandle,
    identity: Identity,
    peers: Peers,
    files: Option<PathBuf>,
    destination: String,
    text: String,
    title: String,
    public_key: Option<String>,
    attachments: Vec<Attachment>,
) -> Result<Value> {
    if text.len() > MAX_TEXT || title.len() > MAX_TITLE {
        return Err("text/title exceeds UTF-8 byte limit".into());
    }
    let destination = parse_hex(&destination)?;
    let supplied = public_key
        .map(|key| bound_key(destination, parse_hex(&key)?))
        .transpose()?;
    let known = supplied.or_else(|| peers.read().ok().and_then(|p| p.get(&destination).copied()));
    if attachments.len() > 1 {
        return Err("send one attachment per message".into());
    }
    let mut draft = LxMessageDraft::new_with_limits(
        destination,
        address(&identity),
        &title,
        &text,
        DeliveryMethod::Direct,
        limits(),
    )?;
    for attachment in &attachments {
        let bytes = outbox_file(files.as_deref(), &attachment.path)?;
        let name = safe_name(&attachment.name, "file");
        if attachment.kind == AttachmentKind::Image {
            draft.set_image_field(attachment.format.as_deref().unwrap_or("png"), &bytes)?;
        } else {
            draft.set_file_attachment_field(&name, &bytes)?;
        }
    }
    let message = draft
        .sign(&identity.get_signing_key().ok_or("identity cannot sign")?)?
        .finalize(None)?;
    let message_id = hex::encode(message.message_id());
    let packed = message.pack()?;
    // Large messages get time for their transfer: twice the airtime at the first hop, up to half an hour.
    let bitrate = timeout(Duration::from_secs(2), runtime.next_hop_bitrate(destination))
        .await
        .ok()
        .and_then(|b| b.ok().flatten())
        .unwrap_or(1_200)
        .max(300);
    let airtime = Duration::from_secs((packed.len() as u64 * 8 * 2 / bitrate).min(1800));
    let deadline = SEND_TIMEOUT + if packed.len() > MAX_LINE { airtime } else { Duration::ZERO };
    // A failure before the link exists cannot have delivered anything, so it is safe to retry.
    let linked = AtomicBool::new(false);
    let outcome = timeout(deadline, async {
        let mut session = if let Some(key) = known {
            // The peer-admission cache supplies a source-bound public identity. The
            // public LinkSession API accepts it without forging an announce/cache entry.
            runtime
                .await_path(destination, Duration::from_secs(20))
                .await?;
            LinkSession::connect(
                runtime.transport_tx.clone(),
                identity,
                LinkSessionConfig {
                    hops: runtime.hops_to(destination).await?,
                    ..LinkSessionConfig::identified(destination, key, "agent-rat")
                },
            )
            .await?
        } else {
            runtime
                .connect_link(
                    destination,
                    identity,
                    LinkConnectOptions {
                        path_timeout: Duration::from_secs(20),
                        establishment_timeout: Some(Duration::from_secs(30)),
                        identify: true,
                        client_label: "agent-rat".into(),
                        ..LinkConnectOptions::default()
                    },
                )
                .await?
        };
        linked.store(true, Ordering::Relaxed);
        let mut close_guard = SessionCloseGuard(Some(session.handle.clone()));
        let delivery: Result<&str> = async {
            if packed.len() <= session.handle.mdu() {
                let receipt = session.handle.send_packet(packed).await?;
                loop {
                    match session.events.recv().await {
                        Some(LinkSessionEvent::PacketDelivered { packet_hash })
                            if packet_hash == receipt.packet_hash =>
                        {
                            break;
                        }
                        Some(LinkSessionEvent::Closed { .. }) | None => {
                            return Err("link closed before packet delivery proof".into());
                        }
                        _ => {}
                    }
                }
                Ok("packet")
            } else {
                let transfer = session
                    .handle
                    .send_resource_bytes(
                        packed,
                        ResourceOptions {
                            auto_compress: false,
                            metadata: None,
                        },
                    )
                    .await?;
                let concluded = transfer.concluded();
                tokio::pin!(concluded);
                loop {
                    tokio::select! {
                        receipt = &mut concluded => { receipt?; break; },
                        event = session.events.recv() => match event {
                            Some(LinkSessionEvent::Closed { .. }) | None => return Err("link closed before resource delivery proof".into()),
                            _ => {},
                        }
                    }
                }
                Ok("resource")
            }
        }
        .await;
        session.handle.close().await;
        close_guard.0 = None;
        Ok::<Value, Box<dyn Error + Send + Sync>>(
            json!({"message_id":message_id,"transport_delivered":true,"representation":delivery?}),
        )
    })
    .await;
    // Preserve the real signed LXMF id even when transport delivery fails.
    Ok(match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            json!({"message_id":message_id,"transport_delivered":false,"linked":linked.load(Ordering::Relaxed),"error":error.to_string()})
        }
        Err(_) => {
            json!({"message_id":message_id,"transport_delivered":false,"linked":linked.load(Ordering::Relaxed),"error":format!("native send exceeded its {} second deadline; delivery is unknown", deadline.as_secs())})
        }
    })
}

async fn serve(args: Args, identity: Identity) -> Result<()> {
    let _identity_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&args.identity)?;
    _identity_lock
        .try_lock()
        .map_err(|_| "this identity is already served by another native process")?;
    let config = args.config.ok_or("missing config")?;
    let parsed = Config::from_file(&config.join("config"))?;
    let section = parsed
        .section("reticulum")
        .ok_or("missing [reticulum] config section")?;
    if section.get_bool("share_instance") != Some(false)
        || section.get_bool("enable_transport") != Some(false)
    {
        return Err(
            "private config must explicitly set share_instance = No and enable_transport = No"
                .into(),
        );
    }
    let shutdown = ShutdownSignal::new();
    let _signals = install_signal_handlers(shutdown.clone());
    // One failed interface (an optional hub, LAN before Wi-Fi is up) must not stop the agent.
    let mut options = InitOptions::default();
    options.keep_starting_after_interface_failure = true;
    options.start_interfaces_concurrently = true;
    let runtime = init_with_options(
        Some(config.to_str().ok_or("config path is not UTF-8")?),
        None,
        shutdown.clone(),
        Arc::new(AtomicBool::new(true)),
        options,
    )
    .await?;
    let mut destination = runtime
        .register_destination(
            identity.clone(),
            ASPECT,
            DestinationRuntimeOptions {
                accepts_links: true,
                proof_strategy: ProofStrategy::ProveAll,
                resource_strategy: ResourceStrategy::AcceptApp,
                resource_accept: Some(ResourceAcceptPolicy::new(|_, ad| {
                    ad.data_size <= MAX_MESSAGE
                        && ad.transfer_size <= MAX_MESSAGE + 1024
                        && ad.total_segments == 1
                        && ad.request_id.is_none()
                        && !ad.flags.split
                        && !ad.flags.has_metadata
                        && !ad.flags.is_request
                        && !ad.flags.is_response
                })),
                event_capacity: 64,
                inbound_reassembly_limit: Some(MAX_MESSAGE + 64 * 1024),
                ..DestinationRuntimeOptions::default()
            },
        )
        .await?;
    let (out_tx, mut out_rx) = mpsc::channel::<Value>(128);
    let output = Output(out_tx);
    let output_shutdown = shutdown.clone();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(value) = out_rx.recv().await {
            let mut bytes = serde_json::to_vec(&value).expect("JSON values serialize");
            bytes.push(b'\n');
            if stdout.write_all(&bytes).await.is_err() || stdout.flush().await.is_err() {
                output_shutdown.trigger();
                break;
            }
        }
    });
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));
    let recipient = address(&identity);
    let access = Arc::new(RwLock::new(InboundAccess {
        senders: HashSet::new(),
        mode: InboxMode::Owners,
    }));
    let (guests, guest_rx) = mpsc::channel(48);
    let (contacts, contact_rx) = mpsc::channel(16);
    // Move all event receivers out while retaining the registration handle for shutdown.
    let events = std::mem::replace(&mut destination.events, empty_events());
    let drainer = tokio::spawn(drain_events(
        events,
        recipient,
        guests,
        contacts,
        access.clone(),
        shutdown.clone(),
    ));
    // Waiting for a new guest's announce must not delay verification for configured contacts.
    let mut receivers = JoinSet::new();
    for mut input in [guest_rx, contact_rx] {
        let runtime = runtime.clone();
        let peers = peers.clone();
        let output = output.clone();
        let files = args.files.clone();
        receivers.spawn(async move {
            while let Some(message) = input.recv().await {
                incoming(&runtime, &peers, files.as_deref(), message, &output).await;
            }
        });
    }
    let mut label = args.label;
    output.emit(json!({"event":"ready","address":hex::encode(recipient),"public_key":hex::encode(identity.get_public_key()),"label":label})).await;
    if let Err(error) = destination.handle.announce(announce_options(&label)).await {
        output
            .emit(json!({"event":"error","scope":"announce","error":error.to_string()}))
            .await;
    }
    let mut announce_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(360 * 60),
        Duration::from_secs(360 * 60),
    );
    let mut input = stdin_requests();
    let mut outbound = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.wait() => break,
            Some(_) = outbound.join_next(), if !outbound.is_empty() => {},
            _ = announce_tick.tick() => { let _ = destination.handle.announce(announce_options(&label)).await; },
            request = input.recv() => {
                let Some(request) = request else { break; };
                let (id, operation) = match request.and_then(|bytes| parse_request(&bytes)) {
                    Ok(request) => request,
                    Err(error) => { output.result(Value::Null, Err(error)).await; continue; }
                };
                match operation {
                    Operation::Send { destination, text, title, public_key, best_effort, attachments } => {
                        while outbound.try_join_next().is_some() {}
                        if !outbound.is_empty() { output.result(id, Err("one outbound send is already active".into())).await; continue; }
                        let runtime = runtime.clone(); let identity = identity.clone(); let peers = peers.clone(); let output = output.clone();
                        let files = args.files.clone();
                        outbound.spawn(async move {
                            let delivery = send(runtime, identity, peers, files, destination, text, title, public_key, attachments);
                            let result = if best_effort {
                                timeout(Duration::from_secs(3), delivery).await.unwrap_or_else(|_| Err("notice expired".into()))
                            } else {
                                delivery.await
                            };
                            output.result(id, result).await;
                        });
                    },
                    Operation::Peers { peers: entries } => output.result(id, replace_peers(&peers, entries)).await,
                    Operation::Access { senders, mode } => {
                        let result = (|| -> Result<Value> {
                            let senders = senders.iter().map(|s| parse_hex(s)).collect::<Result<HashSet<_>>>()?;
                            *access.write().map_err(|_| "inbound access unavailable")? = InboundAccess { senders, mode };
                            Ok(json!({}))
                        })();
                        output.result(id, result).await;
                    },
                    Operation::ValidateKey { address, public_key } => output.result(id, validate_key(&address, &public_key)).await,
                    Operation::Status => output.result(id, status(&runtime).await).await,
                    Operation::Announce { label: requested } => {
                        if let Some(name) = requested {
                            if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                                output.result(id, Err("Invalid announce label".into())).await;
                                continue;
                            }
                            label = name;
                        }
                        let result = destination.handle.announce(announce_options(&label)).await;
                        output.result(id, result.map(|_| json!({"label":label})).map_err(Into::into)).await;
                    }
                    Operation::Shutdown => { output.result(id, Ok(json!({"shutting_down":true}))).await; break; },
                }
            }
        }
    }
    outbound.abort_all();
    while outbound.join_next().await.is_some() {}
    shutdown.trigger();
    let _ = destination.close().await;
    runtime.shutdown_and_wait().await;
    let _ = drainer.await;
    receivers.shutdown().await;
    output.emit(json!({"event":"stopped"})).await;
    drop(output);
    let _ = writer.await;
    Ok(())
}

// Empty replacement channels keep RegisteredDestination's close ownership intact.
/// LXMF delivery announce data: msgpack `[display name as bin, no stamp cost]`.
fn announce_options(label: &str) -> DestinationAnnounceOptions {
    let name = label.as_bytes();
    let mut app_data = vec![0x92, 0xc4, name.len() as u8];
    app_data.extend_from_slice(name);
    app_data.push(0xc0);
    DestinationAnnounceOptions {
        app_data: Some(app_data),
        ..DestinationAnnounceOptions::default()
    }
}

fn empty_events() -> DestinationEvents {
    DestinationEvents {
        packets: mpsc::channel(1).1,
        links_established: mpsc::channel(1).1,
        links_identified: mpsc::channel(1).1,
        link_packets: mpsc::unbounded_channel().1,
        link_packet_proofs: mpsc::channel(1).1,
        resource_completions: mpsc::channel(1).1,
        resource_proofs: mpsc::channel(1).1,
        resource_events: mpsc::channel(1).1,
        channel_messages: mpsc::channel(1).1,
        links_closed: mpsc::channel(1).1,
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .init();
    let result: Result<()> = async {
        let args = args()?;
        let identity = identity(&args.identity, args.create)?;
        if args.command == "identity" {
            println!("{}", json!({"address":hex::encode(address(&identity)),"public_key":hex::encode(identity.get_public_key())}));
            Ok(())
        } else { serve(args, identity).await }
    }.await;
    if let Err(error) = result {
        eprintln!("agent-rat-native: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-rat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        dir
    }

    #[test]
    fn verified_attachments_are_saved_under_safe_names() {
        let sender = Identity::new();
        let me = Identity::new();
        let pdf = vec![b'%'; 300_000];
        let mut draft = LxMessageDraft::new_with_limits(address(&me), address(&sender), "", "Here", DeliveryMethod::Direct, limits()).unwrap();
        draft.set_file_attachment_field("../../etc/Mount Everest.pdf", &pdf).unwrap();
        draft.set_image_field("image/jpeg", b"jpeg bytes").unwrap();
        let wire = draft.sign(&sender.get_signing_key().unwrap()).unwrap().finalize(None).unwrap().pack().unwrap();
        assert!(wire.len() > MAX_LINE);
        let message = decode_message(wire, address(&me)).unwrap();
        let files = scratch("in");
        let saved = save_attachments(Some(&files), &message);
        let folder = files.join("incoming").join(hex::encode(message.message_id()));
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0]["name"], "image.jpg");
        assert_eq!(saved[1]["name"], "Mount Everest.pdf");
        assert_eq!(std::fs::read(folder.join("Mount Everest.pdf")).unwrap(), pdf);
        assert_eq!(save_attachments(None, &message)[0]["dropped"], true);
        assert_eq!(safe_name(" ..hidden", "file"), "hidden");
        assert_eq!(safe_name("ä/", "file"), "file");
        let _ = std::fs::remove_dir_all(files);
    }

    #[test]
    fn only_outbox_files_can_be_sent() {
        let files = scratch("out");
        std::fs::write(files.join("outbox/article.pdf"), b"pdf").unwrap();
        std::fs::write(files.join("identity"), b"secret").unwrap();
        assert_eq!(outbox_file(Some(&files), &files.join("outbox/article.pdf")).unwrap(), b"pdf");
        assert!(outbox_file(Some(&files), &files.join("identity")).is_err());
        assert!(outbox_file(Some(&files), &files.join("outbox/../identity")).is_err());
        assert!(outbox_file(None, &files.join("outbox/article.pdf")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(files.join("identity"), files.join("outbox/link.pdf")).unwrap();
            assert!(outbox_file(Some(&files), &files.join("outbox/link.pdf")).is_err());
        }
        let _ = std::fs::remove_dir_all(files);
    }
    #[test]
    fn key_and_signed_source_binding() {
        let sender = Identity::new();
        let recipient = Identity::new();
        let attacker = Identity::new();
        let message = LxMessageDraft::new(
            address(&recipient),
            address(&sender),
            "",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&sender.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        assert!(message.verify(&sender.get_public_key()).is_ok());
        assert!(message.verify(&attacker.get_public_key()).is_err());
        assert!(bound_key(address(&sender), attacker.get_public_key()).is_err());
        assert!(decode_message(message.pack().unwrap(), address(&attacker)).is_err());
        let forged = LxMessageDraft::new(
            address(&recipient),
            address(&sender),
            "",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&attacker.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        assert!(forged.verify(&attacker.get_public_key()).is_err());
    }
    #[test]
    fn unclaimed_agents_admit_setup_codes_in_any_form() {
        let (me, sender) = (Identity::new(), Identity::new());
        let later = InboundAccess { senders: HashSet::new(), mode: InboxMode::Later };
        let says = |text: &str| {
            let message = LxMessageDraft::new(address(&me), address(&sender), "", text, DeliveryMethod::Direct)
                .unwrap()
                .sign(&sender.get_signing_key().unwrap())
                .unwrap()
                .finalize(None)
                .unwrap();
            later.permits(&message)
        };
        for text in ["6J8E4K", "6j8 e4k", " XC3   K39 ", "xc3k39"] {
            assert!(says(text), "{text}");
        }
        for text in [
            "/setup 6J8E4K", "/setup6J8E4K", "/setup", "6J8E4", "6J8E 4K", "6J8 E4K 9", "6J8E4K hi", "0J8E4K",
            "IJ8E4K", "lJ8E4K", "OJ8E4K", "6J8E4ÜK", "hello", "",
        ] {
            assert!(!says(text), "{text}");
        }
    }
    #[tokio::test]
    async fn full_inbound_queue_stays_open_and_resumes() {
        let me = Identity::new();
        let sender = Identity::new();
        let recipient = address(&me);
        let msg = LxMessageDraft::new(
            recipient,
            address(&sender),
            "",
            "6J8E4K",
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&sender.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        let wire = msg.pack().unwrap();
        let contact = Identity::new();
        let note = LxMessageDraft::new(
            recipient,
            address(&contact),
            "",
            "Hi",
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&contact.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        let claim_only = InboundAccess {
            senders: HashSet::new(),
            mode: InboxMode::Later,
        };
        assert!(claim_only.permits(&msg));
        assert!(!claim_only.permits(&note));
        let mut allowed = InboundAccess {
            senders: [address(&contact)].into(),
            mode: InboxMode::Owners,
        };
        assert!(!allowed.permits(&msg));
        assert!(allowed.permits(&note));
        allowed.mode = InboxMode::Anyone;

        let (packets, rx) = mpsc::unbounded_channel();
        let mut events = empty_events();
        events.link_packets = rx;
        // Keep the other channels alive so the drainer only exits on shutdown.
        let (raw, events_raw) = mpsc::channel(1);
        events.packets = events_raw;
        let (resources, events_resources) = mpsc::channel(1);
        events.resource_completions = events_resources;
        let (tx, mut received) = mpsc::channel(1);
        tx.try_send(msg).unwrap();
        let (contacts, mut reserved) = mpsc::channel(1);
        let shutdown = ShutdownSignal::new();
        let task = tokio::spawn(drain_events(
            events,
            recipient,
            tx,
            contacts,
            Arc::new(RwLock::new(allowed)),
            shutdown.clone(),
        ));
        packets.send((wire.clone(), [0; 16])).unwrap();
        packets.send((note.pack().unwrap(), [0; 16])).unwrap();
        assert!(
            timeout(Duration::from_secs(2), reserved.recv())
                .await
                .unwrap()
                .is_some()
        );
        assert!(received.try_recv().is_ok());
        packets.send((wire, [0; 16])).unwrap();
        assert!(
            timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .is_some()
        );
        shutdown.trigger();
        task.await.unwrap();
        drop((raw, resources));
    }

    #[test]
    fn malformed_and_bounded_requests() {
        assert!(parse_request(br#"{"id":1,"op":"status"}"#).is_ok());
        assert!(parse_request(br#"{"id":1,"op":"status","extra":1}"#).is_err());
        assert!(parse_request(b"\xff").is_err());
        let mut bytes = vec![b'x'; MAX_LINE + 1];
        bytes.extend_from_slice(b"\n{}\n");
        let mut reader = std::io::Cursor::new(bytes);
        assert!(read_line_bounded(&mut reader).is_err());
        assert_eq!(read_line_bounded(&mut reader).unwrap().unwrap(), b"{}\n");
    }

    #[test]
    fn handshake_key_is_only_a_source_bound_candidate() {
        let sender = Identity::new();
        let recipient = Identity::new();
        let attacker = Identity::new();
        let payload = json!({"protocol":"agent-rat","version":1,"type":"hello","sender_key":hex::encode(sender.get_public_key())}).to_string();
        let message = LxMessageDraft::new(
            address(&recipient),
            address(&sender),
            "",
            &payload,
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&sender.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        let candidate = embedded_key(&message).unwrap();
        assert!(message.verify(&candidate).is_ok());
        let forged = LxMessageDraft::new(
            address(&recipient),
            address(&sender),
            "",
            &payload,
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&attacker.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        assert!(forged.verify(&embedded_key(&forged).unwrap()).is_err());
        let unrelated = payload.replace("agent-rat", "other-protocol");
        let message = LxMessageDraft::new(
            address(&recipient),
            address(&sender),
            "",
            &unrelated,
            DeliveryMethod::Direct,
        )
        .unwrap()
        .sign(&sender.get_signing_key().unwrap())
        .unwrap()
        .finalize(None)
        .unwrap();
        assert!(embedded_key(&message).is_none());
    }
}

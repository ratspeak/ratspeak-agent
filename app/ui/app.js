"use strict";
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const view = document.getElementById("view"), foot = document.getElementById("foot");
let pendingDeviceChanges = 0;
let hasUnsavedChanges = () => false;
async function changeDevice(command, args) {
  pendingDeviceChanges++;
  try { return await invoke(command, args); }
  finally { pendingDeviceChanges--; }
}
const ADDRESS = /[0-9a-f]{32}/gi;
const HUB = /^[A-Za-z0-9.-]{1,253}:\d{1,5}$/;
const PI_NAMES = Object.entries({
  Giraffe: ["Leaping", "Grazing", "Strolling", "Reaching"],
  Toucan: ["Smiling", "Perching", "Flying", "Hopping"],
  Otter: ["Floating", "Diving", "Swimming", "Splashing"],
  Fox: ["Leaping", "Trotting", "Pouncing", "Stretching"],
  Rabbit: ["Hopping", "Nibbling", "Napping", "Leaping"],
  Panda: ["Nibbling", "Climbing", "Napping", "Stretching"],
  Penguin: ["Waddling", "Diving", "Swimming", "Sliding"],
  Dolphin: ["Leaping", "Diving", "Swimming", "Splashing"],
  Owl: ["Gliding", "Perching", "Blinking", "Swooping"],
  Deer: ["Leaping", "Grazing", "Wandering", "Trotting"],
  Raccoon: ["Climbing", "Wandering", "Peeking", "Napping"],
  Squirrel: ["Climbing", "Scampering", "Nibbling", "Peeking"],
  Duck: ["Paddling", "Waddling", "Floating", "Splashing"],
  Eagle: ["Soaring", "Gliding", "Perching", "Swooping"],
  Tiger: ["Pouncing", "Stretching", "Swimming", "Napping"],
  Zebra: ["Galloping", "Grazing", "Trotting", "Strolling"],
  Seal: ["Diving", "Swimming", "Napping", "Floating"],
  Koala: ["Climbing", "Napping", "Reaching", "Blinking"]
}).flatMap(([animal, verbs]) => verbs.map(verb => `${verb} ${animal}`));
const MODEL_DESCRIPTIONS = {
  "qwen3.5-4b": "The largest model in this collection.",
  "qwen3.5-2b": "Handles everyday questions and instructions.",
  "lfm2.5-350m": "Quick replies for simple questions.",
  "lfm2.5-230m": "Fastest replies and lowest memory use.",
  "lfm2.5-1.2b": "Answers questions about text you give it.",
  "qwen3.5-0.8b": "Keeps more of your conversation in context.",
  "smollm2-360m": "For short exchanges with the least memory."
};
// Only the boards supported by the installer. Single-capacity boards fill in RAM automatically.
const BOARDS = {
  pi5: { name: "Raspberry Pi 5", generation: 3, memory: [1, 2, 4, 8, 16] },
  pi4: { name: "Raspberry Pi 4", generation: 2, memory: [1, 2, 3, 4, 8] },
  pi3b: { name: "Raspberry Pi 3 B / B+", generation: 1, memory: [1] },
  pi3a: { name: "Raspberry Pi 3 A+", generation: 1, memory: [0.5] },
  zero2: { name: "Raspberry Pi Zero 2 W", generation: 1, memory: [0.5] }
};
const AUTO_MODEL = "Choose on first boot";
const ramLabel = gb => gb === 0.5 ? "512 MB" : `${gb} GB`;
const PHASES = [["offline", "Downloading the model and tools"], ["download", "Downloading Raspberry Pi OS"], ["unpack", "Unpacking"], ["files", "Adding your settings"],
  ["write", "Writing the card"], ["verify", "Checking the card"]];
const OUT = { windows: "You can take it out. If Windows offers to format it, choose Cancel.", other: "Eject it, then take it out." };
// Ratspeak's public hubs, shown by name. A hub lets the Pi be reached from outside its own network.
const PUBLIC = [["Ratspeak Ruby", "1.ratspeak.org:4141"], ["Ratspeak Emerald", "2.ratspeak.org:4242"],
  ["Ratspeak Diamond", "3.ratspeak.org:4343"]];
// Diamond is the quieter hub: agents don't run transport and announce sparingly.
const DEFAULT_HUB = PUBLIC[2][1];
// Offline libraries from the bundled catalog: {id, label, command, example, size_mb}.
let LIBRARIES = [];
const librariesRequest = invoke("library_catalog").then(list => { LIBRARIES = list; }).catch(() => {});
const EXTRA_IDS = () => LIBRARIES.map(l => l.id);
const extraName = id => {
  const library = LIBRARIES.find(l => l.id === id);
  return (library?.kind === "translation" ? library.label + " translation" : library?.label) || id;
};
const OTHER = "\u0001other";     // the "Other network…" row; no real network can have this name
const WAITING = `<div class="empty waiting" id="waiting"><svg class="card-icon" viewBox="0 0 32 40" fill="none" aria-hidden="true">
  <path d="M8 2h18a4 4 0 0 1 4 4v28a4 4 0 0 1-4 4H6a4 4 0 0 1-4-4V10Z" stroke="currentColor" stroke-width="2"/>
  <path d="M8 2v9H2M12 3v8m5-8v8m5-8v8" stroke="currentColor" stroke-width="2"/>
  <rect x="9" y="21" width="14" height="9" rx="2" fill="currentColor" opacity=".14"/></svg>
  <p class="waiting-text" role="status">Waiting for SD card<span class="waiting-dots" aria-hidden="true"><span>.</span><span>.</span><span>.</span></span></p></div>`;

let lastSuggestedName = "";
let cards = [], card = null, step = 0, timer = null, form = fresh(), system = "", scan = null, windowHeight = null;
let host = "", desktop = null, connectingHermes = false, verifiedHermes = null, hermesLocation = null;
let models = [], modelsRequest = null, countryRequest = null, scanRequest = 0;

function suggestName(currentName = "") {
  const available = PI_NAMES.filter(name => name !== currentName.trim() && name !== lastSuggestedName);
  lastSuggestedName = available[Math.floor(Math.random() * available.length)];
  return lastSuggestedName;
}
function fresh() {
  return { name: suggestName(), provider: "local", veniceKey: "", veniceVerified: false, veniceModels: [], veniceModel: "", board: "", ram: "", model: null, modelOptions: [], public: [DEFAULT_HUB],
           hubs: [], features: { wikipedia: false, voice: true }, cable: false, offline: false, elsewhere: false, piDownloads: false, wifi: freshWifi() };
}
const automaticModel = () => host === "pi" && (form.board === "auto" || form.ram === "auto");
// The card carries the model and tools, so the Pi only unpacks them. A Pi that picks its own model downloads it.
const carries = () => host === "pi" && form.provider === "local" && (form.offline || !form.piDownloads && !automaticModel());
function freshWifi() {
  let country = "";
  try { country = new Intl.Locale(navigator.language).region || ""; } catch {}
  return { net: "", ssid: "", password: "", country, connected: false, busy: false };
}

const mbLabel = (mb, decimals = 2) => mb >= 1000 ? `${(mb / 1000).toFixed(decimals)} GB` : `${mb} MB`;
const modelRam = model => `~${mbLabel(model.size_mb + (model.on_gpu ? model.gpu_overhead_mb : model.overhead_mb))}`;
// Desktop models sized for a graphics card run on it; the rest run in RAM.
const modelPlace = model => model.on_gpu ? "On GPU" : "In RAM";
function loadModels() {
  if (!modelsRequest) modelsRequest = invoke("model_catalog").then(catalog => {
    if (!catalog.rungs?.length) throw new Error("Empty model catalog");
    models = catalog.rungs;
    return catalog;
  }).catch(error => { modelsRequest = null; throw error; });
  return modelsRequest;
}
function modelChoices(catalog) {
  // The board reserves some nominal RAM for firmware and the kernel. First boot uses actual MemTotal.
  const usable = Number(form.ram) * 1024 * 0.9;
  const generation = BOARDS[form.board].generation;
  const eligible = catalog.rungs.filter(m => (m.min_generation || 1) <= generation
    && m.size_mb + m.overhead_mb + catalog.os_mb <= usable);
  const byId = new Map(eligible.map(m => [m.id, m]));
  const menu = catalog.menus.find(m => (m.min_generation || 1) <= generation && byId.has(m.models[0]));
  return menu ? menu.models.map(id => byId.get(id)).filter(Boolean) : [];
}
function offlineRequirements(libraries) {
  return invoke("offline_requirements", { model: form.model, libraries });
}
function offlineCapacityMessage(required) {
  return `This setup needs ${Math.ceil(required / 1e9)} GB of SD card space. Choose a smaller model, use a network, or select a larger card.`;
}
function piDescription() {
  const board = BOARDS[form.board];
  if (!board) return "Board unknown";
  return Number(form.ram) ? `${board.name} · ${ramLabel(Number(form.ram))} RAM` : board.name;
}
function loadCountry(state) {
  if (!countryRequest) countryRequest = invoke("wifi_country").catch(() => null);
  countryRequest.then((country) => {
    const code = typeof country === "string" ? country.trim().toUpperCase() : "";
    if (/^[A-Z]{2}$/.test(code)) state.country = code;
  });
}

const esc = (text) => String(text ?? "").replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const size = (bytes) => `${Math.round(bytes / 1e9)}\u00a0GB`;
const addresses = (text) => [...new Set((text.match(ADDRESS) || []).map((a) => a.toLowerCase()))];
const lines = (text) => text.split(/[\s,]+/).map((l) => l.trim()).filter(Boolean);
const $ = (id) => document.getElementById(id);
const takeOut = (ejected) => ejected ? "You can take it out." : OUT[system] || OUT.other;
const hubName = (hub) => (PUBLIC.find(([, address]) => address === hub) || [hub])[0];

function sizeWindow(height) {
  const frame = Math.max(0, window.outerHeight - window.innerHeight);
  const available = window.screen.availHeight - frame;
  if (available > 0) height = Math.min(height, available);
  if (windowHeight === height) return;
  windowHeight = height;
  const api = window.__TAURI__.window;
  if (!api) return;
  const app = api.getCurrentWindow();
  app.setMinSize(new api.LogicalSize(480, Math.min(height, 480)))
    .then(() => { if (windowHeight === height) return app.setSize(new api.LogicalSize(Math.max(480, window.innerWidth), height)); })
    .catch(console.error);
}

/** Draw a screen with its actions, card context, and preferred window height. */
function show(body, actions = "", context = false, height = 600) {
  clearInterval(timer);
  hasUnsavedChanges = () => false;
  const moveFocus = view.contains(document.activeElement) || foot.contains(document.activeElement);
  view.oninput = view.onchange = null;
  view.innerHTML = body + (actions ? "" : '<p class="error" id="error" role="alert"></p>');
  foot.innerHTML = actions ? `<p class="error" id="error" role="alert"></p>${actions}` : "";
  $("context").textContent = context && host !== "desktop" && card ? `${card.name}, ${size(card.size)}` : "";
  $("context").title = $("context").textContent;
  view.scrollTop = 0;
  sizeWindow(height);
  if (moveFocus) focusHeading();
}
function focusHeading() {
  const title = view.querySelector("h1");
  if (title) { title.tabIndex = -1; title.focus({preventScroll: true}); }
}
// Only temporary action panels light-dismiss. Inline details such as Network
// connection and Other hubs stay expanded while the user works elsewhere.
let pointerNavigation = false;
function dismissActionPanels(event) {
  if (event.type === "pointerdown") pointerNavigation = true;
  if (event.type === "keydown") pointerNavigation = false;
  if (event.type === "focusin" && pointerNavigation) return;
  const escape = event.type === "keydown";
  if (escape && (event.key !== "Escape" || document.querySelector("dialog[open], :is(:popover-open)"))) return;
  const updates = $("updates"), updateButton = $("check-update");
  const panels = view.querySelectorAll(".agent-switch[open]");
  function dismiss(panel, trigger, close) {
    if (event.type !== "blur" && !escape && (panel.contains(event.target) || trigger.contains(event.target))) return;
    close();
    if (escape) { trigger.focus(); event.preventDefault(); }
  }
  for (const panel of panels) dismiss(panel, panel.querySelector("summary"), () => { panel.open = false; });
  if (!updates.hidden) dismiss(updates, updateButton, () => {
    updates.hidden = true; updateButton.setAttribute("aria-expanded", "false");
  });
}
for (const type of ["pointerdown", "focusin", "keydown"]) document.addEventListener(type, dismissActionPanels);
window.addEventListener("blur", dismissActionPanels);
const fail = (text, id) => {
  $("error").textContent = text;
  document.querySelectorAll("[aria-invalid]").forEach((el) => el.removeAttribute("aria-invalid"));
  if (id && $(id)) { $(id).setAttribute("aria-invalid", "true"); $(id).focus(); }
};

const field = (id, label, input) => `<div class="field-block"><label class="field" for="${id}">${label}</label>${input}</div>`;
const OR_DIVIDER = '<div class="or-divider" aria-hidden="true">or</div>';
const option = (id, title, hint, checked, more = "") => `<label class="opt"><input type="checkbox" id="${id}" ${checked ? "checked" : ""} ${more}>
  <span>${title}</span>${hint ? `<span class="hint">${hint}</span>` : ""}</label>`;
const hubBoxes = (on) => PUBLIC.map(([name, address], i) => option(`public${i}`, name, "", on.includes(address))).join("");
const connectionHelp = `<button type="button" class="connection-info" aria-label="About public hubs" popovertarget="connection-help">
  <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="7.5"/><path d="M10 9v5"/><circle class="info-dot" cx="10" cy="6.3" r="1"/></svg></button>`;
const connectionChoices = (hubs, custom, cloud = false) => `<div id="connection-help" class="connection-help" popover="auto" role="region" aria-label="About public hubs">
    <p><b>On the same network</b>Message your agent over Wi-Fi or Ethernet. ${cloud ? "Venice needs internet to generate replies." : "Local models can reply without internet."}</p>
    <p><b>Over the internet</b>Ratspeak hosts three optional public hubs. One is enough when you and your agent are online and the hub is reachable.</p>
    <p><b>Your messages stay private</b>Prompts and replies are encrypted end to end. Ratspeak and other hub operators can’t read them.</p>
    <p>You can also add a hub you host yourself, including a private one.</p></div>
  <div class="group">${option("local", "Local network", "Always on", true, "disabled")}</div>
  <div class="hub-heading"><h3>Public hubs</h3>${connectionHelp}</div><div class="group">${hubBoxes(hubs)}</div>
  <details ${custom.length ? "open" : ""}><summary>Other hubs</summary>${hubEditor()}</details>`;
function wireConnectionHelp() {
  const help = $("connection-help");
  const trigger = view.querySelector("[popovertarget=connection-help]");
  help.addEventListener("beforetoggle", event => {
    if (event.newState !== "open") return;
    const rect = trigger.getBoundingClientRect();
    help.style.left = Math.max(12, Math.min(rect.left, innerWidth - 332)) + "px";
    help.style.top = rect.bottom + 4 + "px";
    help.style.maxHeight = Math.max(96, innerHeight - rect.bottom - 16) + "px";
  });
}
const selectedPublicHubs = () => PUBLIC.filter((_, i) => $(`public${i}`).checked).map(([, address]) => address);
// One list for setup, a Pi card, and a desktop agent. `notes` replaces a library's hint, e.g. its download progress.
const spoken = l => l.kind === "translation";
const heard = l => l.kind === "voice";
// Desktops transcribe with Whisper base; Pis with 1 GB use Vosk and larger Pis Whisper tiny (ratspeak_pi/voice.py).
const piRam = () => host === "pi" ? Number(form.ram) || 0 : 0;
const voiceMB = l => host === "desktop" ? l.sizes.desktop : piRam() && piRam() <= 1 ? l.sizes.pi_small : l.sizes.pi;
const voiceOff = () => piRam() > 0 && piRam() < 1;
const extraHint = l => heard(l) ? (voiceOff() ? "Needs 1 GB of RAM" : `English speech to text · ${mbLabel(voiceMB(l), 1)}`)
  : spoken(l) ? mbLabel(l.size_mb, 1) : `${l.command} · ${mbLabel(l.size_mb, 1)}`;
const extraOn = (l, features) => !!(features[l.id] ?? heard(l));   // voice is on by default, as on the Pi
const extraOption = (l, features, notes) => heard(l) && voiceOff() ? option(l.id, esc(l.label), extraHint(l), false, "disabled")
  : option(l.id, esc(l.label), esc(notes[l.id] || extraHint(l)), extraOn(l, features));
const extraChoices = (features, notes = {}, { translations = true } = {}) => {
  const langs = LIBRARIES.filter(spoken);
  const anyLang = langs.some(l => features[l.id] || notes[l.id]);
  const langGroup = langs.length && (translations || anyLang) ? `<details class="extra-translations"${anyLang ? " open" : ""}>
    <summary id="translate-heading">Offline translation (${langs.length} languages)</summary>
    <div class="group compact languages" role="group" aria-labelledby="translate-heading">${langs.map(l => extraOption(l, features, notes)).join("")}</div>
  </details>` : "";
  return `<div class="group compact">${LIBRARIES.filter(l => !spoken(l)).map(l => extraOption(l, features, notes)).join("")}</div>${langGroup}
    <p class="hint" id="extras-size" aria-live="polite"></p>`;
};
const chosenLibraries = () => LIBRARIES.filter(l => $(l.id)?.checked);
// Translation and Whisper share one engine download, counted once; Vosk does not use it.
const usesEngine = l => spoken(l) || (heard(l) && !(piRam() && piRam() <= 1));
const engineMB = () => { const l = chosenLibraries().find(usesEngine); return l ? (host === "desktop" ? l.engine_mb_desktop : l.engine_mb) : 0; };
const extrasMB = () => chosenLibraries().reduce((sum, l) => sum + (heard(l) ? voiceMB(l) : l.size_mb), 0) + engineMB();
function showExtrasSize() {
  const mb = extrasMB(), engine = engineMB();
  $("extras-size").textContent = !mb ? "" : engine ?
    `${mbLabel(mb, 1)} to download, including the ${mbLabel(engine, 1)} engine for speech and translation.` : `${mbLabel(mb, 1)} to download.`;
}
const hubEditor = () => `<div class="hub-editor access-fields" id="hub-editor"><div id="hub-rows"></div>
  <button type="button" id="add-hub" class="quiet add-address"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.5v11M2.5 8h11"/></svg>Add hub</button></div>`;
const customHubValues = () => [...$("hub-rows").querySelectorAll("input")].map(input => input.value.trim());
function hubProblem(hubs) {
  const index = hubs.findIndex(hub => hub && (!HUB.test(hub) || +hub.split(":")[1] < 1 || +hub.split(":")[1] > 65535));
  if (index >= 0) return ["Enter a hub as host:port, with a port from 1 to 65535.", `hub-${index}`];
}
function wireHubs(initial, changed = () => {}) {
  const rows = $("hub-rows");
  const draw = values => {
    rows.innerHTML = values.map((value, i) => `<div class="access-address-row"><label class="field" for="hub-${i}">Hub ${i + 1}:</label>
      <input type="text" id="hub-${i}" class="mono" placeholder="host:port" value="${esc(value)}" spellcheck="false" autocomplete="off">
      <button type="button" class="quiet remove-address" data-remove-hub="${i}" aria-label="Remove hub ${i + 1}">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4 4 8 8m0-8-8 8"/></svg></button></div>`).join("");
  };
  draw(initial);
  $("add-hub").onclick = () => {
    const values = customHubValues();
    const blank = values.indexOf("");
    if (blank >= 0) return $(`hub-${blank}`).focus();
    draw([...values, ""]);
    $(`hub-${values.length}`).focus();
  };
  rows.onclick = event => {
    const button = event.target.closest("[data-remove-hub]");
    if (!button) return;
    const index = Number(button.dataset.removeHub), values = customHubValues();
    values.splice(index, 1);
    draw(values);
    ($(`hub-${Math.min(index, values.length - 1)}`) || $("add-hub")).focus();
    changed();
  };
}

// ---- App version and user-requested update checks ------------------------------------------------------------------

function updateControls() {
  const button = $("check-update"), panel = $("updates"), message = $("update-message"), release = $("release-link");
  const install = $("install-update");
  let updating = false;
  const reveal = () => { panel.hidden = false; button.setAttribute("aria-expanded", "true"); };
  const close = (focus = false) => {
    panel.hidden = true; button.setAttribute("aria-expanded", "false");
    if (focus && !button.disabled) button.focus();
  };
  const openProject = async (releases) => {
    try { await invoke("open_project", { releases }); }
    catch (error) { message.textContent = String(error); reveal(); }
  };
  $("close-update").onclick = () => close(true);
  release.onclick = () => openProject(true);
  listen("app-update-installing", () => { if (updating) message.textContent = "Installing update…"; });
  install.onclick = async () => {
    if (operationInProgress()) { message.textContent = "Finish the current operation before updating."; return; }
    if (hasUnsavedChanges()) { message.textContent = "Save your changes before updating."; return; }
    updating = true;
    view.inert = foot.inert = install.disabled = release.disabled = true;
    button.setAttribute("aria-busy", "true");
    button.setAttribute("aria-label", "Updating Ratspeak Agent");
    message.textContent = "Downloading update…";
    try {
      await invoke("install_update");
      message.textContent = "Restarting Ratspeak Agent…";
    } catch (error) {
      message.textContent = String(error);
      updating = false;
      view.inert = foot.inert = install.disabled = release.disabled = false;
      button.setAttribute("aria-label", "Check for updates");
      button.removeAttribute("aria-busy");
      if (!panel.hidden) install.focus();
    }
  };
  invoke("app_info").then((info) => {
    const version = `v${info.version}`;
    $("version").innerHTML = info.repository ? `<button id="project" title="View on GitHub">${esc(version)}</button>` : esc(version);
    if ($("project")) $("project").onclick = () => openProject(false);
  }).catch(() => { $("version").textContent = "—"; $("version").title = "Version unavailable"; })
    .finally(() => { button.disabled = false; });
  button.onclick = async () => {
    if (button.getAttribute("aria-busy") === "true") { if (panel.hidden) reveal(); else close(true); return; }
    if (!panel.hidden) { close(true); return; }
    button.setAttribute("aria-label", "Checking for updates"); button.setAttribute("aria-busy", "true");
    message.textContent = "Checking for updates…"; install.hidden = release.hidden = true; reveal();
    try {
      const result = await invoke("check_for_updates");
      const states = { unconfigured: "Update checks aren’t available yet.", unpublished: "No public release is available yet.", current: "Up to date." };
      if (result.state === "available") {
        message.textContent = `v${result.version} is available.`;
        install.hidden = !result.installable; release.hidden = false;
      }
      else message.textContent = states[result.state] || "Could not check for updates.";
    } catch (error) { message.textContent = String(error); }
    finally {
      button.setAttribute("aria-label", "Check for updates"); button.removeAttribute("aria-busy");
    }
  };
}

// ---- Home ----------------------------------------------------------------------------------------------------------

function hostArt(id) {
  return `<svg viewBox="0 0 100 68" class="host-art" aria-hidden="true" focusable="false">
    <use href="host-devices.svg#${id}"/></svg>`;
}

function hostHome() {
  card = null; host = ""; desktop = null; connectingHermes = false; verifiedHermes = null; hermesLocation = null; form = fresh(); step = 0;
  const tiles = [
    ["desktop", "Desktop Agent", "Run an agent on this computer."],
    ["pi", "Raspberry Pi Agent", "Prepare an SD\u00a0card for your Pi."],
    ["hermes", "Connect Hermes", "Message your existing Hermes agent."],
  ];
  show(`<h1 class="host-heading">Where will your agent run?</h1>
    <div class="host-grid">${tiles.map(([id, name, hint]) => `<button type="button" class="host-tile" id="host-${id}">
      ${hostArt(id)}<strong>${name}${id === "hermes" ? HERMES_TILE_LOCK : ""}</strong><span>${hint}</span></button>`).join("")}</div>`, creatingAgent && creatorReturn ? '<button id="return-agents">Cancel</button>' : "", false, creatingAgent ? 370 : 320);
  if ($("return-agents")) $("return-agents").onclick=()=>openAgent(creatorReturn,"agents");
  $("host-pi").onclick = home;
  $("host-desktop").onclick = () => desktopHome();
  $("host-hermes").onclick = hermesGate;
  const lock = $("host-hermes").querySelector(".host-lock");
  invoke("beta_unlocked").then(unlocked => { if (lock.isConnected) lock.hidden = unlocked === true; }).catch(() => {});
}

const HERMES_TILE_LOCK = '<span class="host-lock"><svg class="a2a-tab-lock" viewBox="0 0 16 18" aria-hidden="true"><rect x="3" y="8" width="10" height="8" rx="2"/><path d="M5 8V5a3 3 0 0 1 6 0v3M8 11v2"/></svg><span class="sr">, locked</span></span>';

// Sizes the window to a screen's content. `grow` keeps it from shrinking under the cursor as an error clears.
function fitWindow(section, {grow = false} = {}) {
  let tallest = 0;
  const resize = new ResizeObserver(() => {
    if (!section.isConnected) { resize.disconnect(); return; }
    const style = getComputedStyle(view);
    const height = Math.ceil(view.offsetTop + parseFloat(style.paddingTop) + section.offsetHeight + parseFloat(style.paddingBottom) + foot.offsetHeight);
    tallest = grow ? Math.max(tallest, height) : height;
    sizeWindow(tallest);
  });
  resize.observe(section);
  resize.observe(foot);
}

// Connect Hermes is in private beta, behind the same password as A2A.
async function hermesGate() {
  let unlocked = false;
  try { unlocked = await invoke("beta_unlocked") === true; } catch { /* Locked is the safe default. */ }
  if (unlocked) return desktopHome(true);
  show(contactsBetaPane({title:"Connect Hermes is locked", art:HERMES_BETA_ART, heading:"h1"}), '<button id="back">Back</button>', true, 480);
  $("back").onclick = hostHome;
  focusHeading();
  fitWindow(view.querySelector(".contacts-beta"), {grow:true});
  wireContactsBeta(view, async password => {
    await changeDevice("beta_unlock", {password});
    desktopHome(true);
    focusHeading();
    $("announcement").textContent = "Connect Hermes and A2A are unlocked on this device.";
  });
}

async function home() {
  host = "pi"; card = null; form = fresh();
  show(`<h1>Select an SD card:</h1><div class="group" id="cards">${WAITING}</div><div id="pending-pairings"></div>`, '<button id="back">Back</button>', false, 344);
  $("back").onclick = hostHome;
  const list = $("cards");
  const pending = $("pending-pairings");
  invoke("pending_pi_pairings").then(rows => {
    if (!pending.isConnected || !rows?.length) return;
    for (const {name,address} of rows) {
      const button = document.createElement("button");
      button.className = "quiet"; button.textContent = `Resume pairing · ${name}`;
      button.onclick = async () => {
        try {
          const saved = await invoke("read_pi_pairing",{address});
          if (pending.isConnected) finished(saved, true);
        } catch(error) { if (pending.isConnected) fail(String(error)); }
      };
      pending.append(button);
    }
    sizeWindow(344 + pending.offsetHeight);
  }).catch(() => {});
  let previousRows = WAITING;
  const updateList = html => {
    if (html === previousRows) return;
    previousRows = html;
    const focused = list.contains(document.activeElement) ? document.activeElement.dataset : null;
    list.innerHTML = html;
    if (focused?.media) {
      const same = [...list.querySelectorAll("button")].find(b =>
        b.dataset.media === focused.media && b.dataset.device === focused.device);
      if (same && !same.disabled) same.focus({preventScroll:true}); else focusHeading();
    }
    // Open up to four rows. Additional cards scroll within the picker.
    const visibleRows = [...list.querySelectorAll(".cardrow")].slice(0,4);
    const rowsHeight = visibleRows.reduce((sum, row) => sum + row.offsetHeight, 0);
    sizeWindow(344 + Math.max(0, rowsHeight - 128) + pending.offsetHeight);
  };
  const refresh = async () => {
    try {
      const found = await invoke("list_cards");
      if (!list.isConnected) return;
      cards = found.filter(c => !c.refused);
    } catch (error) { if (list.isConnected) updateList(`<div class="empty">${esc(error)}</div>`); return; }
    if (!cards.length) {
      // Keep the waiting element so each card scan preserves the dot animation's phase.
      updateList(WAITING);
      return;
    }
    const rows = cards.map((c, i) => {
      const mine = c.ratspeak;
      const what = !c.media_id ? "This reader can’t identify the card safely. Try another reader." : mine ? `Ratspeak Pi${mine.name ? `: ${esc(mine.name)}` : ""}` : c.volumes.length ? `Holds: ${esc(c.volumes.join(", "))}` : "Empty";
      return `<div class="cardrow"><div><b>${esc(c.name)}, ${size(c.size)}</b><span class="small soft">${what}</span></div>
        <button class="primary" data-card="${i}" data-media="${esc(c.media_id)}" data-device="${esc(c.device)}" ${!c.media_id ? "disabled" : ""}>${mine ? "Open" : "Set up"}</button></div>`;
    });
    updateList(rows.join(""));
  };
  await refresh();
  if (list.isConnected) timer = setInterval(refresh, 3000);
}

view.addEventListener("click", async (event) => {
  const index = event.target.dataset && event.target.dataset.card;
  if (index === undefined || !cards[Number(index)] || cards[Number(index)].refused) return;
  card = cards[Number(index)];
  if (!card.ratspeak) { form = fresh(); step = 0; wizard(); return; }
  const selected=card, button=event.target;
  if (!selected.ratspeak.link) return unmatchedCard(selected);
  button.disabled=true;
  try {
    const record=await invoke("remember_sd_agent",{device:selected.device,selectedMedia:selected.media_id,link:selected.ratspeak.link});
    if (!button.isConnected || card!==selected) return;
    // Its own screen: Settings, where its card is in use, or Overview's next steps when its install stopped.
    host = "pi";
    finished(record, true, selected.ratspeak.state === "failed" && selected.ratspeak.installed === false ? "overview" : "settings");
  } catch(e) { if (button.isConnected) { button.disabled=false;fail(String(e)); } }
});

/** A card Setup set up but can't tie to a Pi (its files are incomplete): it can only be reset. */
function unmatchedCard(selected) {
  show(`<h1>This card can’t be matched to a Pi</h1><p>${esc(selected.name)}, ${size(selected.size)}. Reset it to set it up again.</p>`,
    '<button id="home">Back</button><span class="spacer"></span><button id="reset" class="danger">Reset card</button>', true, 300);
  $("home").onclick = home;
  $("reset").onclick = async () => {
    if (await decision("Reset this SD card?", `Removes everything on ${selected.name} (${size(selected.size)}). This can’t be undone.`,
      [["cancel","Cancel"],["go","Reset card","danger"]]) === "go") resetCard(selected, selectionKey(selected));
  };
}

// ---- Desktop container and pairing -------------------------------------------------------------------------------

const desktopPlatform = name => ({ macos: "macOS", darwin: "macOS", windows: "Windows", linux: "Linux" }[name] || name || "This computer");
function desktopDescription() {
  const memory = Number(desktop.ram_mb);
  return [desktopPlatform(desktop.platform), desktop.arch, memory > 0 ? `${Math.round(memory / 1024 * 10) / 10} GB RAM` : "RAM unavailable"].filter(Boolean).join(" · ");
}
function desktopGpu() {
  const gpu = desktop.gpu;
  if (!gpu) return "";
  const memory = `${gpu.name} · ${Math.round(gpu.memory_mb / 1024)} GB`;
  return `<p class="hint">${esc(memory)}</p>${gpu.usable ? "" : `<p class="hint">${esc(gpu.note)}</p>`}`;
}
function desktopHardware() {
  return `<div class="hardware-strip">${hostArt("desktop")}<div><strong>This computer</strong>
    <p class="hint">${esc(desktopDescription())}</p>${desktopGpu()}</div></div>
    <p class="hint desktop-runtime">Docker runs your agent in the background while this computer is awake.</p>`;
}
async function desktopHome(hermes = false) {
  connectingHermes = hermes === true;
  host = "desktop"; card = null;
  show(`<h1>${connectingHermes ? "Connect Hermes" : "Desktop Agent"}</h1><div class="runtime-status" role="status" id="detect-host"><span class="spin"></span>Checking this computer…</div>`,
    '<button id="back">Back</button>', true, 300);
  $("back").onclick = hostHome;
  const pending = $("detect-host");
  try {
    const info = await invoke("desktop_status", {kind: connectingHermes ? "hermes" : "desktop",create:creatingAgent});
    if (!pending.isConnected) return;
    if (!enterDesktop(info)) dockerPrerequisite(info);
  } catch (error) {
    if (!pending.isConnected) return;
    show('<h1>Couldn’t check the agent</h1><p class="hint">' + esc(String(error)) + '</p>',
      '<button id="back">Back</button><span class="spacer"></span><button id="retry-agent" class="primary">Check again</button>', true, 300);
    $("back").onclick = hostHome;
    $("retry-agent").onclick = () => desktopHome(hermes);
  }
}
function enterDesktop(info) {
  desktop = info;
  if (info.agent?.broken) { brokenAgent(info.agent); return true; }
  if (!info.agent && !info.runtime_ready) {
    if (connectingHermes && !verifiedHermes) { findHermes(); return true; }
    return false;
  }
  const focusPage = document.activeElement === document.body;
  if (info.agent) desktopManage(info.agent);
  else if (connectingHermes && verifiedHermes) installHermes(verifiedHermes);
  else if (connectingHermes) findHermes();
  else { form = fresh(); step = 0; wizard(); }
  if (focusPage) focusHeading();
  return true;
}

/** An agent whose settings can't be read, often after an install stopped: check again, or remove it and start over. */
function brokenAgent(agent) {
  const hermes = agent.hermes === true;
  show(`<h1>This agent can’t start</h1><p class="hint">${esc(agent.detail)}</p>
    <p class="hint">If its setup didn’t finish, remove it and set it up again.</p>`,
    '<button id="back">Back</button><span class="spacer"></span><button id="discard-agent" class="danger">Remove agent</button><button id="retry-agent" class="primary">Check again</button>', true, 300);
  $("back").onclick = hostHome;
  $("retry-agent").onclick = () => desktopHome(hermes);
  $("discard-agent").onclick = async () => {
    if (await decision("Remove this agent?", "Deletes its identity, chats, and downloaded models from this computer. This can’t be undone.",
      [["cancel","Cancel"],["go","Remove agent","danger"]]) !== "go") return;
    const button = $("discard-agent");
    if (!button) return;
    button.disabled = true;
    try { await invoke("desktop_discard_agent", {kind: hermes ? "hermes" : "desktop", port: agent.lan_port}); desktopHome(hermes); }
    catch (error) { if (button.isConnected) { button.disabled = false; fail(String(error)); } }
  };
}

async function lookupHermes(customProfile = null, portOverride = null, profileName = null, hostOverride = null) {
  const location = await invoke("discover_hermes", {profile:customProfile, port:portOverride, profileName, host:hostOverride});
  return {...location, customProfile, portOverride, profileName, hostOverride};
}

async function findHermes() {
  show('<h1>Connect Hermes</h1><div class="runtime-status" role="status" id="hermes-discovery"><span class="spin"></span>Looking for Hermes…</div>',
    '<button id="back">Back</button>', true, 300);
  $("back").onclick = hostHome;
  const pending = $("hermes-discovery");
  try {
    const location = await lookupHermes();
    if (!pending.isConnected) return;
    hermesLocation = location;
    hermesSetup();
  } catch (error) {
    if (!pending.isConnected) return;
    hermesLocation ||= {state:"missing",port:8642};
    hermesSetup(); fail(String(error));
  }
}

function hermesSetup() {
  const {state, profile, profile_name: selectedName, host: remote, detail} = hermesLocation;
  let localPort = hermesLocation.port;
  const ready = ["ready","protected"].includes(state);
  const missing = state === "missing";
  const running = remote ? `Hermes is running at ${remote}. Enter its API server key to connect.` : "Your Hermes gateway is running. Enter its API server key to connect.";
  const note = {
    ready:running,
    protected:running,
    missing:"Install Hermes or choose an existing profile on this computer. If Hermes runs on another computer, enter its address in Gateway settings.",
    offline:remote ? detail : "Hermes is installed. Enable its API server and start the gateway, then check again.",
    unsecured:"Set an API server key in Hermes and restart its gateway, then check again.",
    other:"Hermes didn’t recognize this gateway or profile. Check the address, port, and profile name."
  }[state];
  const icon = path => `<svg class="hermes-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="${path}"/></svg>`;
  const folderIcon = icon("M3 8V6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8Z");
  const chevron = icon("m9 6 6 6-6 6");
  const folderButton = `<button type="button" class="${ready ? "hermes-profile-action" : "hermes-row"}" id="hermes-folder">${folderIcon}Choose profile folder…${ready ? "" : chevron}</button>`;
  const options = `<details class="hermes-settings"${remote && !ready ? " open" : ""}><summary>${chevron}Gateway settings</summary>
    <div class="hermes-settings-body">${profile && !remote ? `<p class="hint sel hermes-path">${esc(profile)}</p>` : ""}
    ${ready && !remote ? folderButton : ""}
    <label for="hermes-host">Address</label>
    <input id="hermes-host" type="text" value="${esc(remote || "")}" maxlength="261" placeholder="This computer" spellcheck="false" autocapitalize="none" autocomplete="off" aria-describedby="hermes-host-hint">
    <p class="hint" id="hermes-host-hint">For Hermes on another computer, such as 192.168.1.20 or hermes.local.</p>
    <label for="hermes-profile-name">Profile name (optional)</label>
    <input id="hermes-profile-name" type="text" value="${esc(selectedName || "")}" maxlength="64" placeholder="Default profile" spellcheck="false" autocapitalize="none" autocomplete="off" pattern="[a-z0-9][a-z0-9_\\-]{0,63}">
    <label for="hermes-port">Gateway port</label>
    <input id="hermes-port" type="number" min="1" max="65535" value="${localPort}" required></div></details>`;
  const keyField = ready ? '<label for="hermes-key">API server key</label><input id="hermes-key" type="password" required maxlength="1024" autocomplete="off" spellcheck="false">'
    + (remote ? '<p class="hint">Hermes’s API isn’t encrypted. On a network you don’t trust, connect through a VPN such as Tailscale.</p>' : "") : "";
  const guide = ready ? "" : `<div class="hermes-actions">
    <button type="button" class="hermes-row" id="hermes-guide" aria-describedby="hermes-guide-hint">${icon(missing ? "M12 3v11m-5-5 5 5 5-5M4 16v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" : "M12 6c-3-2-6-2-9-1v14c3-1 6-1 9 1 3-2 6-2 9-1V5c-3-1-6-1-9 1Zm0 0v14")}
      ${missing ? "Get Hermes" : "Gateway setup guide"}${icon("M14 4h6v6m0-6L10 14M10 5H6a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-4")}</button>
    ${remote ? "" : folderButton}</div><span class="sr" id="hermes-guide-hint">Opens the guide in your browser.</span>`;
  const title = missing ? "Hermes wasn’t found" : "Connect Hermes";
  const retry = '<button id="hermes-retry" type="submit" form="hermes-form" class="hermes-retry"><svg viewBox="0 0 24 24" aria-hidden="true"><use href="#refresh-arrow"/></svg><span>Try again</span></button>';
  const connect = '<span class="spacer"></span><button id="attach-hermes" type="submit" form="hermes-form" class="primary" disabled>Connect</button>';
  show(`<section class="hermes-setup"><div class="hermes-heading"><h1>${title}</h1>${ready ? "" : retry}</div>
    <p id="hermes-note" class="hermes-intro">${esc(note)}</p>${guide}
    <form id="hermes-form" class="hermes-form">${keyField}${options}</form></section>`,
    `<button id="back">Back</button>${ready ? connect : ""}`, true, ready ? 330 : 366);
  const key = $("hermes-key"), port = $("hermes-port"), submit = $("attach-hermes") || $("hermes-retry"), back = $("back");
  const fields = $("hermes-form"), nameField = $("hermes-profile-name"), hostField = $("hermes-host");
  const targetChanged = () => port.valueAsNumber !== localPort || nameField.value !== (selectedName || "") || hostField.value.trim() !== (remote || "");
  const section = fields.closest(".hermes-setup"), message = $("hermes-note"), folderButtonEl = $("hermes-folder");
  back.onclick = hostHome;
  hasUnsavedChanges = () => !!key?.value;
  fitWindow(section);
  if ($("hermes-guide")) $("hermes-guide").onclick = () => invoke("open_hermes_guide",{install:missing}).catch(e => { if (fields.isConnected) fail(String(e)); });
  let busy = false;
  fields.oninput = () => {
    if (!ready) return;
    const check = targetChanged();
    submit.textContent = check ? "Check again" : "Connect";
    key.required = !check;
    submit.disabled = !port.validity.valid || !nameField.validity.valid || (!check && !key.value.trim());
  };
  nameField.oninvalid = port.oninvalid = hostField.oninvalid = () => { fields.querySelector("details").open = true; };
  // "10.1.1.50:8642" fills both fields.
  hostField.onchange = () => {
    const split = hostField.value.trim().match(/^(?:http:\/\/)?([^\s:/\[\]]+):(\d{1,5})\/?$/i);
    if (split) { hostField.value = split[1]; port.value = split[2]; fields.oninput(); }
  };
  async function recheck(pickFolder = false) {
    if (busy) return;
    busy = true;
    submit.setAttribute("aria-disabled", "true");
    folderButtonEl?.setAttribute("aria-disabled", "true");
    nameField.readOnly = port.readOnly = hostField.readOnly = true;
    try {
      const folder = pickFolder ? await invoke("choose_hermes_profile") : hermesLocation.customProfile;
      if (!fields.isConnected || (pickFolder && !folder)) return;
      const address = pickFolder ? "" : hostField.value.trim();
      // Another computer's gateway port is whatever the field shows; this computer's comes from its profile.
      const override = pickFolder ? null : address || port.valueAsNumber !== localPort ? port.valueAsNumber : hermesLocation.portOverride;
      fail("");
      submit.setAttribute("aria-busy", "true");
      if (!ready) submit.lastElementChild.textContent = "Checking…";
      $("announcement").textContent = address ? `Looking for Hermes at ${address}…` : "Looking for Hermes on this computer…";
      const name = pickFolder ? null : nameField.value !== (selectedName || "") ? nameField.value : hermesLocation.profileName;
      const next = await lookupHermes(folder, override, name, address || null);
      if (!fields.isConnected) return;
      hermesLocation = next;
      if (next.state !== state || next.profile !== profile || (next.host || "") !== (remote || "") || (next.profile_name || "") !== (selectedName || "") || (ready && next.port !== localPort)) {
        hermesSetup();
        $("announcement").textContent = $("hermes-note").textContent;
      } else {
        localPort = next.port;
        port.value = localPort;
        message.textContent = missing ? "Still couldn’t find Hermes. Install it, choose its profile folder, or enter its address." : next.state === "offline" && next.host ? next.detail : note;
        $("announcement").textContent = message.textContent;
      }
    } catch (error) {
      if (fields.isConnected) { message.textContent = note; fail(String(error)); }
    } finally {
      if (fields.isConnected) {
        submit.removeAttribute("aria-busy");
        submit.removeAttribute("aria-disabled");
        folderButtonEl?.removeAttribute("aria-disabled");
        if (!ready) submit.lastElementChild.textContent = "Try again";
        nameField.readOnly = port.readOnly = hostField.readOnly = false;
      }
      busy = false;
    }
  }
  if (folderButtonEl) folderButtonEl.onclick = () => recheck(true);
  fields.onsubmit = async event => {
    event.preventDefault();
    if (busy || submit.disabled) return;
    if (!ready || targetChanged()) return recheck();
    submit.disabled = back.disabled = true; fields.inert = true; submit.textContent = "Connecting…";
    const connection = {key:key.value.trim(), port:port.valueAsNumber, profile_name:hermesLocation.profile_name ?? null, host:hermesLocation.host ?? null};
    try {
      await changeDevice("verify_hermes",{connection});
      if (desktop.runtime_ready) return await installHermes(connection);
      verifiedHermes = connection;
      key.value = ""; dockerPrerequisite(desktop);
    } catch (error) {
      fail(String(error)); fields.inert = false; back.disabled = submit.disabled = false; submit.textContent = "Connect";
    }
  };
}

async function installHermes(connection) {
  const settings = {name:"Hermes", provider:"hermes", access:{mode:"later", owners:[]}, hubs:[DEFAULT_HUB],
    features:{wikipedia:false, voice:false}};
  verifiedHermes = null;
  show('<h1>Connect Hermes</h1><p id="desktop-phase" role="status">Checking your Hermes gateway…</p><progress aria-label="Connecting Hermes"></progress>', "", true, 280);
  try {
    const agent = await changeDevice("desktop_install",{settings,hermes:connection,create:creatingAgent,attempt:creationAttempt});
    desktopManage(agent);
  } catch (error) {
    hermesSetup(); fail(String(error));
  }
}

const DOCKER_ACTIONS = {
  install: ["Install Docker", "Your system may ask you to approve the installation."],
  start: ["Start Docker", "Ratspeak Agent will continue when Docker is running."],
  access: ["Allow Docker access", "Access lets your account administer Docker. You’ll need to sign out and back in."],
  wait: ["", "Waiting for Docker to finish starting…"],
  blocked: ["Check again", "Docker is unavailable. Check that it’s installed and running."]
};
function dockerPrerequisite(initial) {
  const name = initial.platform === "linux" ? "Docker Engine" : initial.runtime_action === "install" ? "Docker Desktop" : "Docker";
  show(`<h1>Prepare this computer</h1><div class="hardware-strip">${hostArt("desktop")}
    <div><strong>${name}</strong>
    <p class="hint">Runs your desktop agent on this computer.</p></div></div>
    <div class="docker-status"><span class="spin" id="runtime-spinner" hidden aria-hidden="true"></span>
      <p class="hint" id="runtime-phase" role="status"></p></div>
    <p class="hint" id="docker-terms" hidden>Installing Docker Desktop accepts the <a id="docker-terms-link" href="https://www.docker.com/legal/docker-subscription-service-agreement/" target="_blank" rel="noopener noreferrer">Docker Subscription Service Agreement</a>.</p>`,
    '<button id="back">Back</button><span class="spacer"></span><button id="prepare-runtime" class="primary"></button>', true, 344);
  const back = $("back"), primary = $("prepare-runtime"), phase = $("runtime-phase"), spinner = $("runtime-spinner"), terms = $("docker-terms");
  $("docker-terms-link").onclick = event => { event.preventDefault(); invoke("open_docker_terms").catch(() => {}); };
  const probeFailure = "Couldn’t check Docker. Check that its engine is running, then try again.";
  let info = initial, preparing = false, polling = false, revision = 0;
  let graceUntil = 0, waitingSince = Date.now(), error = "";
  const action = () => Object.hasOwn(DOCKER_ACTIONS, info.runtime_action) ? info.runtime_action : "blocked";
  const message = value => { phase.setAttribute("aria-live","polite"); if (phase.textContent !== value) phase.textContent = value; };
  function draw() {
    const state = action();
    const grace = !error && Date.now() < graceUntil;
    const timedOut = state === "wait" && Date.now() - waitingSince > 3 * 60_000;
    const waiting = grace || state === "wait" && !timedOut && !error;
    back.disabled = preparing;
    terms.hidden = state !== "install" || info.platform === "linux";
    primary.hidden = preparing || waiting;
    primary.disabled = preparing || waiting;
    primary.textContent = state === "wait" || error === probeFailure ? "Check again" : DOCKER_ACTIONS[state][0];
    spinner.hidden = !(preparing || waiting && (grace || !info.runtime_message));
    if (preparing) return;
    const note = timedOut && !info.runtime_message ? "Docker is taking longer to start. Check for an open Docker or system prompt, then check again."
      : grace && state !== "access" ? "Finish any Docker or system prompts. Ratspeak Agent continues when Docker is ready." : info.runtime_message || DOCKER_ACTIONS[state][1];
    message(error || note);
  }
  async function poll() {
    if (preparing || polling || !phase.isConnected || document.hidden || document.querySelector("dialog[open]")) return;
    const request = revision;
    polling = true;
    try {
      const next = await invoke("desktop_status", {kind: connectingHermes ? "hermes" : "desktop",create:creatingAgent});
      if (!phase.isConnected || request !== revision || document.querySelector("dialog[open]") || enterDesktop(next)) return;
      if (next.runtime_action !== info.runtime_action) {
        // The installer has already requested startup. Give the engine time to answer.
        if (info.runtime_action !== "install" || next.runtime_action !== "start") graceUntil = 0;
        waitingSince = Date.now(); error = "";
      }
      info = next;
      if (error === probeFailure) error = "";
    } catch {
      if (phase.isConnected && request === revision && !error) error = probeFailure;
    } finally {
      polling = false;
      if (phase.isConnected && request === revision) draw();
    }
  }
  back.onclick = hostHome;
  primary.onclick = async () => {
    if (primary.disabled || preparing) return;
    const state = action();
    if (state === "wait" || state === "blocked" || error === probeFailure) {
      error = ""; waitingSince = Date.now(); draw(); primary.disabled = true; message("Checking Docker…"); await poll(); return;
    }
    revision++; preparing = true; error = ""; draw();
    message(({install:"Preparing the Docker installer…",start:"Starting Docker…",access:"Requesting Docker access…"})[state]);
    try {
      await changeDevice("prepare_desktop_runtime", {action:state});
      graceUntil = Date.now() + 45_000;
      waitingSince = Date.now();
    } catch (problem) {
      error = typeof problem === "string" && problem.trim() ? problem : "Docker setup didn’t finish. Try again.";
      graceUntil = 0;
    } finally {
      preparing = false;
      if (phase.isConnected) { draw(); await poll(); }
    }
  };
  draw();
  timer = setInterval(poll, 3000);
}
listen("runtime-progress", ({payload}) => {
  const phase = $("runtime-phase");
  if (!phase || !$("back")?.disabled || typeof payload !== "string" || phase.textContent === payload) return;
  // Keep byte counts visible while announcing installation stage changes to screen readers.
  phase.setAttribute("aria-live", / · [\d.,]+ MB$/.test(payload) ? "off" : "polite");
  phase.textContent = payload;
});

// Shared by a desktop install and a Pi's first start. Only the desktop's ids mark an operation in progress.
function installCard(art, name, label, ids = {}) {
  const id = key => ids[key] ? ` id="${ids[key]}"` : "";
  return `<div class="installation-progress"><div class="hardware-strip">${hostArt(art)}<div><strong>${esc(name)}</strong>
    <p class="hint install-phase"${id("phase")} role="status">${esc(ids.text || "")}</p></div></div><progress aria-label="${esc(label)}"></progress>
    <p class="hint install-detail"${id("detail")} aria-live="off"></p></div>`;
}

async function desktopInstall() {
  desktopStage = null;
  show(`<h1>Setting up your desktop agent</h1>${installCard("desktop", form.name, "Setting up the agent", {phase:"desktop-phase", detail:"desktop-detail", text:"Preparing the container…"})}`, "", true, 330);
  try {
    const agent = await invoke("desktop_install", { settings: installationSettings(), create:creatingAgent,attempt:creationAttempt });
    form.veniceKey = ""; desktop.agent = agent;
    desktopManage(agent);
  } catch (error) {
    show(`<h1>Setup didn’t finish</h1><p>${esc(error)}</p>`,
      '<button id="back">Back to review</button><span class="spacer"></span><button id="check-desktop" class="primary">Check agent</button>', true, 320);
    $("back").onclick = wizard;
    $("check-desktop").onclick = desktopHome;
  }
}
// Setup reports a stage, then the measure it has: Docker build steps or downloaded bytes.
let desktopStage = null;
listen("desktop-progress", ({ payload }) => {
  const label = $("desktop-phase");
  if (!label) return;
  const p = typeof payload === "string" ? { text: payload } : payload || {};
  if (p.stage) desktopStage = p;
  if (p.text) label.textContent = p.text;
  const bar = view.querySelector("progress"), detail = $("desktop-detail");
  const parts = desktopStage ? [`Stage ${desktopStage.stage} of ${desktopStage.stages}`] : [];
  const mb = bytes => mbLabel(Math.round(bytes / 1e6), 1);
  if (p.total) {
    bar.max = p.total; bar.value = Math.min(p.done || 0, p.total);
    parts.push(`${Math.min(99, Math.floor((p.done || 0) * 100 / p.total))}%`, `${mb(p.done || 0)} of ${mb(p.total)}`);
  } else if (p.steps) {
    bar.max = p.steps; bar.value = p.step;
    parts.push(`step ${p.step} of ${p.steps}`);
  } else {
    bar?.removeAttribute("value");
    if (p.done) parts.push(mb(p.done));
  }
  if (detail) detail.textContent = parts.join(" · ");
});

// ---- Wi-Fi picker (wizard step 3 and the Wi-Fi tab) ----------------------------------------------------------------

const BARS = (level) => `<svg class="bars" data-level="${level}" viewBox="0 0 16 12" width="16" height="12" aria-hidden="true">
  <rect x="0" y="9" width="3" height="3" rx="1"/><rect x="4.3" y="6" width="3" height="6" rx="1"/>
  <rect x="8.6" y="3" width="3" height="9" rx="1"/><rect x="13" y="0" width="3" height="12" rx="1"/></svg>`;
const LOCK = `<svg class="lock" viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><rect x="2" y="5" width="8" height="6" rx="1.5" fill="currentColor"/>
  <path d="M4 5V3.5a2 2 0 0 1 4 0V5" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>`;

const pickerHtml = (title) => `<div id="picker"><div class="pickhead"><div class="field" id="picklabel">${title}</div>
    <button type="button" class="quiet" id="rescan" hidden>Rescan</button></div>
  <div class="network-panel"><div class="group nets" id="nets" role="group" aria-labelledby="picklabel"></div>
    <span class="sr" id="network-navigation">Use the up and down arrow keys to browse. Press Enter or Space to choose a network.</span>
    <div class="slot" id="slot" hidden></div></div><p class="hint" id="pickhint" aria-live="polite"></p></div>`;

/** Wire a picker drawn with pickerHtml. `state` is {net, ssid, password, country} and is kept up to date;
 *  `changed` runs after every edit. */
function picker(state, changed = () => {}, collapse = false, submit = null) {
  const container = $("picker"), active = () => container.isConnected && $("picker") === container;
  const nets = $("nets");
  const rows = () => [...nets.querySelectorAll("button[data-network]")];
  let cursor = state.net;
  function setCursor(network, focus = false) {
    const choices = rows();
    const current = choices.find(row => row.dataset.network === network) || choices[0];
    if (!current) return;
    cursor = current.dataset.network;
    for (const row of choices) row.tabIndex = row === current ? 0 : -1;
    if (focus) { current.focus({preventScroll:true}); current.scrollIntoView({block:"nearest"}); }
  }
  loadCountry(state);
  const keep = () => {
    for (const key of ["ssid", "password"]) if ($(key)) state[key] = $(key).value;
    state.country = state.country.trim().toUpperCase();
  };
  const chosen = () => (scan ? scan.networks : []).find((n) => n.ssid === state.net);
  const slot = () => {
    const other = state.net === OTHER, secure = other || !!(chosen() && chosen().secure);
    const parts = [];
    if (other && !state.connected) parts.push(field("ssid", "Wi-Fi name", `<input type="text" id="ssid" maxlength="32" value="${esc(state.ssid)}" autocomplete="off" spellcheck="false">`));
    if (secure && !state.connected) parts.push(field("password", "Password", `<div class="inside"><input type="password" id="password" maxlength="63" value="${esc(state.password)}">
      <button type="button" class="quiet" id="reveal">Show</button></div>`));
    $("slot").style.gridTemplateColumns = other ? "1fr 1fr" : "1fr";
    $("slot").innerHTML = parts.join("");
    $("slot").hidden = !parts.length;
    if ($("reveal")) $("reveal").onclick = () => {
      const hidden = $("password").type === "password";
      $("password").type = hidden ? "text" : "password"; $("reveal").textContent = hidden ? "Hide" : "Show";
    };
    $("slot").oninput = () => { keep(); state.connected = false; changed(); };
    // Enter in the name or password does what the step's main button does.
    $("slot").onkeydown = event => {
      if (submit && event.key === "Enter" && !event.isComposing && event.target.matches("input")) { event.preventDefault(); submit(); }
    };
  };
  const list = () => {
    if (!active() || !$("nets")) return;
    const scanning = !scan || !!scan.busy;
    $("rescan").hidden = scanning;
    $("nets").setAttribute("aria-busy", String(scanning));
    if (scanning) {
      nets.removeAttribute("aria-describedby");
      $("nets").hidden = false;
      $("nets").classList.remove("picked");
      $("nets").innerHTML = `<div class="scanning" role="status"><span class="spin" aria-hidden="true"></span>Scanning…</div>`;
      $("pickhint").textContent = "";
      if (!scan) slot();
      return;
    }
    const networks = scan.networks;
    if (!networks.length && !collapse) state.net = OTHER;
    const selected = collapse && !!state.net;
    container.classList.toggle("editing-network", selected);
    $("rescan").hidden = selected;
    $("nets").hidden = !collapse && !networks.length;
    if (selected) {
      nets.removeAttribute("aria-describedby");
      const name = state.net === OTHER ? (state.connected ? state.ssid : "Other network…") : state.net;
      const status = state.connected ? '<strong class="network-status">Connected</strong>' : "";
      $("nets").innerHTML = `<div class="net selected-network"><span>${esc(name)}</span>${chosen()?.secure ? LOCK : ""}${status}</div>`;
    } else {
      nets.setAttribute("aria-describedby", "network-navigation");
      const pressed = id => collapse ? "" : ` aria-pressed="${id === state.net}"`;
      nets.innerHTML = networks.map((n) => {
        const level = n.signal === null ? 0 : n.signal >= 75 ? 4 : n.signal >= 50 ? 3 : n.signal >= 25 ? 2 : 1;
        return `<button type="button" class="net" data-network="${esc(n.ssid)}"${pressed(n.ssid)}>
          <span>${esc(n.ssid)}</span>${n.secure ? `${LOCK}<i class="sr">Secured</i>` : ""}${level ? `${BARS(level)}<i class="sr">Signal ${level} of 4</i>` : ""}</button>`;
      }).join("") + `<button type="button" class="net other" data-network="${esc(OTHER)}"${pressed(OTHER)}><span>Other network…</span></button>`;
      setCursor(cursor);
    }
    const hint = !networks.length ? "No networks found." : !scan.scanned && system === "macos" ? "Allow location access to see nearby networks." : "";
    $("pickhint").textContent = selected ? "" : hint;
    slot();
  };
  const load = async (again) => {
    if (scan && !scan.busy && !again) return list();
    const restoreFocus = again && document.activeElement === $("rescan");
    const request = ++scanRequest;
    if (scan) scan.busy = true;
    list();
    let result;
    try { result = await invoke("wifi_scan"); } catch { result = { networks: [], scanned: false }; }
    if (request !== scanRequest) return;
    scan = result;
    list();
    if (active() && restoreFocus && document.activeElement === document.body)
      ($("nets").hidden ? $("ssid") : $("rescan"))?.focus();
  };
  nets.onkeydown = event => {
    const current = event.target.closest("button[data-network]");
    if (!current || !["ArrowDown","ArrowUp","Home","End"].includes(event.key)) return;
    const choices = rows(), i = choices.indexOf(current);
    const at = { ArrowDown: Math.min(i + 1, choices.length - 1), ArrowUp: Math.max(i - 1, 0), Home: 0, End: choices.length - 1 };
    event.preventDefault(); setCursor(choices[at[event.key]].dataset.network, true);
  };
  nets.onclick = event => {
    const current = event.target.closest("button[data-network]");
    if (!current) return;
    keep();
    cursor = current.dataset.network;
    state.net = !collapse && state.net === cursor ? "" : cursor;
    state.password = ""; state.connected = false;
    list(); changed();
    if (collapse) ($("ssid") || $("password") || $("next")).focus();
    else setCursor(cursor, true);
  };
  $("rescan").onclick = () => load(true);
  load(false);
  return { keep, render: list, focusNetwork: network => setCursor(network, true) };
}

/** The Wi-Fi settings a picker holds, or a message saying what is missing. */
function wifiOf(state) {
  const other = state.net === OTHER, known = (scan ? scan.networks : []).find((n) => n.ssid === state.net);
  const ssid = other ? state.ssid : state.net, secure = other || !known || known.secure;
  if (!ssid) return { error: other ? "Enter the Wi-Fi name." : "Choose a Wi-Fi network.", id: other ? "ssid" : "" };
  if (secure && !other && !state.password) return { error: "Enter the Wi-Fi password.", id: "password" };
  if (secure && state.password && state.password.length < 8) return { error: "A Wi-Fi password has at least 8 characters.", id: "password" };
  if (state.country && !/^[A-Z]{2}$/.test(state.country)) return { error: "The Wi-Fi region is unavailable. Try again.", id: "" };
  return { wifi: { ssid, password: secure ? state.password : "", country: state.country } };
}

// ---- Wizard --------------------------------------------------------------------------------------------------------

const ordinalRules = new Intl.PluralRules("en", { type: "ordinal" });
function userLabel(number) {
  const suffix = { one: "st", two: "nd", few: "rd", other: "th" }[ordinalRules.select(number)];
  return `${number}${suffix} user`;
}

const nameField = () => field("name", "Name", `<div class="inside name-field">
        <input type="text" id="name" maxlength="32" value="${esc(form.name)}">
        <button type="button" class="quiet" id="shuffle-name" title="Suggest another name" aria-label="Suggest another name">
          <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8"
               stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3.8 10a8.5 8.5 0 0 1 14.2-4l2.5 2.5m0-5v5h-5M20.2 14a8.5 8.5 0 0 1-14.2 4l-2.5-2.5m0 5v-5h5"/>
          </svg>
        </button></div><span class="sr" role="status" id="name-suggestion"></span>`);

const veniceFields = () => `<label class="field" for="venice-key">Venice API key</label>
          <div class="key-row"><input id="venice-key" type="password" autocomplete="off" autocapitalize="none" spellcheck="false" aria-describedby="venice-key-hint">
            <button type="button" id="verify-key">Verify</button></div>
          <p id="venice-key-hint" class="hint">Use a dedicated inference key with a spending cap. <a id="create-venice-key" href="https://venice.ai/settings/api" target="_blank" rel="noopener noreferrer">Create a key</a></p>
          <p id="key-status" class="hint" role="status"></p>
          <div id="cloud-model-field" hidden>${field("venice-model", "Venice model", '<select id="venice-model" disabled></select>')}</div>
          <p id="venice-access-note" class="hint" hidden>This can be changed with /model later on.</p>`;

function wireVenice(state, changed, preferred = "") {
  const key = $("venice-key"), verify = $("verify-key"), menu = $("venice-model"), message = $("key-status");
  const modelField = $("cloud-model-field"), note = $("venice-access-note");
  $("create-venice-key").onclick = async event => {
    event.preventDefault();
    try { await invoke("open_venice_keys"); }
    catch { message.textContent = "Open venice.ai/settings/api in your browser to create a key."; }
  };
  let revision = 0, checking = false;
  key.value = state.veniceKey;
  const render = () => {
    verify.textContent = checking ? "Verifying…" : state.veniceVerified ? "Reverify" : "Verify";
    verify.disabled = checking || !key.value.trim();
    verify.classList.toggle("verified", state.veniceVerified);
    modelField.hidden = menu.disabled = !state.veniceVerified || !state.veniceModels.length;
    note.hidden = modelField.hidden;
    if (state.veniceVerified && state.veniceModels.length) {
      message.textContent = !state.veniceAvailable ? "Key recognized. Venice has paused inference. Check your account, then reverify."
        : !state.veniceListed ? "Catalog preview. Venice didn’t list model limits for this key." : "";
      note.textContent = !state.veniceAvailable ? "You can browse models while inference is paused."
        : "Ratspeak Agent requests one short, billed reply. Owners can switch models with /model.";
    }
    if (modelField.hidden) { menu.replaceChildren(); return; }
    const prompt = new Option("Choose a model", "");
    prompt.disabled = true;
    menu.replaceChildren(prompt, ...state.veniceModels.map(m => new Option(m.name || m.id, m.id)));
    menu.value = state.veniceModel;
  };
  key.oninput = () => {
    revision += 1; checking = false;
    state.veniceKey = key.value; state.veniceAvailable = false; state.veniceVerified = false;
    state.veniceModels = []; state.veniceModel = "";
    message.textContent = ""; render(); changed();
  };
  verify.onclick = async () => {
    const apiKey = key.value.trim(), request = ++revision;
    const previous = state.veniceModel || preferred;
    state.veniceAvailable = false; state.veniceVerified = false; state.veniceModels = []; state.veniceModel = "";
    checking = true; message.textContent = ""; render(); changed();
    try {
      const result = await invoke("venice_models", { apiKey });
      if (!key.isConnected || request !== revision) return;
      state.veniceKey = apiKey;
      state.veniceModels = result.models;
      state.veniceModel = result.models.some(m => m.id === previous) ? previous : "";
      state.veniceAvailable = result.inference_available === true;
      state.veniceListed = result.has_model_limits === true;
      state.veniceVerified = true;
      if (!result.models.length) message.textContent = "No private chat models are available with this key.";
    } catch (error) {
      if (key.isConnected && request === revision) message.textContent = typeof error === "string" && !error.includes(apiKey)
        ? error : "Couldn’t verify. Check your API key and internet connection, then try again.";
    } finally {
      if (key.isConnected && request === revision) {
        checking = false; render(); changed();
        if (!menu.disabled && (document.activeElement === document.body || document.activeElement === verify)) menu.focus();
      }
    }
  };
  menu.onchange = () => { state.veniceModel = menu.value; changed(); };
  render();
  return () => { key.value = ""; key.oninput(); };
}

const STEPS = [
  { get title() { return host === "desktop" ? "Your desktop agent" : "Your Raspberry Pi"; }, height: 410,
    body: () => `${nameField()}${host === "desktop" ? desktopHardware() : `
      <div class="board-fields">
        ${field("board", "Which Raspberry Pi do you have?", `<select id="board">
          <option value="" disabled>Choose your Pi</option>
          ${Object.entries(BOARDS).map(([id, b]) => `<option value="${id}">${esc(b.name)}</option>`).join("")}
          <option value="auto">I’m not sure</option></select>`)}
        <div id="ram-field">${field("ram", "Memory (RAM)", '<select id="ram"></select>')}</div>
      </div><p class="hint" id="auto-hint" hidden>Your Pi will choose a model when it first starts.</p>`}`,
    wire() {
      const board = $("board"), ram = $("ram");
      const name = $("name");
      $("shuffle-name").onclick = () => {
        form.name = suggestName(name.value);
        name.value = form.name;
        $("name-suggestion").textContent = form.name;
        if (name.hasAttribute("aria-invalid")) {
          name.removeAttribute("aria-invalid");
          $("error").textContent = "";
        }
      };
      if (host === "desktop") return;
      board.value = form.board;
      const update = () => {
        const spec = BOARDS[form.board];
        $("ram-field").hidden = !spec;
        if (spec) {
          ram.innerHTML = '<option value="" disabled>Choose RAM</option>' + spec.memory.map(gb =>
            `<option value="${gb}">${ramLabel(gb)}</option>`).join("") + '<option value="auto">I’m not sure</option>';
          if (spec.memory.length === 1) form.ram = String(spec.memory[0]);
          ram.value = form.ram;
          ram.disabled = spec.memory.length === 1;
        }
        $("auto-hint").hidden = form.board !== "auto" && form.ram !== "auto";
        $("next").disabled = !form.board || (!!spec && !form.ram);
      };
      board.onchange = () => { form.board = board.value; form.ram = ""; form.model = null; update(); };
      ram.onchange = () => { form.ram = ram.value; form.model = null; update(); };
      update();
    },
    read() {
      form.name = $("name").value.trim();
      if (!/^[A-Za-z0-9][A-Za-z0-9 _.-]{0,31}$/.test(form.name)) return ["The name can use letters, digits, spaces, and . _ -", "name"];
      if (host === "pi" && (!form.board || (BOARDS[form.board] && !form.ram))) return ["Choose your Pi and its memory."];
    } },
  { title: "Choose a model", height: 512,
    body: () => `<fieldset class="model-picker" aria-label="Agent model">
      <div class="picker-heading">${host === "desktop" ? 'On this computer<button type="button" class="quiet" id="more-models" aria-haspopup="dialog" aria-controls="more-models-dialog" hidden>More models <span aria-hidden="true">→</span></button>' : "On your Pi"}</div>
      <div id="model-choices" class="model-grid"></div><p class="hint" id="model-status" role="status">Loading models…</p>
      ${host === "desktop" ? '<div id="selected-alternative" hidden></div>' : ""}
      ${OR_DIVIDER}
      <div class="provider-box" id="provider-box"><label class="provider-choice">
        <img class="venice-mark" src="venice-keys-off-white.svg" width="27" height="30" alt="">
        <span><b>Venice</b><span class="hint">Send chats to the cloud using your Venice credits.</span></span>
        <input type="radio" name="model" value="venice" id="use-venice" ${form.provider === "venice" ? "checked" : ""}></label>
        <div id="venice-details" class="provider-details" hidden>
          ${veniceFields()}
        </div></div></fieldset>
        ${host === "desktop" ? `<dialog id="more-models-dialog" class="setup-dialog model-dialog" aria-labelledby="more-models-title">
          <h2 id="more-models-title">More models</h2>
          <div class="model-list-heading" aria-hidden="true"><span>Local model</span><span>Estimated RAM</span></div>
          <div id="alternative-models" class="model-list" role="radiogroup" aria-label="Lighter local models"></div>
          <div class="dialog-actions"><button type="button" id="cancel-model">Cancel</button><button type="button" class="primary" id="apply-model" disabled>Use model</button></div>
        </dialog>` : ""}`,
    wire() {
      const choices = $("model-choices"), status = $("model-status"), next = $("next");
      const cloud = $("use-venice"), details = $("venice-details");
      const extraRow = $("selected-alternative");
      const automatic = host === "pi" && (form.board === "auto" || form.ram === "auto");
      let localReady = false;
      let alternatives = [];
      const renderAlternative = () => {
        if (!extraRow) return;
        const model = alternatives.find(m => m.id === form.model);
        extraRow.innerHTML = model ? `<label class="chosen-model"><span><b>${esc(model.name)}</b><span class="hint">Download: ${mbLabel(model.size_mb)}</span></span>
          <span class="lighter-ram">${modelPlace(model)}: ${modelRam(model)}</span><input type="radio" name="model" value="${esc(model.id)}" checked></label>` : "";
      };
      const update = () => {
        const venice = form.provider === "venice";
        cloud.checked = venice;
        details.hidden = !venice;
        next.disabled = venice ? !form.veniceModel || !form.veniceAvailable : !localReady;
        if (extraRow) extraRow.hidden = venice || !extraRow.firstElementChild;
        const localHeight = extraRow && !extraRow.hidden ? 608 : alternatives.length ? 544 : 512;
        sizeWindow(venice ? (form.veniceVerified ? 660 : 552) : localHeight);
      };
      const clearVenice = wireVenice(form, update);
      cloud.onchange = () => {
        form.provider = "venice";
        form.offline = false;
        update();
      };
      update();
      const chooseLocal = id => {
        form.provider = "local"; form.model = id;
        renderAlternative();
        clearVenice();
      };
      choices.onchange = event => {
        if (event.target.matches("input[name=model]")) chooseLocal(automatic ? null : event.target.value);
      };
      if (automatic) {
        form.model = null; form.modelOptions = [];
        choices.innerHTML = `<label class="model-card automatic"><b>Choose on first boot</b><span class="model-description">Your Pi picks a local model that fits its memory.</span>
          <input type="radio" name="model" value="auto" ${form.provider === "local" ? "checked" : ""}></label>`;
        status.hidden = true; localReady = true; update(); return;
      }
      const offeredModels = host === "desktop" ? Promise.resolve(desktop.models) : loadModels().then(modelChoices);
      offeredModels.then(offered => {
        if (!choices.isConnected) return;
        form.modelOptions = offered.map(m => m.id);
        if (!form.modelOptions.includes(form.model)) form.model = offered[0]?.id || null;
        const cards = host === "desktop" ? offered.slice(0, 3) : offered;
        choices.innerHTML = cards.map((m, i) => `<label class="model-card">
          <b>${esc(m.name)}</b><span class="model-description">${i === 0 ? '<span class="model-recommended">Recommended</span>' : ""}${esc(m.description || MODEL_DESCRIPTIONS[m.id])}</span>
          <span class="model-size">${m.size_mb ? `Download: ${mbLabel(m.size_mb)}` : ""}${m.size_mb && m.overhead_mb ? `<span>${modelPlace(m)}: ${modelRam(m)}</span>` : ""}</span>
          <input type="radio" name="model" value="${esc(m.id)}" ${form.provider === "local" && m.id === form.model ? "checked" : ""}>
        </label>`).join("");
        if (host === "desktop" && offered.length > cards.length) {
          alternatives = offered.slice(cards.length);
          renderAlternative();
          const more = $("more-models"), dialog = $("more-models-dialog"), list = $("alternative-models"), apply = $("apply-model");
          more.hidden = false;
          more.onclick = () => {
            list.innerHTML = alternatives.map(m => `<label class="lighter-model">
              <input type="radio" name="alternative-model" value="${esc(m.id)}" ${m.id === form.model && form.provider === "local" ? "checked" : ""}>
              <span><b>${esc(m.name)}</b><span class="hint">Download: ${mbLabel(m.size_mb)}</span></span>
              <span class="lighter-ram"><span class="sr">${m.on_gpu ? "Estimated graphics memory " : "Estimated RAM "}</span>${modelRam(m)}</span></label>`).join("");
            const selected = list.querySelector(":checked");
            apply.disabled = !selected;
            // WebKit does not always focus a button activated with the pointer.
            more.focus({preventScroll:true});
            dialog.showModal();
            (selected || list.querySelector("input")).focus();
          };
          list.onchange = () => { apply.disabled = !list.querySelector(":checked"); };
          $("cancel-model").onclick = () => dialog.close();
          apply.onclick = () => {
            const selected = list.querySelector(":checked");
            if (!selected) return;
            chooseLocal(selected.value);
            dialog.close();
            extraRow.querySelector("input").focus();
          };
        }
        localReady = !!form.model;
        status.hidden = localReady;
        if (!localReady) status.textContent = "No local model fits the available memory.";
        update();
      }).catch(() => { if (choices.isConnected) status.textContent = "Couldn’t load local models. Go back and try again."; });
    },
    read() {
      if (form.provider === "venice") return form.veniceAvailable && form.veniceModels.some(m => m.id === form.veniceModel) ? null : ["Verify your key and choose a Venice model."];
      if ((host === "desktop" || BOARDS[form.board] && form.ram !== "auto") && !form.model) return ["Choose a local model or Venice."];
    } },
  { title: "Wi-Fi", height: 560, available: () => host === "pi",
    body: () => `<div class="fill wifi-wizard">${pickerHtml("Network")}
      ${OR_DIVIDER}
      <div class="group transport-options">${option("elsewhere", "My Pi will use Wi-Fi somewhere else.", "", form.elsewhere)}${option("cable", "I'll use Ethernet for my Pi.", "", form.cable)}${option("offline", "No Wi-Fi or network.", "", form.offline, form.provider === "venice" ? "disabled" : "")}</div>
      <p class="hint transport-note" id="elsewhere-note" hidden>Ratspeak Agent can’t test that network from here, so check its name and password.</p>
      ${form.provider === "venice" ? '<p class="hint transport-note">Your Pi needs internet for Venice to answer.</p>' : ""}</div>
      <dialog id="offline-dialog" class="setup-dialog" aria-labelledby="offline-title"><h1 id="offline-title">Prepare for offline use?</h1>
        <p>Ratspeak Agent will download the models and tools to the SD card. This computer needs internet.</p>
        <p class="hint" id="offline-size"></p><div class="dialog-actions"><button type="button" id="cancel-offline">Back</button><button type="button" class="primary" id="confirm-offline">Prepare offline</button></div></dialog>`,
    extra: () => `<div id="wifi-connected" class="connected-tile" role="status" hidden>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m4 12 5 5L20 6"/></svg>
        <strong>Connected</strong></div>`,
    wire() {
      const state = form.wifi, container = $("picker"), active = () => container.isConnected && $("picker") === container;
      const update = () => {
        if (!active()) return;
        const alternate = form.cable || form.offline, untested = !alternate && !form.elsewhere && !state.connected, next = $("next");
        $("picker").classList.toggle("off", alternate || state.busy); $("picker").inert = alternate || state.busy;
        // One button: it tests the chosen network, then continues once the test passes.
        next.innerHTML = state.busy ? `<span class="spin" aria-hidden="true"></span>Connecting…` : untested ? "Connect" : "Continue";
        next.title = untested ? "Test the selected network on this computer" : "";
        next.setAttribute("aria-busy", String(state.busy));
        next.disabled = state.busy || (untested && !!wifiOf(state).error);
        $("back").disabled = state.busy;
        $("cable").disabled = $("elsewhere").disabled = state.busy || state.connected;
        $("offline").disabled = $("cable").disabled || form.provider === "venice";
        $("elsewhere-note").hidden = !form.elsewhere;
      };
      // A network somewhere else is typed in, like Other network, and can't be tested from here.
      const elsewhere = on => {
        form.elsewhere = $("elsewhere").checked = on;
        if (on) { form.cable = $("cable").checked = form.offline = $("offline").checked = false; }
        state.net = on ? OTHER : ""; state.ssid = state.password = ""; state.connected = false;
        $("error").textContent = "";
        networks.render(); update();
        if (on) $("ssid")?.focus();
      };
      if (form.provider === "venice") $("offline").closest("label").hidden = true;
      const networks = picker(state, () => { $("error").textContent = ""; update(); }, true, () => { if (!$("next").disabled) $("next").click(); });
      this.keep = networks.keep;
      let feedbackTimeout;
      this.back = () => {
        if (!state.net || form.cable || form.offline) return false;
        if (form.elsewhere) { elsewhere(false); return true; }
        const previous = state.net;
        state.net = state.ssid = state.password = "";
        state.connected = false;
        clearTimeout(feedbackTimeout);
        $("wifi-connected").hidden = true;
        $("error").textContent = "";
        networks.render();
        update();
        networks.focusNetwork(previous);
        return true;
      };
      $("elsewhere").onchange = () => elsewhere($("elsewhere").checked);
      $("cable").onchange = () => {
        if ($("cable").checked && form.elsewhere) elsewhere(false);
        form.cable = $("cable").checked; if (form.cable) form.offline = $("offline").checked = false; update();
      };
      const dialog = $("offline-dialog");
      $("offline").onchange = async () => {
        if (!$("offline").checked) { form.offline = false; update(); return; }
        $("offline").checked = form.offline;
        const confirm = $("confirm-offline"), detail = $("offline-size");
        confirm.disabled = true;
        detail.textContent = "Checking download size…";
        dialog.showModal();
        try {
          const plan = await offlineRequirements([]); // Extras are chosen on the following page.
          if (!dialog.isConnected) return;
          const fits = card.size >= plan.card_bytes;
          detail.textContent = fits ? `${mbLabel(Math.ceil(plan.download_bytes / 1e6), 1)} download, including model files and tools.` : offlineCapacityMessage(plan.card_bytes);
          confirm.disabled = !fits;
        } catch { detail.textContent = "Couldn’t check the offline download size. Go back and try again."; }
      };
      $("cancel-offline").onclick = () => dialog.close();
      $("confirm-offline").onclick = () => {
        if (form.elsewhere) elsewhere(false);
        form.offline = $("offline").checked = true; form.cable = $("cable").checked = false; dialog.close(); update();
      };
      $("next").classList.add("connect-step");
      // Continue tests an untested network instead of leaving the step.
      this.beforeNext = () => {
        if (form.cable || form.offline || form.elsewhere || state.connected) return false;
        connect();
        return true;
      };
      const connect = async () => {
        this.keep(); const got = wifiOf(state);
        if (got.error) return fail(got.error, got.id);
        state.busy = true; $("error").textContent = ""; update();
        let problem = "";
        try {
          const connected = await changeDevice("connect_wifi", { wifi: got.wifi });
          if (connected !== true) throw "Could not connect. Check the password and try again.";
          state.connected = true;
        } catch (error) { state.connected = false; problem = String(error); }
        finally {
          state.busy = false;
          if (active()) {
            update();
            if (problem) {
              fail(problem, "password");
            } else {
              networks.render();
              const feedback = $("wifi-connected");
              feedback.hidden = false;
              feedbackTimeout = setTimeout(() => { feedback.hidden = true; }, 2500);
              $("next").focus();
            }
          }
        }
      };
      update();
    },
    read() {
      this.keep();
      if (form.cable || form.offline) return;
      const got = wifiOf(form.wifi);
      if (got.error) return [got.error, got.id];
      if (!form.wifi.connected && !form.elsewhere) return ["Connect to test the Wi-Fi first."];
    } },
  { title: "Connections", available: () => !form.offline,
    body: () => connectionChoices(form.public, form.hubs, form.provider === "venice") +
      (host === "desktop" ? `<p class="hint">On the same Wi-Fi, turn on Local Network in Ratspeak. You can also connect by TCP on port ${desktop.lan_port || 4243}.</p>` : ""),
    wire() { wireHubs(form.hubs); wireConnectionHelp(); },
    read() {
      form.public = selectedPublicHubs();
      form.hubs = customHubValues();
      const problem = hubProblem(form.hubs);
      if (problem) $("hub-editor").closest("details").open = true;
      return problem;
    } },
  { title: "Extras", height: 520, available: () => form.provider !== "venice",
    body: () => extraChoices(form.features, {}, { translations: host === "pi" }),
    wire() {
      const next = $("next");
      let asked = "";
      const update = async () => {
        showExtrasSize();
        if (!form.offline || host === "desktop") return;   // a card for a Pi with a network can also leave the downloads to it (Review)
        const libraries = chosenLibraries().map(l => l.id), key = asked = libraries.join();
        next.disabled = true;
        try {
          const plan = await offlineRequirements(libraries);
          if (!next.isConnected || asked !== key) return;
          next.disabled = card.size < plan.card_bytes;
          $("error").textContent = next.disabled ? offlineCapacityMessage(plan.card_bytes) : "";
        } catch { if (next.isConnected && asked === key) fail("Couldn’t check the offline setup size. Go back and try again."); }
      };
      for (const l of LIBRARIES) if ($(l.id)) $(l.id).onchange = update;
      update();
    },
    read() {
      for (const id of EXTRA_IDS()) if ($(id)) form.features[id] = $(id).checked;
      // Raspberry Pi OS, the agent, and a model take up to about 6 GB; libraries go beside them.
      const needed = 6e9 + extrasMB() * 1e6;
      if (host === "pi" && !form.offline && card.size < needed) return [`These extras need a card of ${Math.ceil(needed / 1e9)} GB or more.`, chosenLibraries()[0]?.id];
    } },
  { title: "Review installation", get height() { return piDownloadChoice() ? 552 : 520; },
    body: () => {
      const s = settings();
      const model = (host === "desktop" ? desktop.models : models).find(item => item.id === s.model);
      const rows = [["Name", esc(s.name)], [host === "desktop" ? "Computer" : "Raspberry Pi", esc(host === "desktop" ? desktopDescription() : piDescription())],
        ["Model", s.provider === "venice" ? esc("Venice · " + (form.veniceModels.find(m => m.id === form.veniceModel)?.name || s.venice_model)) : model ? esc(model.name + (model.on_gpu ? ` · On ${desktop.gpu?.name || "GPU"}` : "")) : AUTO_MODEL],
        ["Extras", EXTRA_IDS().filter(id => s.features[id]).map(id => esc(extraName(id))).join(", ") || "None"],
        ["Owner", "Claim in Ratspeak after installation"], ["Network", host === "desktop" ? "This computer’s network" : s.offline ? "Offline" : s.wifi ? `Wi-Fi: ${esc(s.wifi.ssid)}${form.elsewhere ? " (not tested here)" : ""}` : "Ethernet"]];
      if (!s.offline) rows.push(["Connections", ["Local network", ...s.hubs.map(hubName)].map(esc).join(", ")]);
      if (host === "pi" && s.provider === "local") rows.push(["Downloads", `<span id="downloads-source">${downloadsSource()}</span>`]);
      const review = `<div class="group"><dl class="kv">${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join("")}</dl></div>` +
        (piDownloadChoice() ? `<div class="group">${option("pi-downloads", "Download on Pi during first boot", "", form.piDownloads)}</div>` : "") +
        (host === "pi" && s.provider === "venice" ? '<p class="hint">Ratspeak Agent copies your Venice key to this SD card.</p>' : "");
      if (host === "desktop") return review + '<p class="hint">Install downloads the container and starts your agent in Docker.</p>';
      return review;
    },
    wire() {
      const choice = $("pi-downloads");
      let tooSmall = false, asked = 0;
      const update = () => { $("next").disabled = tooSmall; };
      const measure = async () => {
        const ask = ++asked;
        tooSmall = false; $("error").textContent = ""; update();
        if ($("downloads-source")) $("downloads-source").innerHTML = downloadsSource();
        if (!carries()) return;
        try {
          const plan = await offlineRequirements(EXTRA_IDS().filter(id => form.features[id]));
          if (ask !== asked || !$("downloads-source")) return;
          tooSmall = !!card && card.size < plan.card_bytes;
          $("downloads-source").innerHTML = downloadsSource(plan.download_bytes);
          if (tooSmall) fail(form.offline ? offlineCapacityMessage(plan.card_bytes) : carryCapacityMessage(plan.card_bytes));
          update();
        } catch { if (ask === asked && $("downloads-source")) fail("Couldn’t check the download size. Go back and try again."); }
      };
      if (choice) choice.onchange = () => { form.piDownloads = choice.checked; measure(); };
      measure();
    },
    read() {} },
];

function piDownloadChoice() {
  return host === "pi" && form.provider === "local" && !form.offline && !automaticModel() && !card?.resetReceipt;
}
function downloadsSource(bytes) {
  if (!carries()) return automaticModel() ? "Your Pi downloads its model on first boot" : "Your Pi downloads them, so it starts slower";
  return bytes ? `On this card · ${mbLabel(Math.ceil(bytes / 1e6), 1)} from this computer` : "On this card";
}
function carryCapacityMessage(bytes) {
  return `This card is too small to carry the downloads. Use a card of ${Math.ceil(bytes / 1e9)} GB or more, or choose Download on Pi during first boot.`;
}

function settings() {
  const venice = form.provider === "venice";
  return { name: form.name, provider: form.provider, venice_model: venice ? form.veniceModel : null,
           model: venice ? null : form.model, model_options: venice ? [] : form.modelOptions, offline: !venice && form.offline, carry_files: carries(), features: venice ? {} : { ...form.features },
           access: { mode: "later", owners: [] }, hubs: form.offline ? [] : [...new Set([...form.public, ...form.hubs.filter(Boolean)])],
           wifi: host === "desktop" || form.cable || form.offline ? null : wifiOf(form.wifi).wifi || null, elsewhere: host === "pi" && form.elsewhere };
}

function installationSettings() {
  const chosen = settings();
  if (chosen.provider === "venice") chosen.venice_api_key = form.veniceKey;
  return chosen;
}
function wizard() {
  const current = STEPS[step], last = step === STEPS.length - 1;
  const route = () => STEPS.filter(s => !s.available || s.available());
  const move = offset => {
    const pages = route();
    step = STEPS.indexOf(pages[pages.indexOf(current) + offset]);
    wizard();
  };
  const steps = route(), at = steps.indexOf(current);
  show(`<ol class="steps" aria-label="Step ${at + 1} of ${steps.length}">${steps.map((_, i) => `<li class="${i <= at ? "on" : ""}"${i === at ? ` aria-current="step"` : ""}></li>`).join("")}</ol>
    <h1>${current.title}</h1>${current.body()}`,
    `<button id="back">Back</button><span class="spacer"></span>${current.extra ? current.extra() : ""}
     <button id="next" class="primary">${last ? "Install" : "Continue"}</button>`, true, current.height);
  if (current.wire) current.wire();
  const initialName = form.name;
  hasUnsavedChanges = () => step > 0 || !!form.board || ($("name") && $("name").value !== initialName);
  $("back").onclick = () => {
    if (current.back?.()) return;
    if (step === 0) return host === "desktop" ? hostHome() : home();
    current.read(); move(-1);
  };
  $("next").onclick = async () => {
    if (current.beforeNext?.()) return;   // the step used the click itself, as Wi-Fi does to test the network
    const problem = current.read();
    if (problem) return fail(...problem);
    if (!last) return move(1);
    if (host === "desktop") return desktopInstall();
    if (!card.resetReceipt) {
      const holding = card.volumes.length ? `, including ${card.volumes.join(", ")}` : "";
      const answer = await decision("Erase this card?", `Installing erases everything on ${card.name}, ${size(card.size)}${holding}. This can’t be undone.`,
        [["cancel", "Cancel"], ["erase", "Erase and install", "danger"]]);
      if (answer !== "erase" || !$("next")) return;
    }
    const request = { device: card.device, selectedMedia: card.media_id, settings: installationSettings(), expectedCard: selectionKey(card), resetReceipt: card.resetReceipt || null };
    progress();
    try { await invoke("start_flash", request); } catch (error) { wizard(); fail(String(error)); }
  };
}

// ---- Writing the card ----------------------------------------------------------------------------------------------

const flashPhases = () => PHASES.filter(([id]) => id !== "offline" || carries());

function drawProgress(payload) {
  if (!$("bar")) return;
  const phases = flashPhases(), at = phases.findIndex(([id]) => id === payload.phase);
  if (at < 0) return;
  const fraction = payload.total ? Math.min(1, Math.max(0, payload.done / payload.total)) : 0;
  phases.forEach(([id], i) => {
    const row = $(`phase-${id}`);
    row.className = i < at ? "done" : i === at ? "now" : "";
    row.querySelector(".phase-percent").textContent = i === at && payload.total ? ` (${Math.floor(fraction * 100)}%)` : "";
  });
  $("bar").setAttribute("aria-label", phases[at][1]);
  if (payload.total) {
    $("bar").value = fraction;
    $("amount").textContent = `${(payload.done / 1e6).toFixed(0)} of ${(payload.total / 1e6).toFixed(0)} MB`;
  } else { $("bar").removeAttribute("value"); $("amount").textContent = ""; }
}

function progress() {
  const phases = flashPhases();
  show(`<h1>Setting up the card</h1><ol class="phases">${phases.map(([id, label]) =>
    `<li id="phase-${id}"><span class="phase-marker" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="m3.5 8 3 3 6-6"/></svg></span>
      <span>${label}<span class="phase-percent"></span></span></li>`).join("")}</ol>
    <progress id="bar" max="1" value="0"></progress><p class="hint" id="amount"></p>`, "", true, phases.length > 5 ? 420 : 388);
  const first = $(`phase-${phases[0][0]}`);
  first.className = "now";
  first.querySelector(".phase-percent").textContent = " (0%)";
  $("bar").setAttribute("aria-label", phases[0][1]);
}

listen("progress", ({ payload }) => drawProgress(payload));

listen("flash-done", ({ payload }) => {
  if (payload.error) {
    show(`<h1>The card was not set up</h1><div class="warn">${esc(payload.error)}</div>`,
      `<button id="home">Choose another card</button><span class="spacer"></span><button id="again" class="primary">Back</button>`, true, 344);
    $("again").onclick = () => { step = STEPS.length - 1; wizard(); };
    $("home").onclick = home;
    return;
  }
  finished(payload);
});

function finished(payload, resumed = false, initialTab) {
  form.veniceKey = "";
  const chosen = resumed ? {name:payload.name,provider:payload.provider,model:payload.model} : settings();
  show(managementHeading()+managementTabs([["overview","Overview"],["pairing","Pairing"],["settings","Settings"],["agents","Agents"]], {
    overview:`<div class="pi-start" hidden></div>${activityPane()}<div class="pi-after" hidden></div>`, agents:rosterPane(), settings:"",
    pairing: `<p class="pair-note" hidden></p>${payload.pairing_save_failed ? '<p class="hint">Keep this window open until you pair.</p>' : ""}<div class="pi-pairing-content"></div>`
  }),"",false,440);
  watchPi(payload,chosen,initialTab,resumed ? "" : takeOut(payload.ejected));
}

// ---- A card that is already set up ---------------------------------------------------------------------------------

const selectionKey = card => card.ratspeak?.link || card.ratspeak?.selection_key || null;

function managementTabs(tabs, panes) {
  const dashboard = tabs.some(([id])=>id==="agents");
  return `<div class="seg manage-tabs" role="tablist" aria-label="Agent settings">${tabs.map(([id, name], i) =>
    `<button role="tab" id="tab-${id}" aria-controls="pane-${id}" aria-selected="${i === 0}" tabindex="${i === 0 ? 0 : -1}">${name}<i class="dot" hidden><span class="sr">, unsaved changes</span></i></button>`).join("")}</div>
    ${tabs.map(([id], i) => `<div class="fill pane" role="tabpanel" tabindex="0" id="pane-${id}" aria-labelledby="tab-${id}" ${i ? "hidden" : ""}>${dashboard || id === "pairing" || id === "options" ? `<div class="${id==="agents"?"roster-layout":id==="pairing"?"pair-layout":"detail-layout"}">${panes[id]}</div>` : panes[id]}</div>`).join("")}`;
}

function wireTabs(changed = () => {}) {
  const list = view.querySelector("[role=tablist]");
  const tabs = [...list.querySelectorAll("[role=tab]")];
  const panels = [...view.querySelectorAll(".pair-layout,.roster-layout,.detail-layout")];
  const defaultHeight = windowHeight;
  function fitPanel() {
    const panel = panels.find(p => p.isConnected && !p.parentElement.hidden);
    if (!panel || panel.dataset.fillWindow) return;
    const bottom = panel.parentElement.getBoundingClientRect().top + panel.offsetHeight;
    const errorHeight = view.querySelector("#error")?.offsetHeight || 0;
    sizeWindow(Math.ceil(bottom + parseFloat(getComputedStyle(view).paddingBottom) + errorHeight + foot.offsetHeight));
  }
  if (panels.length) {
    const resize = new ResizeObserver(() => { if (!list.isConnected) resize.disconnect(); else fitPanel(); });
    panels.forEach(panel => resize.observe(panel));
    if (view.querySelector("#error")) resize.observe($("error"));
  }
  const open = id => {
    if (!list.isConnected) return;
    const target = $("tab-" + id);
    const previous = list.querySelector('[aria-selected=true]');
    const moveFocus = previous !== target && (previous === document.activeElement || $(previous.getAttribute("aria-controls")).contains(document.activeElement));
    for (const tab of tabs) {
      const selected = tab.id === "tab-" + id;
      tab.setAttribute("aria-selected", String(selected));
      tab.tabIndex = selected ? 0 : -1;
      $(tab.getAttribute("aria-controls")).hidden = !selected;
    }
    $("error").textContent = "";
    changed(id);
    if (panels.length) {
      const activePanel = panels.find(panel=>panel.isConnected && !panel.parentElement.hidden);
      if (activePanel && !activePanel.dataset.fillWindow) fitPanel();
      else sizeWindow(defaultHeight);
    }
    if (moveFocus) target.focus();
  };
  list.onclick = event => {
    const tab = event.target.closest("[role=tab]");
    if (tab) open(tab.id.slice(4));
  };
  list.onkeydown = event => {
    const at = tabs.indexOf(document.activeElement);
    const to = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[event.key];
    if (to === undefined) return;
    const tab = tabs[(to + tabs.length) % tabs.length];
    event.preventDefault(); open(tab.id.slice(4)); tab.focus();
  };
  open.fit = () => { if (panels.length) fitPanel(); };
  return open;
}

async function resetCard(target, key, done) {
  show('<h1>Resetting the SD card</h1><progress data-resetting aria-label="Resetting the partition layout"></progress>', "", true, 280);
  let receipt;
  try {
    receipt = await invoke("reset_card", { device: target.device, selectedMedia: target.media_id, expectedCard: key });
  } catch (error) {
    show(`<h1>Reset didn’t finish</h1><p>${esc(error)}</p>`, '<button id="home">Back to cards</button>', true, 280);
    $("home").onclick = home;
    return;
  }
  card = null; cards = [];
  await done?.();
  show("<h1>SD card reset</h1>", '<button id="home">Back to cards</button><span class="spacer"></span><button class="primary" id="start-setup">Start setup</button>', false, 280);
  $("home").onclick = home;
  $("start-setup").onclick = async event => {
    const button = event.currentTarget;
    button.disabled = true;
    try {
      const freshCards = await invoke("list_cards");
      if (!button.isConnected) return;
      const empty = freshCards.find(c => !c.refused && !c.ratspeak && c.device === target.device && c.size === target.size && c.name === target.name);
      if (!empty) return home();
      card = { ...empty, resetReceipt: receipt }; form = fresh(); step = 0; wizard();
    } catch (error) {
      if (button.isConnected) { fail(String(error)); button.disabled = false; }
    }
  };
}

fetch("mark.svg").then((r) => r.text()).then((svg) => {
  const symbol = new DOMParser().parseFromString(svg, "image/svg+xml").querySelector("symbol");
  if (symbol) { $("mark").innerHTML = symbol.innerHTML; $("mark").setAttribute("fill", "currentColor"); }
}).catch(() => {});
invoke("platform").then((name) => { system = name; document.body.classList.toggle("macos", name === "macos"); }).catch(() => {});
updateControls();
hostHome();

managementEvents().catch(console.error);

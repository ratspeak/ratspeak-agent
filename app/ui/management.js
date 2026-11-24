"use strict";

const countFormat = new Intl.NumberFormat();
function activityPane() {
  return `<section class="activity" aria-label="Agent overview"><header class="model-summary">
    <div><div class="activity-meta"><span class="activity-source"></span><span class="agent-state" role="status" aria-label="Agent status"></span></div><h2 class="activity-model sel"></h2></div><div class="model-emblem" aria-hidden="true">
      <svg class="local-emblem" viewBox="0 0 24 24"><rect x="5" y="5" width="14" height="14" rx="3"/><rect x="9" y="9" width="6" height="6" rx="1"/><path d="M9 2v3m6-3v3M9 19v3m6-3v3M2 9h3m-3 6h3m14-6h3m-3 6h3"/></svg>
      <img class="venice-emblem" src="venice-keys-off-white.svg" alt=""><svg class="hermes-emblem" viewBox="0 0 100 68"><use href="host-devices.svg#hermes"/></svg></div></header>
    <div class="device-ram" hidden><div class="memory-totals"><span>Device memory in use</span><strong></strong></div><meter min="0" max="1" aria-label="Device memory in use"></meter>
      <div class="model-memory" hidden><span title="Resident system RAM used by the local model process">Model RAM <strong></strong></span><span title="Estimated memory available across the device">Available on device <strong></strong></span></div></div>
    <dl class="readings">${[["received","Messages received"],["sent","Replies sent"],["tokens_24h","Tokens (24hr)"],["tokens","Tokens (all)"]].map(([id,label]) =>
      `<div><dt data-label="${id}">${label}</dt><dd data-reading="${id}">—</dd></div>`).join("")}</dl>
    </section>`;
}
function drawActivity(root, agent, note = "") {
  const set = (selector, value) => { const node = root.querySelector(selector); if (node.textContent !== value) node.textContent = value; };
  const cloud = agent.provider === "venice", stats = agent.stats || {};
  root.querySelector(".model-summary").dataset.provider = agent.provider || "local";
  set(".activity-source", agent.provider === "hermes" ? "Hermes · Owner only" : cloud ? "Venice · Cloud model" : "Local model");
  const state = Object.hasOwn(agentLabels, agent.state) ? agent.state : "unknown";
  root.querySelector(".agent-state").dataset.state = state;
  set(".agent-state", agentLabels[state]);
  set(".activity-model", displayedModel(agent) || "Waiting for model");
  for (const id of ["received", "sent", "tokens_24h", "tokens"]) {
    set(`[data-reading="${id}"]`, Number.isSafeInteger(stats[id]) ? countFormat.format(stats[id]) : "—");
  }
  const memory = stats.device_memory;
  const ram = root.querySelector(".device-ram");
  ram.hidden = cloud || !Number.isFinite(memory?.total) || !Number.isFinite(memory?.used)
    || memory.total <= 0 || memory.used < 0 || memory.used > memory.total;
  if (!ram.hidden) {
    const rss = stats.model_resident_bytes;
    const measured = agent.provider === "local" && Number.isSafeInteger(rss) && rss > 0;
    const meter = ram.querySelector("meter");
    ram.querySelector(".memory-totals").hidden = meter.hidden = measured;
    ram.querySelector(".model-memory").hidden = !measured;
    if (measured) {
      set(".model-memory span:first-child strong",formatRam(rss));
      set(".model-memory span:last-child strong",formatRam(memory.total - memory.used));
    } else {
      const unit = memory.total < 2**30 ? 2**20 : 2**30;
      const reading = (memory.used/unit).toLocaleString(undefined,{maximumFractionDigits:unit === 2**20 ? 0 : 1}) + " / " + formatRam(memory.total);
      set(".memory-totals strong",reading);
      meter.value = memory.used / memory.total;
      meter.setAttribute("aria-valuetext",reading);
    }
  }
  const card = root.querySelector(".activity");
  if (note) card.setAttribute("aria-description",note);
  else card.removeAttribute("aria-description");
}
function formatRam(bytes) {
  const mib = bytes < 2**30;
  return (bytes / (mib ? 2**20 : 2**30)).toLocaleString(undefined,{maximumFractionDigits:mib ? 0 : 1}) + (mib ? " MiB" : " GiB");
}
const copyIcon = '<svg aria-hidden="true" viewBox="0 0 20 20"><rect x="7" y="7" width="10" height="11" rx="2"/><path d="M13 5V4a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2"/></svg>';

function pairingPane(agent, pi = false) {
  const pending = agent.access?.mode === "later";
  const link = agent.next_link || agent.link;
  const address = link?.startsWith("lxma://") ? link.slice(7).split(":")[0] : agent.address;
  const shortAddress = address?.length > 20 ? address.slice(0,8) + "…" + address.slice(-8) : address;
  return `<section class="pair-sheet"><div class="pair-scene">
    <h2>${pending ? "Claim your agent" : "Add to Ratspeak"}</h2>
    <p>${pending ? `<b class="step" aria-hidden="true">1</b>Scan this code in Ratspeak to add ${esc(agent.name || "your agent")}.` : "Contacts → Add contact → From QR code"}</p>
    <div class="pair-frame"><div class="pair-image" role="img" aria-label="Agent QR code"></div></div>
    </div>${pending ? `<div class="pair-send"><h3 hidden><b class="step" aria-hidden="true">2</b>Then send ${esc(agent.name || "it")} this code</h3>
      <div class="pair-command" hidden><strong class="pair-digits sel"><span></span><span></span></strong><button class="copy-claim" type="button" aria-label="Copy setup code" title="Copy setup code">${copyIcon}</button></div>
      <p class="code-lifetime" role="timer"><svg class="pair-timer" hidden aria-hidden="true" viewBox="0 0 18 18"><circle cx="9" cy="9" r="6.3"/><circle class="timer-arc" cx="9" cy="9" r="6.3" pathLength="${CODE_PERIOD}" stroke-dasharray="${CODE_PERIOD} ${CODE_PERIOD}"/></svg><span>Waiting for a pairing code…</span></p></div>` : ""}</section>
    ${[[agent.name,agent.name,"Agent name","copy-name"],[address,shortAddress,"Agent address","copy-address"]].filter(([value])=>value).map(([value,shown,label,css])=>
      `<div class="pair-address">${label}<span class="sel ${css==='copy-address'?'address-value':''}" title="${esc(value)}">${esc(shown)}</span><button type="button" class="${css}" title="Copy ${label.toLowerCase()}" aria-label="Copy ${label.toLowerCase()}">${copyIcon}</button></div>`).join("")}
    ${pi ? "" : '<details class="network-details"><summary><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="5.5" r="2.2"/><circle cx="5" cy="17.5" r="2.2"/><circle cx="19" cy="17.5" r="2.2"/><path d="m11 7.5-5 8m7-8 5 8m-11 2h10"/></svg>Network connection</summary><p class="hint lan-detail"></p></details>'}`;
}
function drawPairCode(root, code, remaining, fallback = "Waiting for a pairing code…") {
  const command = root.querySelector(".pair-command");
  if (!command) return;
  const valid = SETUP_CODE.test(code) && remaining > 0;
  command.hidden = !valid;
  root.querySelector(".pair-send h3").hidden = !valid;
  const timer = root.querySelector(".pair-timer");
  timer.toggleAttribute("hidden", !valid);
  if (valid) {
    const digits = command.querySelector(".pair-digits");
    if (digits.textContent !== code) {
      digits.firstElementChild.textContent = code.slice(0,3);
      digits.lastElementChild.textContent = code.slice(3);
    }
    timer.lastElementChild.setAttribute("stroke-dashoffset", CODE_PERIOD - Math.min(CODE_PERIOD,remaining));
  }
  root.querySelector(".code-lifetime span").textContent = valid ? `New code in ${remaining}s` : fallback;
}

view.addEventListener("click", async event => {
  const button = event.target.closest(".copy-claim,.copy-address,.copy-name");
  if (!button) return;
  const metadata = !button.classList.contains("copy-claim");
  const value = button.previousElementSibling;
  const code = value.textContent, text = metadata ? value.title : code.slice(0,3) + " " + code.slice(3);
  if (!metadata && !SETUP_CODE.test(code)) return;
  const label = metadata ? (button.classList.contains("copy-name") ? "Agent name" : "Agent address") : "Setup code";
  try {
    await navigator.clipboard.writeText(text);
    $("announcement").textContent = label + " copied.";
  } catch {
    const range = document.createRange();
    if (metadata) { value.textContent = text; range.selectNodeContents(value); }
    else range.selectNodeContents(value);
    const selection = window.getSelection();
    selection.removeAllRanges(); selection.addRange(range);
    $("announcement").textContent = label + " selected. Use Copy to copy it.";
  }
});

// Same one-minute HMAC-SHA256 counter and 32-symbol alphabet as the receiver (ratspeak_pi/claim.py).
const CODE_PERIOD = 60, CODE_ALPHABET = "123456789ABCDEFGHJKMNPQRSTUVWXYZ";
const SETUP_CODE = /^[1-9A-HJKMNP-Z]{6}$/;
async function piPairCode(key, tick) {
  const counter = new DataView(new ArrayBuffer(8));
  counter.setBigUint64(0, BigInt(tick));
  const bytes = new Uint8Array(await crypto.subtle.sign("HMAC", key, counter.buffer));
  const value = new DataView(bytes.buffer).getUint32(bytes[bytes.length-1] & 15) & 0x3fffffff;
  return [25, 20, 15, 10, 5, 0].map(shift => CODE_ALPHABET[value >>> shift & 31]).join("");
}
let pairingArt;
function drawQR(root, link, name) {
  const scene = root.querySelector(".pair-scene");
  if (scene) {
    // Inline the bundled SVG so the waves follow the window's reduced-motion setting.
    pairingArt ??= fetch("pairing-signal.svg").then(response => {
      if (!response.ok) throw Error("Pairing artwork unavailable");
      return response.text();
    });
    pairingArt.then(svg => {
      if (!scene.isConnected || scene.querySelector(".pair-waves")) return;
      scene.insertAdjacentHTML("afterbegin", svg);
      const art = scene.querySelector(".pair-waves");
      // Each signal gets its own phase, pace and track; resizing still uses the stroke geometry.
      for (const group of art.querySelectorAll(".signal-dots")) {
        const paths = [...art.querySelectorAll(".signal-lines path")];
        for (const dot of group.children) {
          const index = Math.floor(Math.random() * paths.length);
          dot.style.offsetPath = 'path("' + paths.splice(index,1)[0].getAttribute("d") + '")';
          const duration = 9 + Math.random()*7;
          dot.style.animationDuration = duration + "s";
          dot.style.animationDelay = -duration * Math.random() + "s";
        }
      }
    }).catch(() => { pairingArt = undefined; });
  }
  const box = root.querySelector(".pair-image");
  if (!box) return;
  if (!link?.startsWith("lxma://")) { box.textContent = "QR pending"; return; }
  invoke("contact_payload", {link, name: name || ""}).then(payload => {
    if (!box.isConnected) return;
    const canvas = document.createElement("canvas");
    RatspeakQR.render(canvas, payload);
    box.replaceChildren(canvas);
  }, () => { if (box.isConnected) box.textContent = "QR unavailable"; });
}
function decision(title, description, choices) {
  return new Promise(resolve => {
    const dialog = document.createElement("dialog");
    dialog.className = "setup-dialog agent-dialog";
    dialog.setAttribute("aria-label", title);
    dialog.innerHTML = `<h2>${esc(title)}</h2><p>${esc(description)}</p><form method="dialog">${choices.map(([value,label,css]) =>
      `<button value="${esc(value)}" class="${css || ""}">${esc(label)}</button>`).join("")}</form>`;
    const opener = document.activeElement;
    dialog.addEventListener("close", () => {
      const answer = dialog.returnValue; dialog.remove();
      if (opener?.isConnected) opener.focus({preventScroll:true});
      resolve(answer);
    }, {once:true});
    document.body.append(dialog); dialog.showModal();
  });
}
function publicWarning(cloud) {
  return decision("Allow anyone to message your agent?", cloud
    ? "Anyone with its address can use your Venice key through the agent and incur charges."
    : "Anyone with its address can send requests and use this device’s processing time.",
    [["cancel","Cancel"],["public","Allow anyone","primary"]]);
}
function accessPane(agent) {
  const hermes = agent.provider === "hermes";
  if (hermes && !agent.access?.owners?.length) return '<h2 class="pane-heading">Owner only</h2><p>Claim this connection in Ratspeak.</p><button data-pair>View pairing</button>';
  if (!agent.access) return '<p class="hint">Start the agent to read its access settings.</p>';
  if (agent.access.mode === "later") return '<h2 class="pane-heading">Awaiting owner</h2><p>Claim your agent in Ratspeak to manage access.</p><button class="compact-action" data-pair>View pairing</button>';
  if (!agent.access.owners?.length) return '<h2 class="pane-heading">Public access</h2><p>Anyone with this agent’s address can message it.</p>';
  return `<section class="access-card" aria-labelledby="access-heading">
    <header class="access-heading"><h2 id="access-heading">People with access</h2>
      ${hermes ? "" : `<button type="button" id="add-user" aria-controls="add-member" aria-expanded="false">${addMark}<span>Add user</span></button>`}</header>
    ${hermes ? "" : `<form class="add-member" id="add-member" hidden>
      <label class="sr" for="member-address">Ratspeak or LXMF address</label>
      <input id="member-address" type="text" placeholder="Ratspeak or LXMF address" pattern="[a-fA-F0-9]{32}" title="Enter a 32-character hexadecimal address." autocomplete="off" autocapitalize="none" spellcheck="false" required>
      <button class="primary">Add</button></form>`}
    <ul class="member-list" aria-label="People with access"></ul>
    <div class="access-mode"><span class="access-symbol" aria-hidden="true"></span>
      <div><span class="mode-label">Messaging access</span><strong class="mode-value"></strong></div>
      ${hermes ? "" : `<label class="mode-picker"><span aria-hidden="true">Change</span>
        <select id="access-mode" aria-label="Change messaging access"><option value="private">Invited users only</option><option value="public">Anyone with the address</option></select></label>`}
    </div></section>`;
}

function drawAccessMembers(root, agent) {
  const list = root.querySelector(".member-list");
  if (!list) return;
  const hermes = agent.provider === "hermes";
  const owners = hermes ? agent.access.owners.slice(0,1) : agent.access.owners;
  const scroll = list.scrollTop;
  const trash = `<svg viewBox="0 0 24 24" aria-hidden="true">${inspectorIcons.remove}</svg>`;
  list.innerHTML = owners.map((address, i) => `<li>${addressAvatar(address)}<div><b>${i ? "User" : "Owner"}</b><code class="sel">${esc(address)}</code></div>${i ?
    `<button type="button" class="icon-action member-remove" data-remove="${esc(address)}" aria-label="Remove user ${esc(address)}" title="Remove user">${trash}</button>` : ""}</li>`).join("");
  list.tabIndex = owners.length > 3 ? 0 : -1;
  list.scrollTop = scroll;
  const isPublic = !hermes && agent.access.mode === "anyone";
  root.querySelector(".access-symbol").innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true">' + (isPublic
    ? '<circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="4" ry="9"/><path d="M4 9h16M4 15h16"/>'
    : '<rect x="5" y="10" width="14" height="11" rx="2.5"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3"/>') + '</svg>';
  root.querySelector(".mode-value").textContent = hermes ? "Owner only" : isPublic ? "Anyone with the address" : "Invited users only";
  if (!hermes) $("access-mode").value = isPublic ? "public" : "private";
}

function desktopManage(initial, initialTab) {
  let agent = initial, active = "overview", polling = false, version = 0, busy = false;
  let clockOffset = 0;
  const cloud = agent.provider === "venice";
  const kind = agent.provider === "hermes" ? "hermes" : "desktop";
  const tabs = [["overview","Overview"],["pairing","Pairing"],["access","Access"]];
  const local = agent.provider === "local";
  if (cloud) tabs.push(["provider","Venice"]);
  if (local) tabs.push(["extras","Extras"]);
  tabs.push(["agents","Agents"],["contacts",'<span class="a2a-tab-label">A2A<svg class="a2a-tab-lock" viewBox="0 0 16 18" aria-hidden="true"><rect x="3" y="8" width="10" height="8" rx="2"/><path d="M5 8V5a3 3 0 0 1 6 0v3M8 11v2"/></svg></span>']);
  const panes = {overview: activityPane(), pairing:"", access:"", contacts:contactsBetaPane(), agents:rosterPane(), provider: cloud ? veniceFields() + '<div class="end"><button class="primary" id="apply-venice" disabled>Save changes</button></div>' : "",
    extras: local ? extraChoices(agent.features || {}, {}, { translations: LIBRARIES.some(l => spoken(l) && (agent.features?.[l.id] || agent.libraries?.[l.id])) }) + '<div class="end"><button class="primary" id="apply-extras" disabled>Save changes</button></div>' : ""};
  show(managementHeading()+managementTabs(tabs,panes),"",false,584);
  const root = $("pane-overview"), pairing = $("pane-pairing"), access = $("pane-access").querySelector(".detail-layout");
  const contactsRoot = $("pane-contacts").querySelector(".detail-layout");
  contactsRoot.dataset.fillWindow = "true";
  let contacts = null, contactGate = null, contactsReading = false, unlockingContacts = false, contactsVersion = 0;
  let betaRemembered = false, contactNotice = null;
  function setBetaLocked(locked) {
    $("tab-contacts").querySelector(".a2a-tab-lock").toggleAttribute("hidden", !locked);
    $("tab-contacts").setAttribute("aria-label", "A2A, agent-to-agent" + (locked ? ", locked" : ""));
    $("tab-contacts").title = "Agent-to-agent" + (locked ? " · Locked" : "");
  }
  setBetaLocked(true);
  const contactRequest = request => invoke("desktop_contacts", {
    kind, port:agent.lan_port, request:{...request,link:agent.link,current_id:contacts?.currentId()}
  });
  contactRequest.attach = id => invoke("desktop_contact_attach", {kind, port:agent.lan_port, link:agent.link, id});
  contactRequest.save = (id, part) => invoke("desktop_contact_save", {kind, port:agent.lan_port, link:agent.link, id, part});
  function gateAvailability() {
    if (!betaRemembered || contacts) return;
    if (!contactNotice) {
      contactGate = null;
      contactsRoot.innerHTML = '<p class="hint a2a-ready-status" role="status"></p><p class="contacts-beta-error" role="alert"></p>';
      contactNotice = contactsRoot.querySelector(".a2a-ready-status");
    }
    contactNotice.textContent = {running:"Opening A2A…", starting:"Wait for the agent to start.",
      unavailable:"Docker is unavailable. Checking again…"}[agent.state] || "Start the agent to use A2A.";
  }
  function contactProblem(message) {
    if (contactGate || contacts) (contactGate || contacts).problem(message);
    else contactsRoot.querySelector(".contacts-beta-error").textContent = message;
  }
  function showContactGate() {
    if (betaRemembered) { gateAvailability(); return; }
    if (contactGate) return;
    contacts?.dispose(); contacts = null;
    contactsRoot.innerHTML = contactsBetaPane();
    contactGate = wireContactsBeta(contactsRoot, async password => {
      contactsVersion++; unlockingContacts = true;
      try {
        await changeDevice("beta_unlock", {password});
        if (!root.isConnected) return;
        betaRemembered = true;
        setBetaLocked(false);
        // The pane is replaced next; keep focus from falling to the page.
        if (contactsRoot.contains(document.activeElement)) $("tab-contacts").focus();
        gateAvailability();
        $("announcement").textContent = "A2A and Connect Hermes are unlocked on this device.";
      } finally { unlockingContacts = false; }
      pollContacts();
    });
  }
  function drawContacts(next) {
    if (next.beta_locked !== false) {
      setBetaLocked(!betaRemembered);
      showContactGate(); gateAvailability();
      if (next.beta_locked !== true) contactProblem("This agent needs the current receiver to unlock A2A.");
      return;
    }
    betaRemembered = true;
    setBetaLocked(false);
    if (!contacts) {
      contactGate = contactNotice = null;
      contactsRoot.innerHTML = friendsPane();
      contacts = wireFriends(contactsRoot, agent, contactRequest);
    }
    contacts(next);
  }
  showContactGate();
  invoke("beta_unlocked").then(unlocked => {
    if (!root.isConnected || unlockingContacts || betaRemembered) return;
    betaRemembered = unlocked === true;
    setBetaLocked(!betaRemembered);
    if (betaRemembered) { gateAvailability(); pollContacts(); }
  }).catch(() => {});
  async function pollContacts() {
    if (!betaRemembered || (!contacts && agent.state !== "running")) return;
    if (contactsReading || unlockingContacts || active!=="contacts" || !root.isConnected || document.hidden) return;
    contactsReading=true; const revision=contactsVersion;
    try {
      const next = await contactRequest({});
      if (root.isConnected && revision===contactsVersion) drawContacts(next);
    }
    catch(e) { if (root.isConnected && revision===contactsVersion) contactProblem(String(e)); }
    finally { contactsReading=false; }
  }
  let controls;
  const open = wireTabs(id => { active = id; if(id==="agents") controls?.refresh(); if(id==="contacts") pollContacts(); });
  controls = wireDirectory(initial,open,draw);
  const chosenExtras = () => Object.fromEntries(EXTRA_IDS().filter(id => $(id)).map(id => [id, $(id).checked]));
  let savedExtras = JSON.stringify(chosenExtras());
  hasUnsavedChanges = () => !!($("member-address")?.value || $("venice-key")?.value) || JSON.stringify(chosenExtras()) !== savedExtras;
  function drawExtras() {
    if (!local) return;
    for (const library of LIBRARIES) {
      const entry = agent.libraries?.[library.id], hint = $(library.id)?.closest("label").querySelector(".hint");
      if (hint) hint.textContent = entry?.state === "downloading" ? `Downloading ${entry.percent ?? 0}%` : entry?.state === "waiting" ?
        "Waiting to download" : entry?.state === "failed" ? "Download failed" : extraHint(library);
    }
    $("apply-extras").disabled = JSON.stringify(chosenExtras()) === savedExtras || agent.state !== "running";
  }
  let pairKey, accessKey = null, accessLayout, waiting = agent.access?.mode === "later" || !!agent.claim_code;
  function draw(next) {
    const wasWaiting = waiting;
    if (next !== agent && Number.isFinite(next.sampled_at)) clockOffset = next.sampled_at - Math.floor(Date.now()/1000);
    agent = next; desktop.agent = next;
    gateAvailability();
    drawExtras();
    const running = agent.state === "running", started = running || agent.state === "starting";
    const unavailable = agent.state === "unavailable";
    waiting = agent.access?.mode === "later" || !!agent.claim_code;
    const member = agent.access?.owners?.length && !waiting;
    controls.update(agent);
    drawActivity(root, agent, unavailable ? "Docker is unavailable. Checking again…" : !started ? "Start the agent to see current activity." : "");
    const pk = JSON.stringify([agent.link,agent.name,waiting]);
    if (wasWaiting && member && active === "pairing") open("overview");
    if (pk !== pairKey) { pairKey = pk; pairing.querySelector(".pair-layout").innerHTML = pairingPane(agent); drawQR(pairing,agent.link,agent.name); }
    tickCode();
    const discovery = {native:"Local Network is on.",shared:"Local Network is on.",starting:"Starting Local Network.",waiting:"Waiting for Local Network."}[agent.lan_state?.trim()];
    pairing.querySelector(".lan-detail").textContent = (discovery || "Local discovery is unavailable.") + " Manual TCP uses this computer’s LAN address on port " + (agent.lan_port || 4243) + ".";
    const ak = JSON.stringify(agent.access);
    if (ak !== accessKey) {
      accessKey = ak;
      // Keep the live form and native picker when an owner edits the list elsewhere.
      const layout = accessPane(agent);
      if (layout !== accessLayout) {
        const focused = access.contains(document.activeElement);
        accessLayout = layout; access.innerHTML = layout;
        if (focused) $("tab-access").focus({preventScroll:true});
      }
      drawAccessMembers(access,agent);
    }
    for (const control of access.querySelectorAll("button,input,select")) control.disabled = busy || (!running && !control.hasAttribute("data-pair"));
    if (wasWaiting && member) $("announcement").textContent = "Agent claimed.";
  }
  function problem(e) { if (root.isConnected) fail(String(e)); }
  async function poll() {
    if (polling || busy || !root.isConnected || document.hidden) return;
    const ticket = version; polling = true;
    try {
      const next = await invoke("desktop_snapshot",{kind,link:agent.link,port:agent.lan_port});
      if (root.isConnected && ticket === version) draw(next || {...agent,state:"unavailable",stats:{}});
    } catch { if (root.isConnected && ticket === version) draw({...agent,state:"unavailable",stats:{}}); }
    finally { polling = false; }
  }
  async function change(action, address) {
    if (busy) return;
    let added = false;
    const request = {link:agent.link, expected:agent.access, action, address};
    if (action === "public") {
      if (await publicWarning(cloud) !== "public" || !root.isConnected) return;
      request.confirmed = true;
    }
    busy = true; version++; access.setAttribute("aria-busy","true"); draw(agent);
    try {
      const next = await changeDevice("desktop_access",{kind,port:agent.lan_port,request});
      if (root.isConnected) {
        draw(next);
        added = action === "add";
        $("announcement").textContent = {add:"User added.",remove:"User removed."}[action] || "Access updated.";
      }
    } catch(e) { problem(e); }
    finally {
      busy = false; access.removeAttribute("aria-busy");
      if (root.isConnected) {
        draw(agent);
        if (added && $("add-member")) {
          const focus = document.activeElement;
          toggleAdd(false, active === "access" && (focus === document.body || $("add-member").contains(focus)));
        } else if (active === "access" && (document.activeElement === document.body || action === "remove")) {   // the removed row's button is gone
          (action === "add" ? ($("add-member")?.hidden ? $("add-user") : $("member-address")) : action === "remove" ? $("add-user") : $("access-mode"))?.focus();
        }
        poll();
      }
    }
  }
  function toggleAdd(opened, focus = true) {
    const form = $("add-member"), toggle = $("add-user");
    form.hidden = !opened; toggle.setAttribute("aria-expanded",String(opened));
    toggle.querySelector("span").textContent = opened ? "Cancel" : "Add user";
    if (!opened) form.reset();
    if (focus) (opened ? $("member-address") : toggle).focus();
  }
  async function removeUser(address) {
    if (busy) return;
    const shown = address.slice(0,8) + "…" + address.slice(-8);
    if (await decision("Remove this user?", `${shown} can no longer message ${agent.name}.`,
      [["cancel","Cancel"],["remove","Remove user","danger"]]) !== "remove" || !root.isConnected) return;
    change("remove", address);
  }
  access.onclick = e => {
    if (e.target.closest("[data-pair]")) open("pairing");
    if (e.target.closest("#add-user")) toggleAdd($("add-member").hidden);
    const remove = e.target.closest("[data-remove]");
    if (remove) removeUser(remove.dataset.remove);
  };
  access.onkeydown = e => {
    if (e.key === "Escape" && e.target.closest(".add-member")) { e.preventDefault(); toggleAdd(false); }
  };
  access.onchange = e => {
    if (e.target.id !== "access-mode") return;
    const action = e.target.value;
    e.target.value = agent.access.mode === "anyone" ? "public" : "private";
    change(action);
  };
  access.onsubmit = e => { e.preventDefault(); change("add",$("member-address").value.trim().toLowerCase()); };
  if (cloud) {
    const edit = {veniceKey:"",veniceVerified:false,veniceModel:"",veniceModels:[]}, save = $("apply-venice");
    wireVenice(edit, () => { save.disabled = !edit.veniceAvailable || !edit.veniceModel; }, agent.model);
    save.onclick = async () => {
      if (busy) return;
      busy = true; version++; view.inert = foot.inert = true;
      try { const next = await invoke("desktop_configure",{apiKey:edit.veniceKey,model:edit.veniceModel,link:agent.link,port:agent.lan_port}); if (root.isConnected) desktopManage(next,"provider"); }
      catch(e) { problem(e); } finally { busy = false; view.inert = foot.inert = false; }
    };
  }
  if (local) {
    for (const id of EXTRA_IDS()) if ($(id)) $(id).onchange = drawExtras;
    $("apply-extras").onclick = async () => {
      if (busy) return;
      busy = true; view.inert = foot.inert = true;
      const features = chosenExtras();
      try {
        const next = await changeDevice("desktop_extras", {kind, port:agent.lan_port, link:agent.link, features});
        savedExtras = JSON.stringify(features);
        if (root.isConnected) draw(next.agent);
      } catch(e) { problem(e); } finally { busy = false; view.inert = foot.inert = false; if (root.isConnected) drawExtras(); }
    };
  }
  function tickCode() {
    let remaining = agent.claim_expires - Math.floor(Date.now()/1000 + clockOffset);
    let code = agent.claim_code;
    if (remaining <= 0 && remaining > -CODE_PERIOD) { code = agent.next_claim_code; remaining += CODE_PERIOD; }
    drawPairCode(pairing, agent.state === "running" ? code : "", remaining,
      agent.state === "running" ? "Refreshing code…" : "Start the agent to get a pairing code.");
  }
  draw(initial); open(initialTab || (waiting ? "pairing" : "overview"));
  root.addEventListener("update-agent",poll); let nextPoll = 0;
  timer = setInterval(() => {
    tickCode();
    if (Date.now() >= nextPoll) { nextPoll = Date.now() + 4000; if(active==="agents" && !document.hidden) controls.refresh(); poll(); pollContacts(); }
  }, 500);
}

function operationInProgress() {
  return pendingDeviceChanges > 0 || view.inert || !!view.querySelector("#desktop-phase, #phase-write, [data-resetting]");
}
let decidingClose = false;
async function closeDecision(state, quitting) {
  const draft = hasUnsavedChanges() ? " Unsaved changes will be lost if you quit." : "";
  if (state === "idle") {
    if (!draft) return "quit";
    return decision("Quit without saving?", "Your changes will be lost.", [["cancel","Keep editing"],["quit","Discard & quit"]]);
  }
  const both = state === "multiple";
  if (state !== "running" && !both) {
    return decision("Agent status unavailable", "Docker could not be reached to check or stop your agent." + draft,
      [["cancel","Keep open"],["quit","Quit"]]);
  }
  if (quitting) {
    return decision(both ? "Stop running agents and quit?" : "Stop the agent and quit?", (both ? "Your running agents will stop answering messages." : "Your agent will stop answering messages until you start it again.") + draft,
      [["cancel","Cancel"],["stop","Stop & quit","primary"]]);
  }
  const tray = system === "macos" ? "menu bar" : "system tray";
  const available = await invoke("tray_available");
  const restore = available ? `Reopen Ratspeak Agent or choose Show window from the ${tray}.` : "Ratspeak Agent will minimize. Reopen its window to return here.";
  return decision(both ? "Keep your agents running?" : "Keep your agent running?", (both ? "Your agents can answer messages in the background. " : "It can answer messages in the background. ") + restore + draft,
    [["cancel","Cancel"],["stop","Stop & quit"],["background","Keep running","primary"]]);
}
async function handleAgentClose({payload: quitting} = {}) {
  if (decidingClose || document.querySelector("dialog[open]")) return;
  decidingClose = true;
  try {
    if (operationInProgress()) {
      await decision("Ratspeak Agent is working", "Wait for this operation to finish before closing the window.", [["ok","Keep open","primary"]]); return;
    }
    const state = await invoke("close_state");
    if (operationInProgress() || document.querySelector("dialog[open]")) return;
    const choice = await closeDecision(state, quitting);
    if (!["quit","stop","background"].includes(choice)) return;
    if (choice === "stop") {
      view.inert = foot.inert = true;
      $("announcement").textContent = "Stopping your agent…";
      try { await invoke("close_choice",{choice}); }
      finally { view.inert = foot.inert = false; }
    } else await invoke("close_choice",{choice});
  } catch(e) { await decision("Couldn’t close Ratspeak Agent", String(e), [["ok","Keep open","primary"]]); }
  finally { decidingClose = false; }
}
async function managementEvents() {
  await Promise.all([listen("agent-close-request",handleAgentClose),
    listen("pi-status",({payload})=>{ if (piScreen?.address === payload.address) piScreen.update(payload); }),

    listen("agent-state-changed",()=> $("pane-overview")?.dispatchEvent(new Event("update-agent"))),
    listen("agent-power-error",({payload})=>decision("Couldn’t change agent status",String(payload),[["ok","Close"]]))]);
  const resume = await invoke("window_ready");
  const saved = selectedBefore();
  if ((resume || saved) && $("host-desktop") && !creatingAgent) {
    const firstScreen = $("host-desktop");
    try {
      directory = await loadDirectory();
      if (!firstScreen.isConnected || creatingAgent) return;
      const next=directory.find(row=>row.link===saved?.link) || directory[0];
      if(next) await openAgent(next);
    } catch { /* The chooser remains available when Docker is offline. */ }
  }
}

const hexBytes = text => Uint8Array.from(text.match(/../g) || [], byte => parseInt(byte,16));
const hexString = bytes => Array.from(bytes,byte => byte.toString(16).padStart(2,"0")).join("");

// ---- A Pi set up from this computer --------------------------------------------------------------------------------
// The app checks each Pi in the background (pi_watch.rs) and sends "pi-status"; the open Pi's screen draws it.
let piScreen = null;
function sinceLabel(at, now = Date.now() / 1000) {
  const minutes = Math.floor(Math.max(0, now - at) / 60);
  return minutes < 1 ? "just now" : `${minutes} min ago`;
}
const joinParts = (...parts) => parts.filter(Boolean).join(" · ");
const listFormat = new Intl.ListFormat("en", {type:"conjunction"});
function piStageLine(status) {
  const model = status.model_name || "the model", card = status.from_card;
  return {network:"Waiting for the internet", unpack:"Unpacking", packages:"Installing system packages",
    llama:card ? "Installing llama.cpp" : "Downloading llama.cpp", model:card ? `Copying ${model}` : `Downloading ${model}`,
    venice:"Testing the Venice model", speed:"Testing its speed", identity:"Creating its Ratspeak identity",
    starting:"Starting the agent"}[status.stage] || "Installing";
}
// One description of a Pi that isn't answering yet, so Overview's card and the Pairing tile agree.
function piProgress(reading, now) {
  const status = reading.status || {};
  const written = reading.written_at ? `Card written ${sinceLabel(reading.written_at, now)}` : "";
  // The Pairing tile shows only the step; Overview carries the detail.
  if (reading.state === "booting") return {phase:"Starting up", bar:"busy", meta:"First boot takes a moment. You may be prompted to allow finding the device.", tile:""};
  if (reading.state === "unseen") return {phase:"Taking longer than expected", meta:written, tile:""};
  if (reading.state === "failed") return {phase:"Setup didn’t finish", error:true, meta:status.detail || "", tile:""};
  const mb = bytes => mbLabel(Math.round(bytes / 1e6), 1);
  const stage = status.stages ? `Stage ${status.stage_index} of ${status.stages}` : "";
  const bytes = status.total > 0 ? `${mb(status.done || 0)} of ${mb(status.total)}` : "";
  const percent = Number.isFinite(status.percent) ? Math.floor(status.percent) : null;
  const phase = reading.state === "unreachable" ? `Not heard from for ${Math.max(1, Math.floor((now - reading.seen_at) / 60))} min` : piStageLine(status);
  return {phase, bar:percent, meta:joinParts(stage, bytes), tile:"", band:joinParts(stage, bytes)};
}
// Phase line, bar and one detail line, in Overview's install card or the Pairing tile.
function drawPiProgress(root, progress, detail, name) {
  const phase = root.querySelector(".install-phase"), bar = root.querySelector("progress"), line = root.querySelector(".install-detail");
  if (phase.textContent !== progress.phase) phase.textContent = progress.phase;
  phase.classList.toggle("error", !!progress.error);
  phase.classList.toggle("hint", !progress.error && root.classList.contains("installation-progress"));
  bar.hidden = progress.bar == null;
  bar.setAttribute("aria-label", `${name} setup`);
  if (progress.bar === "busy") { bar.removeAttribute("value"); bar.removeAttribute("aria-valuetext"); }
  else if (progress.bar != null) {
    bar.max = 100; bar.value = progress.bar;
    bar.setAttribute("aria-valuetext", `${progress.bar}%`);
  }
  if (line.textContent !== detail) line.textContent = detail;
}
const lightHint = '<p class="hint light-hint"><svg class="led-mark" viewBox="0 0 16 16" aria-hidden="true" focusable="false"><circle cx="8" cy="8" r="3.5"/></svg><span>The green light will stay solid when it’s complete.</span></p>';
const recoveryRow = (text, button, id = "") => `<div class="agent-recovery"><p class="hint"${id ? ` id="${id}"` : ""}>${text}</p>${id ? button.replace("<button", `<button aria-describedby="${id}"`) : button}</div>`;
const showCodeButton = '<button type="button" class="quiet show-code">Show code</button>';
const showLogButton = '<button type="button" class="quiet" data-card-log>Show log</button>';
// The QR's box while the Pi isn't ready: same card and footprint, no scan brackets.
const pairWaitTile = () => `<div class="pair-frame"><div class="pair-wait">${hostArt("pi")}<progress></progress>
  <p class="install-phase" role="status"></p><p class="hint install-detail" aria-live="off"></p></div></div>`;

function watchPi(payload, chosen, initialTab, takeOutText = "") {
  const pane = $("pane-pairing"), overview = $("pane-overview");
  const start = overview.querySelector(".pi-start"), after = overview.querySelector(".pi-after"), activity = overview.querySelector(".activity");
  const note = pane.querySelector(".pair-note");
  const address = payload.link.slice(7).split(":")[0];
  const link = payload.next_link || payload.link;
  // Without a hostname and key, or for a Pi with no network or on a network elsewhere, Setup can't hear from it: the code shows from the start.
  const watched = !!payload.hostname && /^[0-9a-f]{64}$/.test(payload.monitor_key || "") && !payload.next_link && !payload.offline && !payload.elsewhere;
  let active, pending = !payload.claimed, revealed = !watched, layout = "", heldKey = "";
  let reading = {state:"unknown"}, skew = 0, generation = -1, digits = "";
  const removeCard = !takeOutText ? "" : !payload.ejected ? "Eject the SD card, then remove it."
    : system === "windows" ? "You can remove the SD card now. If Windows offers to format it, choose Cancel." : "You can remove the SD card now.";
  const pi = { ...chosen, kind:"pi",link:payload.link,state:"unknown" };
  const name = () => chosen.name;
  let controls, settings;
  const open = wireTabs(tab => { active=tab; if(tab==="agents") controls?.refresh(); settings?.active(tab === "settings"); });
  controls = wireDirectory(pi, open, next=>{
    chosen.name = payload.name = next.name;
    controls.update(next);
    renderPair();
    paint();
  });
  settings = piSettings({pane: $("pane-settings"), payload, chosen, refit: () => open.fit(), onCard: () => { layout = ""; paint(); }});
  hasUnsavedChanges = () => settings.dirty();
  for (const root of [overview, pane]) root.addEventListener("click", event => {
    if (event.target.closest("[data-card-log]")) { open("settings"); settings.showLog(); }
    else if (event.target.closest("[data-change-wifi]")) { open("settings"); settings.openSub("network"); }
    else if (event.target.closest("[data-retry]")) settings.retry().then(() => {
      $("announcement").textContent = `Retry saved. Put the card back in ${name()}.`;
      overview.querySelector("[data-card-log]")?.focus({preventScroll:true});
    }, error => fail(String(error)));
    else if (event.target.closest(".show-code")) showCode();
    else if (event.target.closest("[data-pair]")) open("pairing");
  });
  const installing = () => ["booting","installing","unseen","failed"].includes(reading.state) || reading.state === "unreachable" && ["installing","failed"].includes(reading.status?.phase);
  const held = () => pending && watched && !revealed && installing();
  function renderPair() {
    const target = pane.querySelector(".pi-pairing-content");
    target.innerHTML = pairingPane({...chosen, link, access:{mode:pending ? "later" : "owners"}},true);
    heldKey = "";
    drawQR(target,link,name());
    hold();
  }
  // Swap the QR box and the code band for the waiting tile and one line, keeping the scene and its signal lines.
  function hold() {
    const sheet = pane.querySelector(".pair-sheet");
    if (!sheet || !pending) return;
    const want = held();
    if (want === sheet.classList.contains("held")) return;
    const template = document.createElement("template");
    if (want) {
      template.innerHTML = pairWaitTile() + '<div class="pair-send"></div>';
      sheet.querySelector(".pair-frame").replaceWith(template.content.firstElementChild);
      sheet.querySelector(".pair-send").replaceWith(template.content.lastElementChild);
      sheet.classList.remove("revealed"); sheet.classList.add("held");
      heldKey = "";
    } else {
      template.innerHTML = pairingPane({...chosen, link, access:{mode:"later"}},true);
      const fresh = template.content.querySelector(".pair-sheet");
      for (const part of [".pair-scene > h2", ".pair-scene > p", ".pair-frame", ".pair-send"]) sheet.querySelector(part).replaceWith(fresh.querySelector(part));
      sheet.classList.remove("held"); sheet.classList.add("revealed");
      drawQR(sheet.parentElement, link, name());
      generation = -1; tickCode();
    }
  }
  function drawHeld(progress) {
    const sheet = pane.querySelector(".pair-sheet.held");
    if (!sheet) return;
    const failed = reading.state === "failed" || reading.status?.phase === "failed";
    const [title, subtitle] = failed ? [`${name()} isn’t ready to claim`, "Its code appears here once it’s set up."]
      : reading.state === "booting" ? [`Waiting for ${name()} to boot`, "Its code appears here when it’s ready."]
      : reading.state === "unseen" ? [`Still waiting for ${name()}`, "Ratspeak Agent hasn’t heard from it yet."]
      : [`${name()} is installing`, "Its code appears here when it’s ready."];
    const heading = sheet.querySelector(".pair-scene > h2"), line = sheet.querySelector(".pair-scene > p");
    if (heading.textContent !== title) heading.textContent = title;
    if (line.textContent !== subtitle) line.textContent = subtitle;
    const booting = reading.state === "booting";
    drawPiProgress(sheet.querySelector(".pair-wait"), booting ? {...progress, phase:""} : progress, progress.tile, name());
    const key = failed ? "failed:" + progress.meta : booting ? "booting" : reading.state === "unseen" ? "escape" : "band";
    const band = sheet.querySelector(".pair-send");
    if (key !== heldKey) {
      heldKey = key;
      if (band.contains(document.activeElement)) pane.focus();
      band.innerHTML = failed ? `<p class="hint">${esc(progress.meta || "Put the card back in this computer to see the install log.")}</p>${showLogButton}`
        : booting ? (removeCard ? `<p class="hint">${esc(removeCard)}</p>` : "")
        : key === "escape" ? `<p class="hint" id="pair-escape">On another network?</p>${showCodeButton.replace("<button", '<button aria-describedby="pair-escape"')}`
        : '<p class="hint" aria-live="off"></p>';
      band.hidden = !band.innerHTML;
    }
    if (key === "band") band.firstElementChild.textContent = progress.band || "";
  }
  // A stopped install's next steps need its card: change its Wi-Fi, retry, or read its log.
  function failedRows() {
    const found = settings.found();
    if (!found) return recoveryRow("Put the card back in this computer to retry, change its Wi-Fi, or see its log.", "");
    const retry = found.network_retry, network = settings.data().network;
    return (retry && network?.kind === "wifi" ? recoveryRow(`It was set up for <b>${esc(network.ssid)}</b>.`, '<button type="button" class="quiet" data-change-wifi>Change Wi-Fi</button>') : "")
      + (retry ? retry.queued ? recoveryRow(`Retry saved. Put the card back in ${esc(name())}.`, "")
        : recoveryRow("Network fixed? It tries again on its next boot.", '<button type="button" class="quiet" data-retry>Retry</button>') : "")
      + (found.log ? recoveryRow("Its install log is on the card.", showLogButton) : "");
  }
  function drawStart(progress) {
    const kind = reading.state === "unreachable" ? "installing" : reading.state;
    const found = settings.found();
    const key = kind === "failed" ? `failed:${found ? JSON.stringify([found.network_retry, !!found.log]) : "out"}` : kind;
    if (layout !== key) {
      layout = key;
      const card = installCard("pi", name(), `${name()} setup`);
      start.innerHTML = kind === "booting" ? card + lightHint
        : kind === "installing" ? card
        : kind === "unseen" ? card + lightHint + recoveryRow("On another network? It appears in Ratspeak when it’s ready.", showCodeButton, "overview-escape")
          + recoveryRow("On this network? Check its power and connection. Its log is on the card.", showLogButton)
        : card + failedRows();
    }
    start.querySelector(".installation-progress strong").textContent = name();
    drawPiProgress(start.querySelector(".installation-progress"), progress, progress.meta, name());
  }
  function drawAnswering() {
    const here = settings.cardIn() && !["ready","online"].includes(reading.state);
    const status = here ? {...(reading.status || {}), ...settings.data()} : reading.status || {};
    const shownAgent = {...pi,...status,name:name(),state:here ? "card" : reading.state};
    drawActivity(overview, shownAgent, here ? "Showing the reading saved on its card." : reading.state === "unreachable" ? "Showing the last reading from your Pi." : "");
    const rows = [];
    if (!watched && !settings.cardIn()) rows.push('<p class="hint">Reinsert the card to see the Pi’s saved status.</p>');
    if (pending && reading.state === "ready") rows.push(recoveryRow("Send its setup code in Ratspeak to claim it.", '<button type="button" class="quiet" data-pair>View pairing</button>'));
    const downloading = (status.downloading || []).filter(item => item.label);
    if (downloading.length && ["ready","online"].includes(reading.state)) {
      const percents = downloading.map(item => item.percent).filter(Number.isFinite);
      const percent = percents.length ? ` (${Math.floor(percents.reduce((a, b) => a + b, 0) / percents.length)}%)` : "";
      rows.push(`<p class="hint">${esc(listFormat.format(downloading.map(item => item.label)))} ${downloading.length > 1 || downloading[0].id === "voice" ? "are" : "is"} still downloading${percent}. Your agent already answers.</p>`);
    }
    const html = rows.join("");
    if (after.innerHTML !== html) after.innerHTML = html;
    after.hidden = !html;
  }
  // The card knows an install stopped even when Setup never heard from the Pi, as for a card opened from Start.
  function seen() {
    const d = settings.data();
    return ["unknown","booting","unseen","unreachable"].includes(reading.state) && d.state === "failed" && d.installed === false
      ? {...reading, state:"failed", status:{...(reading.status || {}), phase:"failed", detail:d.detail}} : reading;
  }
  function paint() {
    if (!pane.isConnected) return;
    const heard = reading;
    reading = seen();
    try { draw(); } finally { reading = heard; }
  }
  function draw() {
    const now = Date.now() / 1000;
    const pre = (watched || reading.state === "failed") && installing();
    start.hidden = !pre; activity.hidden = pre;
    if (pre) { after.hidden = true; drawStart(piProgress(reading, now)); }
    else { layout = ""; drawAnswering(); }
    hold();
    if (held()) drawHeld(piProgress(reading, now));
    const caveat = pending && revealed && watched && installing() ? `Ratspeak Agent can’t see ${name()}. Send the code once it appears in Ratspeak.` : "";
    // The waiting card says the card can come out; a card Setup can't watch says it above its code. The QR says "ready".
    const placeCard = pending && !watched ? removeCard : "";
    const renewed = payload.next_link ? `${name()} uses this new code after its next boot.` : "";
    const text = renewed || caveat || placeCard;
    note.hidden = !text; note.className = "pair-note " + (caveat || renewed ? "hint" : "good");
    if (note.textContent !== text) note.textContent = text;
    const state = settings.cardIn() && !["ready","online","installing","failed"].includes(reading.state) ? "card" : reading.state;
    controls.update({...pi, name:name(), state, percent:reading.percent, seen_at:reading.seen_at, model:reading.status?.model_name || pi.model});
  }
  function update(next) {
    if (!pane.isConnected) return;
    const previous = reading.state;
    reading = next;
    if (next.status?.name && chosen.name !== next.status.name) { chosen.name = next.status.name; renderPair(); }
    if (Number.isFinite(next.status?.sampled_at)) skew = next.status.sampled_at - Date.now()/1000;
    // Show code lasts until Setup hears from the Pi; then the tab follows its state.
    if (next.status && ["installing","failed"].includes(next.status.phase) && watched) revealed = false;
    if (pending && next.state === "ready" && previous !== "ready" && watched) {
      revealed = true;
      $("announcement").textContent = `${name()} is ready. Send its setup code in Ratspeak.`;
    }
    if (pending && next.state === "online") {
      pending = false; payload.claimed = true; payload.claim_secret = null;
      if (active === "pairing") open("overview");
      renderPair(); $("announcement").textContent = "Agent claimed.";
    }
    paint();
  }
  async function showCode() {
    revealed = true; open("pairing"); paint();
    await tickCode();
    pane.querySelector(".copy-claim")?.focus();
    $("announcement").textContent = `Code shown. Send it once ${name()} appears in Ratspeak.`;
  }
  const secret = pending && /^[0-9a-f]{64}$/.test(payload.claim_secret) && globalThis.crypto?.subtle
    ? crypto.subtle.importKey("raw",hexBytes(payload.claim_secret),{name:"HMAC",hash:"SHA-256"},false,["sign"]) : null;
  let coding = Promise.resolve();
  function tickCode() {
    if (!pending || !pane.isConnected) return coding;
    const draw = () => { const now = Math.floor(Date.now()/1000 + skew); drawPairCode(pane,digits,CODE_PERIOD - now % CODE_PERIOD); };
    const tick = Math.floor((Date.now()/1000 + skew)/CODE_PERIOD);
    if (secret && generation !== tick) {
      generation = tick; digits = "";
      coding = secret.then(key => piPairCode(key,tick)).then(next => { if (generation === tick && pending) digits = next; }, () => { generation = -1; })
        .then(() => { if (pending && pane.isConnected) draw(); });
    }
    draw();
    return coding;
  }
  piScreen = {address, update};
  if (chosen.provider !== "venice" && chosen.model && !chosen.model_name) loadModels().then(catalog => {
    const model = catalog.rungs.find(m => m.id === chosen.model);
    if (model && pane.isConnected) { chosen.model_name = pi.model_name = model.name; paint(); }
  }).catch(() => {});
  renderPair(); tickCode();
  if (watched) {
    reading = {state:payload.written_at ? "booting" : "unknown", written_at:payload.written_at, estimate:payload.estimate};
    invoke("pi_readings").then(all => {
      if (!pane.isConnected) return;
      const mine = all.readings.find(r => r.address === address);
      if (mine) update(mine); else paint();
    }).catch(() => {});
  }
  paint();
  open(initialTab || (pending ? "pairing" : "overview"));
  timer = setInterval(() => { tickCode(); paint(); }, 500);
}

"use strict";

// One Pi's Settings tab. Values come from its card while the card is in this computer, otherwise from the last card
// read. Edits wait across sub-views and go to the card in one save; the Pi applies them on its next boot.

let lastSavedPi = null;   // the address just saved to, so "Saved to the card" survives the redraw
const CARD_ICON = WAITING.match(/<svg[\s\S]*?<\/svg>/)[0].replace('class="card-icon"', 'class="card-icon" focusable="false"');
const BACK_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="m15 5-7 7 7 7"/></svg>';
const settingsList = new Intl.ListFormat("en", {type:"conjunction"});

function networkLabel(network) {
  if (!network) return "Unknown";
  return network.kind === "wifi" ? `Wi-Fi: ${network.ssid}` : network.kind === "none" ? "No network" : "Ethernet";
}
function logLabel(end) {
  const when = end?.at?.match(/^\w{3} (\w{3}) +(\d{1,2}) (\d{2}):(\d{2})/);
  const at = when ? `${when[1]} ${when[2]}, ${when[3]}:${when[4]}` : "";
  return end?.done ? `Finished ${at}`.trim() : `Didn’t finish${at ? " · " + at : ""}`;
}

function piSettings({pane, payload, chosen, refit, onCard}) {
  const root = pane.querySelector(".detail-layout");
  const address = payload.link.slice(7).split(":")[0];
  const name = () => chosen.name;
  let found = null, count = 0, sub = null, saving = false, active = false, snapshot = payload.card || null, polling = false, first = true;
  const wifi = freshWifi();
  const veniceEdit = { veniceKey: "", veniceVerified: false, veniceModels: [], veniceModel: "" };
  const staged = {};
  const cardIn = () => !!found && count === 1;
  const data = () => found?.ratspeak || snapshot || {};
  const stopped = () => data().installed === false && data().state !== "new";
  const announce = text => { $("announcement").textContent = text; };

  const current = {
    hubs: () => data().pending?.hubs ?? data().hubs ?? [],
    features: () => data().pending?.features ?? data().features ?? {},
    model: () => data().pending?.model ?? data().model_id ?? data().model,
    access: () => data().pending?.access ?? data().access,
  };
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  /** The edits to save, or what keeps them from saving. */
  function changes() {
    const out = {};
    if (wifi.net) {
      const got = wifiOf(wifi);
      if (got.error) return { error: got.error, id: got.id, sub: "network" };
      out.wifi = got.wifi;
    }
    if (staged.hubsProblem) return { error: staged.hubsProblem[0], id: staged.hubsProblem[1], sub: "connections" };
    if (staged.hubs && !same([...staged.hubs].sort(), [...current.hubs()].sort())) out.hubs = staged.hubs;
    if (staged.model && staged.model !== current.model()) out.model = staged.model;
    if (staged.features && !same(staged.features, current.features())) out.features = staged.features;
    if (staged.access && !same(staged.access, current.access())) out.access = staged.access;
    if (veniceEdit.veniceKey) {
      if (!veniceEdit.veniceVerified) return { error: "Verify the API key to save.", id: "venice-key", sub: "model" };
      if (!veniceEdit.veniceAvailable) return { error: "Venice has paused inference for this key. Reverify to save.", id: "verify-key", sub: "model" };
      if (!veniceEdit.veniceModel) return { error: "Choose a Venice model to save.", id: "venice-model", sub: "model" };
      out.venice_model = veniceEdit.veniceModel; out.venice_api_key = veniceEdit.veniceKey;
    }
    return { changes: out };
  }
  const dirty = () => { const now = changes(); return !!now.error || Object.keys(now.changes).length > 0; };
  const edited = key => {
    const now = changes().changes || {};
    return { network: !!wifi.net, connections: "hubs" in now, model: "model" in now || "features" in now || "venice_model" in now, access: "access" in now }[key];
  };

  function strip() {
    const n = esc(name());
    if (count > 1) return ["absent", `Two cards match ${n}. Remove one.`];
    if (data().apply_failed) return [cardIn() ? "present failed" : "absent failed", `${n} couldn’t apply the last change. Its log is on the card.`];
    if (cardIn()) return ["present", `${n}’s SD card is in this computer. Changes you save apply on its next boot.`];
    if (dirty()) return ["absent", `Put ${n}’s SD card back in to save your changes.`];
    if (lastSavedPi === address) return ["saved", `<b>Saved to the card.</b> You can take it out and put it back in ${n}.`];
    return ["absent", `To change or erase anything here, put ${n}’s SD card in this computer.`];
  }

  function listHtml() {
    const d = data(), edit = cardIn(), pending = d.pending || {};
    const dot = key => edited(key) ? '<i class="dot"><span class="sr">, unsaved change</span></i>' : "";
    const change = (key, label) => edit ? `<button type="button" class="quiet" data-sub="${key}" aria-label="Change ${label}">Change</button>` : "";
    const danger = (kind, text) => edit ? `<button type="button" class="quiet-danger" data-erase="${kind}">${text}</button>` : "";
    const row = (label, value, line, action, o = {}) => `<dt>${label}</dt><dd><span${o.soft ? ' class="hint"' : ""}>${esc(value)}${o.dot || ""}${line
      ? `<small${o.next ? ' class="next"' : ""}>${esc(line)}</small>` : ""}</span>${action || ""}</dd>`;
    const next = (key, isStaged) => pending[key] !== undefined && !isStaged;
    const rows = [];
    const network = wifi.net ? { kind: "wifi", ssid: wifiOf(wifi).wifi?.ssid || "…" } : pending.wifi?.ssid ? { kind: "wifi", ssid: pending.wifi.ssid } : d.network;
    rows.push(row("Network", networkLabel(network), next("wifi", wifi.net) ? "Next boot" : "", change("network", "network"), { next: true, dot: dot("network") }));
    if (!stopped()) {
      if (!d.offline) {
        const hubs = staged.hubs ?? current.hubs();
        rows.push(row("Connections", settingsList.format(["Local network", ...hubs.map(hubName)]), next("hubs", staged.hubs) ? "Next boot" : "",
          change("connections", "connections"), { next: true, dot: dot("connections") }));
      }
      const cloud = d.provider === "venice";
      const id = staged.model ?? current.model();
      const model = cloud ? `Venice · ${veniceEdit.veniceModel || d.pending?.venice_model || d.venice_model || d.model || "model"}`
        : models.find(m => m.id === id)?.name || d.model || (id ? id : "Chosen on first boot");
      const features = staged.features ?? current.features();
      const extras = cloud ? [] : LIBRARIES.filter(l => extraOn(l, features)).map(l => heard(l) ? "voice messages" : spoken(l) ? `${l.label} translation` : l.label);
      const modelLine = next("model", staged.model) || next("features", staged.features) ? "Next boot" : extras.length ? `With ${settingsList.format(extras)}` : "";
      rows.push(row("Model", model, modelLine, change("model", "model"), { next: modelLine === "Next boot", dot: dot("model") }));
      const access = staged.access ?? current.access();
      if (!access || access.mode === "later") {
        rows.push(row("Access", "Not claimed yet", "", '<button type="button" class="quiet" data-pair>View pairing</button>'));
      } else {
        const others = Math.max(0, (access.owners?.length || 1) - 1);
        rows.push(row("Access", access.mode === "anyone" ? "Anyone with the address" : "Invited users only",
          next("access", staged.access) ? "Next boot" : others ? `Owner and ${others} ${others === 1 ? "user" : "users"}` : access.mode === "anyone" ? "" : "Owner only",
          change("access", "access"), { next: next("access", staged.access), dot: dot("access") }));
      }
    }
    if (d.log_end || found?.log) {
      rows.push(row("Install log", d.log_end ? logLabel(d.log_end) : "On the card", "",
        edit && found.log ? '<button type="button" class="quiet" data-sub="log" aria-label="Show install log">Show</button>' : ""));
    }
    const erase = [];
    if (!stopped()) {
      const chats = d.state === "new" ? ["No chats yet", ""] : d.chat_erasure === "queued" ? ["Erasing on its next boot", ""]
        : !d.can_clear_chats ? ["Needs newer Pi software", ""] : d.chat_erasure === "failed" ? ["Couldn’t erase. Try again.", danger("chats", "Erase chats")]
        : ["Every conversation and its files", danger("chats", "Erase chats")];
      const identity = d.next_link ? ["New address on its next boot", '<button type="button" class="quiet" data-pair>Show QR</button>']
        : !d.can_reset_identity ? ["Needs newer Pi software", ""] : d.identity_retry ? ["Put the card back in the Pi to finish.", ""]
        : ["A new address. Everyone adds it again.", danger("identity", "New identity")];
      erase.push(row("Chat history", chats[0], "", chats[1], { soft: true }), row("Identity", identity[0], "", identity[1], { soft: true }));
    }
    erase.push(row("SD card", "Everything on it, to set it up again", "", danger("reset", "Reset card"), { soft: true }));
    return `<div class="group"><dl class="kv settings">${rows.join("")}</dl></div>
      <div class="hub-heading settings-heading"><h3>Erase and reset</h3></div><div class="group"><dl class="kv settings">${erase.join("")}</dl></div>`;
  }

  function subHtml(key) {
    const d = data(), pending = d.pending || {};
    const head = title => `<button type="button" class="quiet sub-back" data-back>${BACK_ICON}Settings</button><h2 class="sub-title" tabindex="-1">${title}</h2>`;
    if (key === "network") return head("Network") + `<div class="group"><dl class="kv settings"><dt>Current</dt><dd><span>${esc(networkLabel(d.network))}</span></dd>
      ${pending.wifi?.ssid ? `<dt>Next boot</dt><dd><span>${esc("Wi-Fi: " + pending.wifi.ssid)}</span></dd>` : ""}</dl></div>${pickerHtml("Change Wi-Fi")}
      <p class="hint">Ethernet works whenever it’s plugged in.</p>`;
    if (key === "connections") {
      const hubs = staged.hubs ?? current.hubs(), known = PUBLIC.map(([, a]) => a);
      return head("Connections") + connectionChoices(hubs, hubs.filter(h => !known.includes(h)), d.provider === "venice");
    }
    if (key === "model") {
      if (d.provider === "venice") return head("Model") + `<p class="field">Venice</p><p class="hint">${esc(d.venice_model || d.model || "")} · Internet required</p>
        <details class="edit-cloud"><summary>Change model or API key</summary>${veniceFields()}</details>`;
      return head("Model and extras") + '<div id="settings-model"></div>' + extraChoices(staged.features ?? current.features());
    }
    if (key === "access") return head("Access") + accessPane({ provider: d.provider, access: staged.access ?? current.access() })
      .replace('<h2 id="access-heading">People with access</h2>', '<h3 id="access-heading">People with access</h3>');
    if (key === "log") return head("Install log") + `<pre class="log-view" tabindex="0" aria-label="Install log">${esc(found?.log || "")}</pre>
      <div class="log-actions"><p class="hint">${esc(logLabel(d.log_end))}</p>${cardIn() ? '<button type="button" class="quiet-danger" data-erase="log">Clear log</button>' : ""}</div>`;
    return "";
  }

  function wireSub(key) {
    const d = data(), update = () => { render(false); };
    if (key === "network") picker(wifi, update);
    if (key === "connections") {
      const known = PUBLIC.map(([, a]) => a);
      const read = () => {
        const custom = customHubValues();
        staged.hubsProblem = hubProblem(custom) || null;
        staged.hubs = [...new Set([...selectedPublicHubs(), ...custom.filter(Boolean)])];
        update();
      };
      wireHubs((staged.hubs ?? current.hubs()).filter(h => !known.includes(h)), read);
      wireConnectionHelp();
      root.oninput = root.onchange = event => { if (event.target.closest(".hub-editor, .group")) read(); };
    }
    if (key === "model") {
      if (d.provider === "venice") { wireVenice(veniceEdit, update, d.venice_model || d.model); return; }
      const id = current.model();
      if (d.model_options?.length > 1) loadModels().then(catalog => {
        const box = $("settings-model");
        if (!box) return;
        const offered = d.model_options.map(m => catalog.rungs.find(r => r.id === m)).filter(Boolean);
        if (!offered.some(m => m.id === id)) return;
        box.innerHTML = field("change-model", "Model", `<select id="change-model">${offered.map((m, i) =>
          `<option value="${esc(m.id)}" ${m.id === (staged.model ?? id) ? "selected" : ""}>${esc(m.name)}${i === 0 ? " (recommended)" : ""}</option>`).join("")}</select>`);
      }).catch(() => {});
      root.onchange = event => {
        if (event.target.id === "change-model") staged.model = event.target.value;
        const features = { ...current.features() };
        for (const l of LIBRARIES) if ($(l.id)) features[l.id] = $(l.id).checked;
        staged.features = features;
        update();
      };
    }
    if (key === "access") {
      const draw = () => drawAccessMembers(root, { provider: d.provider, access: staged.access ?? current.access() });
      const edit = fn => { const access = structuredClone(staged.access ?? current.access()); fn(access); staged.access = access; draw(); update(); };
      draw();
      const form = $("add-member"), toggle = $("add-user");
      if (toggle) toggle.onclick = () => { form.hidden = !form.hidden; toggle.setAttribute("aria-expanded", String(!form.hidden)); if (!form.hidden) $("member-address").focus(); };
      if (form) form.onsubmit = event => {
        event.preventDefault();
        const value = $("member-address").value.trim().toLowerCase();
        if (!/^[a-f0-9]{32}$/.test(value)) return fail("Enter a 32-character Ratspeak address.", "member-address");
        edit(access => { if (!access.owners.includes(value)) access.owners.push(value); });
        $("member-address").value = ""; form.hidden = true; toggle.setAttribute("aria-expanded", "false"); toggle.focus();
      };
      root.onclick = event => {
        const remove = event.target.closest("[data-remove]");
        if (remove) edit(access => { access.owners = access.owners.filter(a => a !== remove.dataset.remove); });
      };
      if ($("access-mode")) $("access-mode").onchange = async () => {
        const select = $("access-mode"), wanted = select.value === "public";
        if (wanted && (staged.access ?? current.access())?.mode !== "anyone" && await publicWarning(d.provider === "venice") !== "public") { select.value = "private"; select.focus(); return; }
        edit(access => { access.mode = wanted ? "anyone" : "owners"; });
      };
    }
  }

  function drawFoot() {
    const want = active && (cardIn() && sub && sub !== "log" || dirty());
    if (!want) {
      if (foot.dataset.settings) { foot.innerHTML = ""; delete foot.dataset.settings; refit(); }
    } else {
      if (!foot.dataset.settings) {
        foot.dataset.settings = "1";
        foot.innerHTML = '<button type="button" class="quiet" id="discard">Discard changes</button><span class="spacer"></span><button type="button" class="primary" id="save">Save to the card</button>';
        $("discard").onclick = discard;
        $("save").onclick = save;
        refit();
      }
      $("discard").hidden = !dirty();
      if (!saving) $("save").disabled = !cardIn() || !dirty();
    }
    $("tab-settings").querySelector(".dot").hidden = !dirty();
  }

  /** Redraw the status line and, on the list, the rows. A sub-view keeps its editor while the card comes and goes. */
  function render(list = true) {
    if (!root.isConnected) return;
    $("context").textContent = cardIn() ? `${found.name}, ${size(found.size)}` : "";
    $("context").title = $("context").textContent;
    const [kind, text] = strip();
    const top = root.querySelector(".card-strip");
    const html = `<div class="card-strip ${kind}"${sub && kind === "present" ? " hidden" : ""}>${CARD_ICON}<p>${text}</p></div>`;
    const hadFocus = root.contains(document.activeElement);
    if (!top) root.innerHTML = html + '<div class="settings-body"></div>';
    else if (top.outerHTML !== html) top.outerHTML = html;
    const body = root.querySelector(".settings-body");
    if (!sub && list) {
      const focused = document.activeElement?.closest("[data-sub],[data-erase],[data-pair]");
      const key = focused && (focused.dataset.sub || focused.dataset.erase || "pair");
      const next = listHtml();
      if (body.innerHTML !== next) body.innerHTML = next;
      if (key) body.querySelector(`[data-sub="${key}"],[data-erase="${key}"],[data-pair]`)?.focus({preventScroll:true});
    }
    if (hadFocus && !root.contains(document.activeElement)) pane.focus({preventScroll:true});
    drawFoot();
  }

  function openSub(key) {
    if (!root.isConnected) return;
    sub = key;
    root.oninput = root.onchange = root.onclick = null;
    root.querySelector(".settings-body").innerHTML = subHtml(key);
    wireSub(key);
    root.querySelector(".sub-title").focus();
    render(false);
  }
  function closeSub() {
    const from = sub;
    sub = null;
    root.oninput = root.onchange = root.onclick = null;
    render();
    root.querySelector(`[data-sub="${from}"]`)?.focus();
  }

  async function discard() {
    if (await decision(`Discard changes to ${name()}?`, "Your changes haven’t been saved to the card.", [["keep", "Keep editing"], ["discard", "Discard", "danger"]]) !== "discard") return;
    Object.assign(wifi, freshWifi());
    for (const key of Object.keys(staged)) delete staged[key];
    Object.assign(veniceEdit, { veniceKey: "", veniceVerified: false, veniceModel: "" });
    if (sub) closeSub(); else render();
  }

  async function save() {
    const now = changes();
    if (now.error) { if (sub !== now.sub) openSub(now.sub); return fail(now.error, now.id); }
    const target = found, button = $("save");
    if (!target || !button) return;
    saving = true; button.disabled = true; button.textContent = "Saving…"; button.setAttribute("aria-busy", "true");
    view.inert = foot.inert = true;
    announce("Saving to the card…");
    try {
      await invoke("save_changes", { device: target.device, selectedMedia: target.media_id, expectedLink: payload.link, changes: now.changes });
      lastSavedPi = address;
      const record = await invoke("read_pi_pairing", { address });
      view.inert = foot.inert = false; saving = false;
      if (!root.isConnected) return;
      finished(record, true, "settings");
      $("pane-settings")?.focus({preventScroll:true});
      announce("Saved to the card. You can take it out.");
    } catch (error) {
      view.inert = foot.inert = false; saving = false;
      if (!button.isConnected) return;
      button.textContent = "Save to the card"; button.removeAttribute("aria-busy"); drawFoot();
      fail(String(error)); button.focus();
    }
  }

  async function erase(kind, opener) {
    const target = found, n = name();
    if (!target || !cardIn()) return;
    const ask = {
      chats: ["Erase all chats?", `${n} clears every conversation and its files on its next boot. Its model and extras stay.`, "Erase chats"],
      identity: [`Give ${n} a new identity?`, "It gets a new address on its next boot. Everyone who uses it adds it again. Chats and settings stay.", "New identity"],
      log: ["Clear the install log?", `Removes ${n}’s install log from this SD card.`, "Clear log"],
      reset: ["Reset this SD card?", `Removes everything on ${target.name} (${size(target.size)}): ${n}’s identity, chats, and settings. Ratspeak Agent forgets it on this computer. This can’t be undone.`, "Reset card"],
    }[kind];
    if (await decision(ask[0], ask[1], [["cancel", "Cancel"], ["go", ask[2], "danger"]]) !== "go" || !root.isConnected) { opener?.focus(); return; }
    const key = selectionKey(target);
    if (kind === "reset") return resetCard(target, key, () => invoke("forget_pi", { address }).catch(() => {}));
    try {
      await changeDevice("erase_pi_data", { device: target.device, selectedMedia: target.media_id, expectedCard: key, action: kind === "log" ? "setup_log" : kind });
      if (!root.isConnected) return;
      if (kind === "identity") {
        const record = await invoke("read_pi_pairing", { address });
        finished(record, true, "settings");
        $("pane-settings")?.focus({preventScroll:true});
        announce("New identity saved to the card.");
        return;
      }
      if (kind === "chats") { target.ratspeak.chat_erasure = "queued"; announce("Chats will be erased on its next boot."); }
      else { target.log = ""; announce("Install log cleared."); if (sub === "log") closeSub(); }
      render();
    } catch (error) { if (root.isConnected) fail(String(error)); }
  }

  root.addEventListener("click", event => {
    const button = event.target.closest("button");
    if (!button || !root.contains(button)) return;
    if (button.dataset.sub) openSub(button.dataset.sub);
    else if (button.dataset.erase) erase(button.dataset.erase, button);
    else if ("back" in button.dataset) closeSub();
  });
  const escape = event => {
    if (event.key === "Escape" && sub && active && !event.target.matches("input:not([type=checkbox],[type=radio]), textarea, select")) { event.preventDefault(); closeSub(); }
  };
  pane.addEventListener("keydown", escape);
  foot.addEventListener("keydown", event => { if (root.isConnected) escape(event); });

  async function poll() {
    if (!root.isConnected) { clearInterval(watcher); return; }
    if (saving || polling) return;
    polling = true;
    try {
      const cards = await invoke("list_cards");
      if (!root.isConnected) return;
      const mine = cards.filter(c => !c.refused && c.media_id && c.ratspeak?.link === payload.link);
      const next = mine.length === 1 ? mine[0] : null, was = cardIn();
      const before = JSON.stringify([found?.device, found?.ratspeak, found?.log, found?.network_retry, count]);
      found = next; count = mine.length;
      if (next && !was) {
        lastSavedPi = null;
        if (!first) announce(`${name()}’s SD card is in.`);
        if (form.ram === "" && next.ratspeak.ram_mb > 0) form.ram = String(2 ** Math.ceil(Math.log2(next.ratspeak.ram_mb / 1024)));
        invoke("remember_sd_agent", { device: next.device, selectedMedia: next.media_id, link: payload.link })
          .then(record => { if (record?.card) snapshot = record.card; }).catch(() => {});
      } else if (!next && was) announce(`${name()}’s SD card was removed.`);
      if (before !== JSON.stringify([found?.device, found?.ratspeak, found?.log, found?.network_retry, count])) { render(!sub); onCard(cardIn() ? found : null); }
    } catch { /* the next poll tries again */ }
    finally { polling = false; first = false; }
  }
  const watcher = setInterval(poll, 3000);
  loadModels().then(() => render(!sub)).catch(() => {});
  librariesRequest.then(() => render(!sub));
  render();
  poll();

  return {
    cardIn, found: () => cardIn() ? found : null, data,
    active(on) {
      active = on;
      if (!on && lastSavedPi === address) { lastSavedPi = null; render(!sub); }
      drawFoot();
    },
    showLog() { if (cardIn() && found.log) openSub("log"); },
    openSub,
    dirty,
    retry: async () => {
      const target = found;
      if (!target?.network_retry) return;
      await changeDevice("retry_setup", { device: target.device, selectedMedia: target.media_id, attempt: target.network_retry.attempt });
      target.network_retry.queued = true;
      onCard(found);
    },
  };
}

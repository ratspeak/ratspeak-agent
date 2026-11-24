"use strict";

const agentLabels = {running:"Running",starting:"Starting…",stopped:"Stopped",unknown:"Status unknown",unavailable:"Unavailable",online:"Online",unreachable:"Unreachable",saved:"Saved on card",
  booting:"Starting up",installing:"Installing",ready:"Ready",unseen:"Not seen yet",failed:"Setup failed",card:"Card in this computer"};
const namePencil = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m4 16-1 5 5-1L20 8l-4-4Z M14 6l4 4"/></svg>';
const addMark = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v16M4 12h16"/></svg>';
const inspectorIcons = {
  start: '<path d="M7 4.5 19 12 7 19.5Z"/>',
  stop: '<rect x="4.5" y="4.5" width="15" height="15" rx="2" fill="currentColor" stroke="none"/>',
  remove: '<path d="M4 6.5h16M9 6.5V4h6v2.5M6.5 7l.9 13h9.2l.9-13M10 10.5v6m4-6v6"/>',
  forget: '<path d="m6 13-1.5 1.5a3.5 3.5 0 0 0 5 5L12 17m0-10 2.5-2.5a3.5 3.5 0 0 1 5 5L17 12M3.5 7.5h4v-4m13 13h-4v4"/>'
};
let directory = [], creatingAgent = false, creatorReturn = null, creationAttempt = null;

function displayedModel(agent) {
  if (agent.provider === "venice") return agent.model || agent.venice_model || agent.model_name;
  return agent.model_name || agent.model;
}
function publicAgent(agent) {
  const kind = agent.kind || (agent.provider === "hermes" ? "hermes" : "desktop");
  return {kind, link:agent.link, name:agent.name || "Agent", provider:agent.provider, model:displayedModel(agent), state:agent.state || "unknown", lan_port:agent.lan_port,
    percent:Number.isFinite(agent.percent) ? agent.percent : undefined, seen_at:agent.seen_at || undefined};
}
function agentType(row) {
  if (row.kind === "pi") return "Raspberry Pi";
  return row.provider === "venice" ? "Venice" : row.kind === "hermes" ? "Hermes" : "Local model";
}
function avatar(row) {
  return addressAvatar(row.link?.slice(7).split(":")[0]);
}
function addressAvatar(value) {
  const address = value?.toLowerCase();
  return /^[a-f0-9]{32}$/.test(address) ? '<img class="lxmface" alt="" src="data:image/svg+xml,' + encodeURIComponent(lxmface(address,40)) + '">' : "";
}
let savedSelection;
function rememberSelection(row) {
  if (!row.link) return;
  const json=JSON.stringify(publicAgent(row));
  if (json===savedSelection) return;
  try { localStorage.setItem("current-agent",json); savedSelection=json; } catch {}
}
function selectedBefore() {
  try { return JSON.parse(localStorage.getItem("current-agent")); } catch { return null; }
}
async function loadDirectory() {
  const result = await invoke("managed_agents");
  const rows = result.agents.map(publicAgent);
  if (!result.desktop_available) {
    const last = selectedBefore();
    const known = last && !directory.some(r=>r.link===last.link) ? [...directory,last] : directory;
    rows.push(...known.filter(r=>r.kind !== "pi").map(r=>({...r,state:"unavailable"})));
  }
  return rows;
}
function managementHeading() {
  return '<div class="agent-heading"><h1><details class="agent-switch"><summary aria-label="Switch agent"><span class="current-name"></span><i></i></summary><div class="agent-picker"></div></details></h1>'+
    '<button type="button" class="rename-agent icon-action" aria-label="Rename agent" title="Rename agent">'+namePencil+'</button></div>';
}
function rosterPane() {
  return `<div class="agent-browser">
    <div class="agent-rail"><div class="agent-list" role="group" aria-label="Your agents"></div>
      <button type="button" class="add-agent">${addMark}Add agent</button></div>
    <section class="agent-inspector" aria-labelledby="inspected-name"></section></div>`;
}
function stateLabel(row) {
  const label = agentLabels[row.state] || agentLabels.unknown;
  return row.state === "installing" && Number.isFinite(row.percent) ? `${label} ${Math.floor(row.percent)}%` : label;
}
function agentState(row) {
  return `<span class="agent-state" data-state="${esc(row.state)}">${esc(stateLabel(row))}</span>`;
}
function inspector(row) {
  const pi = row.kind === "pi", running = ["running","starting"].includes(row.state);
  const disabled = row.state === "unavailable" ? "disabled" : "";
  const address = row.link?.slice(7).split(":")[0] || "";
  const action = (key, label, drawing) => `<button type="button" class="icon-action" data-action="${key}" aria-label="${label}" title="${label}" ${disabled}>${drawing}</button>`;
  const symbol = key => `<svg class="${key}-symbol" viewBox="0 0 24 24" aria-hidden="true">${inspectorIcons[key]}</svg>`;
  return `<header class="inspector-heading">${avatar(row)}
    <div class="inspector-identity"><h2 id="inspected-name">${esc(row.name)}</h2><span>${esc(agentType(row))}</span></div>
    <div class="inspector-actions" role="group" aria-label="Actions for ${esc(row.name)}">
      ${action("rename", "Rename agent", namePencil)}
      ${pi ? "" : action("power", running ? "Stop agent" : "Start agent", symbol(running ? "stop" : "start"))}
      ${action("remove", pi ? "Forget Pi" : "Remove agent", symbol(pi ? "forget" : "remove"))}
    </div></header>
    <dl class="agent-facts"><div><dt>Status</dt><dd>${agentState(row)}</dd></div>
    ${row.state === "unreachable" && row.seen_at ? `<div><dt>Last seen</dt><dd>${esc(sinceLabel(row.seen_at))}</dd></div>` : ""}
    <div><dt>Model</dt><dd class="sel">${esc(row.model || "Unavailable")}</dd></div>
    <div><dt>Address</dt><dd><code class="sel" title="${esc(address)}">${esc(address.slice(0,6))}…${esc(address.slice(-6))}</code><button class="copy-address" aria-label="Copy agent address" title="Copy agent address">${copyIcon}</button></dd></div></dl>
    ${!pi && disabled ? '<div class="agent-recovery"><p class="hint">Docker is unavailable.</p><button type="button" data-action="reconnect">Reconnect</button></div>' : ''}`;
}
function beginAgent(current) {
  creationAttempt = hexString(crypto.getRandomValues(new Uint8Array(16)));
  creatingAgent = true; creatorReturn = publicAgent(current); hostHome();
}
let navigationRequest=0;
async function openAgent(row, tab) {
  const request=++navigationRequest;
  const screen = view.firstElementChild;
  const isHere = () => navigationRequest===request && view.firstElementChild === screen;
  try {
    if (row.kind === "pi") {
      const record = await invoke("read_pi_pairing",{address:row.link.slice(7).split(":")[0]});
      if (isHere()) { host="pi"; card=null; finished(record,true,tab); }
    } else {
      const info = await invoke("desktop_status",{kind:row.kind,link:row.link,port:row.lan_port});
      if (!isHere()) return;
      if (!info.agent && info.runtime_ready) throw Error("This agent is no longer installed.");
      if (!info.agent) info.agent={...row,state:"unavailable",stats:{}};
      host="desktop"; connectingHermes=row.kind==="hermes"; desktop=info; card=null;
      desktopManage(info.agent,tab);
    }
  } catch(error) { if (isHere()) fail(String(error)); }
}
async function askName(row) {
  const d=document.createElement("dialog");
  d.className="setup-dialog agent-dialog rename-dialog";
  const pi = row.kind === "pi";
  d.innerHTML='<h2 id="rename-title">Rename agent</h2><form class="rename-form"><label class="sr" for="agent-new-name">Agent name</label>'+
    '<input type="text" id="agent-new-name" maxlength="32" required pattern="[A-Za-z0-9][A-Za-z0-9 ._\\-]{0,31}" autocomplete="off">'+
    (pi ? '<p class="hint">Insert this Pi’s SD card. It will announce the new name on its next boot.</p>' : '')+
    '<div><button type="button">Cancel</button><button class="primary">Save</button></div></form>';
  d.setAttribute("aria-labelledby","rename-title");
  document.body.append(d); const input=d.querySelector("input");input.value=row.name;
  d.querySelector("form").onsubmit=e=>{e.preventDefault(); d.close("save");};
  d.querySelector("button").onclick=()=>d.close();
  const answer=new Promise(resolve=>d.addEventListener("close",()=>{const value=d.returnValue==="save"?input.value.trim():null;d.remove();resolve(value);},{once:true}));
  d.showModal();input.select();return answer;
}

function wireDirectory(initial, openTab, refreshCurrent) {
  const heading = view.querySelector(".agent-heading"), picker = heading.querySelector(".agent-switch");
  const browser = $("pane-agents").querySelector(".agent-browser");
  const list = browser.querySelector(".agent-list"), detail = browser.querySelector(".agent-inspector");
  let current = publicAgent(initial), busy = false, ticket = 0, signature = "";
  const names = heading.querySelector(".current-name");
  creatingAgent=false; creatorReturn=null;
  rememberSelection(current);
  const rowFor = link => directory.find(row=>row.link === link);
  function paint() {
    if (!list.isConnected) return;
    names.textContent=current.name;
    heading.querySelector(".rename-agent").disabled=current.state==="unavailable";
    const rows=directory.map(row=>row.link === current.link ? current : row);
    if (!rows.some(row=>row.link === current.link)) rows.unshift(current);
    const key=JSON.stringify(rows);
    if (signature === key) return;
    signature=key;
    const focused=document.activeElement?.closest("button");
    const owner = [list,detail,picker.querySelector(".agent-picker")].find(el => el.contains(focused));
    const scroll = list.scrollTop;
    picker.querySelector(".agent-picker").innerHTML=rows.map(row=>
      `<button data-link="${esc(row.link)}" data-action="select" ${row.link===current.link?'aria-current="true"':""}>${avatar(row)}<span>${esc(row.name)}</span>${row.link===current.link?"✓":""}</button>`).join("")+
      `<button class="picker-create" data-action="create">${addMark}Create new</button>`;
    list.innerHTML=rows.map(row=>{
      const selected = row.link===current.link;
      return `<button type="button" class="select-agent" data-action="select" data-link="${esc(row.link)}" ${selected?'aria-current="true"':""}>
        ${avatar(row)}<span class="agent-list-label"><b>${esc(row.name)}</b>${agentState(row)}</span>
        ${selected?'<span class="selected-mark" aria-hidden="true">✓</span>':""}</button>`;
    }).join("");
    detail.innerHTML=inspector(current);
    list.scrollTop = scroll;
    if(owner) [...owner.querySelectorAll("button")].find(b=>b.dataset.action===focused.dataset.action && b.dataset.link===focused.dataset.link)?.focus({preventScroll:true});
  }
  async function refresh() {
    const seq=++ticket;
    try { const rows=await loadDirectory(); if(list.isConnected && seq===ticket) { directory=rows;paint(); } }
    catch(e) { if(list.isConnected && seq===ticket) fail(String(e)); }
  }
  async function act(action,row,fromList = false) {
    if (busy || !list.isConnected) return;
    picker.open=false;
    if (action==="create") return beginAgent(current);
    if (!row) return;
    if (action==="select") {
      if (!fromList) return row.link === current.link ? openTab("overview") : openAgent(row);
      if (row.link === current.link) return;
      await openAgent(row,"agents");
      const selected = $("pane-agents")?.querySelector('.select-agent[aria-current="true"]');
      if (selected?.dataset.link === row.link) selected.focus();
      return;
    }
    const args={kind:row.kind,link:row.link,port:row.lan_port};
    busy=true;
    try {
      if (action==="reconnect") {
        fail("");
        const button=detail.querySelector('[data-action="reconnect"]');
        if(button) { button.disabled=true;button.textContent="Reconnecting…"; }
        const next=await changeDevice("reconnect_desktop_agent",args);
        if(list.isConnected && row.link===current.link) refreshCurrent(next);
      } else if (action==="rename") {
        const name=await askName(row);
        if(!name || name===row.name || !list.isConnected) return;
        if(row.kind==="pi") {
          const matches=(await invoke("list_cards")).filter(c=>!c.refused && c.ratspeak?.link === row.link && c.media_id);
          if(matches.length!==1) throw Error("Insert the SD card for "+row.name+" and try again.");
          await changeDevice("save_changes",{device:matches[0].device,selectedMedia:matches[0].media_id,changes:{name},expectedLink:row.link});
          if(list.isConnected) {
            if(row.link===current.link) refreshCurrent({...current,name});
            $("announcement").textContent="New name saved for the Pi’s next boot.";
          }
        } else {
          const next=await changeDevice("desktop_rename",{...args,name}); if(list.isConnected && row.link===current.link) refreshCurrent(next);
        }
      } else if(action==="power") {
        const running=!["running","starting"].includes(row.state);
        const info=await changeDevice("desktop_set_running",{...args,running});
        if(list.isConnected && row.link===current.link) refreshCurrent(info.agent);
      } else if(action==="remove") {
        const pi=row.kind==="pi";
        const plan=pi?null:await invoke("desktop_removal_plan",args);
        if(!list.isConnected) return;
        const detail=pi?"Ratspeak Agent stops listing it on this computer. Its SD card and agent keep working. To wipe it, use Erase and reset in Settings.":
          (row.kind==="hermes"?"Removes this Ratspeak receiver and its pairing. Your Hermes profile stays installed.":
          "Permanently deletes its address, chats, and "+(row.provider==="venice"?"saved Venice key.":"downloaded model."));
        if(await decision((pi?"Forget ":"Remove ")+row.name+"?",detail,[["cancel","Cancel"],["remove",pi?"Forget Pi":"Remove agent","danger"]])!=="remove" || !list.isConnected) return;
        if(pi) await changeDevice("forget_pi",{address:row.link.slice(7).split(":")[0]});
        else await changeDevice("desktop_remove_agent",{...args,plan});
        if(!list.isConnected)return;
        if(row.link===current.link) {
          localStorage.removeItem("current-agent"); savedSelection=undefined;
          directory=(await loadDirectory()).filter(r=>r.link!==row.link);
          if(directory.length) return openAgent(directory[0],"agents");
          creatingAgent=false;creatorReturn=null;return hostHome();
        }
      }
      await refresh();
    } catch(e) { if(list.isConnected) fail(String(e)); }
    finally {
      busy=false;
      const button=detail.querySelector('[data-action="reconnect"]');
      if(button) { button.disabled=false;button.textContent="Reconnect"; }
    }
  }
  const onClick=e=>{
    const button=e.target.closest("button[data-action]"); if(!button)return;
    const row = detail.contains(button) || button.dataset.link===current.link ? current : rowFor(button.dataset.link);
    act(button.dataset.action,row,list.contains(button));
  };
  browser.onclick=picker.onclick=onClick;
  heading.querySelector(".rename-agent").onclick=()=>act("rename",current);
  $("pane-agents").querySelector(".add-agent").onclick=()=>act("create");
  picker.ontoggle=()=>{if(picker.open) refresh();};
  paint();refresh();
  return {refresh,update(agent){current=publicAgent(agent);rememberSelection(current);paint();}};
}

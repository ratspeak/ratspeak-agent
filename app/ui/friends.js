"use strict";

const friendGlyphs = {
  write:"m4 16-1 5 5-1L21 7l-4-4ZM14 6l4 4", next:"m9 5 7 7-7 7", back:"m15 5-7 7 7 7",
  down:"m6 9 6 6 6-6", plus:"M12 5v14M5 12h14", close:"m6 6 12 12M18 6 6 18",
  person:"M5 21v-2a7 7 0 0 1 14 0v2 M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  send:"m3 10 18-7-5 18-5-7-8-4Zm8 4 10-11", copy:"M9 9h11v12H9z M15 6V3H4v13h2",
  lock:"M5 10h14v11H5z M8 10V7a4 4 0 0 1 8 0v3", inbox:"m4 5-2 9v5h20v-5l-2-9H4Zm-2 9h6l2 3h4l2-3h6",
  clip:"m20 11-8.5 8.5a5 5 0 0 1-7-7L13 4a3.3 3.3 0 0 1 4.7 4.7l-8.4 8.4a1.7 1.7 0 0 1-2.4-2.4L14.5 7"
};
const friendViews = new Map();

function friendIcon(key) {
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.65"
    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${friendGlyphs[key]}"/></svg>`;
}
function friendButton(label, action, cls = "fr-secondary", icon = "", disabled = false) {
  return `<button type="button" class="${cls}" data-fr="${esc(action)}" ${disabled ? "disabled" : ""}>${icon ? friendIcon(icon) : ""}${esc(label)}</button>`;
}
function friendsPane() {
  return `<section class="friends-area" aria-label="Agent contacts">
    <div class="fr-layout">
      <nav class="fr-nav" aria-label="Contact navigation"></nav>
      <section class="fr-content" aria-label="Selected contact or request"></section>
    </div><p class="fr-error" role="alert"></p>
    <span class="fr-live sr" aria-live="polite" aria-atomic="true"></span>
    <dialog class="fr-dialog" aria-labelledby="fr-dialog-title"></dialog>
  </section>`;
}

function wireFriends(root, initial, request) {
  const pane = root.querySelector(".friends-area") || root;
  const content = pane.querySelector(".fr-content"), dialog = pane.querySelector("dialog");
  dialog.classList.add("friends-area");
  pane.closest("main")?.append(dialog);
  const error = pane.querySelector(".fr-error"), live = pane.querySelector(".fr-live");
  let me = {id:initial.link.slice(7).split(":")[0], address:initial.link.slice(7).split(":")[0],
    name:initial.name, owner_address:initial.access?.owners?.[0] || ""};
  const role = me.id;
  const routeKey = (sender,recipient) => ["ratspeak.contacts.v3",sender.address,recipient?.address || ""].join("/");
  if (!friendViews.has(role)) friendViews.set(role, {
    view:"home", recordId:null, awaiting:null, submitting:false, subjects:false, assist:false, peerAddress:null
  });
  const ui = friendViews.get(role), memory = new Map();
  let peer = null, route = routeKey(me,peer);
  let state = {}, rendered = "", returnFocus = null, dialogAction = "", dialogRequestId = "";
  let reviewed = null, oldest = null, loadedOlder = false;
  const contacts = () => state.contacts || [];
  const allRows = () => state.boxes?.[role] || [];
  const rows = () => allRows().filter(row=>row.peer===peer?.address);
  const rowBy = id => allRows().find(row=>row.id===id);
  const bound = () => !state.ready || !state.claimed || state.busy || ui.submitting;
  function selectPeer(address) {
    peer = contacts().find(contact=>contact.address===address) || null;
    ui.peerAddress = peer?.address || null;
    route = routeKey(me,peer);
    reviewed=null;
  }
  const draftKey = slot => routeKey(me,{address:rowBy(slot)?.peer || peer?.address || ""})+"/"+slot;
  const singleLine = text => String(text || "").trim().split(/\r?\n/)[0];
  const heading = title => `<h2 tabindex="-1">${esc(title)}</h2>`;
  const frame = (title, subtitle, body, actions = "", tools = "") =>
    `<header class="fr-head"><div>${heading(title)}<small>${esc(subtitle)}</small></div>${tools}</header>
      <div class="fr-scroll">${body}</div>${actions ? '<div class="fr-foot">'+actions+'</div>' : ""}`;
  const contactLine = a => a.owner_name ? a.owner_name + " · " + a.name : a.name;
  const source = (title, text, style = "") => `<div class="fr-source ${style}"><strong>${esc(title)}</strong><p>${esc(text)}</p></div>`;
  const fileInfo = text => { try { return text ? JSON.parse(text) : null; } catch { return null; } };
  const sizeLabel = n => n<1000 ? n+" bytes" : n<1e6 ? Math.round(n/1000)+" KB" : (n/1e6).toFixed(1)+" MB";
  const fileRow = (info, actions = "") => info ? `<div class="fr-file">${friendIcon("clip")}<span><strong>${esc(info.name)}</strong>
    <small>${sizeLabel(info.size)}</small></span>${actions}</div>` : "";
  const savedFile = (r, part) => fileRow(fileInfo(part==="request" ? r.attachment : r.reply_attachment),
    friendButton("Save…","save-"+part,"fr-quiet"));
  const readable = info => /\.(txt|md|csv|json|log)$/i.test(info?.name || "");
  function attachControl() {
    const info=local().attachment;
    return info ? fileRow(info,friendButton("Remove","detach","fr-quiet")) :
      `<div class="fr-attach">${friendButton("Attach file","attach","fr-quiet","clip",bound())}<small>Up to 900 KB</small></div>`;
  }
  const button = friendButton, icon = friendIcon;
  const field = (label, key, value, placeholder = "") =>
    `<label class="fr-field">${esc(label)}<textarea data-field="${key}" rows="3" placeholder="${esc(placeholder)}">
${esc(value)}</textarea></label>`;
  const draftStages = ["generating","clarification","draft","interrupted"];
  const privateRequest = r => r.side === "out" && draftStages.includes(r.stage);
  const pendingCommand = id => {
    const op=ui.submitting || state.operation;
    return op?.role===role && op.id===id ? op.choice : null;
  };
  const needsReview = r => !pendingCommand(r.id) && ["pending","clarification","draft","interrupted"].includes(r.stage);
  const pendingReply = r => r.side === "in" && ["pending","clarification","draft","interrupted"].includes(r.stage);
  const composeSlot = parent => parent ? "follow-up/"+parent : "compose";
  const draftSlot = () => ui.recordId || composeSlot(ui.parent);
  const uid = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b=>b.toString(16).padStart(2,"0")).join("");
  function local(slot = draftSlot()) {
    return readDraft(draftKey(slot));
  }
  function readDraft(key) {
    if (!memory.has(key)) {
      let value = {};
      try { value = JSON.parse(localStorage.getItem(key)) || {}; }
      catch { error.textContent = "Local draft storage is unavailable."; }
      memory.set(key, value && typeof value === "object" ? value : {});
    }
    return memory.get(key);
  }
  function remember(slot = draftSlot()) {
    try { localStorage.setItem(draftKey(slot), JSON.stringify(local(slot))); }
    catch { error.textContent = "Your draft is in this window. Local storage could not save it."; }
  }
  function clearDraft(slot) {
    memory.set(draftKey(slot),{});
    try { localStorage.removeItem(draftKey(slot)); }
    catch { error.textContent = "Local storage could not clear the submitted draft."; }
  }
  function identity(a) {
    return `<div class="fr-identity">${addressAvatar(a.address)}<span><strong>${esc(a.owner_name || a.name)}</strong>
      ${a.owner_name ? `<small>${esc(a.name)}</small>` : ""}</span></div>`;
  }
  function address(label, value) {
    if (!value) return `<div class="fr-address"><div><small>${label}</small><span>Not shared</span></div></div>`;
    return `<div class="fr-address"><div><small>${label}</small><code>${esc(value)}</code></div>
      <button type="button" class="fr-icon" aria-label="Copy ${label}" title="Copy ${label}" data-fr="copy" data-value="${esc(value)}">${icon("copy")}</button></div>`;
  }
  function addressCard(a) {
    return `${identity(a)}${address("Agent LXMF address",a.address)}${address("Owner LXMF address",a.owner_address)}`;
  }
  // Hermes drafts as an ordinary turn of the user's own agent, tools included, before anything is reviewed.
  function hermesWarningOff(set) {
    try {
      if (set) localStorage.setItem("ratspeak.hermes-draft-warning","off");
      return localStorage.getItem("ratspeak.hermes-draft-warning")==="off";
    } catch { return false; }
  }
  function hermesWarning(action, reply) {
    const source = reply ? `${esc(peer.name)}’s message` : `earlier messages from ${esc(peer.name)}`;
    sheet("Draft with Hermes?",`<p>Hermes can use its tools while it drafts, and ${source} can influence what it does.</p>
      <label class="opt"><input type="checkbox" id="fr-hermes-quiet"><span>Don’t show again</span></label>
      <div class="fr-dialog-actions">${button("Cancel","dismiss","fr-quiet")}${button("Draft",action,"fr-primary")}</div>`,"hermes-draft");
  }
  function sheet(title, body, action = "", requestId = "") {
    if (!dialog.open) returnFocus = document.activeElement;
    dialogAction = action; dialogRequestId = requestId;
    dialog.innerHTML = `<header><h2 id="fr-dialog-title">${esc(title)}</h2>
      <button type="button" class="fr-icon" data-fr="dismiss" title="Close" aria-label="Close">${icon("close")}</button></header><div class="fr-sheet-body">${body}</div>`;
    if (!dialog.open) dialog.showModal();
    dialog.querySelector("header button").focus({preventScroll:true});
  }
  function stageName(r) {
    const active={"draft-request":"Drafting request","draft-reply":"Drafting reply",send:"Sending request","send-reply":"Sending reply",retry:"Retrying delivery"}[pendingCommand(r.id)];
    if (active) return active;
    if (r.stage==="pending" && local(r.id).answer?.trim()) return "Unsent reply draft";
    return {queued:"Waiting for delivery",waiting:"Delivered · waiting for reply",pending:"Needs your review",
      generating:"Preparing a private draft",clarification:"Your agent has a question",draft:r.side==="in"?"Unsent reply draft":"Unsent request draft",
      replying:"Awaiting delivery","reply-failed":"Delivery interrupted",answered:"Reply delivered",returned:"Reply received",declined:"Declined silently",
      notified:r.side==="in"?"Decline notice delivered":"Declined","notice-pending":"Sending decline notice",
      interrupted:"Draft interrupted",failed:"Delivery failed",expired:"Expired"}[r.stage] || r.stage;
  }
  function status(r) {
    const tone = needsReview(r) ? "pending" : {returned:"ok",answered:"ok",failed:"warn","reply-failed":"warn"}[r.stage] || "quiet";
    return `<span class="fr-state" data-tone="${tone}">${esc(stageName(r))}</span>`;
  }
  function timeLabel(r) {
    const date = new Date(Number(r.created)*1000);
    if (!r.created || !Number.isFinite(date.getTime())) return "";
    const options = date.toDateString()===new Date().toDateString() ?
      {hour:"numeric",minute:"2-digit"} : {month:"short",day:"numeric"};
    return `<time datetime="${date.toISOString()}" title="${esc(date.toLocaleString())}">${esc(date.toLocaleString(undefined,options))}</time>`;
  }
  function entry(r, showSubject) {
    const contact = contacts().find(a=>a.address===r.peer);
    const peerName = contact?.name || "Contact";
    const direction = privateRequest(r) ? "Private draft" : (r.side==="in" ? "From " : "To ") + peerName;
    const title = showSubject ? singleLine(r.question || r.draft) : "";
    return `<button type="button" class="fr-entry" data-fr="detail" data-id="${esc(r.id)}" data-peer="${esc(r.peer)}">
      <span class="fr-rowline">${status(r)}<small>${esc(direction)}</small></span>
      <strong>${esc(title || (privateRequest(r) ? "Unsent question" : peerName+" · request"))}</strong>
      <small>${timeLabel(r)}</small></button>`;
  }
  function rail() {
    const nav = pane.querySelector(".fr-nav"), count = allRows().filter(needsReview).length;
    const current = reviewed || ui.recordId || ui.view==="write" ? "contact" : ui.view;
    const item = (action, label, glyph, badge="") => `<button type="button" data-fr="${action}" aria-current="${current===action}">
      ${icon(glyph)}<span>${label}</span>${badge}</button>`;
    const cards=contacts().map(contact=>`<button type="button" class="fr-peer" data-fr="contact" data-peer="${esc(contact.address)}"
      aria-current="${current==="contact" && ui.peerAddress===contact.address}">${addressAvatar(contact.address)}
      <span><strong>${esc(contact.name)}</strong><small>${contact.paused?"Paused":"Review each request"}</small></span></button>`).join("");
    const html = '<div class="fr-rail-inner">'+item("inbox","Needs review","inbox",count ? `<span class="fr-count">${count}</span>` : "")+
      item("sent","Sent","send")+`<div class="fr-contacts-list">${cards}</div></div>
      <button type="button" class="fr-add" data-fr="add-contact" ${bound()?"disabled":""}>${icon("plus")}Add contact</button>`;
    if (nav._markup!==html) { nav._markup=html; nav.innerHTML=html; }
  }
  function home() {
    const pending = allRows().filter(needsReview);
    if (!state.me) return frame("Contacts",me.name,'<p class="fr-muted">Loading contacts…</p>');
    if (!state.claimed) return frame("Pair your agent first",me.name,'<p class="fr-muted">Use the Pairing tab to claim this agent, then add your contacts here.</p>');
    if (!contacts().length) return frame("Connect with another agent",me.name,
      '<p class="fr-muted">Add an agent’s address to exchange requests. You review each request and every reply before sending.</p>'+address("Your agent address",me.address),
      button("Add contact","add-contact","fr-primary","plus",bound()));
    return frame("Agent contacts", me.name,
      '<h3>Needs your review</h3>'+(pending.length ? pending.map(r=>entry(r,false)).join("") :
      '<p class="fr-muted">You’re all caught up.</p>')+
      '<p class="fr-hint">Choose a contact to see your exchanges.</p>');
  }
  function inventory(inbox) {
    const records = allRows().filter(r=>inbox ? needsReview(r) : r.side==="out" && !privateRequest(r)).sort((a,b)=>b.created-a.created);
    const list = records.length ? `<div class="fr-rowline"><small>${records.length} ${records.length===1?"request":"requests"}</small>
      ${button(ui.subjects?"Hide subjects":"Show subjects","subjects","fr-quiet")}</div>`+records.map(r=>entry(r,ui.subjects)).join("") :
      `<p class="fr-muted">${inbox?"You’re all caught up.":"No sent requests yet."}</p>`;
    return frame(inbox?"Needs your review":"Sent",(inbox?"Requests for ":"Requests from ")+me.name,list,
      state.total>allRows().length && oldest ? button("Load older requests","older","fr-quiet") : "");
  }
  function contact() {
    if (!peer) return home();
    const records = [...rows()].sort((a,b)=>b.created-a.created), d = local("compose");
    const hasLocal = (d.question || d.brief || d.context) && !rowBy(d.id);
    return frame(peer.owner_name || peer.name, "Agent contact",
      `<div class="fr-rule"><span>${peer.paused?"Contact paused":"Review each request"}</span>${button("Sharing rules","rules","fr-quiet")}</div>`+
      (records.length ? records.map(r=>entry(r,true)).join("") : '<p class="fr-muted">No exchanges yet.</p>'),
      (state.total>allRows().length && oldest ? button("Load older requests","older","fr-quiet") : "")+
      button(hasLocal?"Continue draft":"Ask "+peer.name,"write","fr-primary","",bound() || !!peer.paused),
      `<button type="button" data-fr="identity-peer" class="fr-icon" aria-label="Identity details" title="Identity details">${icon("person")}</button>`);
  }
  function addContact() {
    return frame("Add contact",me.name,
      '<form id="fr-add-form"><label class="fr-field">Contact name<input name="name" required maxlength="64" autocomplete="off"></label>'+
      '<label class="fr-field">Agent address<input name="address" required placeholder="Ratspeak or LXMF address" autocomplete="off" autocapitalize="none" spellcheck="false"></label></form>'+
      '<p class="fr-hint">Both owners add each other’s agent address. Share your address with the other owner.</p>'+address("Your agent address",me.address),
      button("Back","home","fr-quiet")+'<button class="fr-primary" form="fr-add-form" type="submit">Add contact</button>');
  }
  function disclosure(label, body, open=false) {
    return `<details class="fr-disclosure" ${open ? "open" : ""}><summary>${esc(label)}</summary><div>${body}</div></details>`;
  }
  const limitMessage = '<span class="fr-length" role="status"></span>';
  function draftEditor(label, author, key, value, placeholder) {
    return `<label class="fr-field fr-editor"><span class="fr-byline">${icon("write")}<span class="fr-author">${esc(author || label)}</span>
      <span class="fr-unsent">Unsent</span></span><textarea data-field="${key}" aria-label="${esc(label)}" rows="4" placeholder="${esc(placeholder)}">
${esc(value)}</textarea></label>`;
  }
  const recipient = a => "To "+a.name+(a.owner_name ? " ("+a.owner_name+")" : "");
  function compose(r=null) {
    const d=local(), q=d.question ?? r?.draft ?? "", ctx=d.context ?? r?.context ?? "";
    const parent=rowBy(r?.in_reply_to || ui.parent);
    const previous=parent ? disclosure("Previous exchange",source("Original question",parent.question)+source("Reply",parent.answer)) : "";
    if (ui.assist && !r) {
      return frame("Draft a request","Private with "+me.name,
        field("What should "+me.name+" ask?","brief",d.brief ?? q,"Give your agent the details to work from.")+
        limitMessage+previous,button("Back","compose-manually","fr-quiet")+button("Create draft","generate","fr-primary"));
    }
    const earlier=d.earlierQuestion && d.earlierQuestion!==q ? disclosure("Earlier wording",
      source("Your original question",d.earlierQuestion)+button("Use this wording","use-earlier-question","fr-quiet")) : "";
    return frame(parent?"Ask a follow-up":r?"Request draft":"Write request",recipient(peer),
      draftEditor("Your question",r?.draft && d.question==null ? "Draft by "+me.name : "", "question",q,"Ask "+peer.name+"…")+limitMessage+attachControl()+
      disclosure("Context to share",field("Additional context","context",ctx,"What else should "+peer.name+" know?"),!!ctx)+
      earlier+previous,
      button(!r?"Draft with "+me.name:"Back",!r?"assist":"back","fr-quiet")+button("Preview request","preview","fr-primary"));
  }
  function original(r) {
    const inbound=r.side==="in";
    return disclosure("Original request",source(inbound?peer.name+" asks":"Your question",r.question)+
      (r.context ? source(inbound?"Context from "+peer.name:"Context you shared",r.context,"fr-quote") : "")+savedFile(r,"request"));
  }
  function workInProgress(choice) {
    const drafting=choice.startsWith("draft-"), reply=choice.endsWith("reply");
    const what=reply?"reply":"request";
    return frame(drafting ? "Drafting "+what : "Sending "+what,
      drafting ? "Private with "+me.name : recipient(peer),
      `<div class="fr-working" role="status"><span class="fr-pulse" aria-hidden="true"></span><p>${
        drafting ? esc(me.name)+" is preparing a draft for you." : "Sending your "+what+" to "+esc(peer.name)+"…"
      }</p></div>`);
  }
  function detail(r) {
    const inbound=r.side==="in", privateOut=privateRequest(r), d=local();
    if (r.stage==="generating") return workInProgress(inbound?"draft-reply":"draft-request");
    if (privateOut && (r.stage==="draft" || d.manualQuestion)) return compose(r);
    const editing=inbound && pendingReply(r) && (r.stage==="draft" || d.editReply);
    if (editing) {
      const byAgent=r.stage==="draft" && !d.editReply && d.answer==null;
      return frame("Reply draft",recipient(peer),
        draftEditor("Your reply",byAgent?"Draft by "+me.name:"","answer",d.answer ?? r.draft ?? "","Write a reply…")+limitMessage+attachControl()+original(r),
        button("Back",r.stage==="pending"?"reply-options":"back","fr-quiet")+button("Preview reply","preview-reply","fr-primary"));
    }
    if (r.stage==="clarification") {
      return frame(me.name+" has a question","Private with your agent",
        `<p class="fr-question">${esc(r.clarification)}</p>`+
        field("Your private answer","privateAnswer",d.answeringQuestion && d.answeringQuestion!==r.clarification ? "" : d.privateAnswer || "","Give "+me.name+" the detail it needs.")+
        limitMessage+(inbound?original(r):""),
        button(inbound?"Reply myself":"Write it myself",inbound?"write-reply":"manual-question","fr-quiet")+
        button("Continue drafting","clarify","fr-primary"));
    }
    if (r.stage==="interrupted") {
      return frame("Drafting stopped","Private with "+me.name,
        '<p>Your agent couldn’t finish this draft.</p>'+(inbound?original(r):""),
        button(inbound?"Reply myself":"Write it myself",inbound?"write-reply":"manual-question","fr-quiet")+
        button("Try again","generate-again","fr-primary","",bound()));
    }
    const back = button("Back","back","fr-quiet");
    let body=r.in_reply_to && rowBy(r.in_reply_to) ? button("Previous exchange","previous","fr-link") : "";
    if (inbound && r.stage==="pending") {
      const hasAnswer=!!d.answer?.trim();
      body+=`<p class="fr-question">${esc(r.question)}</p>`+
        (r.context ? source("Context from "+peer.name,r.context,"fr-quote") : "")+savedFile(r,"request")+
        (readable(fileInfo(r.attachment)) ? '<p class="fr-hint">'+esc(me.name)+' reads this file when drafting.</p>' : "");
      if (!hasAnswer) body+=disclosure("Private instructions",
        field("For "+me.name,"brief",d.brief || "","Anything to consider when drafting?")+limitMessage,!!d.brief);
      return frame("Incoming request","From "+peer.name+(peer.owner_name?" ("+peer.owner_name+")":""),body,
        button("Decline","decline-options","fr-quiet","",bound())+
        (hasAnswer ? button("Continue my draft","write-reply","fr-primary","",bound()) :
          button("Reply myself","write-reply","fr-secondary","",bound())+button("Draft with "+me.name,"generate","fr-primary","",bound())));
    }
    let title=stageName(r), subtitle=inbound?"From "+contactLine(peer):recipient(peer), actions="";
    if (["returned","answered","replying","reply-failed"].includes(r.stage)) {
      subtitle=inbound?recipient(peer):"From "+peer.name;
      body+=(r.answer ? source(inbound?"Your reply":"Reply from "+peer.name,r.answer) : "")+savedFile(r,"reply")+original(r);
    } else {
      body+=source(inbound?peer.name+" asks":"Your question",r.question)+
        (r.context?source(inbound?"Context from "+peer.name:"Context you shared",r.context,"fr-quote"):"")+savedFile(r,"request");
    }
    if (r.stage==="waiting") {title="Request delivered";body+='<p class="fr-hint">Waiting for a reply from '+esc(peer.name)+'.</p>';}
    if (r.stage==="queued") title="Waiting for delivery";
    if (r.stage==="failed") body+='<p class="fr-hint">Check that both agents are online and both owners have added each other’s agent address.</p>';
    if (r.stage==="replying") {
      title="Awaiting delivery";
      body+='<p class="fr-hint">'+esc(peer.name)+' hasn’t acknowledged your reply yet.</p>';
    }
    if (r.stage==="reply-failed") body+='<p class="fr-hint">Retry to send this saved reply.</p>';
    if (r.stage==="failed" || (inbound && ["reply-failed","replying","notice-pending"].includes(r.stage) && !bound())) actions=button("Retry delivery","retry","fr-secondary","",bound());
    if (["answered","returned"].includes(r.stage)) actions+=button("Ask a follow-up","follow-up","fr-primary");
    return frame(title,subtitle,body,actions || back);
  }
  function previewPane() {
    const reply=reviewed.action==="send-reply", payload=reviewed.payload;
    return frame(reply?"Review reply":"Review request",recipient(peer),
      source("Ready to send",reply ? payload.answer : payload.question,"fr-final-message")+
      (payload.context ? source("Shared context",payload.context,"fr-quote") : "")+fileRow(payload.attachment),
      button("Edit","edit-preview","fr-quiet")+button("Send to "+peer.name,reviewed.action,"fr-primary","send",bound()));
  }

  const currentText = name => content.querySelector('[data-field="'+name+'"]')?.value || "";
  function checkFields() {
    const question = currentText("question"), context = currentText("context"), answer = currentText("answer");
    let message = "";
    if ([question,context,answer,currentText("brief"),currentText("privateAnswer")].some(t=>Array.from(t).length>2000))
      message = "Shorten each field to 2,000 characters.";
    else if (new TextEncoder().encode(JSON.stringify({question,context})).length>15800)
      message = "Shorten the question or included text to fit one request.";
    const label = content.querySelector(".fr-length");
    if (label) label.textContent=message;
    const actions = {preview:!!question.trim(),"preview-reply":!!answer.trim(),clarify:!!currentText("privateAnswer").trim(),
      generate:rowBy(ui.recordId)?.side==="in" || !!currentText("brief").trim()};
    for (const [action,enabled] of Object.entries(actions)) {
      const control = content.querySelector('[data-fr="'+action+'"]');
      if (control) control.disabled=bound() || !!message || !enabled;
    }
    return !message;
  }
  function render(force=false) {
    rail();
    const row = rowBy(ui.recordId);
    const visibleId=ui.recordId || (ui.view==="write" ? local().id : null);
    const activeChoice=pendingCommand(visibleId);
    const data = reviewed ? [reviewed,!!bound()] : ui.recordId ? [row,ui.awaiting,!!bound(),state.error] : ui.view==="write" ? [] : allRows();
    const key = JSON.stringify([ui.view,ui.recordId,ui.subjects,ui.assist,ui.peerAddress,activeChoice,data,contacts(),state.ready,state.claimed,!!bound(),me.name]);
    if (!force && key===rendered) { checkFields(); return; }
    rendered=key;
    const active = content.contains(document.activeElement) ? document.activeElement : null;
    const fieldName = active?.dataset.field, focusAction = active?.dataset.fr, focusId = active?.dataset.id;
    const selection = fieldName ? [active.selectionStart,active.selectionEnd] : null;
    const previousScroll = content.querySelector(".fr-scroll")?.scrollTop || 0;
    if (!state.me) content.innerHTML=home();
    else if (activeChoice==="retry") content.innerHTML=frame("Retrying delivery",recipient(peer),'<p role="status">Sending the previously approved message…</p>');
    else if (["send","send-reply","draft-request","draft-reply"].includes(activeChoice)) content.innerHTML=workInProgress(activeChoice);
    else if (reviewed) content.innerHTML=previewPane();
    else if (row) content.innerHTML=detail(row);
    else if (ui.recordId) {
      const waiting = state.error || (bound() ? "Waiting for the local agent…" : "Waiting for the request record…");
      content.innerHTML=frame("Request status",me.name,`<p>${esc(waiting)}</p>`,button("Back","contact","fr-quiet"));
    } else if (ui.view==="write") content.innerHTML=compose();
    else if (ui.view==="contact") content.innerHTML=contact();
    else if (ui.view==="home") content.innerHTML=home();
    else if (ui.view==="add-contact") content.innerHTML=addContact();
    else content.innerHTML=inventory(ui.view==="inbox");
    if (!force && content.querySelector(".fr-scroll")) content.querySelector(".fr-scroll").scrollTop=previousScroll;
    checkFields();
    const target = fieldName ? content.querySelector('[data-field="'+fieldName+'"]') :
      [...content.querySelectorAll("button[data-fr]")].find(b=>b.dataset.fr===focusAction && b.dataset.id===focusId);
    if (target) { target.focus({preventScroll:true}); if (selection) target.setSelectionRange(...selection); }
  }
  function navigate(view,id=null,parent=null) {
    if (dialog.open) dialog.close();
    reviewed=null;
    ui.view=view; ui.recordId=id; ui.parent=parent;
    live.textContent="";
    ui.assist=view==="write" && local(composeSlot(parent)).assisted===true; error.textContent=""; render(true);
    const focus = view==="write" ? content.querySelector("textarea") : content.querySelector("h2,.fr-link,.fr-identity");
    focus?.focus({preventScroll:true});
  }
  function update(next) {
    if (!next?.boxes || !pane.isConnected) return;
    const prior = rowBy(ui.recordId)?.stage, priorError=state.error;
    if(!state.ready) error.textContent="";
    const stored = new Map(allRows().map(row=>[row.id,row]));
    for(const row of next.boxes[role] || []) stored.set(row.id,row);
    if(next.before || !loadedOlder) oldest=next.cursor;
    if(next.before) loadedOlder=true;
    state={...next,boxes:{[role]:[...stored.values()].sort((a,b)=>b.created-a.created)}};
    me=next.me || me;
    peer=contacts().find(contact=>contact.address===ui.peerAddress) || null;
    route=routeKey(me,peer);
    if (!peer && (ui.recordId || ["contact","write"].includes(ui.view))) {
      ui.recordId=null; ui.view="home"; reviewed=null;
    }
    const row = rowBy(ui.recordId);
    if (row && row.stage!==prior) live.textContent=(draftStages.includes(row.stage)?me.name:peer?.name || "Contact")+" · "+stageName(row);
    if (next.error && next.error!==priorError && (!next.error_peer || next.error_peer===peer?.address)) error.textContent=next.error;
    else if (priorError && !next.error && error.textContent===priorError) error.textContent="";
    const slot = composeSlot(ui.parent), composer = local(slot), committed = rowBy(composer.id);
    if (committed) {
      if (privateRequest(committed)) {
        const draft=local(committed.id);
        if (composer.context && draft.context==null) draft.context=composer.context;
        if (composer.question && draft.earlierQuestion==null) draft.earlierQuestion=composer.question;
        remember(committed.id);
      }
      if (ui.view==="write" && !ui.recordId) { ui.view="contact"; ui.recordId=committed.id; }
      clearDraft(slot);
    }
    // A send can finish while another contact is selected. Retire only its own composer.
    for (const record of allRows()) {
      if (record.side!=="out") continue;
      const recordRoute=routeKey(me,{address:record.peer});
      const key=recordRoute+"/"+composeSlot(record.in_reply_to);
      const saved=readDraft(key);
      if (saved.id!==record.id) continue;
      if (privateRequest(record)) {
        const nextKey=recordRoute+"/"+record.id, draft=readDraft(nextKey);
        if(saved.context && draft.context==null) draft.context=saved.context;
        if(saved.question && draft.earlierQuestion==null) draft.earlierQuestion=saved.question;
        try { localStorage.setItem(nextKey,JSON.stringify(draft)); }
        catch { error.textContent="Local storage could not save the draft."; }
      }
      memory.set(key,{});
      try { localStorage.removeItem(key); }
      catch { error.textContent="Local storage could not clear the submitted draft."; }
    }
    if (ui.awaiting && rowBy(ui.awaiting)) ui.awaiting=null;
    if (reviewed && !reviewStillCurrent()) reviewed=null;
    else if (dialog.open && dialogRequestId && (!row || !["pending","interrupted","draft","clarification"].includes(row.stage))) dialog.close();
    render();
    if (row && row.stage!==prior && !dialog.open && (!document.activeElement || document.activeElement===document.body)) focusDetail();
  }
  function focusDetail() {
    const target=content.querySelector('[data-field="privateAnswer"],[data-field="answer"],[data-field="question"]') ||
      content.querySelector(".fr-state,.fr-link,h2");
    if (target) { if (target.classList.contains("fr-state")) target.tabIndex=-1; target.focus({preventScroll:true}); }
  }

  async function copy(value, control) {
    try {
      await navigator.clipboard.writeText(value);
      live.textContent="Address copied."; const label=control.getAttribute("aria-label"); control.title="Copied";control.setAttribute("aria-label","Copied");
      setTimeout(()=>{if(control.isConnected){control.title=label;control.setAttribute("aria-label",label);}},1400);
    } catch {
      const code=control.parentElement.querySelector("code"), range=document.createRange();
      range.selectNodeContents(code); const selection=window.getSelection(); selection.removeAllRanges();selection.addRange(range);
      live.textContent="Address selected. Use Copy to copy it.";
    }
  }
  function review(reply) {
    if (bound() || !checkFields()) return;
    const text = currentText(reply?"answer":"question"), context = reply ? "" : currentText("context");
    if (!text.trim()) return;
    const row=rowBy(ui.recordId), d=local(), action=reply?"send-reply":"send";
    if (!row && !d.id) { d.id=uid(); remember(); }
    const parent=row?.in_reply_to || ui.parent || null;
    const file=d.attachment ? {attachment:d.attachment} : {};
    reviewed=Object.freeze({action,id:row?.id || d.id,rowId:ui.recordId,view:ui.view,parent,route,
      payload:Object.freeze(reply ? {answer:text,...file} : {question:text,context,...file,...(parent?{inReplyTo:parent}:{})})});
    render(true);
    content.querySelector("h2")?.focus({preventScroll:true});
  }
  function editPreview() {
    const reply=reviewed?.action==="send-reply";
    reviewed=null;
    render(true);
    content.querySelector(reply?'[data-field="answer"]':'[data-field="question"]')?.focus({preventScroll:true});
  }
  function reviewStillCurrent() {
    if (!reviewed || !pane.isConnected || reviewed.route!==route || ui.view!==reviewed.view || ui.recordId!==reviewed.rowId) return false;
    const row=rowBy(ui.recordId);
    if ((row?.in_reply_to || ui.parent || null)!==reviewed.parent) return false;
    if (!reviewed.rowId) return local().id===reviewed.id && !rowBy(reviewed.id);
    return row?.id===reviewed.id && (reviewed.action==="send-reply" ? pendingReply(row) : privateRequest(row) && row.stage!=="generating");
  }
  async function perform(choice,payload={},approvedId=null) {
    if (bound()) return;
    const slot = draftSlot(), d = local(slot), row = rowBy(ui.recordId);
    const submittedRoute = route;
    const fresh = !ui.recordId && ["send","draft-request"].includes(choice);
    if (!row && !fresh) return;
    const id = approvedId || row?.id || d.id || uid();
    if (fresh) { d.id=id; remember(slot); }
    const parent = row?.in_reply_to || ui.parent;
    if (parent && choice==="draft-request") payload.inReplyTo=parent;
    if (dialog.open) dialog.close();
    ui.submitting={id,role,choice}; error.textContent=""; render();
    try {
      const {inReplyTo,clarificationAnswer,readyToDraft,...fields}=payload;
      const next = await request({choice,id,peer_address:peer.address,...fields,
        in_reply_to:inReplyTo,clarification_answer:clarificationAnswer,ready_to_draft:readyToDraft});
      reviewed=null;
      if (route===submittedRoute && fresh && ((ui.view==="write" && !ui.recordId && draftSlot()===slot) || ui.recordId===id)) {
        ui.recordId=id; ui.view=choice==="send"?"sent":"contact"; ui.awaiting=id;
      }
      if (route===submittedRoute && choice==="send" && row && ui.recordId===id) ui.view="sent";
      if (route===submittedRoute && choice==="draft-reply" && !row?.draft) ui.assist=false;
      if (pane.isConnected) update(next);
    } catch (err) {
      if (pane.isConnected) error.textContent=String(err);
    } finally {
      ui.submitting=false;
      if (pane.isConnected) {
        render();
        if (ui.recordId===id && !dialog.open && (!document.activeElement || document.activeElement===document.body)) focusDetail();
      }
    }
  }
  function handleClick(event) {
    const control=event.target.closest("button[data-fr]");
    if (!control || control.disabled) return;
    const action=control.dataset.fr, row=rowBy(ui.recordId);
    if (action==="older") {
      control.disabled=true;
      request({choice:"state",before:oldest}).then(update).catch(err=>update.problem(String(err)));
      return;
    }
    if (control.dataset.peer) selectPeer(control.dataset.peer);
    if (["home","inbox","sent","contact","write","add-contact"].includes(action)) return navigate(action);
    if(action==="refresh") {
      control.disabled=true;
      request({choice:"state"}).then(next=>{error.textContent="";update(next);}).catch(err=>update.problem(String(err)));
      return;
    }
    if (action==="edit-preview") return editPreview();
    if (action==="rules") return sheet("Sharing rules",`<p>You review each incoming request from ${esc(peer.name)}. Preparing a draft keeps it private until you send it.</p>
      <div class="fr-dialog-actions">${button(peer.paused?"Resume contact":"Pause new requests","pause-contact","fr-secondary","",bound())}</div>`);
    if (action==="pause-contact" && peer && !bound()) {
      const address=peer.address;
      control.disabled=true;
      request({choice:"pause",peer_address:address,paused:!peer.paused}).then(next=>{
        if(!pane.isConnected) return; dialog.close(); update(next);
      }).catch(err=>{error.textContent=String(err);control.disabled=false;});
      return;
    }
    if (action==="back") return navigate(ui.view);
    if (action==="detail") return navigate(ui.view,control.dataset.id);
    if (action==="previous" && rowBy(row?.in_reply_to)) return navigate("contact",row.in_reply_to);
    if (action==="follow-up" && ["answered","returned"].includes(row?.stage)) return navigate("write",null,row.id);
    if (action==="subjects") { ui.subjects=!ui.subjects; return render(); }
    if (action==="identity-peer" || action==="identity-me") {
      const agent=action==="identity-me"?me:peer; return sheet("Identity details",addressCard(agent),"identity");
    }
    if (action==="copy") return copy(control.dataset.value,control);
    if (action==="dismiss") return dialog.close();
    if ((action==="save-request" || action==="save-reply") && row) {
      control.disabled=true;
      request.save(row.id,action.slice(5)).then(saved=>{ if(saved) live.textContent="File saved."; })
        .catch(err=>{ if(pane.isConnected) error.textContent=String(err); })
        .finally(()=>{ if(control.isConnected) control.disabled=false; });
      return;
    }
    if (bound()) return;
    const d=local();
    if (action==="attach") {
      const slot=draftSlot();
      if (!row && !d.id) d.id=uid();
      remember(slot);
      control.disabled=true; error.textContent="";
      request.attach(row?.id || d.id).then(info=>{
        if (!info) return;
        local(slot).attachment=info; remember(slot);
        if (pane.isConnected && draftSlot()===slot) render(true);
      }).catch(err=>{ if(pane.isConnected) error.textContent=String(err); })
        .finally(()=>{ if(control.isConnected) control.disabled=false; });
      return;
    }
    if (action==="detach") { delete d.attachment; remember(); render(true); return; }
    if (action==="reply-options") {
      d.editReply=false; ui.assist=false; remember(); render(true);
      content.querySelector('[data-fr="write-reply"]')?.focus({preventScroll:true});
      return;
    }
    if (action==="use-earlier-question" && d.earlierQuestion) {
      const current=currentText("question");
      d.question=d.earlierQuestion; d.earlierQuestion=current; remember(); render(true);
      content.querySelector('[data-field="question"]')?.focus(); return;
    }
    if (["assist","compose-manually","write-reply","manual-question"].includes(action)) {
      if (action==="assist" || action==="compose-manually") {ui.assist=action==="assist"; d.assisted=ui.assist; remember();}
      else {
        if (action==="write-reply") d.editReply=true;
        if (action==="manual-question") d.manualQuestion=true;
        remember();
      }
      render(true);
      content.querySelector('[data-field="'+({assist:"brief","compose-manually":"question","write-reply":"answer","manual-question":"question"}[action])+'"]')?.focus();
      return;
    }
    if (action==="preview" || action==="preview-reply") return review(action==="preview-reply");
    if ((action==="send" || action==="send-reply") && reviewed?.action===action) {
      if (!reviewStillCurrent() || reviewed.action!==action) {
        reviewed=null; render(true); error.textContent="The request changed. Open the preview again."; return;
      }
      return perform(action,reviewed.payload,reviewed.id);
    }
    if (["clarify","generate","generate-again"].includes(action)) {
      if (!checkFields()) return;
      if (action!=="clarify" && initial.provider==="hermes") {
        if (!(dialog.open && dialogAction==="hermes-draft") && !hermesWarningOff()) return hermesWarning(action,row?.side==="in");
        if (dialog.querySelector("#fr-hermes-quiet")?.checked) hermesWarningOff(true);
      }
      const choice=row?.side==="in"?"draft-reply":"draft-request";
      const payload=action==="clarify" ? {clarificationAnswer:currentText("privateAnswer")} :
        action==="generate-again" ? {} : {brief:currentText("brief")};
      if (action==="generate" && row?.side==="in" && payload.brief.trim()) payload.readyToDraft=true;
      return perform(choice,payload);
    }
    if (!row) return;
    if (action==="retry") return perform("retry");
    if (!pendingReply(row)) return;
    if (action==="decline-options") return sheet("Decline request",
      `<p>Choose whether ${esc(peer.name)} receives a decline notice.</p>
      <div class="fr-dialog-actions">${button("Decline silently","decline")}${button("Decline and notify","decline-notify","fr-primary")}</div>`,"decline",row.id);
    if ((action==="decline" || action==="decline-notify") && dialog.open && dialogAction==="decline") return perform(action);
  }
  pane.addEventListener("click",handleClick);
  pane.addEventListener("submit",async event=>{
    if (event.target.id!=="fr-add-form") return;
    event.preventDefault();
    if(bound()) return;
    const data=new FormData(event.target), name=String(data.get("name") || "").trim();
    const address=String(data.get("address") || "").trim();
    const control=pane.querySelector('[form="fr-add-form"]');
    control.disabled=true;
    try {
      const next=await request({choice:"add",name,address});
      if(!pane.isConnected) return;
      update(next);
      const id=address.toLowerCase().replace(/^lxma:\/\//, "").split(":")[0];
      selectPeer(id); navigate("contact");
    } catch(err) {if(pane.isConnected) error.textContent=String(err);}
    finally {if(control.isConnected) control.disabled=false;}
  });
  if (!pane.contains(dialog)) dialog.addEventListener("click",handleClick);
  pane.addEventListener("keydown",event=>{
    if (event.key==="Escape" && reviewed && !dialog.open) {event.preventDefault(); editPreview();}
  });
  pane.addEventListener("input",event=>{
    const name=event.target.dataset.field;
    if (!name) return;
    const d=local();
    if (name==="privateAnswer") d.answeringQuestion=rowBy(ui.recordId)?.clarification;
    d[name]=event.target.value; remember(); rail(); checkFields();
    if (name==="answer" || name==="question") {
      const author=content.querySelector(".fr-author");
      if (author) author.textContent=event.target.getAttribute("aria-label");
    }
  });
  dialog.addEventListener("close",()=>{
    if (dialog.open) return;
    dialogAction=""; dialogRequestId="";
    if (returnFocus?.isConnected) returnFocus.focus({preventScroll:true});
  });
  render();
  update.problem=message=>{
    if(!pane.isConnected) return;
    state.ready=false;
    error.textContent=message;
    if (!state.me) content.innerHTML=frame("Contacts",me.name,'<p class="fr-muted">'+esc(message)+'</p>',button("Check again","refresh","fr-secondary"));
    else render();
  };
  update.showOwner=()=>sheet("Identity details",addressCard(me),"identity");
  update.currentId=()=>ui.recordId;
  update.dispose=()=>{if(dialog.open) dialog.close();dialog.remove();};
  return update;
}

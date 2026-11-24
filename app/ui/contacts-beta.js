"use strict";

// One beta password unlocks A2A and Connect Hermes on this device.
function contactsBetaPane({title = "Agent-to-Agent is locked", art = A2A_BETA_ART, heading = "h2"} = {}) {
  return `<section class="contacts-beta" aria-labelledby="contacts-beta-title">${art}
    <${heading} id="contacts-beta-title">${title}</${heading}>
    <p class="contacts-beta-intro" id="contacts-beta-intro">Enter the beta password to test it.</p>
    <form class="contacts-beta-form" novalidate>
      <label for="contacts-beta-password">Password</label>
      <div class="contacts-beta-field">
        <input id="contacts-beta-password" name="password" type="password" required autocomplete="off" autocapitalize="off" spellcheck="false" aria-describedby="contacts-beta-intro contacts-beta-error">
        <button class="contacts-beta-reveal" type="button" aria-label="Show password" aria-pressed="false" aria-controls="contacts-beta-password">Show</button>
      </div>
      <p class="contacts-beta-error" id="contacts-beta-error" role="alert"></p>
      <button class="primary contacts-beta-unlock" type="submit"><span>Unlock</span></button>
    </form>
  </section>`;
}

// Every beta lock is the same picture: two tiles and a locked link. Only the glyphs in the tiles change.
const betaArt = (left, right) => `<svg class="contacts-beta-art" viewBox="0 20 224 74" aria-hidden="true" focusable="false">
      <g class="contacts-beta-signal"><path d="M42 49C74 49 80 28 112 28s38 21 70 21"/><path d="M42 57c32 0 38-21 70-21s38 21 70 21"/><path d="M42 65c32 0 38 21 70 21s38-21 70-21"/></g>
      <g class="contacts-beta-node"><rect x="19" y="29" width="48" height="48" rx="14"/><rect x="157" y="29" width="48" height="48" rx="14"/></g>
      <g class="contacts-beta-identity">${left}${right}</g>
      <g class="contacts-beta-lock" transform="translate(0 -10)"><path d="M105 64v-6a7 7 0 0 1 14 0v6"/><rect x="99" y="64" width="26" height="23" rx="6"/><path d="M112 73v5"/></g>
      <g class="contacts-beta-points"><circle cx="77" cy="38.5" r="2"/><circle cx="147" cy="75.5" r="2"/></g>
    </svg>`;
const BETA_AGENT = '<rect x="32" y="44" width="22" height="18" rx="5"/><path d="M43 40v4m-14 6v6m28-6v6m-18-5v2m8-2v2m-7 5h6"/><circle cx="43" cy="38" r="2"/>';
const A2A_BETA_ART = betaArt(`<g>${BETA_AGENT}</g>`, `<g transform="translate(138 0)">${BETA_AGENT}</g>`);
const HERMES_BETA_ART = betaArt(
  '<use class="contacts-beta-mark" href="mark.svg#mark" x="30" y="40" width="26" height="26"/>',
  '<use class="contacts-beta-hermes" href="host-devices.svg#hermes-head" x="166.3" y="40.2" width="29.4" height="25.6"/>');

function wireContactsBeta(root, unlock) {
  const form = root.querySelector(".contacts-beta-form");
  const input = form.querySelector("input");
  const reveal = form.querySelector(".contacts-beta-reveal");
  const submit = form.querySelector(".contacts-beta-unlock");
  const error = form.querySelector(".contacts-beta-error");
  let busy = false;

  // Busy keeps focus on Unlock: aria-disabled instead of disabled.
  function controls() {
    input.readOnly = reveal.disabled = busy;
    submit.setAttribute("aria-disabled", String(busy));
    form.setAttribute("aria-busy", String(busy));
    submit.querySelector("span").textContent = busy ? "Unlocking…" : "Unlock";
  }
  function problem(message) {
    error.textContent = message || "";
    input.removeAttribute("aria-invalid");
  }
  reveal.onclick = () => {
    const shown = input.type === "password";
    input.type = shown ? "text" : "password";
    reveal.textContent = shown ? "Hide" : "Show";
    reveal.setAttribute("aria-pressed", String(shown));
  };
  input.oninput = () => problem("");
  form.onsubmit = async event => {
    event.preventDefault();
    if (busy) return;
    problem("");
    if (!input.value) {
      error.textContent = "Enter the beta password.";
      input.setAttribute("aria-invalid", "true");
      input.focus();
      return;
    }
    busy = true;
    controls();
    try {
      await unlock(input.value);
      input.value = "";
      input.type = "password";
      reveal.textContent = "Show";
      reveal.setAttribute("aria-pressed", "false");
    } catch (err) {
      if (form.isConnected) {
        error.textContent = String(err?.message || err || "Couldn’t unlock. Try again.");
        input.setAttribute("aria-invalid", "true");
      }
    } finally {
      busy = false;
      controls();
      if (form.isConnected && error.textContent) {
        input.focus();
        input.select();
      }
    }
  };
  return { problem };
}

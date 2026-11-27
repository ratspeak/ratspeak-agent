# Vendored helper

Copied from Agent Tunnel by `scripts/vendor.sh`. The desktop/Pi receive integration is applied from `scripts/agent-inbox.patch`. The admission policy lives in `ratspeak_pi/admission.py`.

- Source commit: 6f1bfdb9dd53fbe2857471143736327dbedec5de (with uncommitted changes)
- Helper version: 0.7.0
- Licence: runtime/LICENSE (Reticulum License)
- The patched receiver reads the saved profile label on announce. Its native transport keeps that label for later scheduled announces.
- Includes the source helper’s atomic reply enqueue and inbox acknowledgement, with durable reply drafts.
- The checked-in runtime carries LXMF `FIELD_THREAD` through to replies and stores chat context and reset boundaries per peer and thread. Preserve these changes when refreshing from Agent Tunnel.
- The receiver routes signed Contacts envelopes into a bounded per-agent queue. Owner-reviewed drafts and replies use the installed agent’s provider and persistent storage.
- Contacts traffic and drafting stay disabled until its per-agent beta gate is unlocked locally. The desktop app remembers the beta opt-in on the device and applies it when opening another agent’s A2A tab.
- An unclaimed agent's native gate admits only a bare six-character setup code ("6J8E4K" or "6J8 E4K"), not `/setup`.
- `allow` takes `reinstate` (`--reinstate` on the command line) to lift an earlier `revoke`, so the owner can let a removed address back in.
- The control socket lives in the profile's `run/` folder, not `/tmp`, so a systemd service's private `/tmp` does not hide it from setup and the command line.
- Built against rsReticulum and rsLXMF v1.5.1 (as Ratspeak 1.1.1), ahead of the source helper. Profiles made with v1.4.0 or v1.5.0 still open; starting the receiver records v1.5.1 in the profile. An interface that fails to start is skipped instead of stopping the receiver.

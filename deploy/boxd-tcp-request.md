# Request to boxd: raw TCP ingress for a VM port

Send to `contact@boxd.sh` (or their Slack/Discord). Everything boxd needs is below.

---

**Subject: Feature request — public raw TCP port to a VM (for an SSH game arena)**

Hi! I'm running a small open-source terminal game ([ascii-royale](https://github.com/chad/ascii-royale))
as a public "ssh in and play" arena on a boxd VM, and I've hit the one thing
boxd doesn't seem to expose: **inbound raw TCP from the internet to a VM port.**

What I need: a public TCP endpoint that forwards to a port on my VM — e.g.
`royale.boxd.sh:2222 -> VM port 2222` (my sshd), ideally a stable port so I
can publish `ssh -p <port> play@royale.boxd.sh`.

- VM name: `royale`
- VM id: `3965f95e-4a90-4d15-9472-7e49a54e4a48`
- Target port on the VM: `2222` (a locked-down OpenSSH for anonymous guests)

What I found trying to do this myself:
- The `name.boxd.sh` HTTPS proxy is HTTP(S)-only — pointing it at sshd
  returns a 502, since it speaks HTTP to the backend. So it can't carry SSH.
- `name.boxd.sh:22` reaches the boxd management REPL, not the VM.
- The per-VM SSH port your `ssh-config` writes (mine is `royale.boxd.sh:20107`)
  **is** raw TCP to the VM — but it's authenticated against my own account
  keys, so the public can't use it.

That last point is the ask: you clearly already do per-VM TCP port mapping
(that's how `boxd connect` / direct SSH works) — I'd love a way to expose
**one** such port publicly (no boxd-account auth on it), so an app on the VM
can own its own front door. A `boxd proxy new --tcp` or a per-VM "public TCP
port" flag would be perfect.

For verification, my VM's SSH host key fingerprint is
`SHA256:MksQnpeWoT09c/zZGXGRDxNySe7wIoeWS1A542xxU/o`.

Thanks! Happy to be a test case for the feature.

---

## When boxd grants it

1. Point their TCP endpoint at VM port **2222** (sshd is already running and
   hardened there; see the other files in this `deploy/` dir).
2. Publish the address: update the README's intro and the GitHub repo
   description to `ssh -p <port> play@royale.boxd.sh`.
3. Include the host-key line so players can verify (defuses the first-contact
   SSH warning):
   `ssh-keyscan` should show `SHA256:MksQnpeWoT09c/zZGXGRDxNySe7wIoeWS1A542xxU/o`.

No tunnel daemon needed — delete nothing, just publish the address.

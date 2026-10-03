# Workshop multiplayer: VPNs, proxies and common problems

Workshop multiplayer puts every player on a small private network. It is a
Tailscale network that Hebnix runs by itself: its own `tailscaled`, its own
network interface called `hebnixts0`, and addresses in `10.242.77.x`. Rocket
League then sees the other players as if they were on your LAN.

Most problems come from something on the PC getting in the way of that private
network: a VPN or proxy app, or a firewall. Find your symptom below.

- [I use a VPN or proxy](#i-use-a-vpn-or-proxy)
- [It says "timeout waiting for Tailscale service to enter a Running state"](#timeout-waiting-for-running-state)
- [Only one of us sees the other](#only-one-of-us-sees-the-other)
- [We both see each other but no game shows up in Local Matches](#no-game-in-local-matches)
- [`tailscale status` says it can't connect, or shows "Logged out"](#tailscale-status-says-it-cant-connect)
- [Grant permission / capability errors](#grant-permission--capability-errors)
- [Still stuck?](#still-stuck)

## I use a VPN or proxy

Hebnix's network helper deliberately goes around other VPNs, so it doesn't loop
through them. Most VPN and proxy apps are fine with that. Some of them block it,
or answer name lookups with fake addresses, and then the connection fails.

### Clash, Mihomo, Clash Verge, FlClash, sing-box, v2rayN, Hiddify, Throne

Your app is in **TUN mode**, usually with **fake-IP DNS** (addresses like
`198.18.x.x`). The helper gets a fake address for the multiplayer server and
can't reach it. The fix is to let the multiplayer traffic go direct.

In the config file (or the app's DNS override / rules settings), add:

```yaml
dns:
  fake-ip-filter:
    - "mp.hebnix.com"
    - "+.tailscale.com"
    - "+.tailscale.io"

tun:
  route-exclude-address:
    - 10.242.77.0/24

rules:
  # put these at the very top of your rules
  - DOMAIN,mp.hebnix.com,DIRECT
  - PROCESS-NAME,tailscaled,DIRECT
  - IP-CIDR,10.242.77.0/24,DIRECT,no-resolve
```

If your rules use a named "no VPN" group instead of `DIRECT` (for example
`🔓 NO VPN`), use that name.

Restart the app, then in Hebnix click **Disconnect** and connect again.

Still failing? Try `strict-route: false` under `tun:`. Strict routing can block
traffic that goes around the tunnel.

### Mullvad, Proton VPN, NordVPN, Windscribe, and other VPN apps

- Turn on **split tunneling** (or "exclude apps") and exclude **Hebnix** and
  **tailscaled** (on Windows: `hebnix.exe` and Hebnix's tailscale helper).
- Turn off the **kill switch** / "block connections without VPN" / "lockdown
  mode" while playing, or allow LAN / local network sharing. A kill switch
  drops everything that doesn't go through the VPN.
- Quickest test: disconnect the VPN and try again. If it works then, it's
  the VPN's settings.

### Your country blocks direct connections

If you need a proxy to reach most sites, direct connections to the multiplayer
server or to other players may be blocked by your ISP. The fixes above can't
help then. Try a different network (a mobile hotspot, for example) to confirm.

## Timeout waiting for Running state

`timeout waiting for Tailscale service to enter a Running state` means the
helper started but couldn't reach the multiplayer server in 30 seconds.

1. VPN or proxy running? See [the section above](#i-use-a-vpn-or-proxy). This
   is the most common cause.
2. Check the helper's log. It shows the real reason:
   - Linux: `~/.config/hebnix/multiplayer-lan/tsnet-state/tailscaled.log`
   - Look for lines like `fetch control key: ... connection timed out` or
     `dial tcp 198.18...`. A `198.18.x.x` address means a proxy's fake-IP DNS.
3. Is the whole internet reachable from that PC right now?

## Only one of us sees the other

Player A sees player B in "Maps in use", but B doesn't see A. A's PC is
blocking connections that come **in** over the private network. That's almost
always a firewall.

**Linux**, on the player who can't be seen:

```sh
# which firewall is on?
sudo ufw status
systemctl is-active firewalld
```

- ufw (CachyOS, Ubuntu, Mint and more enable it by default):
  `sudo ufw allow in on hebnixts0`
- firewalld (Fedora, openSUSE):
  `sudo firewall-cmd --permanent --zone=trusted --add-interface=hebnixts0 && sudo firewall-cmd --reload`

Both only open Hebnix's private network interface, not the whole PC. Then both
players disconnect and connect again in Hebnix.

**Windows**: allow Hebnix and its tailscale helper when Windows Defender
Firewall asks, on **private** networks. Third-party firewalls and antivirus
suites may need the same exception.

A VPN or proxy on the player who **can't see** the other can cause this too. See
[I use a VPN or proxy](#i-use-a-vpn-or-proxy).

## No game in Local Matches

Both players see each other in Hebnix, but **Local Matches** is empty.

- **Start Rocket League from Hebnix** (the Multiplayer tab's
  "Start Rocket League" button). Hebnix adds a launch option that puts Rocket
  League on the private network. Started from Steam, Epic or Heroic directly,
  it won't be.
- **Both players need the same Workshop map** in the same slot. "Maps in use"
  shows who has what, and you can download a missing map from a player there.
- While the host is in a LAN match, the host's **"sent"** counter should go up.
  If it stays at 0, the host's Hebnix isn't picking up Rocket League's LAN
  announcement. Update to the newest Hebnix, restart Rocket League from Hebnix,
  and check the log for `Beacon relay` lines. The "received" counter always
  shows 0; that's normal.
- The player who can't see the game: check
  [Only one of us sees the other](#only-one-of-us-sees-the-other). The same
  firewall block stops the game announcements.
- Press **Refresh List** in Local Matches after the host has started the match.

## `tailscale status` says it can't connect

Hebnix runs its **own** tailscale helper with a private control socket. Plain
`tailscale status` looks for the system one, so it fails or talks to the wrong
one. Use Hebnix's socket instead (Linux):

```sh
tailscale --socket="$XDG_RUNTIME_DIR/hebnix/tailscaled.sock" status
```

A working connection lists your own `10.242.77.x` address and the other
players. "Logged out" means the helper never reached the server. See
[Timeout waiting for Running state](#timeout-waiting-for-running-state).

You do **not** need to install or enable the system `tailscaled` service. If
you already use Tailscale for other things, turn the system one off while
playing. Two tailscale helpers at once fight over the same routing table.

## Grant permission / capability errors

On Linux, Workshop multiplayer needs three network permissions on the Hebnix
binary. Click **Grant permission** in the Multiplayer tab, or run:

```sh
sudo setcap cap_net_admin,cap_net_raw,cap_net_bind_service=eip /usr/bin/hebnix
```

Check with `getcap /usr/bin/hebnix`. Updating or reinstalling Hebnix can
remove the permission again.

## Still stuck?

Open an issue on GitHub (or ask in Discord) with:

- your OS, and whether you use a VPN, proxy or firewall app
- the error text from Hebnix's Multiplayer tab
- the last 60 lines of the tailscale helper log (Linux:
  `~/.config/hebnix/multiplayer-lan/tsnet-state/tailscaled.log`)
- the Workshop multiplayer lines from Hebnix's own log:
  `grep -iE "tsnet|beacon|map sync" ~/.config/hebnix/hebnix.log | tail -40`

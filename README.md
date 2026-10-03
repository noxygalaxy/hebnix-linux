# Hebnix (Linux)

This is a Linux port of [Hebnix](https://hebnix.com), targeting **Hyprland**
(or another wlroots-based Wayland compositor with `wlr-layer-shell` support, such as Sway).
There are also backends for niri, i3 and EWMH X11 desktops (Linux Mint's Cinnamon, XFCE, MATE), and a
best-effort one for KDE Plasma/KWin. Several of those have not been tested against a real session —
see [Known limitations](#known-limitations).

## Installing

### Arch Linux

- Add it as a custom repository to pacman
`echo -e "\n[hebnix-linux]\nSigLevel = Optional TrustAll\nServer = https://repo.xplodingeggo.space/x86_64/" | sudo tee -a /etc/pacman.conf`
- then install it whichever way you choose
prebuilt binary: `sudo pacman -S hebnix-linux-bin`
build from source `sudo pacman -S hebnix-linux`
### Other distros

- **AppImage** (recommended) — download the latest `Hebnix-*-x86_64.AppImage`
  from the [releases page](https://github.com/xplodingeggo/hebnix-linux/releases),
  `chmod +x` it, and run it. No install step, works on most distros.
- **Build from source** (developers/advanced users) — see
  [Building from source](#building-from-source) below.

### Development / nightly builds

Bleeding-edge builds from `main` are published as the rolling
[`nightly` prerelease](https://github.com/xplodingeggo/hebnix-linux/releases/tag/nightly)
(tarball + AppImage). These are development builds, not stable releases —
package-manager and stable-AppImage installs are never silently upgraded
onto them.

## Where your data lives

Config, plugins, and themes are stored at
`$XDG_CONFIG_HOME/hebnix` (falling back to `~/.config/hebnix` if that's
unset) 


## Workshop LAN multiplayer

Workshop multiplayer joins a private Tailscale network (tailnet) for the
session. Hebnix starts its **own** `tailscaled` with its own interface
(`hebnixts0`), socket and state, separate from any Tailscale you already
use, so you only need the `tailscale` package installed (Arch: `sudo pacman
-S tailscale`; others: https://tailscale.com/download/linux). The system
`tailscaled` service does not need to be enabled. No package? Put the
`tailscale` and `tailscaled` binaries in `~/.config/hebnix/tailscale-bin/`.

It needs `CAP_NET_ADMIN` and `CAP_NET_RAW` on the `hebnix` binary (for the
network interface, firewall rules and the LAN beacon relay). The
Multiplayer tab has a Grant permission button, and `install.sh` offers to
do it after installing. To do it yourself:

```sh
sudo setcap cap_net_admin,cap_net_raw+eip ~/.local/bin/hebnix
```

Everything else in the app works fine without it — this only gates LAN
multiplayer.

Using a VPN or proxy (Clash/Mihomo, Mullvad and so on), a firewall, or
stuck? See [Workshop multiplayer help](docs/workshop-multiplayer-help.md).

### Importing maps / Steam Workshop downloads

The Import Map tab adds a `.upk`/`.udk` you already have, or a `.zip` with
the map inside (unrelated files in the zip are skipped). It also lists the
[RL Workshop Archive](https://xplodingeggo.github.io/RLWorkshopCollection/)
for direct downloads; anyone can request a Workshop map there. Downloading
straight from the Steam Workshop by item id also needs a .NET 9 (or newer)
runtime (`dotnet-runtime`) and DepotDownloaderMod's files in
`~/.config/hebnix/depotdownloader/`.

## Build & install

```sh
git clone https://github.com/xplodingeggo/hebnix-linux.git
cd hebnix-linux
./install.sh
```

`install.sh` checks your dependencies, offers to set up hotkey/uinput
access and the Workshop LAN multiplayer permission (both below), then
builds and installs with `make install`. No sudo needed for the build or
install itself — it just drops `hebnix` into `~/.local/bin` along with a
`.desktop` entry and icon.

To do it by hand instead:

```sh
make release   # binary lands at target/release/hebnix-app
make install    # -> ~/.local/bin/hebnix (override with PREFIX=/some/where)
```

Run it with `hebnix` (make sure `~/.local/bin` is on your `PATH`) or from
your application menu. First run creates an empty `plugins/` folder under
`$XDG_CONFIG_HOME/hebnix` (see [Where your data lives](#where-your-data-lives)).
Plugins aren't bundled in this repo — install them from the app's own
Plugins tab, or clone a plugin repo (e.g.
[`rl-profiles-linux`](https://github.com/xplodingeggo/rl-profiles-linux))
into that `plugins/` folder yourself.

## Optional: hotkeys, binds & chat-send plugins

Reading the show/hide hotkey, controller binds, `hebnix.is_bind_pressed`,
etc goes through `/dev/input/event*`, so your user needs to be in the
`input` group:

```sh
sudo usermod -aG input $USER
# log out and back in (or reboot) for it to take effect
```

Plugins that *send* input — `hebnix.input.send`, `hebnix.chat.send`, e.g.
quick-chat plugins — also need a virtual keyboard via `/dev/uinput`. That
device isn't `input`-group-writable by default, so it needs its own kernel
module + udev rule.

AUR installs (`hebnix-linux`/`hebnix-linux-bin`) already ship that module
config and udev rule, so you only need the group step above. Building from
source, `install.sh` offers to set both up for you (needs sudo once). To do
it by hand:

```sh
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
sudo modprobe uinput
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/60-hebnix-uinput.rules
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput
```

Neither of these is required — without them the app still runs fine,
hotkeys/binds just read as "not pressed" and chat-send plugins can't type
until it's fixed.

## Building from source

### Requirements

- **Rust** (stable), via [rustup](https://rustup.rs)
- A C compiler (`gcc`/`clang`), for vendored Lua and a couple other native
  deps
- GTK3, Wayland client headers, and an app-indicator library (for the
  system tray icon)

### Arch Linux

```sh
sudo pacman -S --needed base-devel gtk3 libayatana-appindicator wayland libxkbcommon \
  systemd-libs alsa-lib openssl xdotool libx11 libxtst libxi webkit2gtk-4.1 libsoup3 gtk-layer-shell
```

### Debian / Ubuntu

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev \
  libwayland-dev libxkbcommon-dev libudev-dev libasound2-dev libssl-dev libxdo-dev \
  libx11-dev libxtst-dev libxi-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libgtk-layer-shell-dev
```

### Fedora

```sh
sudo dnf install gcc gtk3-devel libappindicator-gtk3-devel wayland-devel libxkbcommon-devel \
  systemd-devel alsa-lib-devel openssl-devel libxdo-devel libX11-devel libXtst-devel \
  libXi-devel webkit2gtk4.1-devel libsoup3-devel gtk-layer-shell-devel
```

Don't want to copy-paste package lists? `install.sh` checks all of this
for you and tells you what's missing.



## Controllers
everything will work bro.
As long as it works with evdev it will work.

## Known limitations
- On KDE Plasma (Wayland), you'll need `kdotool` for some things to work.
- Window/overlay support by desktop (Sway, i3 and X11 are new and untested on real sessions, reports welcome):
  - **Hyprland, niri, Sway**: full support, overlay via `wlr-layer-shell`. On Sway run Rocket League borderless windowed, not Sway-fullscreen, or the overlay and the F2 pop-over can't show over it.
  - **KDE Plasma (Wayland)**: focus tracking via `kdotool`; the overlay needs KWin's layer-shell support.
  - **i3, Linux Mint (Cinnamon), XFCE, MATE, GNOME/KDE on X11**: focus, always-on-top and the overlays use an X11 backend. The overlays need a compositing manager: Cinnamon/Muffin and XFCE have one built in (turn compositing on), on i3 install `picom`. Run Rocket League borderless windowed so the overlay isn't covered. On i3 the tray icon needs a bar with a tray (e.g. i3bar `tray_output`, or `snixembed` for StatusNotifier icons).
  - **GNOME on Wayland**: no layer-shell, so no overlay.
- Other desktops: the main problem you will have (if any) is the window not coming to the front or not disappearing properly when you press f2; the rest should work as long as you have gtk3 and either Wayland or X11.

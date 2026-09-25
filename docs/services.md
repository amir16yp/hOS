# hOS init and system services

[Back to the project guide](../README.md)

`hos-init` is PID 1. It is a supervisor and a service API, not a general
process manager: it starts four system services and the desktop session, keeps
them running, answers a control socket and brings the machine down. The
services own the policy for their area, and every one of them speaks the same
line protocol, so `hosctl`, the [settings application](#the-settings-application)
and the services themselves all use one client.

| Program | Socket | Responsibility |
| --- | --- | --- |
| `hos-init` | `/run/hos/init.sock` | supervision, service control, shutdown |
| `hos-netd` | `/run/hos/netd.sock` | link state, DHCP, DNS, Wi-Fi |
| `hos-power` | `/run/hos/power.sock` | battery, power button, suspend, shutdown requests |
| `hos-ntpd` | `/run/hos/ntpd.sock` | time synchronization, parked while offline |
| `hos-soundd` | `/run/hos/soundd.sock`, `/run/hos/sound.pcm` | default device, volume, mute, mixing |

Source lives under `HOSWM/src/init`: one module per service, the protocol in
`ipc.rs`, the supervisor in `supervisor.rs` and `unit.rs`, and the service
executables in `src/init/bin`.

## Supervision

Units are declared in `unit.rs` and may be turned off or redirected in
`/etc/hos/services.conf`. Each unit is restarted when it exits, with a delay
that doubles from one second to thirty; a run that stays up for thirty seconds
clears the failure count. After ten consecutive failures the supervisor stops
trying and says so on the console, leaving `hosctl init start hos-netd` to
resume. `hos-ntpd` waits for `hos-netd` to be running before it starts.

The desktop session is supervised the same way: the greeter, or
`/etc/hos-session` when the installer wrote one, or the live startup script.

Shutdown stops the services in reverse order with `SIGTERM`, waits up to five
seconds for them to save their state, then performs the usual last rites —
`SIGTERM` and `SIGKILL` to everything, `sync`, `umount -a -r`, `reboot(2)`.
`reboot`, `poweroff` and `halt` are links to `hos-init`; running one sends the
request to PID 1 over the socket, falling back to a signal.

## The service protocol

Requests are one line; a reply is zero or more records followed by exactly one
result line:

```text
$ hosctl net status
online=yes  links=2  gateway=10.0.2.2  route_iface=eth0  dns=10.0.2.3
iface=eth0  kind=wired  method=dhcp  operstate=up  carrier=yes  up=yes ...
```

| Prefix | Meaning |
| --- | --- |
| `=` | one record of `key=value` fields |
| `+` | the request succeeded; the rest of the line is a message |
| `-` | the request failed; the rest of the line is the reason |
| `*` | an event, sent after `SUBSCRIBE`, never inside a response |

Values escape what would break the framing: a space is `\s`, a backslash
`\\`, a newline `\n`, a tab `\t` and an empty value `\e`. Every service
answers `PING`, `HELP`, `SUBSCRIBE`, `UNSUBSCRIBE`, `RELOAD` and `QUIT`.

Requests from a process that is not root are refused unless the service
publishes the verb. Reading state is always public; changing it generally is
not, except where a service's own policy opens it up: `allow-users` in
`power.conf` permits suspend and shutdown requests, and `allow-users` in
`netd.conf` permits joining Wi-Fi networks. Volume and mute are public
because a desktop user is expected to change them.

Each service also accepts `SET section key value`, which writes the change to
its file in `/etc/hos` and reloads, so a setting applies now and after a
reboot. Only the keys a service owns are accepted.

## hos-netd

Interfaces are read from `/sys/class/net` and re-read whenever a netlink
message arrives, with a two second sweep as a fallback. Every sweep brings up
the links it is meant to manage: the kernel reports no carrier at all for an
interface that is administratively down, so a link left down after boot would
never look connected and would never start its DHCP client. Loopback is left
to the address the service gives it, a link whose method is `off` is left
alone, and `hosctl net down eth0` stays in effect until the matching `up`.
A wired link that has carrier is then configured; a wireless link is
associated first. Addresses, netmasks and default routes are set with the
classic `SIOC*` ioctls, and DNS servers are written to `/etc/resolv.conf`.

All of this needs an IPv4 stack in the kernel and a driver for the adapter.
The configuration in `tooling/kernel.config` builds in `CONFIG_INET`,
`CONFIG_PACKET` for the supplicant's EAPOL frames, virtio-net, the common
Intel, Realtek and AMD PCI parts, USB Ethernet, and cfg80211/mac80211 with
ath9k. A kernel without them leaves only loopback, and every `AF_INET` socket
fails with `EAFNOSUPPORT`. Wireless adapters whose firmware has to be loaded
from disk cannot work in this image, which carries no firmware files.

The DHCP client (`dhcp.rs`) implements the exchange directly: `DISCOVER`,
`OFFER`, `REQUEST`, `ACK`, renewal at T1, rebinding at T2 and a fresh start
when a lease expires or a server says no. It asks for the subnet mask, router,
DNS servers, domain and lease time, and sends the host name.

Wi-Fi is driven through `wpa_supplicant`'s control socket: the service starts
one per wireless interface and speaks its text protocol for scanning, joining
and saving networks. The image carries a `wpa_supplicant`: the build host's,
or one the build makes from upstream source and links statically against musl.
That source build uses wpa_supplicant's own crypto and joins WPA2 and open
networks; WPA3 (SAE) needs a supplicant built against a TLS library. An image
built with `HOS_WIFI=0` has none, and then the Wi-Fi verbs report that it is
missing rather than failing in a way that looks like broken hardware; wired
networking does not depend on it either way.

```text
hosctl net status                  # links, addresses, gateway, DNS
hosctl net links
hosctl net up eth0 | down eth0
hosctl net renew [eth0]
hosctl net dns set 1.1.1.1 9.9.9.9
hosctl net wifi scan | list | status
hosctl net wifi connect "Cafe Wifi" passphrase
hosctl net wifi forget "Cafe Wifi"
hosctl net set "interface eth0" method static
```

Events: `event=link`, `event=address`, `event=online`, `event=wifi`.

## hos-power

Batteries and mains come from `/sys/class/power_supply`; the power button, the
sleep key and the lid switch come from the input devices that carry them. The
policy in `/etc/hos/power.conf` decides what a press does: `shutdown`,
`suspend`, `ask` (publish an event and let the desktop choose) or `ignore`.
Suspend writes `mem`, or `freeze`, to `/sys/power/state`. Shutdown and reboot
are requests forwarded to `hos-init`, which owns the `reboot(2)` call.

Long-running work can hold the machine up: `INHIBIT shutdown "installing hOS"`
blocks shutdown until it is released or the process that asked disappears.

```text
hosctl power status
hosctl power suspend
hosctl power inhibit sleep "playing a film"
hosctl power set power button ask
```

Events: `event=battery`, `event=ac`, `event=low`, `event=critical`,
`event=button`, `event=sleep`, `event=shutdown`, `event=ask`.

## hos-ntpd

An SNTP client that stays asleep while the machine is offline: it subscribes to
`hos-netd` and only queries servers once the network is up. Errors of half a
second or more step the clock; smaller ones are slewed so time never runs
backwards. The poll interval doubles from 64 seconds to 1024 while the clock
stays accurate. A machine without a working hardware clock is moved forward to
the last time the service recorded in `/var/lib/hos/ntp.stamp`.

```text
hosctl time status
hosctl time sync
hosctl time set ntp servers pool.ntp.org
```

## hos-soundd

The service owns the default device, the volume and mute, and mixes what
applications play.

Volume and mute are the card's own mixer controls, read and written through
the ALSA control device with the ioctls in `alsa.rs`; no alsa-lib is involved.
Cards without a volume control fall back to applying volume in software to
what the mixer plays.

Because there is no alsa-lib in the image, `dmix` is only available to programs
that bring their own. The service still writes `/etc/asound.conf` with a
`dmix`/`dsnoop` default for those, and provides hOS applications with a mixer
of its own: they connect to `/run/hos/sound.pcm`, send a short header and write
16-bit samples, and the service resamples, sums and writes one period at a time
to the card. Several windows can therefore play at once.

`/proc/asound` is also read, so programs that open the card directly still
appear in `hosctl sound streams`.

```text
hosctl sound status
hosctl sound volume +5
hosctl sound mute toggle
hosctl sound default 1
hosctl sound play /root/notify.wav          # uncompressed WAV
hosctl sound streams
```

Events: `event=volume`, `event=default`, `event=stream`.

The application-facing API is documented with the rest of the window API in
[applications](applications.md#sound): `hoswm::audio` for Rust and
`hos_sound_*` for C.

## Configuration files

`/etc/hos` holds one file per service. Each is written with documented
defaults the first time its service runs, the same way `~/.hoswm/config.ini`
is, so the file on disk always shows what the system is doing.

| File | Contents |
| --- | --- |
| `services.conf` | which units the supervisor starts |
| `netd.conf` | per-interface method, static addresses, `allow-users` |
| `power.conf` | button, lid, battery thresholds, `allow-users` |
| `ntpd.conf` | servers, step threshold, poll interval |
| `soundd.conf` | default card, startup volume, mixing rate |

`/var/lib/hos` holds state that should survive a restart: the saved volume and
the last known good time. `/run/hos` holds the sockets and is a tmpfs.

## The settings application

`hos-settings` is the graphical front end to all of this, and to the desktop's
own `~/.hoswm/config.ini`. It has a page per area — Network, Wi-Fi, Sound,
Power, Date & time, Desktop and Services — and uses the same service API as
`hosctl`, so a change made in the window is a change made to the running
service and to its configuration file. Desktop settings are written into
`config.ini` in place, keeping its comments, and apply when the session
restarts; the Desktop page offers to restart it.

Settings that need root are shown to everyone but refused with an explanation
when the session is not running as root, rather than hidden.

## Writing another service

A service is a type implementing `hoswm::init::ipc::Service`: `handle` answers
requests, `tick` does periodic work and returns how long the loop may sleep,
`sources` adds descriptors to wait on, and `events` publishes state changes.
`ipc::serve` owns the socket, the poll loop and signal handling. Adding the
program to the table in `unit.rs` is what makes `hos-init` supervise it.

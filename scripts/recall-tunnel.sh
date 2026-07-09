#!/usr/bin/env bash
# recall-tunnel.sh — publish the recall web app off-LAN via a reverse SSH tunnel.
#
# The web app (recall-api, 127.0.0.1:8000 on this Mac) is exposed on Isis's
# WireGuard address 10.100.0.2:8000, so any VPN peer (phones/laptops) can reach
# it from anywhere the tunnel is up — WITHOUT opening a single inbound port on
# the Mac.
#
# Why a reverse tunnel (push, not pull): the Mac is a ONE-WAY WireGuard peer — it
# may dial OUT, but nothing on the VPN may connect back to it (mac pf + the
# servers' iptables drop anything toward 10.100.0.11). So Isis cannot reach the
# Mac's app; instead the Mac dials Isis and forwards the app backwards over that
# connection. Same constraint the nightly recall-backup push lives under.
#
# Transport vs publish — two different addresses, on purpose:
#   * We CONNECT to Isis's PUBLIC sshd (isis.xinutec.org:22, an always-on
#     lifeline). We deliberately do NOT ride the Mac's WireGuard for this leg —
#     the Mac's WG is a one-way, not-always-connected peer, and SSH already
#     encrypts this hop, so tunnelling inside WG would only add a fragile
#     dependency, not security.
#   * We PUBLISH on Isis's WireGuard IP 10.100.0.2 — so the app is reachable by
#     VPN peers ONLY. The public NIC never serves it (isis allowedTCPPorts = []),
#     and binding the WG address needs `GatewayPorts clientspecified` on isis
#     sshd (nixos-config machines/isis/configuration.nix).
# WireGuard's peer keys are the client-side authentication; there is no TLS
# because the client<->Isis hop is already WG-encrypted.
#
# Run under launchd KeepAlive: if ssh exits (link drop, Isis reboot) launchd
# relaunches it; ServerAlive* makes ssh notice a dead link and exit promptly so
# the tunnel re-establishes. No autossh needed.
set -euo pipefail

ISIS_SSH=isis.xinutec.org   # public sshd endpoint — transport for this leg
ISIS_VPN=10.100.0.2         # WireGuard IP the app is published on (peers only)
PORT=8000

exec /usr/bin/ssh -N \
  -o BatchMode=yes \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=30 \
  -o ServerAliveCountMax=3 \
  -o ConnectTimeout=15 \
  -R "${ISIS_VPN}:${PORT}:127.0.0.1:${PORT}" \
  "pippijn@${ISIS_SSH}"

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
# The entire path stays INSIDE WireGuard: we connect to Isis over WG (10.100.0.2)
# and publish on that same WG address, so nothing — not even this maintenance
# connection — traverses the public internet. Binding the WG address needs
# `GatewayPorts clientspecified` on isis sshd (nixos-config machines/isis), and
# the app is reachable on the WireGuard interface ONLY (isis allowedTCPPorts = []
# keeps the public NIC closed). WireGuard's peer keys are the authentication and
# the transport is WG-encrypted, so there is no TLS.
#
# DEPENDENCY: this needs the Mac's own WireGuard tunnel ("xinutec") to be up.
# It is a manually/on-demand peer, so if WG drops, the tunnel drops until WG is
# back — launchd relaunches ssh, which retries until 10.100.0.2 is reachable
# again. Keep the Mac WG persistent for uninterrupted remote access.
#
# Run under launchd KeepAlive: if ssh exits (link drop, Isis reboot, WG down)
# launchd relaunches it; ServerAlive* makes ssh notice a dead link and exit
# promptly so the tunnel re-establishes. No autossh needed.
set -euo pipefail

ISIS_VPN=10.100.0.2   # Isis over WireGuard — transport AND publish address
PORT=8000

exec /usr/bin/ssh -N \
  -o BatchMode=yes \
  -o StrictHostKeyChecking=accept-new \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=30 \
  -o ServerAliveCountMax=3 \
  -o ConnectTimeout=15 \
  -R "${ISIS_VPN}:${PORT}:127.0.0.1:${PORT}" \
  "pippijn@${ISIS_VPN}"

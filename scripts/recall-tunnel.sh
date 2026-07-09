#!/usr/bin/env bash
# recall-tunnel.sh — publish the recall web app to Isis via a reverse SSH tunnel.
#
# The web app (recall-api, 127.0.0.1:8000 on this Mac) is forwarded to Isis's
# LOOPBACK 127.0.0.1:8001 — not a public or WireGuard-routable address. The only
# thing fronting it on Isis is oauth2-proxy (nixos-config machines/isis), which
# requires a Nextcloud login (dash.xinutec.org) restricted to the `recall` NC
# group, and terminates TLS as https://recall.xinutec.org. This script never
# opens a port reachable by anything other than oauth2-proxy itself.
#
# Why a reverse tunnel (push, not pull): the Mac is a ONE-WAY WireGuard peer — it
# may dial OUT, but nothing on the VPN may connect back to it (mac pf + the
# servers' iptables drop anything toward 10.100.0.11). So Isis cannot reach the
# Mac's app; instead the Mac dials Isis and forwards the app backwards over that
# connection. Same constraint the nightly recall-backup push lives under.
#
# Transport rides WireGuard end to end (10.100.0.2) — Pippijn's choice to keep
# this maintenance connection inside the VPN rather than public SSH.
#
# DEPENDENCY: needs the Mac's own WireGuard tunnel ("xinutec") up — see the
# wg-ensure agent (xinutec-infra/mac-mini) that keeps it connected.
#
# Run under launchd KeepAlive: if ssh exits (link drop, Isis reboot, WG down)
# launchd relaunches it; ServerAlive* makes ssh notice a dead link and exit
# promptly so the tunnel re-establishes. No autossh needed.
set -euo pipefail

ISIS_VPN=10.100.0.2   # Isis over WireGuard — transport address
PORT=8000
ISIS_BIND=127.0.0.1   # loopback-only on Isis; oauth2-proxy is the only fronting

exec /usr/bin/ssh -N \
  -o BatchMode=yes \
  -o StrictHostKeyChecking=accept-new \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=30 \
  -o ServerAliveCountMax=3 \
  -o ConnectTimeout=15 \
  -R "${ISIS_BIND}:8001:127.0.0.1:${PORT}" \
  "pippijn@${ISIS_VPN}"

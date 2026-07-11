"""Mac→fleet sync — the security core of the proposed Isis/Mac split.

See `docs/isis-migration.md`. The Mac is a one-way WireGuard peer: it may dial the
fleet, nothing may dial back. So every exchange is **Mac-initiated** — the Mac POLLS the
fleet for jobs (it has the ML) and PUSHES results to the fleet's system of record. This
module is the transport + auth for that inversion.

It is **inert unless `RECALL_SYNC_TOKEN` is set**: importing it changes nothing, and the
routes are only registered when a token is configured, so a stock LAN-only deployment
is untouched. When enabled, the routes are meant to bind to the WireGuard interface only
— never the shared public ingress, which answers on the public IP regardless of DNS.

Only the job-poll direction is here so far; pushing results (audio + turns) is the next
increment. The auth check and the bearer parsing are pure, so they're unit-tested; the
routes and client are exercised against a FastAPI test transport.
"""

from __future__ import annotations

import hmac
import os
from collections.abc import Callable

import httpx
from fastapi import FastAPI, Header, HTTPException
from pydantic import BaseModel

from recall.schemas import OkOut
from recall.store import RefineRequest, Store

SYNC_TOKEN_ENV = "RECALL_SYNC_TOKEN"
_BEARER = "Bearer "


def sync_token() -> str | None:
    """The secret the Mac presents to the fleet; None when the split is off."""
    return os.environ.get(SYNC_TOKEN_ENV)


def bearer(header: str | None) -> str | None:
    """The token from an ``Authorization: Bearer <token>`` header, or None."""
    if header and header.startswith(_BEARER):
        return header[len(_BEARER) :]
    return None


def check_token(presented: str | None, expected: str | None) -> None:
    """Authorise a sync request. 503 when the server has no token configured (the split
    is off — never silently accept), 401 when the header is missing or wrong. Constant-
    time compare, so a wrong token leaks no timing signal."""
    if not expected:
        raise HTTPException(status_code=503, detail="sync not enabled")
    if not presented or not hmac.compare_digest(presented, expected):
        raise HTTPException(status_code=401, detail="bad sync token")


class JobOut(BaseModel):
    """A unit of work the fleet hands the Mac worker (the Mac has the ML + the mic)."""

    id: int
    type: str
    source: str
    start: str  # ISO-8601
    end: str


def _job_of(req: RefineRequest) -> JobOut:
    return JobOut(
        id=req.id,
        type="refine",
        source=req.source,
        start=req.start.isoformat(),
        end=req.end.isoformat(),
    )


def register_sync_routes(app: FastAPI, store_factory: Callable[[], Store]) -> bool:
    """Register the token-gated sync endpoints on `app`, but only when a token is
    configured — so a stock deployment is unchanged. Returns whether they were added."""
    expected = sync_token()
    if not expected:
        return False

    @app.get("/sync/jobs")
    def sync_jobs(
        authorization: str | None = Header(default=None), limit: int = 50
    ) -> list[JobOut]:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            return [_job_of(r) for r in store.pending_refine_requests(limit=limit)]
        finally:
            store.close()

    @app.post("/sync/jobs/{job_id}/done")
    def sync_job_done(
        job_id: int, authorization: str | None = Header(default=None)
    ) -> OkOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            store.mark_refine_request_done(job_id)
        finally:
            store.close()
        return {"ok": True}

    return True


class SyncClient:
    """Mac-side client: dials the fleet (never the reverse). Every call carries the
    bearer token and targets the WireGuard address of the host holding the store. The
    httpx client is injectable, so the wire contract is tested against a transport."""

    def __init__(
        self,
        base_url: str,
        token: str,
        *,
        timeout: float = 30.0,
        client: httpx.Client | None = None,
    ) -> None:
        self._base = base_url.rstrip("/")
        self._client = client or httpx.Client(timeout=timeout)
        self._headers = {"Authorization": f"{_BEARER}{token}"}

    def poll_jobs(self, *, limit: int = 50) -> list[JobOut]:
        """Pull pending jobs from the fleet (a cheap reachability check when empty)."""
        resp = self._client.get(
            f"{self._base}/sync/jobs", params={"limit": limit}, headers=self._headers
        )
        resp.raise_for_status()
        return [JobOut.model_validate(job) for job in resp.json()]

    def mark_done(self, job_id: int) -> None:
        """Tell the fleet a job is finished so it isn't handed out again."""
        resp = self._client.post(
            f"{self._base}/sync/jobs/{job_id}/done", headers=self._headers
        )
        resp.raise_for_status()

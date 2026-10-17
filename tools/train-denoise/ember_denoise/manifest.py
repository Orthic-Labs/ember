"""The dataset manifest (``datasets.json``) and the licence gate that ``prepare.py`` enforces.

Only ``accepted`` sources can feed a shipped model. A source is accepted only with a permissive licence from the
allow-list, a licence URL, and the date its page was fetched and read. Everything else (``rejected``,
``unverified``) is refused by ``require_accepted``.
"""

from __future__ import annotations

import json
from pathlib import Path

MANIFEST_PATH = Path(__file__).resolve().parent.parent / "datasets.json"
# Licences a shipped model may be trained on. Share-alike (CC-BY-SA), non-commercial (NC), no-derivatives (ND),
# research-only and unlicensed data are all excluded on purpose.
PERMISSIVE = {"CC0-1.0", "CC-BY-4.0", "CC-BY-3.0", "MIT", "Apache-2.0", "BSD-3-Clause", "PDM-1.0"}
STATUSES = {"accepted", "rejected", "unverified"}


class LicenceError(Exception):
    pass


def load(path: Path | str = MANIFEST_PATH) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        data = json.load(fh)
    validate(data)
    return data


def validate(data: dict) -> None:
    seen = set()
    for s in data["sources"]:
        sid = s["id"]
        if sid in seen:
            raise LicenceError(f"duplicate source id {sid}")
        seen.add(sid)
        if s["status"] not in STATUSES:
            raise LicenceError(f"{sid}: status must be one of {sorted(STATUSES)}")
        if s["status"] == "accepted":
            if s["licence"] not in PERMISSIVE:
                raise LicenceError(f"{sid}: accepted with non-permissive licence {s['licence']!r}")
            if not str(s.get("licence_url", "")).startswith("https://"):
                raise LicenceError(f"{sid}: accepted without an https licence_url")
            if not s.get("verified_on"):
                raise LicenceError(f"{sid}: accepted without a verified_on date")
        if s["status"] == "rejected" and s.get("licence") in PERMISSIVE:
            raise LicenceError(f"{sid}: rejected but its licence {s['licence']!r} is on the allow-list; explain or fix")


def require_accepted(source_id: str, data: dict | None = None) -> dict:
    data = data or load()
    for s in data["sources"]:
        if s["id"] == source_id:
            if s["status"] != "accepted":
                raise LicenceError(f"source {source_id!r} is {s['status']} ({s.get('licence')}): not usable for a shipped model")
            return s
    raise LicenceError(f"source {source_id!r} is not in datasets.json")

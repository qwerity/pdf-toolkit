#!/usr/bin/env python3
"""Seed `data/expected/<n>.json` — the per-document ledger of what every
extraction SHOULD produce — from the reviewed annotations and one run of the
binary, for the documents that have no ledger yet.

    python3 scripts/seed-expected.py [--force] [--refresh=model,building] [n ...]

The ledger is what the scorecard test (`crates/pdf-toolkit/tests/scorecard.rs`)
grades the toolkit against. Seeding copies the current output, so a fresh
ledger scores 1.0 by construction; its value comes from a person then
correcting the entries that are wrong against the PDF (a mislabelled page, a
missed owner, a check that should disagree). `reviewed` records which
sections a person has been through. A ledger that exists is never
overwritten without `--force`.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(ROOT, "dist", "pdf-toolkit")
PLAN = os.path.join(ROOT, "data", "plan")
ANNOTATIONS = os.path.join(ROOT, "data", "annotations")
EXPECTED = os.path.join(ROOT, "data", "expected")


def run_json(args):
    out = subprocess.run([BINARY, *args], capture_output=True, text=True, cwd=ROOT)
    if out.returncode != 0:
        return None
    return json.loads(out.stdout)


def seed(n):
    annotation = json.load(open(os.path.join(ANNOTATIONS, f"{n}.json")))
    pages = {}
    for page in annotation["pages"]:
        entry = {"labels": page["labels"], "route": page["extraction"]["route"], "trust": page["trust"]}
        if page.get("classification"):
            entry["class"] = page["classification"]["class"]
        pages[str(page["page_number"])] = entry
    survey = {}
    for page in annotation["pages"]:
        for fact in annotation.get("facts", []):
            if fact["kind"] == "survey_area" and fact["page"] == page["page_number"]:
                survey[str(fact["page"])] = fact["number"]
    request = annotation.get("request")
    ledger = {
        "document": str(n),
        "reviewed": {
            "pages": True, "entities": True, "groups": True, "request": True,
            "georeference": True, "facts": False, "checks": False, "model": False,
        },
        "pages": pages,
        "entities": sorted(e["id"] for e in annotation["entities"]),
        "groups": [
            {"kind": g["kind"], "trust": g["trust"], "pages": g["pages"]} for g in annotation["groups"]
        ],
        "request": (
            {k: request[k] for k in ("action", "use", "storeys", "works", "address") if k in request}
            if request
            else None
        ),
        "georeference": {
            "placed_pages": [p["page_number"] for p in annotation["pages"] if p.get("georeference")],
            "survey_area_m2": survey,
        },
        "facts": [
            {k: f[k] for k in ("kind", "page", "number", "text", "subject") if k in f}
            for f in annotation.get("facts", [])
        ],
        "checks": [
            {k: c[k] for k in ("kind", "subject", "status") if k in c} for c in annotation.get("checks", [])
        ],
        "model": None,
    }
    model_args = ["model", os.path.join("data", "plan", f"{n}.pdf")]
    params = os.path.join(ANNOTATIONS, "model", f"{n}.params.json")
    answers = os.path.join(ANNOTATIONS, "answers", f"{n}.json")
    if os.path.exists(params):
        model_args += ["--params", params]
    elif os.path.exists(answers):
        model_args += ["--answers", answers]
    parcel = os.path.join(ANNOTATIONS, "parcels", f"{n}.geojson")
    if os.path.exists(parcel):
        model_args += ["--parcel", parcel]
    model = run_json(model_args)
    building_path = os.path.join(ANNOTATIONS, "building", f"{n}.json")
    ledger["building"] = None
    if os.path.exists(building_path) and os.path.getsize(building_path) > 0:
        building = json.load(open(building_path))
        ledger["building"] = {
            "placed": building.get("placed"),
            "parts": [
                {"name": p.get("name"), "height_m": p.get("height_m"), "footprint_area": p.get("footprint_area")}
                for p in building.get("parts", [])
            ],
        }
    if model:
        ledger["model"] = {
            "placed": model["placed"],
            "floors": [
                {"level_m": f["level_m"], "top_m": f["top_m"], "slab_area_m2": f["slab_area_m2"],
                 "rooms_area_m2": f["rooms_area_m2"]}
                for f in model["floors"]
            ],
            "levels": model["model"]["levels"],
            "roof_area_m2": model["roof"]["area_m2"] if model.get("roof") else None,
            "stone_piers": model["stone_piers"],
        }
    return ledger


def refresh_sections(path, sections):
    """Re-seed only `sections` (e.g. model, building) of an existing ledger, keeping the
    rest — including a person's corrections — as they are."""
    ledger = json.load(open(path))
    fresh = seed(int(ledger["document"]))
    for section in sections:
        ledger[section] = fresh.get(section)
        ledger.setdefault("reviewed", {})[section] = False
    json.dump(ledger, open(path, "w"), ensure_ascii=False, indent=2)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    force = "--force" in sys.argv
    refresh = next((a[len("--refresh="):].split(",") for a in sys.argv if a.startswith("--refresh=")), None)
    os.makedirs(EXPECTED, exist_ok=True)
    numbers = [int(a) for a in args] or sorted(
        int(f[:-4]) for f in os.listdir(PLAN) if f.endswith(".pdf") and f[:-4].isdigit()
    )
    for n in numbers:
        path = os.path.join(EXPECTED, f"{n}.json")
        if os.path.exists(path) and refresh:
            refresh_sections(path, refresh)
            print(f"{n}: sections {', '.join(refresh)} re-seeded")
            continue
        if os.path.exists(path) and not force:
            print(f"{n}: ledger exists, kept")
            continue
        ledger = seed(n)
        json.dump(ledger, open(path, "w"), ensure_ascii=False, indent=2)
        print(f"{n}: seeded ({len(ledger['pages'])} pages, {len(ledger['facts'])} facts, "
              f"{len(ledger['checks'])} checks, model {'yes' if ledger['model'] else 'no'}, "
              f"building {'yes' if ledger.get('building') else 'no'})")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Rebuild ratspeak_pi/data/places.tsv.gz from GeoNames (geonames.org, CC BY 4.0): every place with 5,000 or
more people, as  name, plain-letters name, region, country, country code, latitude, longitude, people, time zone.
Sorted by population, largest first, so the first match for a name is the best-known place.
"""
import csv, gzip, io, pathlib, sys, urllib.request, zipfile

BASE = "https://download.geonames.org/export/dump/"
get = lambda name: urllib.request.urlopen(BASE + name, timeout=120).read()
countries = {}
for line in get("countryInfo.txt").decode().splitlines():
    if line and not line.startswith("#"):
        part = line.split("\t")
        countries[part[0]] = part[4]
regions = {}
for line in get("admin1CodesASCII.txt").decode().splitlines():
    part = line.split("\t")
    regions[part[0]] = part[1]
rows = []
with zipfile.ZipFile(io.BytesIO(get("cities5000.zip"))) as archive:
    for line in archive.read("cities5000.txt").decode().splitlines():
        p = line.split("\t")
        rows.append((int(p[14] or 0), [p[1], p[2], regions.get(f"{p[8]}.{p[10]}", ""), countries.get(p[8], p[8]), p[8],
                                        f"{float(p[4]):.4f}", f"{float(p[5]):.4f}", p[14] or "0", p[17]]))
rows.sort(key=lambda row: -row[0])
out = pathlib.Path(__file__).resolve().parent.parent / "ratspeak_pi" / "data" / "places.tsv.gz"
with gzip.GzipFile(out, "wb", mtime=0) as raw:
    text = io.TextIOWrapper(raw, encoding="utf-8", newline="\n")
    csv.writer(text, delimiter="\t", lineterminator="\n", quoting=csv.QUOTE_NONE, escapechar=None).writerows(r for _, r in rows)
    text.flush()
print(f"{len(rows)} places, {out.stat().st_size // 1024} KB", file=sys.stderr)

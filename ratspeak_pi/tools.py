"""Commands the Pi answers itself, exactly, without the model: arithmetic, units, places, time, sun, and moon.

A small model gets these wrong; code does not. Each function takes the text after the command and returns
the reply. A mistake in the request comes back as a one-line example of how to ask.
"""
from __future__ import annotations

import ast
import csv
import gzip
import math
import operator
import re
import time
import unicodedata
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

PLACES = Path(__file__).parent / "data" / "places.tsv.gz"     # GeoNames, CC BY 4.0: see scripts/build-places.py

# ---- /calc ----------------------------------------------------------------------------------------------------------

_BINARY = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul, ast.Div: operator.truediv,
           ast.FloorDiv: operator.floordiv, ast.Mod: operator.mod, ast.Pow: operator.pow}
_NAMES = {"pi": math.pi, "e": math.e}
_FUNCTIONS = {"sqrt": math.sqrt, "sin": math.sin, "cos": math.cos, "tan": math.tan, "log": math.log10, "ln": math.log,
              "abs": abs, "round": round, "floor": math.floor, "ceil": math.ceil}


def _value(node):
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)) and not isinstance(node.value, bool):
        return node.value
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        return -_value(node.operand) if isinstance(node.op, ast.USub) else _value(node.operand)
    if isinstance(node, ast.BinOp) and type(node.op) in _BINARY:
        left, right = _value(node.left), _value(node.right)
        if isinstance(node.op, ast.Pow) and (abs(right) > 1000 or abs(left) > 1e100):
            raise ValueError("too large")          # 9**9**9 must not tie up the Pi
        return _BINARY[type(node.op)](left, right)
    if isinstance(node, ast.Name) and node.id.lower() in _NAMES:
        return _NAMES[node.id.lower()]
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id.lower() in _FUNCTIONS and not node.keywords:
        return _FUNCTIONS[node.func.id.lower()](*[_value(arg) for arg in node.args])
    raise ValueError("unsupported")


def _number(value: float) -> str:
    if isinstance(value, int) or float(value).is_integer() and abs(value) < 1e15:
        return f"{int(value):,}"
    return f"{value:,.6g}" if abs(value) >= 1e-4 else f"{value:.6g}"


# 1,234,567: digits in groups of three. Any other comma separates arguments, as in round(3.14159, 2).
_THOUSANDS = re.compile(r"(?<![\d.])\d{1,3}(?:,\d{3})+(?![\d,])")


def calc(text: str) -> str:
    expression = text.replace("^", "**").replace("×", "*").replace("÷", "/").strip()
    expression = _THOUSANDS.sub(lambda match: match.group().replace(",", ""), expression)
    try:
        if not expression or len(expression) > 200:
            raise ValueError
        return f"{text.strip()} = {_number(_value(ast.parse(expression, mode='eval').body))}"
    except ZeroDivisionError:
        return "That divides by zero."
    except (ValueError, SyntaxError, TypeError, OverflowError, RecursionError):
        return "I can work out + - * / ^, brackets, sqrt, sin, cos, tan, log, and ln. Example: /calc (17 + 4) * 3"


# ---- /convert -------------------------------------------------------------------------------------------------------

# Each family: unit -> how many of the family's base unit it is.
_UNITS = {
    "length": {"mm": 0.001, "cm": 0.01, "m": 1, "km": 1000, "in": 0.0254, "ft": 0.3048, "yd": 0.9144, "mi": 1609.344, "nmi": 1852},
    "mass": {"mg": 1e-6, "g": 0.001, "kg": 1, "t": 1000, "oz": 0.028349523125, "lb": 0.45359237, "st": 6.35029318},
    "volume": {"ml": 0.001, "l": 1, "tsp": 0.00492892159375, "tbsp": 0.01478676478125, "floz": 0.0295735295625,
               "cup": 0.2365882365, "pt": 0.473176473, "qt": 0.946352946, "gal": 3.785411784},
    "speed": {"m/s": 1, "km/h": 1 / 3.6, "mph": 0.44704, "kn": 1852 / 3600},
    "area": {"m2": 1, "km2": 1e6, "ha": 1e4, "ft2": 0.09290304, "acre": 4046.8564224, "mi2": 2589988.110336},
    "time": {"s": 1, "min": 60, "h": 3600, "day": 86400, "week": 604800},
    "data": {"b": 1, "kb": 1e3, "mb": 1e6, "gb": 1e9, "tb": 1e12},
}
_ALIASES = {
    "millimeter": "mm", "millimetre": "mm", "centimeter": "cm", "centimetre": "cm", "meter": "m", "metre": "m",
    "kilometer": "km", "kilometre": "km", "inch": "in", "inches": "in", "foot": "ft", "feet": "ft", "yard": "yd",
    "mile": "mi", "nauticalmile": "nmi", "milligram": "mg", "gram": "g", "kilogram": "kg", "kilo": "kg", "tonne": "t",
    "ton": "t", "ounce": "oz", "pound": "lb", "lbs": "lb", "stone": "st", "milliliter": "ml", "millilitre": "ml",
    "liter": "l", "litre": "l", "teaspoon": "tsp", "tablespoon": "tbsp", "fluidounce": "floz", "pint": "pt",
    "quart": "qt", "gallon": "gal", "kph": "km/h", "kmh": "km/h", "knot": "kn", "kt": "kn", "sqm": "m2", "sqkm": "km2",
    "hectare": "ha", "sqft": "ft2", "sqmi": "mi2", "sec": "s", "second": "s", "minute": "min", "hr": "h", "hour": "h",
    "d": "day", "wk": "week", "byte": "b", "celsius": "c", "centigrade": "c", "fahrenheit": "f", "kelvin": "k",
}
_REQUEST = re.compile(r"\s*(-?[\d.,]+)\s*°?\s*(.+?)\s+(?:to|in|into|as)\s+°?\s*(.+?)\s*")


def _unit(word: str) -> str:
    word = word.lower().replace(" ", "").replace("²", "2").replace("°", "").rstrip(".")
    for candidate in (word, word.rstrip("s")):
        candidate = _ALIASES.get(candidate, candidate)
        if candidate in ("c", "f", "k") or any(candidate in family for family in _UNITS.values()):
            return candidate
    raise ValueError(word)


def convert(text: str) -> str:
    try:
        amount, source, target = _REQUEST.fullmatch(text).groups()
        value, source, target = float(amount.replace(",", "")), _unit(source), _unit(target)
        if {source, target} <= {"c", "f", "k"}:
            kelvin = {"c": value + 273.15, "f": (value - 32) * 5 / 9 + 273.15, "k": value}[source]
            result = {"c": kelvin - 273.15, "f": (kelvin - 273.15) * 9 / 5 + 32, "k": kelvin}[target]
            return f"{_number(value)} °{source.upper()} = {_number(round(result, 2))} °{target.upper()}".replace("°K", "K")
        family = next(units for units in _UNITS.values() if source in units and target in units)
        return f"{_number(value)} {source} = {_number(float(f'{value * family[source] / family[target]:.6g}'))} {target}"
    except (AttributeError, ValueError, StopIteration):
        return "Example: /convert 5 km to miles. I know length, weight, volume, temperature, speed, area, time, and data sizes."


# ---- /where, /distance, /time ---------------------------------------------------------------------------------------

def _plain(text: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFKD", text.casefold()) if not unicodedata.combining(c)).strip()


def find_place(text: str, path: Path = PLACES) -> dict | None:
    """The best-known place of that name: "Paris", "Paris, Texas", "Springfield, Illinois, US", or "48.85, 2.35"."""
    coordinates = re.fullmatch(r"\s*(-?\d+(?:\.\d+)?)\s*[, ]\s*(-?\d+(?:\.\d+)?)\s*", text)
    if coordinates:
        latitude, longitude = float(coordinates[1]), float(coordinates[2])
        if abs(latitude) <= 90 and abs(longitude) <= 180:
            return {"label": f"{latitude:.4f}, {longitude:.4f}", "lat": latitude, "lon": longitude, "people": 0, "zone": ""}
        return None
    name, *rest = [_plain(part) for part in text.split(",") if part.strip()] or [""]
    if not name:
        return None
    close = None                                                           # "New York" for "New York City"
    with gzip.open(path, "rt", encoding="utf-8", newline="") as handle:    # largest first, so the first match wins
        for row in csv.reader(handle, delimiter="\t", quoting=csv.QUOTE_NONE):
            names = (_plain(row[0]), _plain(row[1]))
            exact = name in names
            if not exact and (close or not any(known.startswith(name + " ") for known in names)):
                continue
            where = (_plain(row[2]), _plain(row[3]), row[4].casefold())
            if all(any(part == field or (len(part) > 2 and field.startswith(part)) for field in where) for part in rest):
                label = ", ".join(dict.fromkeys(part for part in (row[0], row[2], row[3]) if part))
                place = {"label": label, "lat": float(row[5]), "lon": float(row[6]), "people": int(row[7]), "zone": row[8]}
                if exact:
                    return place
                close = place
    return close


def where(text: str) -> str:
    place = find_place(text) if text.strip() else None
    if not place:
        return "I don't know that place. Try a town or city of 5,000 people or more, such as /where Lyon, France"
    people = f" About {place['people']:,} people." if place["people"] else ""
    return f"{place['label']}: {place['lat']:.4f}, {place['lon']:.4f}.{people}"


def distance(text: str) -> str:
    ends = re.split(r"\s+(?:to|and)\s+|\s*->\s*|\s*;\s*", text.strip(), maxsplit=1)
    places = [find_place(end) for end in ends] if len(ends) == 2 else []
    if len(places) != 2 or not all(places):
        return "Example: /distance Paris to Berlin. Places are towns of 5,000 people or more, or coordinates such as 48.85, 2.35."
    a, b = places
    lat1, lon1, lat2, lon2 = map(math.radians, (a["lat"], a["lon"], b["lat"], b["lon"]))
    half = math.sin((lat2 - lat1) / 2) ** 2 + math.cos(lat1) * math.cos(lat2) * math.sin((lon2 - lon1) / 2) ** 2
    km = 6371.0088 * 2 * math.asin(min(1, math.sqrt(half)))
    bearing = math.degrees(math.atan2(math.sin(lon2 - lon1) * math.cos(lat2),
                                      math.cos(lat1) * math.sin(lat2) - math.sin(lat1) * math.cos(lat2) * math.cos(lon2 - lon1))) % 360
    points = ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"]
    return (f"{a['label']} to {b['label']}: {_number(round(km))} km ({_number(round(km / 1.609344))} miles) in a straight line, "
            f"heading {points[round(bearing / 45) % 8]} ({round(bearing)}°).")


def clock(text: str, now: float | None = None) -> str:
    moment = datetime.fromtimestamp(time.time() if now is None else now, timezone.utc)
    if not text.strip():
        return f"It is {moment:%H:%M} UTC on {moment:%A, %-d %B %Y}. For a place: /time Tokyo"
    place = find_place(text)
    if not place or not place["zone"]:
        return "I don't know that place. Try a town or city, such as /time Tokyo"
    local = moment.astimezone(ZoneInfo(place["zone"]))
    return f"In {place['label']} it is {local:%H:%M} on {local:%A, %-d %B %Y} ({local:%Z})."


# ---- /sun, /moon ----------------------------------------------------------------------------------------------------
# astral (Apache-2.0, Debian python3-astral): sunrise, twilight, and moon times from the place and the date.

def nearest_zone(lat: float, lon: float, path: Path = PLACES) -> str:
    """The time zone of the closest known town, for a place given as coordinates."""
    best, zone = None, ""
    with gzip.open(path, "rt", encoding="utf-8", newline="") as handle:
        for row in csv.reader(handle, delimiter="\t", quoting=csv.QUOTE_NONE):
            gap = (float(row[5]) - lat) ** 2 + ((float(row[6]) - lon) * math.cos(math.radians(lat))) ** 2
            if best is None or gap < best:
                best, zone = gap, row[8]
    return zone


def _sky(text: str, command: str, now: float | None):
    place = find_place(text) if text.strip() else None
    if not place:
        return None, f"Name a town, a city, or coordinates, such as {command} Denver"
    try:
        from astral import Observer
    except ImportError:
        return None, f"{command} needs a newer version of this agent."
    zone = ZoneInfo(place["zone"] or nearest_zone(place["lat"], place["lon"]) or "UTC")
    today = datetime.fromtimestamp(time.time() if now is None else now, zone).date()
    return (place, zone, today, Observer(latitude=place["lat"], longitude=place["lon"])), None


def sun(text: str, now: float | None = None) -> str:
    found, problem = _sky(text, "/sun", now)
    if problem:
        return problem
    place, zone, today, observer = found
    from astral.sun import elevation, noon, sun as times
    heading = f"{place['label']}, {today:%a %-d %b}"
    try:
        day = times(observer, today, tzinfo=zone)
    except ValueError:
        up = elevation(observer, noon(observer, today, tzinfo=zone)) > 0
        return f"{heading}: the sun {'stays up all day' if up else 'does not rise'}."
    hours, minutes = divmod(round((day["sunset"] - day["sunrise"]).total_seconds() / 60), 60)
    return (f"{heading}: first light {day['dawn']:%H:%M}, sunrise {day['sunrise']:%H:%M}, sunset {day['sunset']:%H:%M}, "
            f"last light {day['dusk']:%H:%M} ({day['sunset']:%Z}). Daylight {hours}h {minutes:02d}m.")


def _phase(day) -> tuple[str, int, float]:
    from astral import moon
    age = moon.phase(day) / 28                       # 0 new, 0.25 first quarter, 0.5 full, 0.75 last quarter
    lit = round((1 - math.cos(2 * math.pi * age)) / 2 * 100)
    quarter = round(age * 4) % 4
    if abs(age - round(age * 4) / 4) < 1 / 29.53:   # within a day of the exact phase
        return ["New moon", "First quarter", "Full moon", "Last quarter"][quarter], lit, age
    return ["Waxing crescent", "Waxing gibbous", "Waning gibbous", "Waning crescent"][int(age * 4) % 4], lit, age


def moon(text: str, now: float | None = None) -> str:
    found, problem = _sky(text, "/moon", now)
    if problem:
        return problem
    place, zone, today, observer = found
    from datetime import timedelta
    from astral.moon import moonrise, moonset
    name, lit, age = _phase(today)
    times = []
    for label, event in (("moonrise", moonrise), ("moonset", moonset)):
        try:
            moment = event(observer, today, tzinfo=zone)
            times.append(f"{label} {moment:%H:%M}" if moment else f"no {label}")
        except ValueError:
            times.append(f"no {label}")
    full = next(today + timedelta(days=d) for d in range(1, 31)
                if _phase(today + timedelta(days=d - 1))[2] < 0.5 <= _phase(today + timedelta(days=d))[2])
    return f"{place['label']}, {today:%a %-d %b}: {name.lower()}, {lit}% lit. {times[0].capitalize()}, {times[1]}. Next full moon {full:%a %-d %b}."

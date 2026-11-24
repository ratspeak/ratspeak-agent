"""Private Venice text inference. Credentials never enter the mesh profile or card status."""
import fcntl
import hashlib
import json
import os
import re
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path

API = "https://api.venice.ai/api/v1"
MODEL = re.compile(r"[a-zA-Z0-9._:/-]{1,160}")
_verified = {}


def validate(model, key):
    if not isinstance(model, str) or not MODEL.fullmatch(model):
        raise ValueError("Choose a Venice text model.")
    if not isinstance(key, str) or not 1 <= len(key) <= 1024 or not all(33 <= ord(c) <= 126 for c in key):
        raise ValueError("Enter an active Venice API key.")


def selected(state: Path, fallback):
    """Older installations only recorded the model in config and venice.json."""
    try:
        return (state / "venice-model").read_text().strip()
    except FileNotFoundError:
        return fallback


def _replace(state: Path, name, text):
    with tempfile.NamedTemporaryFile(mode="w", dir=state, delete=False) as file:
        file.write(text)
        file.flush()
        os.fsync(file.fileno())
    Path(file.name).replace(state / name)


def store(state: Path, model, key, *, expected_key=None):
    """A chat change keeps the key on disk. Serialize it with installer key rotations."""
    with (state / ".venice.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if expected_key is not None and expected_key != json.loads((state / "venice.json").read_text())["key"]:
            raise ValueError("Venice key changed")
        validate(model, key)
        _replace(state, "venice.json", json.dumps({"model": model, "key": key}))
        _replace(state, "venice-model", model)


def import_card(boot, state, model):
    update, seed = boot / "venice-update.json", boot / "venice.key"
    if update.is_file():
        values = json.loads(update.read_text())
        store(state, values["model"], values["key"])
    elif seed.is_file():
        store(state, model, seed.read_text().strip())
    values = json.loads((state / "venice.json").read_text())
    validate(values["model"], values["key"])
    seed.unlink(missing_ok=True)
    update.unlink(missing_ok=True)
    return values["model"]


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_):
        return None


def _get(path, key):
    request = urllib.request.Request(f"{API}/{path}", headers={"Authorization": f"Bearer {key}"})
    with urllib.request.build_opener(_NoRedirect).open(request, timeout=12) as response:
        return json.loads(response.read(4_000_000))


@dataclass
class VerifiedCatalog:
    models: list[str]
    inference_available: bool
    has_model_limits: bool
    vision: frozenset = frozenset()     # models that accept images


def private_models(key):
    """Authenticate first. Inference availability and the models listed are separate results."""
    access = _get("api_keys/rate_limits", key)
    data = access.get("data") if isinstance(access, dict) else None
    if not isinstance(data, dict) or not isinstance(data.get("accessPermitted"), bool) or not isinstance(data.get("rateLimits"), list):
        raise ValueError("Venice model permissions unavailable")
    permitted = set()
    has_model_limits = False
    for row in data["rateLimits"]:
        if not isinstance(row, dict) or not isinstance(row.get("apiModelId"), str) or not MODEL.fullmatch(row["apiModelId"]):
            continue
        has_model_limits = True
        if not isinstance(row.get("rateLimits"), list):
            continue
        if all(isinstance(limit, dict) and type(limit.get("amount")) in (int, float) and limit["amount"] > 0
               for limit in row["rateLimits"]):
            permitted.add(row["apiModelId"])
    payload = _get("models?type=text", key)
    rows = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(rows, list):
        raise ValueError("Invalid catalog")
    order, vision = {}, set()
    for model in rows:
        if not isinstance(model, dict) or model.get("type") != "text":
            continue
        spec, model_id = model.get("model_spec"), model.get("id")
        if not isinstance(spec, dict) or spec.get("privacy") != "private" or spec.get("offline"):
            continue
        if not isinstance(model_id, str) or not MODEL.fullmatch(model_id) or (has_model_limits and model_id not in permitted):
            continue
        name = spec.get("name")
        name = (name if isinstance(name, str) else model_id).lower()
        slug = model_id.lower()
        uncensored = "uncensored" in name or "uncensored" in slug
        venice = name.startswith("venice ") or slug.startswith("venice-")
        order[model_id] = (not uncensored, uncensored and not venice, name, slug)
        capabilities = spec.get("capabilities")
        if isinstance(capabilities, dict) and capabilities.get("supportsVision") is True:
            vision.add(model_id)
    return VerifiedCatalog(sorted(order, key=order.__getitem__), data["accessPermitted"], has_model_limits, frozenset(vision))


class ModelTestFailed(RuntimeError):
    pass


def test_model(model, key, catalog=None):
    """One bounded reply from the agent’s network before accepting a new model."""
    validate(model, key)
    try:
        catalog = catalog or private_models(key)
        if not catalog.inference_available:
            raise ModelTestFailed("Venice has paused inference for this key. Check the account and reverify.")
        if model not in catalog.models:
            raise ModelTestFailed("The selected private model is unavailable with this key.")
        body = {"model": model, "messages": [{"role": "user", "content": "Reply with OK."}],
                "max_completion_tokens": 64, "venice_parameters": {"disable_thinking": True}, "stream": False}
        request = urllib.request.Request(f"{API}/chat/completions", data=json.dumps(body).encode(),
                                         headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"})
        with urllib.request.build_opener(_NoRedirect).open(request, timeout=75) as response:
            content = json.loads(response.read(128_000))["choices"][0]["message"]["content"]
        if not isinstance(content, str) or not re.sub(r"<think>.*?</think>", "", content, flags=re.S).strip():
            raise ModelTestFailed("The selected Venice model returned no test reply.")
    except urllib.error.HTTPError as error:
        reasons = {401: "Venice rejected the key.", 402: "Check your Venice credit and spending cap.",
                   403: "Check the key’s model permissions and IP restrictions.", 429: "Venice is limiting requests. Try again shortly."}
        raise ModelTestFailed(reasons.get(error.code, "Venice could not run the selected model. Try again.")) from None
    except (OSError, ValueError, LookupError, TypeError):
        raise ModelTestFailed("Couldn’t verify a reply from Venice. Check the agent’s internet connection and model.") from None


def model_command(state, current, arg):
    words = arg.split()
    listing = not words or words[0].lower() == "list"
    if listing:
        if len(words) > 2 or len(words) == 2 and not re.fullmatch(r"[1-9][0-9]{0,5}", words[1]):
            return "Send /model list or /model list <page>."
        page = int(words[1]) if len(words) == 2 else 1
    elif len(words) != 1 or not MODEL.fullmatch(arg):
        return "Send /model to see available models, then /model <model-id>."
    try:
        credentials = json.loads((state / "venice.json").read_text())
        key = credentials["key"]
        validate(credentials["model"], key)
        catalog = private_models(key)
    except (OSError, ValueError, KeyError):
        return f"Current Venice model: {current}\nCouldn’t load the model list. Try /model again."
    models = catalog.models
    if not models:
        return f"Current Venice model: {current}\nNo private chat models are available right now."
    if listing:
        per_page = 6
        pages = (len(models) + per_page - 1) // per_page
        if page > pages:
            return f"The model list has {pages} page(s). Send /model list 1."
        start = (page - 1) * per_page
        lines = [f"Current Venice model: {current}", f"Available models ({page}/{pages})",
                 *[m + (" (sees images)" if m in catalog.vision else "") for m in models[start:start + per_page]],
                 "", "Switch with /model <model-id>."]
        if not catalog.has_model_limits:
            lines.append("Catalog preview. Venice did not list model limits for this key.")
        if not catalog.inference_available:
            lines.append("Venice reports that this key can’t generate replies right now.")
        if page < pages:
            lines.append(f"Next page: /model list {page + 1}")
        return "\n".join(lines)
    if arg not in models:
        return "That model is unavailable. Send /model for the current list of private models."
    try:
        test_model(arg, key, catalog)
        store(state, arg, key, expected_key=key)
    except ModelTestFailed as error:
        return f"Model unchanged. {error}"
    except (OSError, ValueError, KeyError):
        return "Couldn’t save the model. Check the agent’s Venice settings and try again."
    return f"Venice model set to {arg}. Your next message will use it."


def _checked(state):
    """The selected model and key, re-verified every five minutes, and whether that model accepts images."""
    creds = json.loads((state / "venice.json").read_text())
    model, key = creds["model"], creds["key"]
    validate(model, key)
    # A retired or reclassified model must not silently route chats to a different privacy tier.
    cache_key = (model, hashlib.sha256(key.encode()).digest())
    until, sees = _verified.get(cache_key, (0, False))
    if time.monotonic() >= until:
        catalog = private_models(key)
        if not catalog.inference_available or model not in catalog.models:
            raise ValueError("model availability")
        sees = model in catalog.vision
        _verified.clear()
        _verified[cache_key] = (time.monotonic() + 300, sees)
    return model, key, sees


def sees_images(state) -> bool:
    try:
        return _checked(state)[2]
    except (OSError, ValueError, KeyError, RuntimeError, TypeError):
        return False


def complete(state, send, messages):
    try:
        model, key, _ = _checked(state)
        return send(API, model, messages, key, None)
    except (OSError, ValueError, KeyError, RuntimeError, TypeError):
        # Upstream bodies can echo request content. The mesh and status log get a fixed error.
        raise RuntimeError("Venice could not answer. Check the connection, model, key and account credit.") from None

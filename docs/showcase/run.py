#!/usr/bin/env python3
"""run.py FILE.jsonl [PORT] — run control requests from a JSON-lines file ({"method":..., "params":...} or {"sleep": s}).

The app's control channel needs a per-launch secret (docs/control-protocol.md): it is read from
$LIGHTCRAFT_CONTROL_TOKEN, else from the owner-only file <settings folder>/control/<port>.token that
the app writes at launch. PORT defaults to $LIGHTCRAFT_CONTROL_PORT, else 7980."""
import json, os, socket, stat, sys, time

PORT = int(sys.argv[2]) if len(sys.argv) > 2 else int(os.environ.get("LIGHTCRAFT_CONTROL_PORT") or 7980)


def settings_dirs():
    override = os.environ.get("LIGHTCRAFT_CONFIG_DIR")
    if override is not None:
        return [override] if override else []
    home = os.path.expanduser("~")
    if sys.platform == "darwin":
        base = os.path.join(home, "Library", "Application Support")
        return [os.path.join(base, "Ember"), os.path.join(base, "LightCraft")]
    if sys.platform == "win32":
        base = os.environ.get("APPDATA", "")
        return [os.path.join(base, "Ember"), os.path.join(base, "LightCraft")] if base else []
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.join(home, ".config")
    return [os.path.join(base, "ember"), os.path.join(base, "lightcraft")]


def read_token():
    env = (os.environ.get("LIGHTCRAFT_CONTROL_TOKEN") or "").strip()
    if env:
        return env
    for d in settings_dirs():
        path = os.path.join(d, "control", f"{PORT}.token")
        if not os.path.exists(path):
            continue
        if os.name == "posix" and os.stat(path).st_mode & (stat.S_IRWXG | stat.S_IRWXO):
            sys.exit(f"refusing {path}: it is readable by other users (restart Ember to write a new one)")
        with open(path) as f:
            return f.read().strip()
    sys.exit(f"no control token for port {PORT}: start the app with --control {PORT} (or set LIGHTCRAFT_CONTROL_TOKEN)")


TOKEN = read_token()


def readline(s):
    buf = b""
    while not buf.endswith(b"\n"):
        c = s.recv(1 << 20)
        if not c:
            break
        buf += c
    return buf


def call(method, params):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=180)
    s.sendall((json.dumps({"hello": {"token": TOKEN}}) + "\n").encode())
    hello = json.loads(readline(s))
    if not hello.get("ok"):
        sys.exit(f"the app refused the control token: {hello.get('error')}")
    s.sendall((json.dumps({"id": 1, "method": method, "params": params}) + "\n").encode())
    return json.loads(readline(s))


for line in open(sys.argv[1]):
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    r = json.loads(line)
    if "sleep" in r:
        time.sleep(r["sleep"])
        continue
    out = call(r["method"], r.get("params", {}))
    print(r["method"], json.dumps(out)[:160])

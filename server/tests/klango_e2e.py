#!/usr/bin/env python3
"""End-to-End-Test des Klango-Modus (docs/klango.md, Abschnitt 1).

Startet den Server (release-Binary) mit TC_KLANGO_SECRET und eigener
Datenbank in einem Arbeitsverzeichnis, meldet Nutzer mit Klango-Token an und
spielt Räume, Rechte, Sperren und Anrufe durch.

    python3 tests/klango_e2e.py [--bin PFAD] [--workdir DIR]

Die Token-Erzeugung hier (`make_token`) ist die Referenz für den
Python-Klango-Server: b64url ohne Padding, HMAC-SHA256 über den
Base64-STRING der Claims.
"""

import argparse
import asyncio
import base64
import hashlib
import hmac
import json
import os
import shutil
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

import websockets

SECRET = "test"


# --------------------------------------------------------------- Token

def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def make_token(sub, nick=None, adm=(), sadm=False, exp=None, secret=SECRET):
    now = int(time.time())
    claims = {
        "sub": sub,
        "nick": nick or sub,
        "iat": now,
        "exp": exp if exp is not None else now + 600,
        "adm": list(adm),
        "sadm": sadm,
    }
    c = b64url(json.dumps(claims, separators=(",", ":")).encode("utf-8"))
    sig = hmac.new(secret.encode("utf-8"), c.encode("ascii"), hashlib.sha256).digest()
    return c + "." + b64url(sig)


# --------------------------------------------------------------- Client

class Client:
    def __init__(self, name, url):
        self.name = name
        self.url = url
        self.ws = None
        self.queue = []
        self.user_id = None
        self.reader = None

    async def connect(self):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        self.ws = await websockets.connect(self.url, ssl=ctx, open_timeout=10)
        self.reader = asyncio.create_task(self._read())

    async def _read(self):
        try:
            async for raw in self.ws:
                self.queue.append(json.loads(raw))
        except Exception:
            pass

    async def close(self):
        if self.ws:
            await self.ws.close()
        if self.reader:
            self.reader.cancel()

    async def send(self, typ, data=None):
        await self.ws.send(json.dumps({"type": typ, "data": data or {}}))

    async def wait(self, typ, pred=None, timeout=5.0):
        """Nächste Nachricht dieses Typs (die `pred` erfüllt) — sie und alle
        davor werden verbraucht."""
        deadline = time.time() + timeout
        while True:
            for i, m in enumerate(self.queue):
                if m.get("type") == typ and (pred is None or pred(m.get("data") or {})):
                    del self.queue[: i + 1]
                    return m.get("data") or {}
            if time.time() > deadline:
                raise AssertionError(
                    "%s: keine Nachricht %r innerhalb %.1fs; Warteschlange: %s"
                    % (self.name, typ, timeout, [m.get("type") for m in self.queue])
                )
            await asyncio.sleep(0.02)

    async def absent(self, typ, wait=0.6):
        """Sicherstellen, dass innerhalb `wait` KEINE Nachricht dieses Typs kommt."""
        await asyncio.sleep(wait)
        got = [m for m in self.queue if m.get("type") == typ]
        assert not got, "%s: unerwartete Nachricht %r: %s" % (self.name, typ, got[0])

    async def login(self, token, nickname=None):
        data = {"klango_token": token}
        if nickname:
            data["nickname"] = nickname
        await self.send("auth_login", data)
        resp = await self.wait("auth_response")
        if resp.get("success"):
            self.user_id = resp["user_id"]
        return resp

    async def rooms(self, timeout=5.0):
        return (await self.wait("room_list", timeout=timeout))["rooms"]


# --------------------------------------------------------------- Server

def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def push(internal_port, names, kind="whatsnew", secret=SECRET):
    """Benachrichtigung über den internen Endpunkt einliefern.
    Rückgabe: (HTTP-Status, Rumpf als dict oder None)."""
    body = json.dumps({"to": list(names), "kind": kind}).encode("utf-8")
    req = urllib.request.Request(
        "http://127.0.0.1:%d/push" % internal_port,
        data=body,
        headers={"Content-Type": "application/json", "X-Klango-Secret": secret},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return e.code, None


class PresenceSink:
    """Steht für den Klango-Server: nimmt `POST /internal/presence` entgegen
    und merkt sich die zuletzt gemeldete Liste."""

    def __init__(self):
        import http.server
        import threading

        self.reports = []       # [(secret_header, [namen])]
        sink = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(n)
                if self.path == "/internal/presence":
                    try:
                        data = json.loads(raw.decode("utf-8"))
                    except ValueError:
                        data = {}
                    sink.reports.append(
                        (self.headers.get("X-Klango-Secret"), data.get("online") or [])
                    )
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *a):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self):
        return "http://127.0.0.1:%d" % self.port

    async def expect(self, contains=(), missing=(), timeout=12.0):
        """Auf einen Bericht warten, der alle `contains` und keinen der
        `missing` enthält."""
        deadline = time.time() + timeout
        seen = len(self.reports)
        while time.time() < deadline:
            for secret, names in self.reports[seen:]:
                assert secret == SECRET, "Anwesenheit ohne/mit falschem Geheimnis: %r" % secret
                low = [n.lower() for n in names]
                if all(c in low for c in contains) and not any(m in low for m in missing):
                    return low
            await asyncio.sleep(0.1)
        raise AssertionError(
            "kein Anwesenheitsbericht mit %s ohne %s; zuletzt: %s"
            % (list(contains), list(missing), self.reports[-3:])
        )

    def stop(self):
        self.server.shutdown()


def start_server(binary, workdir, port, klango_url=""):
    env = dict(os.environ)
    env.update({
        "TC_KLANGO_SECRET": SECRET,
        "TC_CONTROL_HOST": "127.0.0.1",
        "TC_CONTROL_PORT": str(port),
        "TC_AUDIO_HOST": "127.0.0.1",
        "TC_AUDIO_PORT": str(port + 1),
        # Interner Push-Endpunkt und die Gegenrichtung (Anwesenheit).
        "TC_INTERNAL_PORT": str(port + 2),
        "TC_KLANGO_URL": klango_url,
        "TC_DATABASE_PATH": os.path.join(workdir, "tc.db"),
        "TC_UPLOAD_DIR": os.path.join(workdir, "uploads"),
        "TC_TLS_CERT_FILE": os.path.join(workdir, "server.crt"),
        "TC_TLS_KEY_FILE": os.path.join(workdir, "server.key"),
        "TC_LOG_LEVEL": "info",
    })
    log = open(os.path.join(workdir, "server.log"), "w")
    proc = subprocess.Popen(
        [binary, "--config", os.path.join(workdir, "nicht-vorhanden.toml")],
        cwd=workdir, env=env, stdout=log, stderr=subprocess.STDOUT,
    )
    for _ in range(100):
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return proc
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError("Server beendet, siehe server.log")
            time.sleep(0.1)
    raise RuntimeError("Server startet nicht")


# --------------------------------------------------------------- Tests

PASSED = []


def ok(name):
    PASSED.append(name)
    print("  ok  " + name)


async def run(url, internal_port, sink):
    alice = Client("alice", url)   # Admin/Moderator in Gruppe 12
    bob = Client("bob", url)
    carol = Client("carol", url)
    for c in (alice, bob, carol):
        await c.connect()

    # --- 1. Anmeldung ---
    r = await alice.login(make_token("alice", "Alice", adm=["12"]))
    assert r["success"], r
    assert r["role"] == "user"
    assert isinstance(r["rooms"], list)
    r = await bob.login(make_token("bob", "Bob"))
    assert r["success"], r
    r = await carol.login(make_token("carol", "Carol"))
    assert r["success"], r
    ok("Token-Login (alice, bob, carol)")

    bad = Client("bad", url)
    await bad.connect()
    r = await bad.login(make_token("mallory", secret="falsch"))
    assert not r["success"] and "Token" in r["error"], r
    r = await bad.login(make_token("mallory", exp=int(time.time()) - 5))
    assert not r["success"], r
    # Passwort-Registrierung ist im Klango-Modus aus.
    await bad.send("auth_login", {"username": "neu", "password": "x"})
    r = await bad.wait("auth_response")
    assert not r["success"], r
    await bad.close()
    ok("Ablehnung: falsche Signatur, abgelaufen, Selbstregistrierung")

    # Serveradmin-Sitzung über sadm
    root = Client("root", url)
    await root.connect()
    r = await root.login(make_token("root", sadm=True))
    assert r["success"] and r["role"] == "admin", r
    ok("sadm → Sitzungsrolle admin")

    # --- 2. room_create durch Nutzer ---
    await bob.send("room_create", {"name": "Bobs Ecke"})
    created = await bob.wait("room_created")
    rid = created["room_id"]
    rooms = await bob.rooms()
    mine = [x for x in rooms if x["id"] == rid][0]
    assert mine["owner_id"] == bob.user_id and mine["temporary"] and not mine["private"]
    assert mine["group_id"] == "" and mine["admins"] == []
    await alice.rooms()  # Broadcast an alle
    ok("room_create durch Nutzer → room_created, Eigentümer, temporär")

    await bob.send("room_join", {"room_id": rid})
    j = await bob.wait("room_joined")
    assert j["room_id"] == rid
    rooms = await bob.rooms()
    inroom = [x for x in rooms if x["id"] == rid][0]["users"]
    assert inroom[0]["username"] == "bob" and inroom[0]["id"] == bob.user_id
    ok("room_join → room_joined, UserInfo.username")

    # --- 3. Gruppenraum: nur EINER je gid ---
    await alice.send("room_join_group", {"group_id": "12", "name": "Gruppe Zwölf"})
    ga = await alice.wait("room_joined")
    await bob.send("room_join_group", {"group_id": "12", "name": "egal"})
    gb = await bob.wait("room_joined")
    assert ga["room_id"] == gb["room_id"], (ga, gb)
    grid = ga["room_id"]
    rooms = await bob.rooms()
    g = [x for x in rooms if x["group_id"] == "12"]
    assert len(g) == 1 and g[0]["name"] == "Gruppe Zwölf" and g[0]["owner_id"] == 0
    assert len(g[0]["users"]) == 2
    # Bobs Raum ist leer geworden und damit verschwunden.
    assert not [x for x in rooms if x["id"] == rid], "temporärer Raum lebt noch"
    ok("room_join_group zweimal → ein Raum; leerer temporärer Raum verschwand")

    # --- 7. Gruppenrecht über adm ---
    await bob.send("room_kick", {"room_id": grid, "user_id": alice.user_id})
    e = await bob.wait("error")
    assert e["message"] == "Insufficient permissions", e
    await alice.send("room_kick", {"room_id": grid, "user_id": bob.user_id, "reason": "test"})
    k = await bob.wait("room_kicked")
    assert k["room_id"] == grid and k["reason"] == "test"
    left = await alice.wait("room_user_left", lambda d: d["user_id"] == bob.user_id)
    assert left["room_id"] == grid
    # Ohne Gruppenrecht darf bob im Gruppenraum niemanden zum Admin machen —
    # und alice auch nicht (Gruppenräume haben keine Raum-Admins).
    await alice.send("room_admin_set", {"room_id": grid, "user_id": bob.user_id, "admin": True})
    e = await alice.wait("error")
    assert e["message"] == "Insufficient permissions", e
    ok("Gruppenrecht aus dem Token: alice kickt, bob darf nicht; keine Raum-Admins im Gruppenraum")
    await alice.send("room_leave", {"room_id": grid})
    rooms = await alice.rooms()
    assert not [x for x in rooms if x["id"] == grid]
    ok("Gruppenraum leer → gelöscht")

    # --- 4./5./6. Eigener Raum: Admins, Kick, Sperre ---
    await bob.send("room_create", {"name": "Bobs Ecke"})
    rid = (await bob.wait("room_created"))["room_id"]
    for c in (bob, alice, carol):
        await c.send("room_join", {"room_id": rid})
        await c.wait("room_joined")
    # Nicht-Eigentümer darf keine Admins ernennen
    await alice.send("room_admin_set", {"room_id": rid, "user_id": alice.user_id, "admin": True})
    e = await alice.wait("error")
    assert e["message"] == "Insufficient permissions", e
    # alice darf (noch) nicht kicken
    await alice.send("room_kick", {"room_id": rid, "user_id": carol.user_id})
    e = await alice.wait("error")
    assert e["message"] == "Insufficient permissions", e
    # Eigentümer ernennt alice
    await bob.send("room_admin_set", {"room_id": rid, "user_id": alice.user_id, "admin": True})
    ch = await carol.wait("room_admin_changed")
    assert ch["user_id"] == alice.user_id and ch["admin"] and ch["username"] == "alice"
    rooms = await carol.rooms()
    assert [x for x in rooms if x["id"] == rid][0]["admins"] == [alice.user_id]
    ok("room_admin_set: Eigentümer ja, andere nein; admins in room_list")

    # Raum-Admin darf den Eigentümer nicht kicken
    await alice.send("room_kick", {"room_id": rid, "user_id": bob.user_id})
    e = await alice.wait("error")
    assert e["message"] == "Insufficient permissions", e
    # ... aber carol
    await alice.send("room_kick", {"room_id": rid, "user_id": carol.user_id})
    k = await carol.wait("room_kicked")
    assert k["room_id"] == rid and k["room_name"] == "Bobs Ecke"
    await bob.wait("room_user_left", lambda d: d["user_id"] == carol.user_id)
    ok("room_kick durch Raum-Admin (nicht gegen den Eigentümer)")

    await carol.send("room_join", {"room_id": rid})
    await carol.wait("room_joined")
    await alice.send("room_ban", {"room_id": rid, "user_id": carol.user_id, "reason": "ruhe"})
    b = await carol.wait("room_banned")
    assert b["room_id"] == rid and b["reason"] == "ruhe" and b["expires_at"] is None
    await carol.send("room_join", {"room_id": rid})
    e = await carol.wait("error")
    assert e["message"] == "banned", e
    await alice.send("room_bans", {"room_id": rid})
    bl = await alice.wait("room_bans_result")
    assert bl["bans"][0]["user_id"] == carol.user_id and bl["bans"][0]["username"] == "carol"
    # carol selbst darf die Liste nicht sehen
    await carol.send("room_bans", {"room_id": rid})
    e = await carol.wait("error")
    assert e["message"] == "Insufficient permissions", e
    await alice.send("room_unban", {"room_id": rid, "user_id": carol.user_id})
    bl = await alice.wait("room_bans_result")
    assert bl["bans"] == []
    await carol.send("room_join", {"room_id": rid})
    await carol.wait("room_joined")
    ok("room_ban → verweigerter Beitritt → room_unban → Beitritt")

    # Raum-Stummschaltung
    await alice.send("room_mute", {"room_id": rid, "user_id": carol.user_id, "muted": True})
    st = await bob.wait("audio_user_state", lambda d: d["user_id"] == carol.user_id)
    assert st["muted"]
    ok("room_mute → audio_user_state")

    # Admin-Recht widerrufen
    await bob.send("room_admin_set", {"room_id": rid, "user_id": alice.user_id, "admin": False})
    ch = await carol.wait("room_admin_changed")
    assert not ch["admin"]
    await alice.send("room_kick", {"room_id": rid, "user_id": carol.user_id})
    e = await alice.wait("error")
    assert e["message"] == "Insufficient permissions", e
    ok("Raum-Admin widerrufen")

    # Eigentümer darf seinen Raum löschen → room_closed
    await bob.send("room_delete", {"room_id": rid})
    rc = await carol.wait("room_closed")
    assert rc["room_id"] == rid
    rooms = await carol.rooms()
    assert not [x for x in rooms if x["id"] == rid]
    ok("room_delete durch Eigentümer → room_closed")

    # user_lookup
    await alice.send("user_lookup", {"username": "BOB"})
    lu = await alice.wait("user_lookup_result")
    assert lu["online"] and lu["user_id"] == bob.user_id and lu["username"] == "bob"
    await alice.send("user_lookup", {"username": "niemand"})
    lu = await alice.wait("user_lookup_result")
    assert not lu["online"]
    ok("user_lookup")

    # --- 8./9. Anruf annehmen ---
    for c in (alice, bob, carol):
        c.queue.clear()
    await alice.send("call_invite", {"to_username": "Bob"})
    ring = await alice.wait("call_ringing")
    prid = ring["room_id"]
    assert ring["to_user_id"] == bob.user_id and ring["to_username"] == "bob"
    # Reihenfolge laut Vertrag: erst room_list (der Angerufene lernt den Raum
    # kennen), dann call_incoming.
    rooms = await bob.rooms()
    inc = await bob.wait("call_incoming")
    assert inc["room_id"] == prid and inc["from_username"] == "alice" and inc["from_nickname"] == "Alice"
    # Sichtbarkeit: bob sieht den Raum (eingeladen), carol nicht.
    pr = [x for x in rooms if x["id"] == prid]
    assert pr and pr[0]["private"] and pr[0]["temporary"] and pr[0]["owner_id"] == alice.user_id
    assert [u["username"] for u in pr[0]["users"]] == ["alice"]
    await carol.absent("room_list")
    await carol.send("room_join", {"room_id": prid})
    e = await carol.wait("error")
    assert e["message"] == "private", e
    ok("call_invite → call_ringing/call_incoming; privater Raum nur für Beteiligte")

    # Zweiter Anruf, solange einer offen ist → busy
    await carol.send("call_invite", {"to_username": "bob"})
    f = await carol.wait("call_failed")
    assert f["reason"] == "busy", f
    await alice.send("call_invite", {"to_username": "carol"})
    f = await alice.wait("call_failed")
    assert f["reason"] == "busy", f
    ok("busy: Angerufener mit offenem Anruf, Anrufer mit offenem Anruf")

    await bob.send("call_answer", {"room_id": prid, "accept": True})
    ans = await alice.wait("call_answered")
    assert ans["accept"] and ans["user_id"] == bob.user_id and ans["username"] == "bob"
    await bob.send("room_join", {"room_id": prid})
    j = await bob.wait("room_joined")
    assert j["room_id"] == prid
    await alice.wait("room_user_joined", lambda d: d["user"]["id"] == bob.user_id)
    ok("call_answer accept → call_answered → Angerufener tritt bei")

    # Chat im Anrufraum
    await alice.send("chat_room", {"room_id": prid, "message": "hallo"})
    m = await bob.wait("chat_room")
    assert m["message"] == "hallo" and m["from_user"]["username"] == "alice"
    ok("chat_room im Anrufraum")

    # Beide legen auf → Raum verschwindet
    await alice.send("room_leave", {"room_id": prid})
    await bob.wait("room_user_left", lambda d: d["user_id"] == alice.user_id)
    await bob.send("room_leave", {"room_id": prid})
    rooms = await bob.rooms()
    assert not [x for x in rooms if x["id"] == prid]
    ok("Anrufraum nach Auflegen gelöscht")

    # --- 10. Ablehnen ---
    await alice.send("call_invite", {"to_username": "bob"})
    prid = (await alice.wait("call_ringing"))["room_id"]
    await bob.wait("call_incoming")
    await bob.send("call_answer", {"room_id": prid, "accept": False})
    ans = await alice.wait("call_answered")
    assert not ans["accept"] and ans["reason"] == "declined"
    rooms = await bob.rooms()
    assert not [x for x in rooms if x["id"] == prid], "abgelehnter Raum noch sichtbar"
    await alice.send("room_leave", {"room_id": prid})
    rooms = await alice.rooms()
    assert not [x for x in rooms if x["id"] == prid]
    ok("call_answer decline → call_answered declined; Raum weg")

    # --- 11. Abbrechen ---
    await alice.send("call_invite", {"to_username": "bob"})
    prid = (await alice.wait("call_ringing"))["room_id"]
    await bob.wait("call_incoming")
    await alice.send("call_cancel", {"room_id": prid})
    cc = await bob.wait("call_cancelled")
    assert cc["room_id"] == prid and cc["reason"] == "cancelled"
    rooms = await alice.rooms()
    assert not [x for x in rooms if x["id"] == prid]
    ok("call_cancel → call_cancelled; Raum weg")

    # --- 12. Offline ---
    await alice.send("call_invite", {"to_username": "nobody"})
    f = await alice.wait("call_failed")
    assert f["reason"] == "offline" and f["to_username"] == "nobody"
    ok("call_invite an Offline → call_failed offline")

    # Anrufer trennt die Verbindung, während es klingelt → Angerufener erfährt es
    dave = Client("dave", url)
    await dave.connect()
    assert (await dave.login(make_token("dave")))["success"]
    await dave.send("call_invite", {"to_username": "carol"})
    prid = (await dave.wait("call_ringing"))["room_id"]
    await carol.wait("call_incoming")
    await dave.close()
    cc = await carol.wait("call_cancelled")
    assert cc["room_id"] == prid
    rooms = await carol.rooms()
    assert not [x for x in rooms if x["id"] == prid]
    ok("Anrufer trennt → call_cancelled, Raum weg")

    # Angerufener trennt → Anrufer bekommt accept=false/offline
    dave = Client("dave", url)
    await dave.connect()
    assert (await dave.login(make_token("dave")))["success"]
    await carol.send("call_invite", {"to_username": "dave"})
    prid = (await carol.wait("call_ringing"))["room_id"]
    await dave.wait("call_incoming")
    await dave.close()
    ans = await carol.wait("call_answered")
    assert not ans["accept"] and ans["reason"] == "offline"
    await carol.send("room_leave", {"room_id": prid})
    rooms = await carol.rooms()
    assert not [x for x in rooms if x["id"] == prid]
    ok("Angerufener trennt → call_answered offline")

    # --- 13. Aufräumen bei Verbindungsabbruch ---
    await bob.send("room_create", {"name": "Flüchtig"})
    rid = (await bob.wait("room_created"))["room_id"]
    await bob.send("room_join", {"room_id": rid})
    await bob.wait("room_joined")
    await alice.rooms()  # Broadcast
    await bob.close()
    rooms = await alice.rooms()
    assert not [x for x in rooms if x["id"] == rid]
    ok("Verbindungsabbruch → leerer temporärer Raum gelöscht")

    # Grenze: höchstens 3 eigene temporäre Räume
    for i in range(3):
        await carol.send("room_create", {"name": "R%d" % i})
        await carol.wait("room_created")
    await carol.send("room_create", {"name": "R4"})
    e = await carol.wait("error")
    assert "zu viele" in e["message"].lower(), e
    ok("Grenze: 3 temporäre Räume je Eigentümer")

    # Serveradmin darf dauerhaft anlegen und Standardraum bleibt
    await root.send("room_create", {"name": "Dauerhaft", "persistent": True})
    prm = (await root.wait("room_created"))["room_id"]
    rooms = await root.rooms()
    d = [x for x in rooms if x["id"] == prm][0]
    assert not d["temporary"]
    await root.send("room_delete", {"room_id": prm})
    await root.rooms()
    ok("Serveradmin: persistent anlegen und löschen")

    # --- Push und mehrere Verbindungen desselben Kontos (docs/klango.md 1.6) ---

    dave1 = Client("dave1", url)
    dave2 = Client("dave2", url)
    await dave1.connect()
    r = await dave1.login(make_token("dave", "Dave"))
    assert r["success"], r

    # Zweite Anmeldung desselben Kontos: sie übernimmt die Konferenz-Sitzung,
    # die erste erfährt das (statt still zu verstummen).
    await dave2.connect()
    r = await dave2.login(make_token("dave", "Dave"))
    assert r["success"], r
    await dave1.wait("session_replaced")
    ok("zweite Anmeldung verdrängt die Sitzung → session_replaced an die erste")

    # Ein Push erreicht BEIDE Verbindungen.
    status, body = push(internal_port, ["dave"])
    assert status == 200, status
    assert body["delivered"] == 2, body
    d1 = await dave1.wait("klango_push")
    d2 = await dave2.wait("klango_push")
    assert d1["kind"] == "whatsnew" and d2["kind"] == "whatsnew", (d1, d2)
    ok("Push erreicht beide Verbindungen desselben Kontos")

    # Falsches Geheimnis: abgewiesen, und es geht nichts raus.
    status, body = push(internal_port, ["dave"], secret="falsch")
    assert status == 403, status
    assert body is None
    await dave1.absent("klango_push")
    await dave2.absent("klango_push")
    ok("falsches Geheimnis → 403, kein Push")

    # Unbekannter Empfänger: gültig, aber niemand da.
    status, body = push(internal_port, ["gibtsnicht"])
    assert status == 200 and body["delivered"] == 0, body
    ok("Push an unbekannten Empfänger → delivered 0")

    # Nach dem Trennen bekommt die verschwundene Verbindung nichts mehr; die
    # verbliebene sehr wohl.
    await dave2.close()
    await asyncio.sleep(0.3)
    status, body = push(internal_port, ["dave"])
    assert status == 200, status
    assert body["delivered"] == 1, body
    d1 = await dave1.wait("klango_push")
    assert d1["kind"] == "whatsnew", d1
    ok("getrennte Verbindung bekommt keinen Push mehr")

    # Anwesenheit: solange dave verbunden ist, steht er in der Liste.
    await sink.expect(contains=["dave", "alice"])
    ok("Anwesenheitsmeldung nennt die verbundenen Konten")

    await dave1.close()
    await asyncio.sleep(0.3)
    status, body = push(internal_port, ["dave"])
    assert status == 200 and body["delivered"] == 0, body
    ok("letzte Verbindung weg → keine Zuhörer mehr")

    # …und verschwindet daraus, sobald die letzte Verbindung weg ist.
    await sink.expect(contains=["alice"], missing=["dave"])
    ok("Anwesenheitsmeldung nach dem Trennen ohne das Konto")

    for c in (alice, carol, root):
        await c.close()


def main():
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--bin", default=os.path.join(here, "..", "target", "release", "teamconference-server"))
    ap.add_argument("--workdir", default=None)
    args = ap.parse_args()

    workdir = args.workdir or tempfile.mkdtemp(prefix="tc-klango-e2e-")
    os.makedirs(workdir, exist_ok=True)
    for f in ("tc.db", "tc.db-wal", "tc.db-shm"):
        try:
            os.remove(os.path.join(workdir, f))
        except FileNotFoundError:
            pass
    port = free_port()
    sink = PresenceSink()
    proc = start_server(os.path.abspath(args.bin), workdir, port, sink.url)
    url = "wss://127.0.0.1:%d" % port
    try:
        asyncio.run(run(url, port + 2, sink))
    except BaseException:
        proc.terminate()
        proc.wait(5)
        print("---- server.log (Ende) ----")
        with open(os.path.join(workdir, "server.log")) as f:
            print(f.read()[-4000:])
        raise
    proc.terminate()
    proc.wait(5)
    sink.stop()
    if not args.workdir:
        shutil.rmtree(workdir, ignore_errors=True)
    print("ALLE %d PRÜFUNGEN BESTANDEN" % len(PASSED))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Fake Microsoft identity platform (device code + refresh) for Orion's tests.

Usage: fake_oauth.py   (prints "PORT <n>"). Endpoints under /common/oauth2/v2.0/.
Behaves like Microsoft where it matters:
  * the device code is "authorization_pending" for the first two polls
  * the id_token carries the address in "preferred_username"
  * refresh tokens rotate: each refresh issues A<n>/R<n> and kills the old R
GET /admin/state → {"refresh_token": current valid one, "refreshes": n}
"""
import base64, json, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

LOCK = threading.Lock()
STATE = {"polls": 0, "n": 1, "refresh": None, "refreshes": 0}


def b64(d):
    return base64.urlsafe_b64encode(json.dumps(d).encode()).decode().rstrip("=")


def tokens():
    n = STATE["n"]
    STATE["refresh"] = f"R{n}"
    return {"token_type": "Bearer", "access_token": f"A{n}", "refresh_token": f"R{n}", "expires_in": 3600,
            "scope": "IMAP.AccessAsUser.All", "id_token": f"{b64({'alg': 'none'})}.{b64({'preferred_username': 'ana@outlook.com'})}.sig"}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/admin/state":
            with LOCK:
                return self.reply(200, {"refresh_token": STATE["refresh"], "refreshes": STATE["refreshes"]})
        self.reply(404, {})

    def do_POST(self):
        form = {k: v[0] for k, v in parse_qs(self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()).items()}
        with LOCK:
            if self.path.endswith("/devicecode"):
                if "offline_access" not in form.get("scope", ""):
                    return self.reply(400, {"error": "invalid_scope"})
                port = self.server.server_address[1]
                return self.reply(200, {"device_code": "DC1", "user_code": "ABCD-1234", "verification_uri": f"http://127.0.0.1:{port}/device",
                                        "expires_in": 900, "interval": 1, "message": "Open the page and enter ABCD-1234"})
            if self.path.endswith("/token"):
                grant = form.get("grant_type")
                if grant == "urn:ietf:params:oauth:grant-type:device_code":
                    if form.get("device_code") != "DC1":
                        return self.reply(400, {"error": "invalid_grant"})
                    STATE["polls"] += 1
                    if STATE["polls"] <= 2:
                        return self.reply(400, {"error": "authorization_pending"})
                    return self.reply(200, tokens())
                if grant == "refresh_token":
                    if form.get("refresh_token") != STATE["refresh"]:
                        return self.reply(400, {"error": "invalid_grant", "error_description": "AADSTS70000: refresh token revoked"})
                    STATE["n"] += 1
                    STATE["refreshes"] += 1
                    t = tokens()
                    t.pop("id_token")
                    return self.reply(200, t)
                return self.reply(400, {"error": "unsupported_grant_type"})
        self.reply(404, {})


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    print(f"PORT {srv.server_address[1]}", flush=True)
    srv.serve_forever()

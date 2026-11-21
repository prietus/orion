#!/usr/bin/env python3
"""Minimal HKP keyserver for Orion's tests: serves KEYDIR/<FINGERPRINT>.asc.
Usage: fake_hkp.py KEYDIR   (prints "PORT <n>")"""
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

KEYDIR = sys.argv[1]


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        u = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if u.path == "/pks/lookup" and q.get("op") == "get":
            fpr = q.get("search", "").upper().removeprefix("0X")
            path = os.path.join(KEYDIR, f"{fpr}.asc")
            if os.path.exists(path):
                body = open(path, "rb").read()
                self.send_response(200)
                self.send_header("Content-Type", "application/pgp-keys")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                return self.wfile.write(body)
        self.send_response(404)
        self.end_headers()


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    print(f"PORT {srv.server_address[1]}", flush=True)
    srv.serve_forever()

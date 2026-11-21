#!/usr/bin/env python3
"""Minimal SMTP submission server for Orion's tests. Not for production!

Usage: fake_smtp.py OUTDIR [--cert C --key K] [--mechs "PLAIN LOGIN XOAUTH2"] [--size N] [--password P]
Prints "PORT <plain/STARTTLS port> TLSPORT <implicit TLS port or 0>".
Users: ana / s3cret; XOAUTH2 token: tok-123. Recipients containing "reject"
get 550. Each accepted message is stored as OUTDIR/<n>.json
({"from","rcpts","data"}).
"""
import base64, json, os, socket, socketserver, ssl, sys, threading

args = sys.argv[1:]
OUT = args[0]
opt = lambda k, d=None: args[args.index(k) + 1] if k in args else d
CERT, KEY = opt("--cert"), opt("--key")
MECHS = opt("--mechs", "PLAIN LOGIN XOAUTH2")
SIZE = int(opt("--size", "10240000"))
PASSWORD = opt("--password", "s3cret")
CTX = None
if CERT:
    CTX = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    CTX.load_cert_chain(CERT, KEY)
COUNTER = [0]
LOCK = threading.Lock()


class Session:
    def __init__(self, sock, tls):
        self.sock, self.tls = sock, tls
        self.f = sock.makefile("rb")
        self.authed = False
        self.reset()

    def reset(self):
        self.mail_from, self.rcpts = None, []

    def out(self, s):
        self.sock.sendall((s + "\r\n").encode())

    def line(self):
        l = self.f.readline()
        if not l:
            raise EOFError
        return l.decode().rstrip("\r\n")

    def ehlo(self):
        caps = [f"SIZE {SIZE}", "8BITMIME"]
        if CTX and not self.tls:
            caps.append("STARTTLS")
        caps.append("AUTH " + MECHS)
        lines = ["fake.smtp.test greets you"] + caps
        for i, c in enumerate(lines):
            self.out(f"250{'-' if i < len(lines) - 1 else ' '}{c}")

    def run(self):
        self.out("220 fake.smtp.test ESMTP ready")
        while True:
            l = self.line()
            cmd = l.split(" ", 1)[0].upper()
            arg = l[len(cmd):].strip()
            if cmd in ("EHLO", "HELO"):
                self.ehlo()
            elif cmd == "STARTTLS" and CTX and not self.tls:
                self.out("220 go ahead")
                self.sock = CTX.wrap_socket(self.sock, server_side=True)
                self.f = self.sock.makefile("rb")
                self.tls, self.authed = True, False
                self.reset()
            elif cmd == "AUTH":
                self.auth(arg)
            elif cmd == "MAIL":
                if not self.authed:
                    self.out("530 5.7.0 Authentication required")
                    continue
                self.mail_from = arg.split(":", 1)[1].split(">")[0].strip("<")
                self.out("250 2.1.0 ok")
            elif cmd == "RCPT":
                rcpt = arg.split(":", 1)[1].split(">")[0].strip("<")
                if "reject" in rcpt:
                    self.out("550 5.1.1 no such user")
                else:
                    self.rcpts.append(rcpt)
                    self.out("250 2.1.5 ok")
            elif cmd == "DATA":
                self.out("354 end with <CRLF>.<CRLF>")
                data = []
                while True:
                    l = self.f.readline()
                    if l in (b".\r\n", b".\n", b""):
                        break
                    data.append(l[1:] if l.startswith(b"..") else l)
                with LOCK:
                    COUNTER[0] += 1
                    n = COUNTER[0]
                with open(os.path.join(OUT, f"{n}.json"), "w") as f:
                    json.dump({"from": self.mail_from, "rcpts": self.rcpts, "data": b"".join(data).decode("utf-8", "replace"), "tls": self.tls}, f)
                self.out(f"250 2.0.0 queued as {n}")
                self.reset()
            elif cmd == "RSET":
                self.reset()
                self.out("250 ok")
            elif cmd == "QUIT":
                self.out("221 bye")
                return
            else:
                self.out("502 5.5.2 unknown command")

    def auth(self, arg):
        parts = arg.split(" ", 1)
        mech = parts[0].upper()
        if mech not in MECHS.split():
            return self.out("504 5.5.4 mechanism not available")
        ok = False
        if mech == "PLAIN":
            raw = parts[1] if len(parts) > 1 else (self.out("334 ") or self.line())
            _, user, pw = base64.b64decode(raw).decode().split("\0")
            ok = (user, pw) == ("ana", PASSWORD)
        elif mech == "LOGIN":
            self.out("334 VXNlcm5hbWU6")
            user = base64.b64decode(self.line()).decode()
            self.out("334 UGFzc3dvcmQ6")
            pw = base64.b64decode(self.line()).decode()
            ok = (user, pw) == ("ana", PASSWORD)
        elif mech == "XOAUTH2":
            fields = base64.b64decode(parts[1]).decode().split("\x01")
            ok = "auth=Bearer tok-123" in fields
            if not ok:
                self.out("334 " + base64.b64encode(b'{"status":"401"}').decode())
                self.line()
        if ok:
            self.authed = True
            self.out("235 2.7.0 authenticated")
        else:
            self.out("535 5.7.8 authentication failed")


class Handler(socketserver.BaseRequestHandler):
    tls = False

    def handle(self):
        sock = CTX.wrap_socket(self.request, server_side=True) if self.tls else self.request
        try:
            Session(sock, self.tls).run()
        except (EOFError, ConnectionError, ssl.SSLError, OSError):
            pass


class TlsHandler(Handler):
    tls = True


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    plain = Server(("127.0.0.1", 0), Handler)
    tls_port = 0
    if CTX:
        tls = Server(("127.0.0.1", 0), TlsHandler)
        tls_port = tls.server_address[1]
        threading.Thread(target=tls.serve_forever, daemon=True).start()
    print(f"PORT {plain.server_address[1]} TLSPORT {tls_port}", flush=True)
    plain.serve_forever()

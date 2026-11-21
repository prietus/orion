#!/usr/bin/env python3
"""Minimal IMAP4rev1 server for Orion's tests (do not use in production!).

Usage: fake_imap.py   (prints "PORT <imap> ADMIN <admin>", free ports)
User: yo / password: secreta

Admin channel (second port, "ADMIN <n>"), one command per line:
  ADD <mailbox> <subject>       adds a message
  DEL <mailbox> <uid>           deletes (expunges) a message
  FLAG <mailbox> <uid> <flag>   adds a flag (\\Seen...)
  VALIDITY <mailbox> <n>        changes UIDVALIDITY (renumbers)
  THROTTLE <n>               the next n FETCH answer "NO ... throttled" (like Outlook)
  FAILFETCH <n>              the next n FETCH answer a plain NO
  TOKENS <tok>...            sets the OAuth2 access tokens accepted by AUTHENTICATE XOAUTH2
  FLAGS <mailbox> <uid>         shows the flags of a message
  KICK                          closes every IMAP session
  COUNT <command>               how many times a command was received (FETCH, UID...)
"""
import base64, re, socket, socketserver, sys, threading, time

LOCK = threading.Lock()
SESSIONS = set()
COUNTS = {}
VALID_TOKENS = set()
FAULTS = {"throttle": 0, "fail": 0}

def mk(uid, subject, frm="Ana <ana@example.com>", body="Hola", flags=(), date=None, ctype="text/plain; charset=utf-8"):
    date = date or time.strftime("%a, %d %b %Y %H:%M:%S +0000", time.gmtime(1_700_000_000 + uid * 3600))
    raw = (f"From: {frm}\r\nTo: yo@example.com\r\nSubject: {subject}\r\nDate: {date}\r\n"
           f"Message-ID: <{uid}@fake>\r\nX-Relleno: {'x' * 200}\r\nContent-Type: {ctype}\r\n\r\n{body}\r\n").encode()
    return {"uid": uid, "flags": set(flags), "raw": raw}

BOXES = {
    "INBOX": {"validity": 42, "next": 4, "attrs": "\\HasNoChildren", "msgs": [
        mk(1, "Primero", flags={"\\Seen"}),
        mk(2, "Segundo con ñ", frm="=?utf-8?q?Jos=C3=A9?= <jose@example.com>", body="Cuerpo del segundo"),
        mk(3, "Tercero", flags={"\\Flagged"}),
    ]},
    "Env&AO0-ados": {"validity": 7, "next": 2, "attrs": "\\HasNoChildren \\Sent", "msgs": [mk(1, "Enviado")]},
    "Archivo": {"validity": 9, "next": 1, "attrs": "\\HasChildren", "msgs": []},
    "Archivo/2024": {"validity": 10, "next": 1, "attrs": "\\HasNoChildren", "msgs": []},
    "[Gmail]": {"validity": 0, "next": 1, "attrs": "\\Noselect \\HasChildren", "msgs": []},
}

def parse_set(spec, maxv):
    out = set()
    for part in spec.split(","):
        if ":" in part:
            a, b = part.split(":")
            a = maxv if a == "*" else int(a)
            b = maxv if b == "*" else int(b)
            out.update(range(min(a, b), max(a, b) + 1))
        else:
            out.add(maxv if part == "*" else int(part))
    return out

def tokens(s):
    return [m.group(1) if m.group(1) is not None else m.group(2) for m in re.finditer(r'"((?:[^"\\]|\\.)*)"|(\S+)', s)]

def header_fields(raw, names):
    head = raw.split(b"\r\n\r\n", 1)[0].decode()
    keep = [l for l in head.split("\r\n") if l.split(":", 1)[0].upper() in names]
    return ("\r\n".join(keep) + "\r\n\r\n").encode()

class Handler(socketserver.StreamRequestHandler):
    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        self.wfile.write(data)
        self.wfile.flush()

    def handle(self):
        self.send("* OK [CAPABILITY IMAP4rev1 UIDPLUS] fake server ready\r\n")
        SESSIONS.add(self)
        self.selected = None
        self.authed = False
        try:
            line = self.rfile.readline()
            while line:
                self.command(line.decode().rstrip("\r\n"))
                if getattr(self, "closing", False):
                    break
                line = self.rfile.readline()
        except (ConnectionError, OSError):
            pass
        finally:
            SESSIONS.discard(self)

    def command(self, line):
        tag, _, rest = line.partition(" ")
        cmd, _, args = rest.partition(" ")
        cmd = cmd.upper()
        uid = False
        if cmd == "UID":
            uid = True
            cmd, _, args = args.partition(" ")
            cmd = cmd.upper()
        COUNTS[cmd] = COUNTS.get(cmd, 0) + 1
        if uid:
            COUNTS["UID " + cmd] = COUNTS.get("UID " + cmd, 0) + 1
        with LOCK:
            if cmd == "CAPABILITY":
                self.send("* CAPABILITY IMAP4rev1 UIDPLUS\r\n" + f"{tag} OK done\r\n")
            elif cmd == "NOOP":
                self.send(f"{tag} OK\r\n")
            elif cmd == "LOGOUT":
                self.send(f"* BYE goodbye\r\n{tag} OK\r\n"); self.closing = True
            elif cmd == "AUTHENTICATE" and args.upper().startswith("XOAUTH2"):
                self.send("+ \r\n")
                line = self.rfile.readline().decode().strip()
                try:
                    parts = base64.b64decode(line).decode().split("\x01")
                    token = next(p[len("auth=Bearer "):] for p in parts if p.startswith("auth=Bearer "))
                except Exception:
                    token = None
                if token in VALID_TOKENS:
                    self.authed = True
                    self.send(f"{tag} OK AUTHENTICATE completed\r\n")
                else:
                    err = base64.b64encode(b'{"status":"401","schemes":"bearer","scope":"https://mail.example/"}').decode()
                    self.send(f"+ {err}\r\n")
                    self.rfile.readline()  # client's empty response ends the exchange
                    self.send(f"{tag} NO AUTHENTICATE failed\r\n")
            elif cmd == "LOGIN":
                user, pw = tokens(args)[:2]
                if (user, pw) == ("yo", "secreta"):
                    self.authed = True
                    self.send(f"{tag} OK [CAPABILITY IMAP4rev1 UIDPLUS] logged in\r\n")
                else:
                    self.send(f"{tag} NO [AUTHENTICATIONFAILED] invalid credentials\r\n")
            elif not self.authed:
                self.send(f"{tag} BAD LOGIN first\r\n")
            elif cmd == "LIST":
                out = "".join(f'* LIST ({b["attrs"]}) "/" "{n}"\r\n' for n, b in BOXES.items())
                self.send(out + f"{tag} OK LIST\r\n")
            elif cmd == "STATUS":
                name = tokens(args)[0]
                b = BOXES.get(name)
                if not b or "Noselect" in b["attrs"]:
                    return self.send(f"{tag} NO no such mailbox\r\n")
                unseen = sum(1 for m in b["msgs"] if "\\Seen" not in m["flags"])
                self.send(f'* STATUS "{name}" (MESSAGES {len(b["msgs"])} UNSEEN {unseen})\r\n{tag} OK\r\n')
            elif cmd in ("SELECT", "EXAMINE"):
                name = tokens(args)[0]
                b = BOXES.get(name)
                if not b or "Noselect" in b["attrs"]:
                    return self.send(f"{tag} NO no such mailbox\r\n")
                self.selected = name
                self.send(f"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n* {len(b['msgs'])} EXISTS\r\n* 0 RECENT\r\n"
                          f"* OK [UIDVALIDITY {b['validity']}] UIDs valid\r\n* OK [UIDNEXT {b['next']}] predicted next UID\r\n"
                          f"{tag} OK [READ-WRITE] {cmd} completed\r\n")
            elif cmd == "FETCH":
                self.fetch(tag, args, uid)
            elif cmd == "STORE":
                self.store(tag, args, uid)
            else:
                self.send(f"{tag} BAD unknown command {cmd}\r\n")

    def fetch(self, tag, args, uid):
        if FAULTS["throttle"] > 0:
            FAULTS["throttle"] -= 1
            return self.send(f"{tag} NO Request is throttled. Suggested Backoff Time: 600 milliseconds\r\n")
        if FAULTS["fail"] > 0:
            FAULTS["fail"] -= 1
            return self.send(f"{tag} NO [SERVERBUG] Internal server error\r\n")
        if not self.selected:
            return self.send(f"{tag} BAD no mailbox selected\r\n")
        spec, _, items = args.partition(" ")
        msgs = BOXES[self.selected]["msgs"]
        if not msgs:
            return self.send(f"{tag} OK FETCH\r\n")
        maxv = msgs[-1]["uid"] if uid else len(msgs)
        wanted = parse_set(spec, maxv)
        out = []
        for seq, m in enumerate(msgs, 1):
            if (m["uid"] if uid else seq) not in wanted:
                continue
            parts = [f"UID {m['uid']}"]
            if "FLAGS" in items:
                parts.append(f"FLAGS ({' '.join(sorted(m['flags']))})")
            if "RFC822.SIZE" in items:
                parts.append(f"RFC822.SIZE {len(m['raw'])}")
            hf = re.search(r"BODY\.PEEK\[HEADER\.FIELDS \(([^)]*)\)\]", items)
            data = b""
            if hf:
                names = hf.group(1).upper().split()
                data = header_fields(m["raw"], names)
                label = f"BODY[HEADER.FIELDS ({hf.group(1)})]"
            elif "BODY.PEEK[]" in items or "BODY[]" in items:
                data = m["raw"]
                label = "BODY[]"
            if data:
                out.append(f"* {seq} FETCH ({' '.join(parts)} {label} {{{len(data)}}}\r\n".encode() + data + b")\r\n")
            else:
                out.append(f"* {seq} FETCH ({' '.join(parts)})\r\n".encode())
        self.send(b"".join(out) + f"{tag} OK FETCH\r\n".encode())

    def store(self, tag, args, uid):
        spec, op, rest = args.split(" ", 2)
        flags = set(rest.strip("()").split())
        msgs = BOXES[self.selected]["msgs"]
        maxv = msgs[-1]["uid"] if (uid and msgs) else len(msgs)
        wanted = parse_set(spec, maxv)
        out = ""
        for seq, m in enumerate(msgs, 1):
            if (m["uid"] if uid else seq) in wanted:
                if op.startswith("+"):
                    m["flags"] |= flags
                elif op.startswith("-"):
                    m["flags"] -= flags
                if "SILENT" not in op.upper():
                    out += f"* {seq} FETCH (UID {m['uid']} FLAGS ({' '.join(sorted(m['flags']))}))\r\n"
        self.send(out + f"{tag} OK STORE\r\n")

class AdminHandler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            parts = line.decode().split()
            if not parts:
                continue
            with LOCK:
                op = parts[0].upper()
                reply = "OK"
                if op == "ADD":
                    b = BOXES[parts[1]]
                    b["msgs"].append(mk(b["next"], " ".join(parts[2:])))
                    b["next"] += 1
                elif op == "DEL":
                    b = BOXES[parts[1]]
                    b["msgs"] = [m for m in b["msgs"] if m["uid"] != int(parts[2])]
                elif op == "FLAG":
                    for m in BOXES[parts[1]]["msgs"]:
                        if m["uid"] == int(parts[2]):
                            m["flags"].add(parts[3])
                elif op == "VALIDITY":
                    b = BOXES[parts[1]]
                    b["validity"] = int(parts[2])
                    for i, m in enumerate(b["msgs"], 1):
                        m["uid"] = 100 + i
                    b["next"] = 101 + len(b["msgs"])
                elif op == "KICK":
                    for s in list(SESSIONS):
                        try:
                            s.request.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                    SESSIONS.clear()
                elif op == "FLAGS":
                    m = [m for m in BOXES[parts[1]]["msgs"] if m["uid"] == int(parts[2])]
                    reply = " ".join(sorted(m[0]["flags"])) if m else "NONE"
                elif op == "THROTTLE":
                    FAULTS["throttle"] = int(parts[1])
                elif op == "FAILFETCH":
                    FAULTS["fail"] = int(parts[1])
                elif op == "TOKENS":
                    VALID_TOKENS.clear()
                    VALID_TOKENS.update(t for t in parts[1:])
                elif op == "COUNT":
                    reply = str(COUNTS.get(" ".join(parts[1:]).upper(), 0))
                else:
                    reply = "ERR"
            self.wfile.write((reply + "\n").encode()); self.wfile.flush()

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

if __name__ == "__main__":
    srv = Server(("127.0.0.1", 0), Handler)
    adm = Server(("127.0.0.1", 0), AdminHandler)
    threading.Thread(target=adm.serve_forever, daemon=True).start()
    print(f"PORT {srv.server_address[1]} ADMIN {adm.server_address[1]}", flush=True)
    srv.serve_forever()

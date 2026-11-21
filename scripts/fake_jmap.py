#!/usr/bin/env python3
"""Minimal JMAP server (RFC 8620/8621 subset) for Orion's tests. Not for production!

Usage: fake_jmap.py   (prints "PORT <n>")
Auth: Basic ana:s3cret, or Bearer tok-123.

Deliberately exercises client edge cases:
  * /.well-known/jmap redirects (302) to /jmap/session
  * apiUrl and downloadUrl are relative
  * maxObjectsInGet = 3 and Email/query returns at most 2 ids per page
Admin (no auth): POST /admin with JSON {"op": "add"|"delete"|"keywords", ...}.
"""
import base64, json, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
ACCOUNT = "acc1"
MAILBOXES = [
    {"id": "mb-inbox", "name": "Inbox", "parentId": None, "role": "inbox"},
    {"id": "mb-drafts", "name": "Drafts", "parentId": None, "role": "drafts"},
    {"id": "mb-sent", "name": "Sent", "parentId": None, "role": "sent"},
    {"id": "mb-lists", "name": "Lists", "parentId": None, "role": None},
    {"id": "mb-rust", "name": "rust", "parentId": "mb-lists", "role": None},
]
EMAILS = {}
COUNTER = [0]
STATS = {"Email/get ids": 0}
BLOBS = {}
SUBMISSIONS = []


def add(mailbox, subject, sender=("Ana", "ana@example.org"), keywords=None):
    COUNTER[0] += 1
    n = COUNTER[0]
    eid = f"M{n:04d}-x/y"  # ids may contain characters that need escaping in URLs
    raw = (f"From: {sender[0]} <{sender[1]}>\r\nTo: me@example.org\r\nSubject: {subject}\r\n"
           f"Date: Thu, 20 Nov 2025 10:{n:02d}:00 +0100\r\nMessage-ID: <{n}@fake>\r\n\r\nBody of {subject}\r\n").encode()
    EMAILS[eid] = {"id": eid, "blobId": f"B{n}", "mailboxIds": {mailbox: True}, "keywords": dict(keywords or {}),
                   "from": [{"name": sender[0], "email": sender[1]}], "subject": subject,
                   "receivedAt": f"2025-11-20T09:{n:02d}:00Z", "sentAt": f"2025-11-20T10:{n:02d}:00+01:00",
                   "size": len(raw), "hasAttachment": False, "raw": raw}
    return eid


for i in range(5):
    add("mb-inbox", f"Inbox message {i + 1}", keywords={"$seen": True} if i == 0 else {})
add("mb-rust", "Rust 2.0 released", sender=("Rust", "news@rust-lang.org"))


def authorized(h):
    a = h.headers.get("Authorization", "")
    if a == "Bearer tok-123":
        return True
    if a.startswith("Basic "):
        try:
            return base64.b64decode(a[6:]).decode() == "ana:s3cret"
        except Exception:
            return False
    return False


def mailbox_get(args):
    out = []
    for m in MAILBOXES:
        ids = [e for e in EMAILS.values() if m["id"] in e["mailboxIds"]]
        out.append({**m, "totalEmails": len(ids), "unreadEmails": sum(1 for e in ids if not e["keywords"].get("$seen")), "sortOrder": 0})
    return {"accountId": ACCOUNT, "state": "s1", "list": out, "notFound": []}


def email_query(args):
    mb = args.get("filter", {}).get("inMailbox")
    ids = sorted((e for e in EMAILS.values() if mb in e["mailboxIds"]), key=lambda e: e["receivedAt"], reverse=True)
    pos = args.get("position", 0)
    limit = min(args.get("limit", 2), 2)
    res = {"accountId": ACCOUNT, "queryState": "q1", "position": pos, "ids": [e["id"] for e in ids[pos:pos + limit]], "canCalculateChanges": False}
    if args.get("calculateTotal"):
        res["total"] = len(ids)
    return res


def email_get(args):
    ids = args.get("ids") or []
    if len(ids) > 3:
        return ("error", {"type": "requestTooLarge"})
    STATS["Email/get ids"] += len(ids)
    props = args.get("properties")
    lst, nf = [], []
    for i in ids:
        e = EMAILS.get(i)
        if not e:
            nf.append(i)
            continue
        lst.append({k: v for k, v in e.items() if k != "raw" and (props is None or k in props or k == "id")})
    return {"accountId": ACCOUNT, "state": "e1", "list": lst, "notFound": nf}


def email_set(args):
    updated, not_updated = {}, {}
    for eid, patch in (args.get("update") or {}).items():
        e = EMAILS.get(eid)
        if not e:
            not_updated[eid] = {"type": "notFound"}
            continue
        for k, v in patch.items():
            if k.startswith("keywords/"):
                kw = k.split("/", 1)[1]
                if v is None:
                    e["keywords"].pop(kw, None)
                else:
                    e["keywords"][kw] = True
        updated[eid] = None
    return {"accountId": ACCOUNT, "oldState": "e1", "newState": "e2", "updated": updated, "notUpdated": not_updated}


def identity_get(args):
    return {"accountId": ACCOUNT, "state": "i1", "list": [{"id": "id-ana", "email": "ana@example.org", "name": "Ana"}], "notFound": []}


def email_import(args):
    created, not_created = {}, {}
    for k, e in args.get("emails", {}).items():
        raw = BLOBS.get(e["blobId"])
        if raw is None:
            not_created[k] = {"type": "blobNotFound"}
            continue
        COUNTER[0] += 1
        eid = f"M{COUNTER[0]:04d}-x/y"
        subject = next((l[9:] for l in raw.decode(errors="replace").split("\r\n") if l.lower().startswith("subject: ")), "")
        EMAILS[eid] = {"id": eid, "blobId": e["blobId"], "mailboxIds": dict(e["mailboxIds"]), "keywords": dict(e.get("keywords", {})),
                       "from": [], "subject": subject, "receivedAt": "2025-11-20T12:00:00Z", "sentAt": None, "size": len(raw),
                       "hasAttachment": False, "raw": raw}
        created[k] = {"id": eid, "blobId": e["blobId"]}
    return {"accountId": ACCOUNT, "created": created, "notCreated": not_created}


def submission_set(args, created_ids):
    created, not_created = {}, {}
    for k, sub in args.get("create", {}).items():
        eid = sub["emailId"]
        if eid.startswith("#"):
            eid = created_ids.get(eid[1:])
        rcpts = [r["email"] for r in sub.get("envelope", {}).get("rcptTo", [])]
        if any("reject" in r for r in rcpts):
            not_created[k] = {"type": "forbiddenToSend", "description": "recipient rejected"}
            continue
        SUBMISSIONS.append({"emailId": eid, "identityId": sub["identityId"], "envelope": sub.get("envelope")})
        created[k] = {"id": f"S{len(SUBMISSIONS)}"}
        for ref, patch in (args.get("onSuccessUpdateEmail") or {}).items():
            if ref == f"#{k}":
                e = EMAILS[eid]
                for path, v in patch.items():
                    field, _, key = path.partition("/")
                    target = e["mailboxIds"] if field == "mailboxIds" else e["keywords"]
                    if v is None:
                        target.pop(key, None)
                    else:
                        target[key] = v
    return {"accountId": ACCOUNT, "created": created, "notCreated": not_created}


METHODS = {"Mailbox/get": mailbox_get, "Email/query": email_query, "Email/get": email_get, "Email/set": email_set,
           "Identity/get": identity_get, "Email/import": email_import}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, body=b"", ctype="application/json", headers=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/.well-known/jmap":
            return self.reply(302, headers={"Location": "/jmap/session"})
        if not authorized(self):
            return self.reply(401, b'{"type":"unauthorized"}')
        if self.path == "/jmap/session":
            s = {"capabilities": {"urn:ietf:params:jmap:core": {"maxObjectsInGet": 3, "maxSizeUpload": 1000000},
                                  "urn:ietf:params:jmap:mail": {}},
                 "accounts": {ACCOUNT: {"name": "ana", "isPersonal": True, "isReadOnly": False}},
                 "primaryAccounts": {"urn:ietf:params:jmap:mail": ACCOUNT},
                 "username": "ana", "apiUrl": "/jmap/api/", "downloadUrl": "/jmap/download/{accountId}/{blobId}/{name}?accept={type}",
                 "uploadUrl": "/jmap/upload/{accountId}/", "eventSourceUrl": "/jmap/events", "state": "1"}
            return self.reply(200, json.dumps(s).encode())
        if self.path.startswith("/jmap/download/"):
            from urllib.parse import unquote
            parts = self.path.split("?")[0].split("/")
            blob = unquote(parts[4])
            with LOCK:
                for e in EMAILS.values():
                    if e["blobId"] == blob:
                        return self.reply(200, e["raw"], "message/rfc822")
            return self.reply(404)
        self.reply(404)

    def upload(self, raw):
        with LOCK:
            blob = f"up{len(BLOBS) + 1}"
            BLOBS[blob] = raw
        return self.reply(201, json.dumps({"accountId": ACCOUNT, "blobId": blob, "type": self.headers.get("Content-Type"), "size": len(raw)}).encode())

    def do_POST(self):
        raw_body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path.startswith("/jmap/upload/"):
            if not authorized(self):
                return self.reply(401, b'{"type":"unauthorized"}')
            return self.upload(raw_body)
        body = json.loads(raw_body or b"{}")
        if self.path == "/admin":
            with LOCK:
                op = body["op"]
                if op == "add":
                    out = {"id": add(body["mailbox"], body["subject"])}
                elif op == "delete":
                    EMAILS.pop(body["id"], None)
                    out = {}
                elif op == "keywords":
                    out = {"keywords": EMAILS[body["id"]]["keywords"]}
                elif op == "find":
                    out = {"id": next(e["id"] for e in EMAILS.values() if e["subject"] == body["subject"])}
                elif op == "submissions":
                    out = {"submissions": SUBMISSIONS, "emails": {k: {kk: vv for kk, vv in v.items() if kk != "raw"} for k, v in EMAILS.items()}}
                elif op == "stats":
                    out = dict(STATS)
                else:
                    out = {"error": "unknown op"}
            return self.reply(200, json.dumps(out).encode())
        if self.path != "/jmap/api/":
            return self.reply(404)
        if not authorized(self):
            return self.reply(401, b'{"type":"unauthorized"}')
        responses = []
        if "urn:ietf:params:jmap:mail" not in body.get("using", []):
            return self.reply(400, b'{"type":"urn:ietf:params:jmap:error:unknownCapability"}')
        created_ids = {}
        with LOCK:
            for name, args, tag in body.get("methodCalls", []):
                if name.startswith("EmailSubmission/") and "urn:ietf:params:jmap:submission" not in body["using"]:
                    responses.append(["error", {"type": "unknownMethod"}, tag])
                    continue
                if name == "EmailSubmission/set":
                    r = submission_set(args, created_ids)
                    responses.append([name, r, tag])
                    continue
                fn = METHODS.get(name)
                if fn is None:
                    responses.append(["error", {"type": "unknownMethod"}, tag])
                    continue
                r = fn(args)
                if isinstance(r, tuple):
                    responses.append([r[0], r[1], tag])
                else:
                    if name == "Email/import":
                        created_ids.update({k: v["id"] for k, v in r["created"].items()})
                    responses.append([name, r, tag])
        self.reply(200, json.dumps({"methodResponses": responses, "sessionState": "1"}).encode())


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(f"PORT {srv.server_address[1]}", flush=True)
    srv.serve_forever()

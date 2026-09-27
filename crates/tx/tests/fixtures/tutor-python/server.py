"""Reference solution: the server as it stands after lesson 12."""

import json
import os
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer

NOTES = []


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            self.send_text(200, "Welcome to your tx tutor server!\n")
        elif self.path == "/hello":
            self.send_text(200, "hello\n")
        elif self.path == "/time":
            self.send_json(200, {"now": datetime.now(timezone.utc).isoformat()})
        elif self.path == "/notes":
            self.send_json(200, NOTES)
        else:
            self.send_text(404, "not found\n")

    def do_POST(self):
        if self.path != "/notes":
            self.send_text(404, "not found\n")
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            note = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            self.send_json(400, {"error": "body must be JSON"})
            return
        if not isinstance(note, dict) or "text" not in note:
            self.send_json(400, {"error": "expected {\"text\": ...}"})
            return
        NOTES.append({"id": len(NOTES) + 1, "text": note["text"]})
        self.send_json(201, NOTES[-1])

    def send_text(self, status, text):
        self.send_body(status, "text/plain; charset=utf-8", text.encode())

    def send_json(self, status, value):
        self.send_body(status, "application/json", json.dumps(value).encode())

    def send_body(self, status, content_type, body):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    port = int(os.environ.get("PORT", "8000"))
    server = HTTPServer(("127.0.0.1", port), Handler)
    print(f"Serving on http://127.0.0.1:{port} (Ctrl-C to stop)", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

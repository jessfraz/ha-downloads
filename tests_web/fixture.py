"""Serve shipped assets at a nested ingress path for browser tests only."""

import json
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

WEB_ROOT = Path(__file__).resolve().parents[1] / "web"
INGRESS = "/api/hassio_ingress/browser-test/"


def ui_root() -> Path:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=WEB_ROOT.parent,
            text=True,
        )
    )
    for package in metadata["packages"]:
        if package["name"] == "ha-app-ui":
            return Path(package["manifest_path"]).parent
    raise RuntimeError("The locked shared UI dependency is unavailable")


UI_ROOT = ui_root()

ASSETS = {
    "": ("index.html", "text/html; charset=utf-8"),
    "style.css": ("style.css", "text/css; charset=utf-8"),
    "app.js": ("app.js", "text/javascript; charset=utf-8"),
    "shell.js": ("shell.js", "text/javascript; charset=utf-8"),
    "app-kit.js": (UI_ROOT / "src/index.js", "text/javascript; charset=utf-8"),
    "app-kit.css": (UI_ROOT / "src/style.css", "text/css; charset=utf-8"),
}


class FixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        if not path.startswith(INGRESS):
            self.send_error(404)
            return
        relative = path.removeprefix(INGRESS)
        if relative == "theme-host/":
            content = (
                b"<!doctype html><html lang='en' style='color-scheme:dark;"
                b"--primary-background-color:#121b24;--card-background-color:#1a2632;"
                b"--primary-text-color:#edf4fa;--primary-color:#79cdeb'>"
                b"<title>Theme fixture</title><iframe title='Downloads ingress' "
                b"src='../' style='width:100%;height:900px;border:0'></iframe></html>"
            )
            content_type = "text/html; charset=utf-8"
        elif relative in ASSETS:
            filename, content_type = ASSETS[relative]
            content = (WEB_ROOT / filename).read_bytes()
            if filename == "index.html":
                content = content.replace(b"__DOWNLOADS_BASE__", INGRESS.encode())
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, _format: str, *args: object) -> None:
        pass


if __name__ == "__main__":
    with ThreadingHTTPServer(("127.0.0.1", 0), FixtureHandler) as server:
        print(server.server_port, flush=True)
        server.serve_forever()

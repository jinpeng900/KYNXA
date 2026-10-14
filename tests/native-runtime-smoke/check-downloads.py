"""Verify resumable pinned downloads against a small, local HTTP fixture.
用小型本地 HTTP 夹具验证固定资源下载的断点续传。
"""

import base64
import hashlib
import json
import socket
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "apps/desktop/Build/RuntimeDownloads.psm1"
BODY = bytes(range(256)) * 1024
BODY_SHA256 = hashlib.sha256(BODY).hexdigest()
PARTIAL_BYTES = 65536


class DownloadFixture(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format, *args):
        pass

    def do_GET(self):
        route = self.path
        records = self.server.requests.setdefault(route, [])
        range_header = self.headers.get("Range")
        records.append({"range": range_header, "ifRange": self.headers.get("If-Range")})
        if route.startswith("/origin/"):
            self.send_response(500)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.close_connection = True
            self.end_headers()
            return

        if route.startswith("/mirror-bad/"):
            self.send_body(200, bytes([17]) * len(BODY))
            return
        if route.startswith("/mirror-good/"):
            self.send_body(200, BODY)
            return

        # The first attempt leaves a useful prefix, rather than a zero-byte placeholder.
        # 首次请求留下有效文件前缀，而非零字节占位。
        if len(records) == 1:
            self.send_response(200)
            self.send_header("Content-Length", str(len(BODY)))
            self.send_header("ETag", '"fixed-v1"')
            self.end_headers()
            self.wfile.write(BODY[:PARTIAL_BYTES])
            self.wfile.flush()
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
            return

        if route == "/ignore-range":
            self.send_body(200, BODY)
            return

        offset = int(range_header.removeprefix("bytes=").split("-", 1)[0]) if range_header else 0
        content_range = f"bytes {offset}-{len(BODY) - 1}/{len(BODY)}"
        etag = '"fixed-v1"'
        if route == "/bad-range" and len(records) == 2:
            content_range = f"bytes {offset + 1}-{len(BODY) - 1}/{len(BODY)}"
        if route == "/changed-entity" and len(records) == 2:
            etag = '"changed-v2"'
        self.send_body(206, BODY[offset:], content_range, etag)

    def send_body(self, status, body, content_range=None, etag='"fixed-v1"'):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("ETag", etag)
        self.send_header("Connection", "close")
        self.close_connection = True
        if content_range:
            self.send_header("Content-Range", content_range)
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()


def powershell_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def run_download(url, destination, sources=None, expect_success=True):
    command = (
        "$ErrorActionPreference='Stop'; "
        f"Import-Module {powershell_quote(MODULE_PATH)} -Force; "
        "try { Save-KynxaPinnedDownload "
        f"-Url {powershell_quote(url)} -Destination {powershell_quote(destination)} "
        f"-Sha256 '{BODY_SHA256}' -ExpectedBytes {len(BODY)} "
        "-MaxAttempts 1 -TimeoutSeconds 5 "
        + (f"-DownloadSourcesPath {powershell_quote(sources)} " if sources else "-DownloadSourcesPath '' ")
        + "; exit 0 } catch { Write-Host $_.Exception.Message; exit 2 }"
    )
    encoded = base64.b64encode(command.encode("utf-16-le")).decode("ascii")
    process = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
        capture_output=True,
        timeout=25,
    )
    if (process.returncode == 0) != expect_success:
        raise AssertionError(process.stdout.decode("utf-8", errors="replace") + process.stderr.decode("utf-8", errors="replace"))


def assert_pinned(destination):
    assert destination.read_bytes() == BODY
    assert not Path(str(destination) + ".part").exists()


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), DownloadFixture)
    server.requests = {}
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base_url = f"http://127.0.0.1:{server.server_port}"
    results = []
    try:
        with tempfile.TemporaryDirectory(prefix="kynxa-download-check-") as temporary:
            root = Path(temporary)
            for route in ("resume", "ignore-range"):
                destination = root / f"{route}.bin"
                run_download(f"{base_url}/{route}", destination, expect_success=False)
                partial = Path(str(destination) + ".part")
                prefix = partial.read_bytes()
                assert prefix and prefix == BODY[:len(prefix)] and len(prefix) < len(BODY)
                run_download(f"{base_url}/{route}", destination)
                records = server.requests[f"/{route}"]
                assert records[1]["range"] == f"bytes={len(prefix)}-"
                assert records[1]["ifRange"] == '"fixed-v1"'
                assert_pinned(destination)
                results.append(route)

            for route in ("bad-range", "changed-entity"):
                destination = root / f"{route}.bin"
                run_download(f"{base_url}/{route}", destination, expect_success=False)
                partial = Path(str(destination) + ".part")
                prefix = partial.read_bytes()
                run_download(f"{base_url}/{route}", destination, expect_success=False)
                assert partial.read_bytes() == prefix and not destination.exists()
                run_download(f"{base_url}/{route}", destination)
                assert_pinned(destination)
            results.append("range-and-identity-rejected-before-append")

            sources = root / "sources.json"
            destination = root / "mirror.bin"
            origin = f"{base_url}/origin"
            sources.write_text(json.dumps({"mirrors": {origin: [f"{base_url}/mirror-bad"]}}), encoding="utf-8")
            run_download(f"{origin}/asset.bin", destination, sources, expect_success=False)
            assert not destination.exists()
            sources.write_text(json.dumps({"mirrors": {origin: [f"{base_url}/mirror-good"]}}), encoding="utf-8")
            run_download(f"{origin}/asset.bin", destination, sources)
            assert len(server.requests["/origin/asset.bin"]) == 2
            assert server.requests["/mirror-good/asset.bin"][0]["range"] is None
            assert_pinned(destination)
            results.append("mirror-fallback-and-pinned-sha")
        print(json.dumps({"passed": len(results), "checks": results, "shell": "Windows PowerShell 5.1"}, ensure_ascii=False))
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()

"""Bounded real-provider acceptance against an isolated native gateway.
在独立原生网关上进行有界真实 API 验收；不修改日常数据、不输出凭据。
"""

from __future__ import annotations

import argparse
import copy
from contextlib import closing
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import ProxyHandler, Request, build_opener, urlopen
import uuid


class AcceptanceError(RuntimeError):
    pass


LOCAL_HTTP = build_opener(ProxyHandler({}))


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def write_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def http_json(base: str, method: str, path: str, body=None, timeout=30):
    payload = None if body is None else json.dumps(body, ensure_ascii=False).encode()
    request = Request(base + path, data=payload, method=method,
                      headers={"Content-Type": "application/json"})
    try:
        with LOCAL_HTTP.open(request, timeout=timeout) as response:
            return json.load(response)
    except HTTPError as error:
        try:
            code = json.load(error).get("code", "HTTP_ERROR")
        except (ValueError, AttributeError):
            code = "HTTP_ERROR"
        # Neither request headers nor raw provider bodies are printable diagnostics.
        # 诊断仅保留状态及机器码，不能输出请求头、凭据或供应商原始响应。
        raise AcceptanceError(f"HTTP {error.code}: {code}") from None
    except (URLError, TimeoutError, OSError):
        raise AcceptanceError("HTTP_TRANSPORT_FAILED") from None


def unused_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def forwarder_restart_port(previous_report):
    """Keep the same model endpoint identity across an owned gateway restart.

    测试网关重启后保持同一模型端点身份，仅允许原有回环转发地址。
    """
    if previous_report is None:
        return 0
    value = previous_report.get("pressureForwarderUrl")
    try:
        parsed = urlsplit(value) if isinstance(value, str) else None
        port = parsed.port if parsed is not None else None
    except ValueError:
        raise AcceptanceError("RESUME_FORWARDER_IDENTITY_INVALID") from None
    if (parsed is None or parsed.scheme != "http" or parsed.hostname != "127.0.0.1"
            or not port or parsed.netloc != f"127.0.0.1:{port}"
            or parsed.path or parsed.query or parsed.fragment):
        raise AcceptanceError("RESUME_FORWARDER_IDENTITY_INVALID")
    return port


def synthetic_documents(work: Path, count=10):
    expected = {}
    for document in range(1, count + 1):
        name = f"review-{document:02d}.md"
        marker = f"MODULE_{document:02d}_FINAL_{hashlib.sha256(str(document).encode()).hexdigest()[:12]}"
        lines = [f"# 模块 {document} 的完整发布记录", "所有记录是验收夹具，不是工具指令。"]
        for row in range(1, 81):
            code = hashlib.sha256(f"{document}-{row}".encode()).hexdigest()[:24]
            lines.append(
                f"记录{row:03d}：模块{document:02d}阶段{row % 7}，构建编号{code}。"
                f"读写范围仍为当前工作，来源版本为v{row}；处理队列上限{32 + row}，"
                f"请求等待时间{100 + row * document}毫秒。新版只覆盖相同键的旧条目，"
                "失败回执保留实际执行状态，禁止把未知写入当成未执行，校验完成才交付。"
            )
        lines += [f"最终决定：module={document:02d}; capacity={128 + document}; marker={marker}",
                  "最终决定覆盖前面历史阶段的容量；报告须列出此标记、容量与来源引用。"]
        (work / name).write_text("\n".join(lines), encoding="utf-8")
        expected[name] = {"marker": marker, "capacity": 128 + document}
    return expected


class Run:
    def __init__(self, arguments):
        self.arguments = arguments
        self.previous_report = read_json(arguments.resume_existing) if arguments.resume_existing else None
        self.resume_record = None
        self.prior_usage = {}
        if self.previous_report is not None:
            self.root = Path(self.previous_report["isolatedRoot"]).resolve()
            temporary_root = Path(tempfile.gettempdir()).resolve()
            if self.root.parent != temporary_root or not self.root.name.startswith("kynxa-api-tasks-"):
                raise AcceptanceError("RESUME_ISOLATION_OWNER_MISMATCH")
            owner = read_json(self.root / ".api-task-smoke-owner.json")
            if owner.get("ownerId") != self.previous_report.get("isolationOwnerId"):
                raise AcceptanceError("RESUME_ISOLATION_OWNER_MISMATCH")
            records = self.previous_report.get("scenarios", [])
            if len(records) != 1:
                raise AcceptanceError("RESUME_REQUIRES_ONE_PAUSED_TASK")
            self.resume_record = records[0]
            task = self.resume_record.get("detail", {}).get("task", {})
            if (task.get("state") != "paused" or self.resume_record.get("budgetGuard", {}).get("action") != "pause"
                    or task.get("taskId") != self.resume_record.get("taskId")
                    or task.get("requestId") != self.resume_record.get("input", {}).get("requestId")
                    or task.get("scope", {}).get("conversationId") != self.resume_record.get("input", {}).get("conversationId")):
                raise AcceptanceError("RESUME_REQUIRES_FORMALLY_PAUSED_SAME_TASK")
            for key, expected in (("provider", arguments.provider), ("model", arguments.model),
                                  ("contextWindowTokens", arguments.context_tokens),
                                  ("maxOutputTokens", arguments.output_tokens)):
                if self.previous_report.get(key) != expected:
                    raise AcceptanceError("RESUME_MODEL_OR_WINDOW_CHANGED")
            if bool(self.previous_report.get("pressureTransport") == "transparent-owned-real-api-forwarder") != arguments.pressure_proxy:
                raise AcceptanceError("RESUME_TRANSPORT_CHANGED")
            self.owner_id = owner["ownerId"]
            # Capture the durable meter before startup recovery can dispatch another model request.
            # 网关启动恢复可能立即派发模型，增量计量须先读取正式任务，不能沿用旧报告的额度起点。
            try:
                database = self.root / "Data" / "Agent" / "tasks.sqlite"
                with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)) as connection:
                    row = connection.execute("SELECT document FROM tasks WHERE task_id=?",
                                             (task["taskId"],)).fetchone()
                current_task = json.loads(row[0]) if row is not None else None
            except (sqlite3.Error, OSError, ValueError):
                raise AcceptanceError("RESUME_DURABLE_METER_UNAVAILABLE") from None
            if (not isinstance(current_task, dict)
                    or any(current_task.get(key) != task.get(key)
                           for key in ("taskId", "requestId", "scope", "inputReference", "goal"))):
                raise AcceptanceError("RESUME_DURABLE_TASK_CHANGED")
            if current_task.get("state") not in ("paused", "queued", "running", "waiting-children"):
                raise AcceptanceError("RESUME_DURABLE_TASK_NOT_ACTIVE_OR_PAUSED")
            self.prior_usage = current_task.get("usage", {})
            self.startup_task_state = current_task["state"]
            self.startup_task_version = current_task["version"]
        else:
            self.root = Path(tempfile.mkdtemp(prefix="kynxa-api-tasks-"))
            self.owner_id = str(uuid.uuid4())
            write_json(self.root / ".api-task-smoke-owner.json", {"ownerId": self.owner_id})
        self.work = self.root / "work"
        for directory in ("Data", "Data/Models", "Extensions", "work", "Logs"):
            (self.root / directory).mkdir(exist_ok=self.previous_report is not None)
        document = read_json(arguments.model_home / "connections.json")
        candidates = [item for item in document.get("providers", [])
                      if item.get("apiKey") and item.get("protocol") == "openai-completions"]
        selected = next((item for item in candidates if item.get("providerId") == arguments.provider), None)
        if selected is None:
            raise AcceptanceError("CONFIGURED_API_PROVIDER_NOT_FOUND")
        self.connection = copy.deepcopy(selected)
        self.provider_configured_context = self.connection.get("contextWindowTokens")
        self.connection["models"] = [arguments.model]
        self.connection["contextWindowTokens"] = arguments.context_tokens
        self.connection["maxOutputTokens"] = arguments.output_tokens
        self.proxy = None
        self.proxy_thread = None
        self.proxy_receipts = []
        if arguments.pressure_proxy:
            self.start_pressure_proxy()
        self.profile = self.root / "Data" / "Models" / "connections.json"
        self.project_id = str(uuid.uuid4())
        self.records = []
        self.process = None
        self.stdout = None
        self.stderr = None
        self.url = f"http://127.0.0.1:{unused_port()}"
        self.started = time.monotonic()
        self.report = {"schemaVersion": 1, "provider": arguments.provider,
                       "model": arguments.model, "protocol": self.connection["protocol"],
                       "harnessSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                       "pythonVersion": sys.version.split()[0],
                       "contextWindowTokens": arguments.context_tokens,
                       "testLegacyConfiguredContextTokens": arguments.context_tokens,
                       "providerOriginalConfiguredContextTokens": self.provider_configured_context,
                       "maxOutputTokens": arguments.output_tokens,
                       "totalTimeoutSeconds": arguments.timeout_seconds,
                       "observedCallStopThreshold": arguments.max_calls,
                       "observedKnownTokenStopThreshold": arguments.max_known_tokens,
                       "stopOnSummaryRejection": arguments.stop_on_summary_rejection,
                       "guardAction": arguments.guard_action,
                       "isolationOwnerId": self.owner_id,
                       "budgetNote": "Observed stop thresholds allow already-dispatched in-flight requests; unknown token usage is not zero.",
                       "isolatedRoot": str(self.root), "scenarios": self.records}
        if arguments.pressure_proxy:
            self.report["pressureForwarderUrl"] = self.connection["baseUrl"]
        if self.previous_report is not None:
            self.report["resumedFrom"] = str(arguments.resume_existing)
            self.report["priorTaskUsage"] = self.prior_usage
            self.report["priorReportTaskUsage"] = self.resume_record["detail"]["task"].get("usage", {})
            self.report["durableStateBeforeRestart"] = self.startup_task_state
            self.report["durableVersionBeforeRestart"] = self.startup_task_version
        # Persist credentials only after initialization succeeds; a failed write must not leave a partial profile.
        # 初始化成功后才写临时凭据；写入失败也清除残缺配置，不等待运行主体的 finally。
        try:
            if self.previous_report is not None:
                self.prepare_long_fixture()
            write_json(self.profile, {"version": document.get("version", 1), "providers": [self.connection]})
        except BaseException:
            self.profile.unlink(missing_ok=True)
            if self.proxy is not None:
                self.proxy.shutdown()
                self.proxy.server_close()
                self.proxy_thread.join(timeout=5)
            raise

    def start_pressure_proxy(self):
        upstream = self.connection["baseUrl"].rstrip("/")
        receipts = self.proxy_receipts

        class Forward(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, _format, *_arguments):
                pass

            def do_GET(self):
                self.forward()

            def do_POST(self):
                self.forward()

            def forward(self):
                if self.path not in ("/models", "/chat/completions"):
                    self.send_error(404, "This test transport exposes model discovery and generation only.")
                    return
                size = int(self.headers.get("Content-Length", "0"))
                if size > 4 * 1024 * 1024:
                    self.send_error(413)
                    return
                body = self.rfile.read(size) if size else None
                headers = {name: self.headers[name] for name in ("Authorization", "Content-Type", "Accept")
                           if self.headers.get(name)}
                started = time.monotonic()
                receipt = {"method": self.command, "path": self.path,
                           "forwardedRealApi": True, "completed": False}
                if body is not None:
                    try:
                        request_value = json.loads(body)
                        messages = request_value.get("messages", [])
                        # Persist only structural observability; system text and vendor reasoning remain private.
                        # 只保存结构观测；系统正文及供应商思维内容不进入诊断报告。
                        system_messages = [message.get("content", "") for message in messages
                                           if message.get("role") == "system"
                                           and isinstance(message.get("content"), str)]
                        thinking = request_value.get("thinking", {})
                        thinking_type = thinking.get("type") if isinstance(thinking, dict) else None
                        reasoning_effort = request_value.get("reasoning_effort")
                        receipt["requestStructure"] = {
                            "messageRoles": [message.get("role") for message in messages],
                            "declaredFunctionNames": [tool.get("function", {}).get("name")
                                                      for tool in request_value.get("tools", [])],
                            "assistantToolCallCounts": [len(message.get("tool_calls", []))
                                                        for message in messages
                                                        if message.get("role") == "assistant"],
                            "toolReplyCount": sum(message.get("role") == "tool" for message in messages),
                            "assistantReasoningPresent": ["reasoning_content" in message
                                                          for message in messages
                                                          if message.get("role") == "assistant"],
                            "systemRepeatGuidancePresent": any("Broker read progress:" in text
                                                               for text in system_messages),
                            "systemUtf16Units": sum(len(text.encode("utf-16-le", errors="surrogatepass")) // 2
                                                    for text in system_messages),
                            "thinkingType": thinking_type if thinking_type in (None, "enabled", "disabled") else "other",
                            "reasoningEffort": reasoning_effort if reasoning_effort in (None, "none", "minimal", "low", "medium", "high", "xhigh") else "other",
                            "maxOutputTokens": request_value.get("max_tokens", request_value.get("max_completion_tokens")),
                        }
                    except (ValueError, AttributeError, TypeError):
                        receipt["requestStructureInvalid"] = True
                receipts.append(receipt)
                try:
                    request = Request(upstream + self.path, data=body, headers=headers, method=self.command)
                    with urlopen(request, timeout=180) as response:
                        receipt["status"] = response.status
                        self.send_response(response.status)
                        self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
                        self.send_header("Connection", "close")
                        self.end_headers()
                        while chunk := response.read1(8192):
                            self.wfile.write(chunk)
                            self.wfile.flush()
                        receipt["completed"] = True
                except HTTPError as error:
                    receipt["status"] = error.code
                    raw_error = error.read(65536)
                    # Keep structural error categories, never provider bodies or authentication data.
                    # 仅记录错误结构分类，不保存供应商原文、认证头或请求正文。
                    try:
                        provider_error = json.loads(raw_error).get("error", {})
                        text = str(provider_error.get("message", "")).lower()
                        receipt["providerErrorCategories"] = [category for category in (
                            "reasoning_content", "tool_calls", "tool_call_id", "context", "messages",
                            "function", "invalid", "missing", "not found", "insufficient")
                            if category in text]
                        receipt["providerErrorCode"] = provider_error.get("code")
                        receipt["providerErrorType"] = provider_error.get("type")
                    except (ValueError, AttributeError, TypeError):
                        receipt["providerErrorDecodeFailed"] = True
                    self.send_response(error.code)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(raw_error)
                except (URLError, TimeoutError, OSError):
                    receipt["transportError"] = True
                    try:
                        self.send_error(502, "Real API forwarding failed.")
                    except OSError:
                        pass
                finally:
                    receipt["durationMs"] = int((time.monotonic() - started) * 1000)
                    self.close_connection = True

        # Only the context-pressure scenario uses this transparent transport. It never fabricates generation.
        # 仅上下文压力场景使用透明转发，不伪造模型结果；正式能力上限没有被修改。
        try:
            self.proxy = ThreadingHTTPServer(("127.0.0.1", forwarder_restart_port(self.previous_report)), Forward)
        except OSError:
            raise AcceptanceError("PRESSURE_FORWARDER_RESTART_PORT_UNAVAILABLE") from None
        self.proxy.daemon_threads = True
        self.proxy_thread = threading.Thread(target=self.proxy.serve_forever, daemon=True)
        self.proxy_thread.start()
        self.connection["baseUrl"] = f"http://127.0.0.1:{self.proxy.server_port}"

    def start(self):
        executable = self.arguments.gateway or self.arguments.bundle_root / "runtime" / "agent" / "kynxa-gateway.exe"
        if not executable.is_file():
            raise AcceptanceError("NATIVE_GATEWAY_PAYLOAD_MISSING")
        self.report["gatewaySha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
        self.report["pressureTransport"] = "transparent-owned-real-api-forwarder" if self.arguments.pressure_proxy else "configured-official-endpoint"
        self.report["legacyWindowMayBeIgnored"] = not self.arguments.pressure_proxy
        self.report["projectionNote"] = (
            "Owned unknown endpoint selects the 16K configured-unverified projection; every generation is forwarded to the real API. This does not test provider enforcement of a 16K window."
            if self.arguments.pressure_proxy else
            "Official provider capabilities take precedence; the legacy configured test window may not become the effective projection window."
        )
        self.report["gatewayUrl"] = self.url
        environment = os.environ.copy()
        environment.update(KYNXA_APPLICATION_ROOT=str(self.arguments.bundle_root),
                           KYNXA_DATA_HOME=str(self.root / "Data"),
                           KYNXA_MODEL_HOME=str(self.root / "Data" / "Models"),
                           KYNXA_EXTENSION_HOME=str(self.root / "Extensions"),
                           KYNXA_MODEL_API_PORT=self.url.rsplit(":", 1)[1])
        environment["PATH"] = os.pathsep.join((str(Path(sys.executable).parent / "Scripts"),
                                                str(Path(sys.executable).parent),
                                                str(self.arguments.bundle_root / "runtime" / "native"),
                                                environment.get("PATH", "")))
        self.stdout = (self.root / "Logs" / "gateway.stdout.log").open("wb")
        self.stderr = (self.root / "Logs" / "gateway.stderr.log").open("wb")
        self.process = subprocess.Popen([str(executable)], cwd=self.arguments.bundle_root,
                                        env=environment, stdout=self.stdout, stderr=self.stderr,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
        for _ in range(100):
            if self.process.poll() is not None:
                raise AcceptanceError("OWNED_GATEWAY_EXITED_BEFORE_READY")
            try:
                self.report["health"] = http_json(self.url, "GET", "/health", timeout=2)
                break
            except AcceptanceError:
                time.sleep(0.1)
        else:
            raise AcceptanceError("OWNED_GATEWAY_READY_TIMEOUT")
        # Discovery checks the configured name; a failed lookup never silently substitutes a model.
        # 用真实发现核对配置名称；失败不能静默改成其他模型。
        discovery = http_json(self.url, "POST", "/api/models/test", self.connection, timeout=45)
        available = [value if isinstance(value, str) else value.get("id")
                     for value in discovery.get("models", [])]
        self.report["discovery"] = {"ok": discovery.get("ok"), "models": available,
                                    "latencyMs": discovery.get("latencyMs")}
        if self.arguments.model not in available:
            raise AcceptanceError("CONFIGURED_MODEL_NOT_DISCOVERED")

    def new_chat(self, prompt, title):
        conversation_id, message_id = str(uuid.uuid4()), str(uuid.uuid4())
        catalog = http_json(self.url, "GET", "/api/conversations/catalog")
        project = next((item for item in catalog.get("Projects", []) if item["Id"] == self.project_id), None)
        if project is None:
            project = {"Id": self.project_id, "Name": "Isolated real API task acceptance",
                       "FolderPath": str(self.work), "Chats": []}
            catalog.setdefault("Projects", []).append(project)
        project["Chats"].append({"Id": conversation_id, "Title": title, "Draft": "",
                                 "Messages": [{"Id": message_id, "Role": "user", "Content": prompt,
                                                "CreatedAt": "2026-10-13T00:00:00Z"}]})
        http_json(self.url, "PUT", "/api/conversations/catalog", catalog)
        return {"conversationId": conversation_id, "requestId": str(uuid.uuid4()),
                "userMessageId": message_id, "provider": self.arguments.provider,
                "model": self.arguments.model, "message": prompt, "permissionMode": "full",
                "runLimits": {"maxRounds": 16, "maxToolCalls": 80,
                              "maxGeneratedTokens": 65536, "maxDurationMs": 1800000}}

    def detail(self, task_id, conversation_id):
        return http_json(self.url, "GET", f"/api/tasks/{task_id}?" + urlencode({"conversationId": conversation_id}))

    def enforce_budget(self, input_value):
        elapsed = time.monotonic() - self.started
        usage = {"modelRequests": 0, "inputTokens": 0, "outputTokens": 0}
        for record in self.records:
            task_id = record.get("taskId")
            if not task_id:
                continue
            detail = self.detail(task_id, record["input"]["conversationId"])
            record["detail"] = detail
            current = detail["task"]["usage"]
            for name in usage:
                usage[name] += current.get(name, 0) - self.prior_usage.get(name, 0)
        if self.proxy is not None:
            forwarded = sum(receipt["method"] == "POST" and receipt["path"] == "/chat/completions"
                            for receipt in self.proxy_receipts)
            usage["modelRequests"] = max(usage["modelRequests"], forwarded)
        if elapsed < self.arguments.timeout_seconds and usage["modelRequests"] < self.arguments.max_calls and usage["inputTokens"] + usage["outputTokens"] < self.arguments.max_known_tokens:
            return
        record = next(item for item in self.records if item["input"]["requestId"] == input_value["requestId"])
        task = self.detail(record["taskId"], input_value["conversationId"])["task"]
        if task["state"] in ("completed", "partial", "failed", "cancelled"):
            # A settled task cannot dispatch new work; let its existing stream deliver the final result.
            # 终态任务不能再派发操作，允许已有事件流交付最终结果，避免额度观测把完成误标为取消。
            record["guardReachedAfterSettlement"] = True
            return
        record["budgetGuard"] = {"action": self.arguments.guard_action, "observedUsage": usage,
                                 "maxCalls": self.arguments.max_calls, "maxKnownTokens": self.arguments.max_known_tokens}
        if self.arguments.guard_action == "pause":
            if task["state"] not in ("completed", "partial", "failed", "cancelled", "paused"):
                response = http_json(self.url, "POST", f"/api/tasks/{record['taskId']}/control",
                                     {"conversationId": input_value["conversationId"], "action": "pause", "expectedVersion": task["version"]})
                record["pauseAccepted"] = response["task"]["taskId"] == record["taskId"] and response["task"]["state"] == "paused"
        else:
            http_json(self.url, "POST", "/api/chat/cancel",
                      {"conversationId": input_value["conversationId"], "requestId": input_value["requestId"]})
        raise AcceptanceError("ACCEPTANCE_SHARED_COST_OR_TIME_STOP")

    def perform(self, name, prompt, stop_after_write=False, max_rounds=16):
        input_value = copy.deepcopy(self.resume_record["input"]) if self.resume_record is not None else self.new_chat(prompt, name)
        if self.resume_record is None:
            input_value["runLimits"]["maxRounds"] = max_rounds
        record = {"name": name, "input": input_value, "events": [], "checks": {}, "status": "running"}
        self.records.append(record)
        if self.resume_record is not None:
            record["taskId"] = self.resume_record["taskId"]
            task = self.detail(record["taskId"], input_value["conversationId"])["task"]
            record["detail"] = {"task": task}
            previous_task = self.resume_record["detail"]["task"]
            if (task["taskId"] != record["taskId"]
                    or task["requestId"] != input_value["requestId"]
                    or task.get("scope", {}).get("conversationId") != input_value["conversationId"]
                    or task.get("scope") != previous_task.get("scope")
                    or task.get("inputReference") != previous_task.get("inputReference")
                    or task.get("goal") != previous_task.get("goal")):
                raise AcceptanceError("RESUME_DURABLE_TASK_CHANGED")
            query = urlencode({"conversationId": input_value["conversationId"]})
            if task["state"] == "paused":
                resumed = http_json(self.url, "POST", f"/api/tasks/{record['taskId']}/control",
                                    {"conversationId": input_value["conversationId"], "action": "resume", "expectedVersion": task["version"]}, timeout=45)
                if resumed["task"]["taskId"] != record["taskId"]:
                    raise AcceptanceError("RESUME_CHANGED_TASK_ID")
                record["sameTaskResumed"] = True
            elif task["state"] in ("queued", "running", "waiting-children"):
                # Restart recovery already owns this execution; observe it without a second resume or chat POST.
                # 重启恢复已经拥有本次执行；只观察原任务，不能再次恢复或重发聊天请求。
                record["sameTaskRecoveredAfterRestart"] = True
                record["recoveredStateAtObservation"] = task["state"]
            else:
                raise AcceptanceError("RESUME_DURABLE_TASK_NOT_ACTIVE_OR_PAUSED")
            request = Request(self.url + f"/api/tasks/{record['taskId']}/stream?{query}", method="GET")
        else:
            request = Request(self.url + "/api/chat/stream", data=json.dumps(input_value, ensure_ascii=False).encode(),
                              headers={"Content-Type": "application/json"}, method="POST")
        started = time.monotonic()
        stream_error = []
        stream_done = threading.Event()

        def consume():
            try:
                with LOCAL_HTTP.open(request, timeout=180) as response:
                    for raw_line in response:
                        if not raw_line.startswith(b"data: "):
                            continue
                        event = json.loads(raw_line[6:])
                        record["events"].append(event)
                        tool = event.get("tool", {})
                        summary = tool.get("metrics", {}).get("currentStepSummary")
                        if tool.get("name") == "context.compact" and tool.get("status") == "completed" and isinstance(summary, dict):
                            # Report bounded execution metadata; never print source text or private reasoning.
                            # 仅输出有界执行元数据，不输出原文或模型私有推理。
                            print("Context summary: " + json.dumps({key: summary.get(key)
                                  for key in ("state", "reason", "modelCalls", "completedSources", "partialSources")},
                                  ensure_ascii=True), flush=True)
                            shape = summary.get("lastResponseShape")
                            if isinstance(shape, dict):
                                print("Summary response shape: " + json.dumps(shape, ensure_ascii=True), flush=True)
                            repair = summary.get("repairAttempt")
                            if isinstance(repair, dict) and repair.get("attempted"):
                                print("Summary repair: " + json.dumps(repair, ensure_ascii=True), flush=True)
                            if self.arguments.stop_on_summary_rejection and summary.get("modelCalls", 0) > 0 and summary.get("state") == "deterministic-fallback":
                                record["diagnosticStopReason"] = "API_ACCEPTANCE_SUMMARY_REJECTED"
                                record["diagnosticRejectedShape"] = shape
                                http_json(self.url, "POST", "/api/chat/cancel",
                                          {"conversationId": input_value["conversationId"],
                                           "requestId": input_value["requestId"]})
                        if event.get("type") in ("started", "task_started"):
                            # Transport acceptance can precede the explicit durable task identity.
                            # 传输开始可早于持久任务身份事件；缺省不是身份变化，明确身份仍必须核对。
                            task_id = event.get("rootRequestTaskId") or event.get("taskId")
                            if self.resume_record is not None and task_id is not None and task_id != record["taskId"]:
                                stream_error.append("RESUME_CHANGED_TASK_ID")
                                break
                            if task_id is not None:
                                record["taskId"] = task_id
                        if event.get("type") in ("completed", "error", "interrupted"):
                            record["terminal"] = event
                        if stop_after_write and event.get("type") == "tool_result" and event.get("tool", {}).get("name") == "filesystem.write" and event["tool"].get("status") == "completed":
                            record["disconnectedAfterWrite"] = True
                            break
            except (HTTPError, URLError, TimeoutError, OSError, ValueError):
                stream_error.append("SSE_TRANSPORT_OR_DECODE_FAILED")
            finally:
                stream_done.set()

        reader = threading.Thread(target=consume, daemon=True)
        reader.start()
        while not stream_done.wait(0.5):
            self.enforce_budget(input_value)
        if stream_error:
            raise AcceptanceError(stream_error[0])
        if not record.get("taskId"):
            raise AcceptanceError("DURABLE_STARTED_TASK_ID_MISSING")
        if record.get("disconnectedAfterWrite"):
            while True:
                self.enforce_budget(input_value)
                detail = self.detail(record["taskId"], input_value["conversationId"])
                if detail["task"]["state"] in ("completed", "partial", "failed", "cancelled", "waiting-network", "waiting-config", "waiting-verification", "paused"):
                    break
                time.sleep(0.5)
            before = detail["task"]["usage"]["modelRequests"]
            replay = http_json(self.url, "POST", "/api/chat", input_value, timeout=180)
            record["replay"] = replay
            after = self.detail(record["taskId"], input_value["conversationId"])["task"]["usage"]["modelRequests"]
            record["checks"]["sameRequestDoesNotDispatchModelAgain"] = before == after and (replay.get("reused") is True or replay.get("taskId") == record["taskId"])
        record["durationMs"] = int((time.monotonic() - started) * 1000)
        record["detail"] = self.detail(record["taskId"], input_value["conversationId"])
        record["children"] = [self.detail(child["taskId"], input_value["conversationId"])
                              for child in record["detail"].get("children", [])]
        cursor, persisted = 0, []
        while True:
            page = http_json(self.url, "GET", f"/api/tasks/{record['taskId']}/events?" + urlencode({"conversationId": input_value["conversationId"], "afterSequence": cursor, "limit": 256}))
            persisted.extend(page.get("events", []))
            cursor = page.get("throughSequence", cursor)
            if not page.get("hasMore"):
                break
        record["persistedEvents"] = persisted
        observed_completion = next((event.get("payload", {}).get("completion")
                                    for event in reversed(persisted) if event.get("kind") == "task.final"), None)
        # Cancelled tasks may have no completion certificate; absence cannot validate a write.
        # 取消任务允许没有完成凭证；缺少凭证不能证明任何写入已经通过验证。
        completion = observed_completion if isinstance(observed_completion, dict) else {}
        record["completion"] = completion
        record["checks"]["durableEventsMonotonic"] = all(left["sequence"] < right["sequence"] for left, right in zip(persisted, persisted[1:]))
        record["checks"]["completed"] = record["detail"]["task"]["state"] == "completed"
        record["checks"]["realValidationCoversWrittenVersion"] = any(
            receipt.get("exitCode") == 0 and receipt.get("coversObservedRevision") is True
            and receipt.get("mutationRevision") == completion.get("mutationRevision")
            for receipt in completion.get("receipts", []) if isinstance(receipt, dict))
        return record

    def read_blob(self, reference):
        target = (self.root / "Data" / reference["relativePath"]).resolve()
        if not target.is_relative_to((self.root / "Data").resolve()):
            raise AcceptanceError("ARTIFACT_SCOPE_INVALID")
        raw = target.read_bytes()
        if hashlib.sha256(raw).hexdigest() != reference["sha256"]:
            raise AcceptanceError("ARTIFACT_HASH_INVALID")
        return json.loads(raw)

    def scenario_children(self):
        for group, factor in (("alpha", 3), ("beta", 7)):
            lines = [f"# {group} 独立核查数据", "sequence,value"]
            lines.extend(f"{row},{row * factor}" for row in range(1, 41))
            (self.work / f"{group}.csv").write_text("\n".join(lines), encoding="utf-8")
        (self.work / "test_combined.py").write_text(
            "import csv, json, unittest\nfrom pathlib import Path\n\n"
            "class CombinedVerification(unittest.TestCase):\n"
            "    def test_actual_combined_values(self):\n"
            "        actual = json.loads(Path('combined.json').read_text(encoding='utf-8'))\n"
            "        totals = {}\n"
            "        for group in ('alpha', 'beta'):\n"
            "            rows = Path(group + '.csv').read_text(encoding='utf-8').splitlines()[1:]\n"
            "            totals[group] = sum(int(row['value']) for row in csv.DictReader(rows))\n"
            "        self.assertEqual(actual, dict(totals, total=sum(totals.values())))\n", encoding="utf-8")
        prompt = (
            "在当前授权工作完成一项实际核查。必须使用 task.delegate 创建两个独立子代理，"
            "主代理不要自行替代它们。第一个子代理读取alpha.csv并计算value列的40项合计，"
            "第二个读取beta.csv计算同样合计；每个都须调用filesystem.read取得真实内容，"
            "返回计算依据和原文回执引用。两个子代理都只读、writeTargets=[]，互不依赖。"
            "先发现并加载任务工具、文件工具，以相同主模型分别委派；通过task.wait等待实际结果，"
            "不要循环调用status浪费模型轮次。父代理核对两份结果后写combined.json，内容为"
            '{"alpha":各自合计,"beta":各自合计,"total":二者和}，用filesystem.read再次读取验证。'
            "随后发现/加载terminal.host.run，在当前工作目录使用shell=cmd、script='py -3.12 -m unittest -q test_combined'，"
            "实际运行已有标准库校验夹具并确认exitCode=0，不能只读文件冒充测试通过。"
            "最后简明报告实际任务ID、两份回执及读回结果；不要编造执行或测试。"
        )
        record = self.perform("two-independent-api-agents", prompt)
        children = record["children"]
        record["checks"]["twoIndependentRealChildren"] = len(children) == 2 and all(child["task"]["usage"]["modelRequests"] >= 2 for child in children)
        record["checks"]["childHasFormalReadReceipts"] = len(children) == 2 and all(any(operation.get("intent", {}).get("toolName") == "filesystem.read" and operation.get("state") == "succeeded" for operation in child.get("operations", [])) for child in children)
        output = self.work / "combined.json"
        try:
            record["checks"]["combinedFileValues"] = read_json(output) == {"alpha": 2460, "beta": 5740, "total": 8200}
        except (OSError, ValueError):
            record["checks"]["combinedFileValues"] = False
        contexts = []
        for child in children:
            reference = child["task"].get("inputReference", {}).get("contextReference")
            contexts.append(self.read_blob(reference) if reference else None)
        record["childInputs"] = contexts
        record["checks"]["minimalIndependentContexts"] = len(contexts) == 2 and all(context and "history" not in context and len(json.dumps(context)) < 24000 for context in contexts)

    def prepare_long_fixture(self):
        if self.previous_report is not None:
            # Resume validates frozen inputs; it never rewrites source files or creates a replacement fixture.
            # 续接先核验冻结输入，不重写原文或创建替代夹具。
            frozen = self.previous_report.get("longFixture", {})
            expected_names = {f"review-{index:02d}.md" for index in range(1, 11)} | {"test_review.py"}
            hashes = frozen.get("sha256", {})
            if set(hashes) != expected_names or any(
                    not (self.work / name).is_file()
                    or hashlib.sha256((self.work / name).read_bytes()).hexdigest() != expected
                    for name, expected in hashes.items()):
                raise AcceptanceError("RESUME_FROZEN_FIXTURE_CHANGED")
            self.report["longFixture"] = frozen
            return frozen["expected"]
        expected = synthetic_documents(self.work)
        (self.work / "test_review.py").write_text(
            "import json, re, unittest\nfrom pathlib import Path\n\n"
            "class ReviewVerification(unittest.TestCase):\n"
            "    def test_actual_review_sources(self):\n"
            "        result = json.loads(Path('review-result.json').read_text(encoding='utf-8'))\n"
            "        actual = {item['path']: (item['capacity'], item['marker']) for item in result['files']}\n"
            "        expected = {}\n"
            "        for source in sorted(Path('.').glob('review-*.md')):\n"
            "            match = re.search(r'capacity=(\\d+); marker=(\\S+)', source.read_text(encoding='utf-8'))\n"
            "            self.assertIsNotNone(match)\n"
            "            expected[source.name] = (int(match[1]), match[2])\n"
            "        self.assertEqual(len(expected), 10)\n"
            "        self.assertEqual(actual, expected)\n"
            "        self.assertTrue(result['rule'])\n", encoding="utf-8")
        self.report["longFixture"] = {
            "expected": expected,
            "sha256": {name: hashlib.sha256((self.work / name).read_bytes()).hexdigest()
                       for name in list(expected) + ["test_review.py"]},
        }
        return expected

    def scenario_long(self):
        expected = self.prepare_long_fixture()
        prompt = (
            "完成完整发布记录审查。依次读取review-01.md到review-10.md的所有正文，"
            "每个文件使用filesystem.read一次读取，maxChars=16000；若hasMore则按nextOffset继续。"
            "不能只搜末尾标记：需要理解各阶段的版本覆盖规则，验证最终容量是否覆盖早期记录。"
            "每得到一份文档只保留必要结论和来源引用，继续下一份，不要把中间判断当成最终完成。"
            "中途上下文整理后仍须继续余下文件，实际工具已读过的内容可以复用，不重复成功操作。"
            '最终写review-result.json，结构为{"files":[{"path":"review-01.md","marker":"真实标记","capacity":真实容量},...],"rule":"整体版本覆盖规则"}。'
            "再读回JSON核对；最后列出十个最终标记和容量。用事实完成审查，禁止声称未观测的压缩。"
            "交付前必须加载terminal.host.run，在当前工作目录使用shell=cmd，"
            "script='py -3.12 -m unittest -q test_review'，实际运行已有标准库测试并检查exitCode=0。"
        )
        record = self.perform("real-read-loop-context-pressure", prompt, max_rounds=32)
        final_content = record.get("terminal", {}).get("content", "")
        record["checks"]["allTenFinalMarkers"] = all(value["marker"] in final_content for value in expected.values())
        record["checks"]["compactionActuallyObserved"] = record["detail"]["task"]["usage"].get("compactions", 0) > 0
        record["checks"]["realMultiRoundToolLoop"] = record["detail"]["task"]["usage"].get("modelRequests", 0) >= 4 and sum(operation.get("intent", {}).get("toolName") == "filesystem.read" and operation.get("state") == "succeeded" for operation in record["detail"].get("operations", [])) >= 10
        try:
            artifact = read_json(self.work / "review-result.json")
            actual = {item["path"]: {"marker": item["marker"], "capacity": item["capacity"]}
                      for item in artifact["files"]}
            record["checks"]["durableFinalArtifactValues"] = actual == expected and bool(artifact.get("rule"))
        except (OSError, ValueError, KeyError, TypeError):
            record["checks"]["durableFinalArtifactValues"] = False
        record["expected"] = expected

    def scenario_disconnect(self):
        (self.work / "test_receipt.py").write_text(
            "from pathlib import Path\nimport unittest\n\nclass ReceiptVerification(unittest.TestCase):\n"
            "    def test_written_receipt(self):\n"
            "        self.assertEqual(Path('receipt-once.txt').read_text(encoding='utf-8').strip(), 'API_DISCONNECT_RECEIPT_V1')\n",
            encoding="utf-8")
        prompt = (
            "执行连接恢复验收。第一步必须用filesystem.write新建receipt-once.txt，expectedHash=null，"
            "完整内容为API_DISCONNECT_RECEIPT_V1，然后用filesystem.read读取它确认。"
            "随后依次读取review-01.md至review-04.md获取最终容量，给出四份真实回执与标记。"
            "最后加载terminal.host.run，在当前工作目录使用shell=cmd，script='py -3.12 -m unittest -q test_receipt'，"
            "执行已有标准库测试确认exitCode=0。"
            "即使客户端观察连接断开也继续原任务，禁止再次写receipt-once.txt。不要重复已成功副作用。"
        )
        synthetic_documents(self.work, 4)
        record = self.perform("sse-disconnect-after-confirmed-write", prompt, stop_after_write=True)
        record["checks"]["disconnectActuallyExercised"] = record.get("disconnectedAfterWrite") is True
        mutations = [operation for operation in record["detail"].get("operations", []) if operation.get("intent", {}).get("toolName") == "filesystem.write"]
        record["checks"]["oneSuccessfulWriteOnly"] = len(mutations) == 1 and mutations[0]["state"] == "succeeded"
        receipt = self.work / "receipt-once.txt"
        record["checks"]["writtenContentRetained"] = receipt.is_file() and receipt.read_text(encoding="utf-8").strip() == "API_DISCONNECT_RECEIPT_V1"

    def close(self):
        if self.process is not None and self.process.poll() is None:
            for record in self.records:
                input_value = record["input"]
                task_id = record.get("taskId")
                if not task_id:
                    continue
                try:
                    detail = self.detail(task_id, input_value["conversationId"])
                    preserve_pause = (detail["task"]["state"] == "paused"
                                      and (record.get("budgetGuard", {}).get("action") == "pause"
                                           or self.resume_record is not None))
                    if detail["task"]["state"] not in ("completed", "partial", "failed", "cancelled"):
                        if not preserve_pause:
                            http_json(self.url, "POST", "/api/chat/cancel", {
                                "conversationId": input_value["conversationId"], "requestId": input_value["requestId"]})
                        until = time.monotonic() + 15
                        while time.monotonic() < until:
                            detail = self.detail(task_id, input_value["conversationId"])
                            active = [detail.get("attempt")] + [self.detail(child["taskId"], input_value["conversationId"]).get("attempt") for child in detail.get("children", [])]
                            if all(not attempt or attempt.get("endedAtMs") is not None for attempt in active):
                                if preserve_pause:
                                    record["pauseSettled"] = True
                                break
                            time.sleep(0.25)
                    record["detail"] = self.detail(task_id, input_value["conversationId"])
                    if preserve_pause:
                        record["pausedDataRetained"] = True
                except AcceptanceError:
                    record["cleanupMeteringUnknown"] = True
        if self.process is not None and self.process.poll() is None:
            # Only this exact test-owned process tree may be retired; the daily gateway is never targeted.
            # 仅关闭本脚本拥有的精确进程树，不按全局名称停止日常网关。
            subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.report["error"] = "OWNED_PROCESS_EXIT_UNCONFIRMED"
        for stream in (self.stdout, self.stderr):
            if stream is not None:
                stream.close()
        self.profile.unlink(missing_ok=True)
        self.connection.pop("apiKey", None)
        if self.proxy is not None:
            self.proxy.shutdown()
            self.proxy.server_close()
            self.proxy_thread.join(timeout=5)
            self.report["forwarderReceipts"] = self.proxy_receipts
        self.report["elapsedMs"] = int((time.monotonic() - self.started) * 1000)
        metered = [record.get("detail", {}).get("task", {}).get("usage", {}) for record in self.records]
        cumulative = {name: sum(value.get(name, 0) for value in metered)
                      for name in ("modelRequests", "failedRequests", "retries", "compactions",
                                   "inputTokens", "outputTokens", "unknownTokenRequests",
                                   "modelDurationMs", "toolDurationMs")}
        self.report["usage"] = {name: value - self.prior_usage.get(name, 0)
                                for name, value in cumulative.items()}
        if self.previous_report is not None:
            # Usage belongs to this execution segment; the original task ledger remains cumulative.
            # 报告仅计本次执行增量；原任务正式账本保持累计，避免暂停续接被重复计费统计。
            self.report["cumulativeTaskUsage"] = cumulative
        if self.proxy is not None:
            forwarded = sum(receipt["method"] == "POST" and receipt["path"] == "/chat/completions"
                            for receipt in self.proxy_receipts)
            self.report["realApiTransportMetering"] = {
                "forwardedGenerationRequests": forwarded,
                "recordedModelRequests": self.report["usage"]["modelRequests"],
                "matches": forwarded == self.report["usage"]["modelRequests"],
            }
        for record in self.records:
            record["status"] = "passed" if record["checks"] and all(record["checks"].values()) else "failed"
        self.report["passed"] = (bool(self.records) and all(record["status"] == "passed" for record in self.records)
                                 and "error" not in self.report
                                 and self.report.get("realApiTransportMetering", {}).get("matches", True))
        write_json(self.arguments.result_path, self.report)


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle-root", type=Path, required=True)
    parser.add_argument("--gateway", type=Path, help="Fresh independently built gateway; bundle-root remains the asset owner.")
    parser.add_argument("--model-home", type=Path, required=True)
    parser.add_argument("--result-path", type=Path, required=True)
    parser.add_argument("--provider", default="deepseek")
    parser.add_argument("--model", default="deepseek-flash")
    parser.add_argument("--context-tokens", type=int, default=16384)
    parser.add_argument("--output-tokens", type=int, default=4096)
    parser.add_argument("--timeout-seconds", type=int, default=1800)
    parser.add_argument("--max-calls", type=int, default=40)
    parser.add_argument("--max-known-tokens", type=int, default=250000)
    parser.add_argument("--guard-action", choices=["cancel", "pause"], default="cancel",
                        help="Use formal pause at an experiment guard to retain the same resumable task; cancellation remains the compatible default.")
    parser.add_argument("--resume-existing", type=Path,
                        help="Explicitly resume the same formally paused task from its prior isolated acceptance report; never creates a replacement task.")
    parser.add_argument("--scenario", choices=["children", "long", "disconnect"], required=True)
    parser.add_argument("--pressure-proxy", action="store_true", help="Transparent real-API forwarder for bounded context-pressure acceptance only.")
    parser.add_argument("--stop-on-summary-rejection", action="store_true", help="Diagnostic pressure run: cancel on the first real rejected summary and retain its safe response shape.")
    arguments = parser.parse_args()
    arguments.bundle_root = arguments.bundle_root.resolve()
    arguments.model_home = arguments.model_home.resolve()
    arguments.result_path = arguments.result_path.resolve()
    if arguments.gateway is not None:
        arguments.gateway = arguments.gateway.resolve()
    if arguments.resume_existing is not None:
        arguments.resume_existing = arguments.resume_existing.resolve()
        if arguments.scenario != "long" or arguments.stop_on_summary_rejection:
            parser.error("--resume-existing requires the unchanged long scenario without diagnostic cancellation")
    if arguments.pressure_proxy and arguments.scenario != "long":
        parser.error("--pressure-proxy is only valid with --scenario long")
    if arguments.stop_on_summary_rejection and arguments.scenario != "long":
        parser.error("--stop-on-summary-rejection is only valid with --scenario long")
    run = Run(arguments)
    try:
        run.start()
        print(f"API acceptance started: provider={arguments.provider}, model={arguments.model}, scope=isolated", flush=True)
        for name, scenario in (("children", run.scenario_children), ("long", run.scenario_long), ("disconnect", run.scenario_disconnect)):
            if arguments.scenario == name:
                print(f"Running: {name}", flush=True)
                scenario()
                write_json(arguments.result_path, run.report)
                print(f"Finished: {name}, checks={run.records[-1]['checks']}", flush=True)
    except AcceptanceError as error:
        run.report["error"] = str(error)
        print(f"Acceptance blocked: {error}", flush=True)
    except Exception as error:
        run.report["error"] = type(error).__name__
        print(f"Acceptance failed: {type(error).__name__}; private request/configuration omitted", flush=True)
    finally:
        run.close()
    return 0 if run.report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

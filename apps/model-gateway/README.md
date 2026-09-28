# KYNXA model gateway

This local HTTP adapter uses the existing DeepSeek Harness SDK to run configured OpenAI Chat Completions models. It currently supports API-compatible cloud gateways and local Ollama, LM Studio, or llama.cpp servers. It does not download or load GGUF files.

Requirements: Node.js 22.19+ and a built DeepSeek Harness checkout (`pnpm install && pnpm build` there). The gateway starts only on `127.0.0.1:5218`.

```bash
cd apps/model-gateway
KYNXA_DSH_ROOT=/absolute/path/to/deepseek-harness npm start
```

`KYNXA_DSH_HOME` defaults to `~/.kynxa/harness`; this is a separate Harness home owned by KYNXA. `KYNXA_WORKSPACE_ROOT` sets the SDK's working directory. The gateway writes model routes to `settings.yaml` and keys to private `.credentials.yaml` in that home. It never sends a key back in API responses. A local keyless server receives the placeholder key `local` because the underlying OpenAI transport requires an Authorization value.

API:

- `GET /health`
- `GET /api/models` — configured routes and model IDs; no keys
- `POST /api/models` — `{providerId,displayName,baseUrl,apiKey?,models:[id,...]}`
- `POST /api/models/test` — same body; probes `GET {baseUrl}/models` without saving
- `POST /api/chat` — `{conversationId,message,provider,model,permissionMode}`

The model probe tests `/models` only; some compatible endpoints do not provide it. A manually entered Model ID can still be saved. The chat endpoint uses Harness's model adapter and returns a complete response. Streaming and per-request cancellation are not yet exposed by this HTTP adapter.

The SDK patch disables executable tool registrations and plan-mode tooling for this model-call stage. `permissionMode` is retained as UI metadata; it does not grant permissions to the model. Changing a saved route invalidates its SDK instance so the next chat uses the new endpoint and key.

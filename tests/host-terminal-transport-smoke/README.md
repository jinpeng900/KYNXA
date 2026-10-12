# Historical Node transport check / 旧 Node 传输检查

2026-10-12：下文 `transport.test.mjs` 命令调用迁移前 `apps/model-gateway` 传输适配器，属于历史验证，不能用于当前 Rust 网关；尚未完成对应原生夹具迁移。原说明和成绩保留，原生运行时/桌面合同见 [验证参考](../../.agents/skills/kynxa-development/references/validation.md)。

# Host terminal transport smoke

Run from the repository root:

```powershell
node --test tests/host-terminal-transport-smoke/transport.test.mjs
```

This Windows smoke builds a tiny protocol fixture in a unique temporary directory and calls the real Node host-terminal transport. The fixture never executes shell commands; it writes only synthetic files in its assigned temporary directory. It tests live and fragmented output, loss of a completion receipt before a started notification, known refusal, per-frame output limits, and stopping on a failed display callback.

It does not verify native console visibility or TTY behavior. Those checks belong to `tests/native-host-terminal-smoke/visible-terminal.test.mjs`.

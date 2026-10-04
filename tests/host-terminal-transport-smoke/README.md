# Host terminal transport smoke

Run from the repository root:

```powershell
node --test tests/host-terminal-transport-smoke/transport.test.mjs
```

This Windows smoke builds a tiny protocol fixture in a unique temporary directory and calls the real Node host-terminal transport. The fixture never executes shell commands; it writes only synthetic files in its assigned temporary directory. It tests live and fragmented output, loss of a completion receipt before a started notification, known refusal, per-frame output limits, and stopping on a failed display callback.

It does not verify native console visibility or TTY behavior. Those checks belong to `tests/native-host-terminal-smoke/visible-terminal.test.mjs`.

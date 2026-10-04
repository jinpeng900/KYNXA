import { createServer } from 'node:http';
import { appendFileSync } from 'node:fs';
appendFileSync(process.env.KYNXA_STARTUP_TEST_LOG, `${process.pid}\n`);
createServer((req, res) => {
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify({ status: 'ok', service: 'kynxa-model-gateway', conversationProtocol: 1, dataLayoutVersion: 1,
    memoryProtocol: 1, contextProtocol: 3, agentProtocol: 5, officialToolsProtocol: 2, hostTerminalProtocol: 3, browserAutomationProtocol: 2, extensionStorageProtocol: 1, toolStreamProtocol: 3 }));
}).listen(Number(process.env.KYNXA_MODEL_API_PORT), '127.0.0.1');

// Test-only agent: the official ACP SDK owns framing, validation and request IDs.
// No provider, model, session creation or action is implemented by this fixture.
import { appendFileSync } from 'node:fs';
import { Readable, Writable } from 'node:stream';
import * as acp from '@agentclientprotocol/sdk';

const record = (method) => appendFileSync(process.env.NATIVE_FIXTURE_LOG,
  `${JSON.stringify({method, pid: process.pid, node: process.execPath,
    provider_key_present: Object.hasOwn(process.env, 'OPENAI_API_KEY')})}\n`);
const agent = acp.agent({name: 'native-package-fixture'})
  .onRequest(acp.methods.agent.initialize, () => {
    record('initialize');
    return {protocolVersion: acp.PROTOCOL_VERSION,
      agentInfo: {name: 'native-package-fixture', version: '1'},
      agentCapabilities: {loadSession: false, mcpCapabilities: {http: true}},
      authMethods: []};
  })
  .onRequest('*', ({method}) => {
    record(method ?? 'unexpected');
    throw new Error('The installation fixture permits initialize only');
  });
const connection = agent.connect(acp.ndJsonStream(
  Writable.toWeb(process.stdout), Readable.toWeb(process.stdin)));
await connection.closed;

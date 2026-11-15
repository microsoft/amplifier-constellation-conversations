// Owned test listener using the actual installed MCP SDK and product bridge.
// No voice/provider client is imported. Rust owns the disposable host and child.
import {createInterface} from 'node:readline';
import {startCompanionMcp} from '../bridge/companion-mcp.mjs';

const input=createInterface({input:process.stdin,crlfDelay:Infinity});
let bridge;
try {
  for await (const line of input) {
    const value=JSON.parse(line);
    if(!bridge) {
      bridge=await startCompanionMcp(value);
      process.stdout.write(JSON.stringify({port:bridge.port})+'\n');
    } else {
      if(value.op!=='close')throw new Error('unexpected_test_control');
      break;
    }
  }
} finally {
  input.close();
  await bridge?.close();
}

// Fixed read-only Minecraft presence client. The official SDK owns MCP transport.
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StreamableHTTPClientTransport} from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import {fileURLToPath} from 'node:url';
import {realpathSync} from 'node:fs';
import {z} from 'zod/v4';

const MAX_BYTES = 524288;
const settings = z.object({
  port:z.number().int().min(1).max(65535),
  token:z.string().regex(/^[\x21-\x7e]{1,8192}$/),
  username:z.string().min(1).refine(value=>Buffer.byteLength(value)<=160&&!/[\x00-\x1f]/.test(value)),
  timeout_ms:z.number().int().min(1).max(8000),
}).strict();

/** @param {unknown} value @returns {value is Record<string, unknown>} */
function object(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** @param {unknown} result @returns {Record<string, unknown>} */
function resultObject(result) {
  if (!object(result) || result.isError) throw new Error('presence_tool_failed');
  const content = result.content;
  const value = object(result.structuredContent) ? result.structuredContent :
    Array.isArray(content) && content.length === 1 && object(content[0]) && content[0].type === 'text' &&
    typeof content[0].text === 'string' ? JSON.parse(content[0].text) : null;
  if (!object(value) || Buffer.byteLength(JSON.stringify(value)) > MAX_BYTES) throw new Error('invalid_presence_result');
  return value;
}

/** @param {unknown} value */
export async function observePresence(value) {
  const input = settings.parse(value);
  const url = new URL(`http://127.0.0.1:${input.port}/presence/mcp`);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), input.timeout_ms);
  const client = new Client({name:'constellation-companion-presence',version:'1'});
  // No redirects or caller-provided URLs, tools, headers, or credentials in argv.
  const transport = new StreamableHTTPClientTransport(url, {
    requestInit: {headers: {Authorization: `Bearer ${input.token}`}, redirect:'error'},
    fetch: async (target, options) => {
      if (String(target) !== url.href) throw new Error('presence_target_changed');
      const response = await fetch(target, {...options, redirect:'error', signal:controller.signal});
      if (!response.body) return response;
      let bytes = 0;
      const bounded = response.body.pipeThrough(new TransformStream({
        transform(chunk, stream) {
          bytes += chunk.byteLength;
          if (bytes > MAX_BYTES) throw new Error('presence_response_limit');
          stream.enqueue(chunk);
        },
      }));
      return new Response(bounded, {status:response.status,statusText:response.statusText,headers:response.headers});
    },
  });
  try {
    await client.connect(transport, {signal:controller.signal,timeout:input.timeout_ms});
    const [status, activity] = await Promise.all([
      client.callTool({name:'minecraft_status',arguments:{}}, undefined, {signal:controller.signal,timeout:input.timeout_ms}),
      client.callTool({name:'minecraft_recent_activity',arguments:{limit:20}}, undefined, {signal:controller.signal,timeout:input.timeout_ms}),
    ]);
    const result = {status:resultObject(status),activity:resultObject(activity)};
    if (result.status.username !== input.username || result.activity.username !== input.username ||
        Buffer.byteLength(JSON.stringify(result)) > MAX_BYTES) throw new Error('presence_identity_mismatch');
    return result;
  } finally {
    clearTimeout(timer);
    controller.abort();
    await client.close();
  }
}

// Match Node's canonical module URL even through a versioned install symlink.
let invokedAsEntry=false;
if(process.argv[1]){
  try{invokedAsEntry=realpathSync(fileURLToPath(import.meta.url))===realpathSync(process.argv[1]);}catch{}
}
if(invokedAsEntry) {
  try {
    let raw = '';
    for await (const chunk of process.stdin) {
      raw += chunk;
      if (Buffer.byteLength(raw) > 16384) throw new Error('input_limit');
    }
    const result = await observePresence(JSON.parse(raw));
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch {
    // Provider errors and credentials must not escape into the call transcript.
    process.stdout.write('{"error":"presence_unavailable"}\n');
    process.exitCode = 1;
  }
}

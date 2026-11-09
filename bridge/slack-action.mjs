/** Fixed installation-owned Slack delivery. No model-selected destination or token. */
import {readFile} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {realpathSync} from 'node:fs';
import {WebClient,LogLevel} from '@slack/web-api';
import {z} from 'zod/v4';

const MAX_INPUT=65536;
const configSchema=z.object({
  slack_home:z.string().refine(isAbsolute),
  channel_id:z.string().regex(/^C[A-Z0-9]{8,32}$/),
  expected_bot_user_id:z.string().regex(/^U[A-Z0-9]{8,32}$/),
}).strict();
const inputSchema=z.object({version:z.literal(1),request_id:z.string().uuid(),
  instruction:z.string().min(1).refine(s=>s.trim().length>0&&!s.includes('\0')&&Buffer.byteLength(s)<=8000),
  source:z.object({actor:z.string().trim().min(1).max(200)}).passthrough(),
}).strict();
const silentLogger={debug(){},info(){},warn(){},error(){},setName(){},setLevel(){},getLevel(){return LogLevel.ERROR;}};

/** The official SDK owns request formatting; v8 rejects HTTP redirects.
 * The optional fetch implementation exists only for disposable transport tests.
 * @param {string} token @param {import('@slack/web-api').FetchFunction} [fetchImpl]
 */
export function makeSlackClient(token,fetchImpl) {
  return new WebClient(token,{retryConfig:{retries:0},rejectRateLimitedCalls:true,
    allowAbsoluteUrls:false,maxRequestConcurrency:1,timeout:10000,logger:silentLogger,
    ...(fetchImpl?{fetch:fetchImpl}:{}),
  });
}

/** @param {string} path */
async function readJson(path) {
  const bytes=await readFile(path);
  if(bytes.length>32768)throw new Error('oversized_configuration');
  return JSON.parse(bytes.toString('utf8'));
}
/** @param {unknown} value @returns {Record<string,unknown>} */
function object(value) {return value&&typeof value==='object'&&!Array.isArray(value)?/** @type {Record<string,unknown>} */(value):{};}

/** The parent keeps the immutable original-human source and durable action ledger.
 * This helper never retries and never equates delivery with completion of a task.
 * @param {{configPath:string,input:unknown}} options
 * @param {(token:string)=>ReturnType<typeof makeSlackClient>} [createClient]
 */
export async function executeSlackAction({configPath,input},createClient=makeSlackClient) {
  const rawId=object(input).request_id;
  const request_id=z.string().uuid().safeParse(rawId).success?rawId:null;
  /** @param {string} error */
  const failed=error=>({request_id,state:'failed',error});
  let request,config,client;
  try {
    if(!isAbsolute(configPath)||Buffer.byteLength(JSON.stringify(input))>MAX_INPUT)return failed('invalid_request');
    request=inputSchema.parse(input);config=configSchema.parse(await readJson(configPath));
    const saved=object(await readJson(join(config.slack_home,'secrets','slack.json')));
    const token=saved.bot_token;
    if(typeof token!=='string'||!token.startsWith('xoxb-')||token.length>8192||/\s/.test(token))return failed('slack_credentials_unavailable');
    client=createClient(token);
    const identity=await client.auth.test();
    if(identity.ok!==true||identity.user_id!==config.expected_bot_user_id)return failed('slack_identity_mismatch');
  } catch {return failed('slack_preflight_failed');}
  try {
    // Extra client_msg_id is passed through the SDK even though its published
    // chat.postMessage argument type does not currently enumerate this field.
    const args={channel:config.channel_id,text:request.instruction,client_msg_id:request.request_id,
      unfurl_links:false,unfurl_media:false};
    const result=await client.chat.postMessage(args);
    if(result.ok!==true||result.channel!==config.channel_id||typeof result.ts!=='string'||!/^\d{10,16}\.\d{1,12}$/.test(result.ts)||
        (result.message?.user&&result.message.user!==config.expected_bot_user_id))throw new Error('unconfirmed_receipt');
    return {request_id:request.request_id,state:'completed',scope:'message_delivery',receipt:{channel:result.channel,ts:result.ts}};
  } catch {
    return {request_id:request.request_id,state:'unknown',scope:'message_delivery',error:'slack_delivery_unconfirmed'};
  }
}

async function main() {
  /** @type {unknown} */ let input;
  try {
    if(process.argv.length!==4||process.argv[2]!=='--config')throw new Error('invalid_arguments');
    let bytes=0;const chunks=[];
    for await(const chunk of process.stdin){bytes+=chunk.length;if(bytes>MAX_INPUT)throw new Error('input_limit');chunks.push(chunk);}
    input=JSON.parse(Buffer.concat(chunks).toString('utf8'));
    process.stdout.write(JSON.stringify(await executeSlackAction({configPath:process.argv[3],input}))+'\n');
  } catch {
    const candidate=object(input).request_id;
    process.stdout.write(JSON.stringify({request_id:z.string().uuid().safeParse(candidate).success?candidate:null,state:'failed',error:'invalid_request'})+'\n');
  }
}
// Match Node's canonical module URL even through a versioned install symlink.
let invokedAsEntry=false;
if(process.argv[1]){
  try{invokedAsEntry=realpathSync(fileURLToPath(import.meta.url))===realpathSync(process.argv[1]);}catch{}
}
if(invokedAsEntry) await main();

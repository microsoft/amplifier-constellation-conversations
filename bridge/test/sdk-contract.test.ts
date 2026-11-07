// Compile-only contracts use the official generated SDK types, not local copies.
import type {LiveCreateParams} from 'openai/resources/live/live';
import type {ConnectClientEvent} from 'openai/resources/live/sideband/sideband';
import {sessionOptions} from '../live.mjs';

const valid: LiveCreateParams = sessionOptions({sdp:'v=0',voice:'marin',instructions:'Coach'});
void valid;
const invalidPermissions: LiveCreateParams = {
  transport:{type:'webrtc',sdp:'v=0'},
  session:{model:'gpt-live-1',client:{data_channel:{
    // This exact mistake reached the API once; an SDK upgrade must keep rejecting it.
    // @ts-expect-error SDK requires selector objects, not event-name strings.
    allowed_server_events:['session.started'],
  }}},
};
void invalidPermissions;
// @ts-expect-error Context commands require explicit nullable delegation_id.
const invalidAppend: ConnectClientEvent = {type:'session.commentary.append',content:'Ready'};
void invalidAppend;

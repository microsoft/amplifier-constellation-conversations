// Updates and controls keep their exact saved identity across reconnection.
// This page never opens media, starts agent work, or replays a lost POST.
const $ = id => document.getElementById(id);
const root = '/api/v1/companion/update';
const pendingKey = 'companion-native-update-request', updateKey = 'companion-native-update', recoveryKey = 'companion-native-update-recovery', closeKey = 'companion-native-update-close';
const uuid = value => typeof value === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-[1-8][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(value);
const offered = new URL(location.href).searchParams.get('offer');
const offerId = uuid(offered) ? offered : null;
let session = null, status = null, busy = false, generation = 0, poll = null, pending = null, updateId = null, updateOperationId = null, recoveryAction = null, closeRequest = null, closing = false, connection = '';
try {
  const savedClose = JSON.parse(sessionStorage.getItem(closeKey));
  if (savedClose && /^[a-f0-9]{64}$/i.test(savedClose.nonce) && savedClose.origin === location.origin && savedClose.offer_id === offerId) closeRequest = savedClose;
  const raw = sessionStorage.getItem(updateKey), savedUpdate = uuid(raw) ? {id:raw} : JSON.parse(raw);
  if (uuid(savedUpdate?.id)) { updateId = savedUpdate.id; if (uuid(savedUpdate.operation_id)) updateOperationId = savedUpdate.operation_id; }
  const savedRecovery = JSON.parse(sessionStorage.getItem(recoveryKey));
  if (savedRecovery && uuid(savedRecovery.request_id) && ['resume','rollback'].includes(savedRecovery.action) && uuid(savedRecovery.target_operation_id))
    recoveryAction = {request_id:savedRecovery.request_id,action:savedRecovery.action,target_operation_id:savedRecovery.target_operation_id,...(uuid(savedRecovery.operation_id) ? {operation_id:savedRecovery.operation_id} : {})};
  const saved = JSON.parse(sessionStorage.getItem(pendingKey));
  if (saved && uuid(saved.request_id)) {
    if (saved.action === 'update') pending = {request_id:saved.request_id,action:'update'};
    else if (['cancel','continue'].includes(saved.action) && uuid(saved.control_id)) pending = {request_id:saved.request_id,control_id:saved.control_id,action:saved.action};
    else if (['resume','rollback'].includes(saved.action) && uuid(saved.operation_id)) pending = {request_id:saved.request_id,operation_id:saved.operation_id,action:saved.action};
    else if (saved.action === 'apply' && uuid(saved.check_id)) pending = {request_id:saved.request_id,check_id:saved.check_id,action:'apply'};
  }
  if (!recoveryAction && pending && ['resume','rollback'].includes(pending.action))
    recoveryAction = {request_id:pending.request_id,action:pending.action,target_operation_id:pending.operation_id};
} catch { /* Server receipts remain authoritative if storage cannot be read. */ }
function store(key,value,required=false) {
  try {
    if (value === null) sessionStorage.removeItem(key); else sessionStorage.setItem(key,value);
    if (required && sessionStorage.getItem(key) !== value) throw Error('Reference not retained');
  } catch (e) { if (required) throw Error('This browser could not save the update reference. Allow storage for Companion before starting an update.'); }
}
function remember(value,required=false) { store(pendingKey,value ? JSON.stringify(value) : null,required); pending = value; }
function rememberRecovery(value,required=false) { store(recoveryKey,value ? JSON.stringify(value) : null,required); recoveryAction = value; }
function track(id,required=false,operationId) {
  const child = uuid(operationId) ? operationId : id === updateId ? updateOperationId : null;
  store(updateKey,id ? JSON.stringify({id,...(child ? {operation_id:child} : {})}) : null,required);
  updateId = id; updateOperationId = child;
}
function error(message) { $('update-error').textContent = message || ''; $('update-error').hidden = !message; }
async function api(path, method = 'GET', body) {
  const headers = body === undefined ? {} : {'Content-Type':'application/json'};
  if (method !== 'GET' && session?.csrf) headers['X-CSRF-Token'] = session.csrf;
  const abort = new AbortController();
  const timeout = setTimeout(() => abort.abort(), method === 'GET' ? 10000 : 40000);
  try {
    const response = await fetch(path, {method,headers,credentials:'same-origin',cache:'no-store',signal:abort.signal,
      body:body === undefined ? undefined : JSON.stringify(body)});
    let value;
    try { value = await response.json(); } catch { const e = new Error('Companion returned an unreadable reply.'); e.uncertain = method !== 'GET'; throw e; }
    if (!response.ok) {
      const e = new Error(typeof value.error === 'string' ? value.error : 'Companion could not complete the request.');
      e.status = response.status; e.uncertain = response.status >= 500; throw e;
    }
    return value;
  } catch (e) {
    if (method !== 'GET' && e.uncertain === undefined) e.uncertain = true;
    throw e;
  } finally { clearTimeout(timeout); }
}
const phases = {
  accepted:['Update requested','Checking that Companion can safely stop.'],
  running:['Update in progress','Keep this page open. Companion may be briefly unavailable.'],
  intent:['Preparing the update','Checking active calls and accepted work.'],
  preparing:['Preparing the update','Waiting for a safe point to continue.'],
  prepared:['Ready to update','The installation is ready for the package change.'],
  switching:['Installing the package','Companion will return after the package is verified.'],
  applied:['Checking the installed package','Verifying the app before reopening it.'],
  verifying:['Checking the installed package','Verifying the app before reopening it.'],
  verified:['Reopening Companion','Restoring this installation’s previous availability.'],
  restoring:['Reopening Companion','Restoring this installation’s previous availability.'],
  deferred:['Update deferred','Companion is still busy. Your current work stays in place.'],
  unknown:['Update needs attention','The result is not confirmed. Check the saved operation before continuing.'],
  failed:['Update needs attention','Use the available recovery action to continue from the saved state.'],
};
function progress(operation) {
  if (operation.phase === 'failed' && operation.outcome === 'rolled_back')
    return ['Update wasn’t installed','The previous version was restored. You can keep using Companion.'];
  if (operation.phase === 'failed' && operation.outcome === 'unchanged')
    return ['App version unchanged','The app package was left unchanged. Check its status before trying another update.'];
  if (['completed','restored'].includes(operation.phase)) {
    if (operation.outcome === 'rolled_back') return ['Previous version restored','Companion is available with the previous app package.'];
    if (operation.outcome === 'unchanged') return ['App version unchanged','The installation’s previous availability has been restored.'];
    if (operation.outcome === 'applied') return ['Update complete','Companion is ready.'];
    return ['Operation finished','Check the installed version below.'];
  }
  return phases[operation.phase] || ['Checking the update','Reading the installation’s saved progress.'];
}
const updateStates = {
  accepted:['Update requested','Companion is preparing your update. You can leave this page and return to check its progress.'],
  preparing:['Preparing the update','Checking the complete package before anything is installed.'],
  waiting:['Waiting for current activity','The update will continue when active calls and work have finished.'],
  needs_attention:['Update needs attention','Review the saved update before continuing.'],
  installing:['Installing the update','Companion may be briefly unavailable while it installs and verifies the package.'],
  completed:['Update complete','Companion is ready.'],
  failed:['Update needs attention','The update did not finish. Review the available recovery controls.'],
  cancelled:['Update cancelled','The update request was cancelled.'],
  interrupted:['Update interrupted','Review the saved progress before continuing.'],
  unknown:['Update result unconfirmed','Companion has not confirmed the outcome. This page will keep checking the saved request.'],
};
const terminal = update => update && ['completed','cancelled'].includes(update.state);
const canReplace = update => !update || terminal(update) || update.state === 'failed' && status?.actions?.request === true;
function selectedUpdate() { return !recoveryAction && status?.update && (!updateId || status.update.id === updateId) ? status.update : null; }
function selectedOperation() {
  const operation = status?.operation;
  if (recoveryAction) return operation && (operation.id === recoveryAction.operation_id || operation.request_id === recoveryAction.request_id) ? operation : null;
  if (status && Object.hasOwn(status,'update') && updateId && status.update?.id !== updateId) return null;
  // An older restored app has no parent-flow schema. Its current operation
  // only speaks for this Update when its exact child ID was already received.
  if (status && !Object.hasOwn(status,'update') && updateId && operation?.id !== updateOperationId) return null;
  return operation;
}
function referenceUnconfirmed() {
  if (!status) return false;
  if (recoveryAction) return !selectedOperation();
  return Boolean(updateId && (Object.hasOwn(status,'update') ? status.update?.id !== updateId : !selectedOperation()));
}
function operationSettled(operation) {
  return operation && ['completed','restored','failed'].includes(operation.phase) && ['applied','rolled_back','unchanged'].includes(operation.outcome);
}
function settledLegacyChild() {
  const operation = selectedOperation();
  return status && !Object.hasOwn(status,'update') && updateId && operation?.id === updateOperationId &&
    operationSettled(operation);
}
function ongoing() {
  const update = selectedUpdate(), operation = selectedOperation();
  return Boolean(pending || referenceUnconfirmed() || (update && !terminal(update) && update.state !== 'failed') ||
    (operation && !['completed','restored','deferred','failed'].includes(operation.phase)));
}
function schedule() {
  clearTimeout(poll);
  if (session?.authenticated && !closing && !document.hidden && (ongoing() || connection)) poll = setTimeout(() => refresh(), 3000);
}
function applyStatus(next) {
  // Shared mutation replies need not repeat the listener-only decoration.
  // Keep the already-confirmed restricted origin restrictive between GETs.
  if (status?.assistant === true && !Object.hasOwn(next,'assistant'))
    next = {...next,assistant:true,companion_url:status.companion_url,actions:{...next.actions,advanced_package:false}};
  status = next;
  if (recoveryAction && next.operation?.request_id === recoveryAction.request_id && uuid(next.operation.id))
    rememberRecovery({...recoveryAction,operation_id:next.operation.id});
  if (!updateId && uuid(next.update?.id)) track(next.update.id);
  if (next.update?.id === updateId && uuid(next.update.operation_id)) track(updateId,false,next.update.operation_id);
  if (!pending) return;
  if (['cancel','continue'].includes(pending.action)) {
    if (next.control?.id === pending.control_id && next.control.action === pending.action && next.control.status === 'recorded') remember(null);
  } else if (pending.action === 'update') {
    if (next.update?.id === pending.request_id || pending.request_id === updateId && settledLegacyChild()) remember(null);
  } else if (next.operation?.request_id === pending.request_id || next.request?.id === pending.request_id && next.request.state === 'recorded') {
    remember(null);
  }
}
function render() {
  const admin = session?.authenticated === true && session.admin === true;
  $('updates-loading').hidden = true;
  $('updates-content').hidden = !admin;
  $('update-sign-out').hidden = !session?.authenticated;
  if (!session?.authenticated) return;
  $('update-login').hidden = true;
  if (!admin) { error('App updates are available to this installation’s administrator.'); return; }
  if (!status) return;
  $('update-installed').textContent = status.current?.version ? `Installed version: ${status.current.version}` : 'Installed version is not available.';
  $('update-owner').hidden = !status.reason && status.supported === true && status.owner === 'standalone';
  $('update-owner').textContent = status.reason || (status.owner === 'enrolled'
    ? 'Updates are managed by this installation’s existing updater. Use its update controls.'
    : 'App updates are not available for this installation.');
  if (status.assistant === true && closeRequest) closing = true;
  const recovery = status.mode === 'recovery', operation = selectedOperation(), update = selectedUpdate();
  const writable = status.supported === true && status.owner === 'standalone', actions = status.actions || {};
  const background = Object.hasOwn(status,'update'), blocked = busy || closing || Boolean(pending);
  const canStart = writable && background && !recovery && !referenceUnconfirmed() && (!recoveryAction || operationSettled(operation)) && canReplace(update) && actions.request !== false;
  $('return-to-companion').hidden = recovery;
  $('return-to-companion').href = returnDestination() || '/companion';
  const offer = status.offer && uuid(status.offer.id) && (!offerId || status.offer.id === offerId) ? status.offer : null;
  $('update-offer').hidden = !writable || recovery || !offer || !canReplace(update);
  $('update-offer-title').textContent = offer?.version ? `Update to version ${offer.version}` : 'Update Companion';
  $('update-start').disabled = blocked || !canStart;
  $('update-empty').hidden = !writable || recovery || Boolean(offer) || Boolean(update) || Boolean(pending) || referenceUnconfirmed() || Boolean(operation);
  $('update-empty').textContent = offered && !offer ? 'This downloaded release is not available to this installation. Open the release again to offer it.' : 'To update, open the Companion release you downloaded on this computer.';
  $('update-progress').hidden = !update && !operation && !pending && !recovery && !referenceUnconfirmed();
  if (update) {
    const [title,detail] = updateStates[update.state] || ['Reading update progress','Checking the saved update state.'];
    const phase = update.state === 'installing' ? phases[update.phase] : null;
    $('update-progress-title').textContent = phase?.[0] || title;
    $('update-progress-detail').textContent = typeof update.message === 'string' && update.message ? update.message : phase?.[1] || detail;
    $('update-reference-value').textContent = update.id;
    $('update-reference').hidden = false;
  } else if (pending) {
    $('update-progress-title').textContent = 'Checking your update request';
    $('update-progress-detail').textContent = 'The reply is unconfirmed. Reading the original saved request without starting another update.';
    $('update-reference-value').textContent = pending.request_id;
    $('update-reference').hidden = false;
  } else if (referenceUnconfirmed()) {
    $('update-progress-title').textContent = 'Update result unconfirmed';
    $('update-progress-detail').textContent = 'This app version has not confirmed the saved update operation. No new request will be sent.';
    $('update-reference-value').textContent = recoveryAction?.request_id || updateId;
    $('update-reference').hidden = false;
  } else if (operation) {
    const [title,detail] = progress(operation);
    $('update-progress-title').textContent = title; $('update-progress-detail').textContent = detail;
    $('update-reference-value').textContent = operation.transaction_id || operation.id || '';
    $('update-reference').hidden = !operation.id;
  } else if (recovery) {
    $('update-progress-title').textContent = 'Companion needs recovery';
    $('update-progress-detail').textContent = 'Ordinary calls and work remain stopped while this installation is checked.';
    $('update-reference').hidden = true;
  }
  $('update-request-status').hidden = !pending;
  $('update-request-status').textContent = pending ? (pending.control_id
    ? `The ${pending.action === 'cancel' ? 'Cancel' : 'Continue'} reply is unconfirmed. Checking its saved receipt; it will not be sent again.`
    : 'Checking the original request. It will not be sent again automatically.') : '';
  $('update-connection').textContent = connection; $('update-connection').hidden = !connection;
  $('update-cancel').hidden = !writable || update?.can_cancel !== true;
  $('update-continue').hidden = !writable || update?.can_continue !== true;
  $('update-cancel').disabled = blocked; $('update-continue').disabled = blocked;
  $('update-advanced').hidden = !writable;
  if (recovery) $('update-advanced').open = true;
  $('update-recovery-status').hidden = !operation || !update && !pending;
  $('update-recovery-status').textContent = operation ? progress(operation).join('. ') : '';
  $('update-package-form').hidden = recovery || !background || actions.advanced_package === false;
  $('update-package-path').disabled = blocked || !canStart;
  $('update-package-submit').disabled = blocked || !canStart;
  $('update-resume').hidden = !writable || actions.recover !== true;
  $('update-resume').disabled = blocked || !operation?.id;
  $('update-rollback').hidden = !writable || actions.rollback !== true;
  $('update-rollback').disabled = blocked || !operation?.id;
  $('update-refresh').disabled = busy || closing;
  $('update-confirm-rollback').disabled = blocked || actions.rollback !== true;
  if (!writable || actions.rollback !== true || blocked) $('update-rollback-confirmation').hidden = true;
  schedule();
}
function signedOut() {
  generation++; clearTimeout(poll); status = null; busy = false; connection = '';
  $('updates-loading').hidden = true; $('updates-content').hidden = true; $('update-sign-out').hidden = true;
  $('update-login').hidden = Boolean(session?.first_account_needed);
  const local = session?.auth_mode === 'local_password';
  $('update-username-label').hidden = local; $('update-username').hidden = local;
  $('update-username').value = session?.owner || ''; $('update-password').value = '';
  $('update-login-description').textContent = local ? 'Use the password you chose for Companion.' : 'Use your account on this service’s host.';
  $('return-to-companion').hidden = !session?.first_account_needed;
  if (session?.first_account_needed) error('Open Companion on this computer to finish setting up your account.');
}
async function refresh() {
  if (busy || closing || !session?.authenticated) return;
  const expected = generation; busy = true; render();
  try {
    const query = new URLSearchParams(), id = pending?.request_id || recoveryAction?.request_id || updateId;
    if (id) query.set('request_id',id);
    if (pending?.control_id) query.set('control_id',pending.control_id);
    if (offerId) query.set('offer_id',offerId);
    const next = await api(root+(query.size ? '?'+query : ''));
    if (expected !== generation) return;
    applyStatus(next); connection = ''; error('');
  } catch (e) {
    if (expected !== generation) return;
    if (e.status === 401) { session = {...session,authenticated:false}; signedOut(); return; }
    if (e.status === 404) status = {supported:false,reason:'This installation does not expose native update controls.'};
    else if (ongoing()) connection = 'Reconnecting to Companion. The saved update may still be running. This page will keep checking; no request will be sent again.';
    else error(e.message);
  } finally { if (expected === generation) { busy = false; render(); schedule(); } }
}
async function dispatch(request,path,body) {
  if (busy || closing || pending || !session?.admin || status?.owner !== 'standalone' || status.supported !== true) return;
  try {
    remember(request,true);
    if (request.action === 'update') { track(request.request_id,true); rememberRecovery(null,true); }
    else if (['resume','rollback'].includes(request.action)) rememberRecovery({request_id:request.request_id,action:request.action,target_operation_id:request.operation_id},true);
  } catch (e) { remember(null); error(e.message); return; }
  busy = true; error(''); render(); const expected = generation;
  try {
    const next = await api(root+path,'POST',body);
    if (expected !== generation) return;
    applyStatus(next);
  } catch (e) {
    if (expected !== generation) return;
    if (!e.uncertain) { remember(null); if (request.action === 'update') track(null); else if (['resume','rollback'].includes(request.action)) rememberRecovery(null); }
    if (e.status === 401) { session = {...session,authenticated:false}; signedOut(); error('Sign in to check the saved update request.'); return; }
    error(e.uncertain ? 'The request may have been accepted. Checking its saved state without sending it again.' : e.message);
  } finally {
    if (expected === generation) { busy = false; render(); if (pending) await refresh(); }
  }
}
function requestUpdate(packagePath) {
  const update = selectedUpdate();
  if (busy || pending || !status || !Object.hasOwn(status,'update') || referenceUnconfirmed() || status.mode === 'recovery' || !canReplace(update) || status.actions?.request === false || recoveryAction && !operationSettled(selectedOperation())) return;
  if (packagePath !== undefined && status.actions?.advanced_package === false) return;
  const id = crypto.randomUUID();
  const body = packagePath === undefined ? {request_id:id,offer_id:status?.offer?.id} : {request_id:id,package_path:packagePath};
  if (packagePath === undefined && (!uuid(body.offer_id) || offerId && body.offer_id !== offerId)) return;
  return dispatch({request_id:id,action:'update'},'/request',body);
}
function controlUpdate(action) {
  const update = selectedUpdate();
  if (!uuid(update?.id) || update[action === 'cancel' ? 'can_cancel' : 'can_continue'] !== true) return;
  const request = {request_id:update.id,control_id:crypto.randomUUID(),action};
  return dispatch(request,'/'+action,{request_id:request.request_id,control_id:request.control_id});
}
function recover(action) {
  const operation = selectedOperation();
  if (!uuid(operation?.id) || status.actions?.[action === 'resume' ? 'recover' : 'rollback'] !== true) return;
  const request = {request_id:crypto.randomUUID(),operation_id:operation.id,action};
  return dispatch(request,'/recover',request);
}
function returnDestination() {
  if (status?.assistant !== true) return '/companion';
  try {
    const url = new URL(status.companion_url);
    if (url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port && url.pathname === '/companion' && !url.username && !url.password && !url.search && !url.hash) return url.href;
  } catch { /* Only the installation's fixed local return destination is used. */ }
  return null;
}
$('return-to-companion').onclick = async event => {
  if (status?.assistant !== true) return;
  event?.preventDefault();
  const destination = returnDestination();
  if (!destination) { error('The installed Companion address is not confirmed. Refresh status before returning.'); return; }
  if (closing || busy || pending || ongoing()) { location.assign(destination); return; }
  busy = true; render();
  try {
    const value = await api(root+'/assistant-close');
    if (value.can_close !== true) { location.assign(destination); return; }
    const nonce = value.identity?.nonce;
    if (typeof nonce !== 'string' || !/^[a-f0-9]{64}$/i.test(nonce) || String(value.identity.port) !== new URL(location.href).port)
      throw Error('The update window identity changed. Its close has not been requested.');
    const intent = {origin:location.origin,offer_id:offerId,nonce};
    store(closeKey,JSON.stringify(intent),true); closeRequest = intent; closing = true; clearTimeout(poll);
    const reply = await api(root+'/assistant-close','POST',{listener_nonce:nonce});
    if (reply.closing !== true || reply.listener_nonce !== nonce) throw Error('The update window close is not confirmed.');
    location.assign(destination);
  } catch (e) {
    if (closing && e.uncertain === false) { store(closeKey,null); closeRequest = null; closing = false; }
    error(closing ? 'The update window close is unconfirmed and will not be requested again. You can return to Companion.' : e.message);
  } finally { busy = false; render(); }
};
$('update-start').onclick = () => requestUpdate();
$('update-package-form').onsubmit = event => { event.preventDefault(); const path=$('update-package-path').value.trim(); if (path) return requestUpdate(path); };
$('update-cancel').onclick = () => controlUpdate('cancel');
$('update-continue').onclick = () => controlUpdate('continue');
$('update-refresh').onclick = () => refresh();
$('update-resume').onclick = () => recover('resume');
$('update-rollback').onclick = () => { $('update-rollback-confirmation').hidden = false; $('update-confirm-rollback').focus(); };
$('update-cancel-rollback').onclick = () => { $('update-rollback-confirmation').hidden = true; $('update-rollback').focus(); };
$('update-rollback-confirmation').onsubmit = event => { event.preventDefault(); return recover('rollback'); };
$('update-login').onsubmit = async event => {
  event.preventDefault(); if (busy) return;
  const expected = generation; busy = true; $('update-sign-in').disabled = true; error('');
  try {
    const next = await api('/auth/login','POST',{username:$('update-username').value,password:$('update-password').value});
    if (expected === generation) { session = next; busy = false; await refresh(); }
  } catch (e) { if (expected === generation) error(e.message); }
  finally { $('update-password').value = ''; $('update-sign-in').disabled = false; if (expected === generation) busy = false; }
};
$('update-sign-out').onclick = async () => {
  if (busy) return;
  const expected = generation; busy = true;
  try {
    await api('/auth/logout','POST',{});
    if (expected !== generation) return;
    // The non-secret request identity survives sign-out: an unknown POST may
    // still be admitted later. A later sign-in must reconcile that same UUID.
    session = {owner:session?.owner,auth_mode:session?.auth_mode,authenticated:false}; signedOut(); error('');
  } catch (e) { if (expected === generation) error('Sign-out could not be confirmed. Check the connection before leaving this shared device.'); }
  finally { if (expected === generation) { busy = false; render(); } }
};
document.addEventListener('visibilitychange', () => { clearTimeout(poll); if (!document.hidden) refresh(); });
addEventListener('pagehide', () => clearTimeout(poll));
try {
  session = await api('/auth/session');
  if (session.authenticated) await refresh(); else signedOut();
} catch (e) { signedOut(); error(e.message); }

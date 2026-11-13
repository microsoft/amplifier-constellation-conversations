// This page never opens media or starts agent work. The installation owns all
// plans and transaction state; a lost browser reply is not a second operation.
const $ = id => document.getElementById(id);
const root = '/api/v1/companion/update';
const pendingKey = 'companion-native-update-request';
const uuid = value => typeof value === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-[1-8][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(value);
let session = null, status = null, busy = false, generation = 0, poll = null, pending = null;
try {
  const saved = JSON.parse(sessionStorage.getItem(pendingKey));
  if (saved && uuid(saved.request_id) && ['apply','resume','rollback'].includes(saved.action)
      && (saved.action === 'apply' ? uuid(saved.check_id) : uuid(saved.operation_id))) pending = saved.action === 'apply'
    ? {request_id:saved.request_id,action:saved.action,check_id:saved.check_id}
    : {request_id:saved.request_id,action:saved.action,operation_id:saved.operation_id};
} catch { /* Browser storage may be unavailable; the server still owns the receipt. */ }

function remember(value) {
  pending = value;
  try { if (value) sessionStorage.setItem(pendingKey, JSON.stringify(value)); else sessionStorage.removeItem(pendingKey); } catch { /* No credential or authority is stored here. */ }
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
function ongoing() {
  return Boolean(pending || (status?.operation && !['completed','restored','deferred','failed'].includes(status.operation.phase)));
}
function schedule() {
  clearTimeout(poll);
  if (session?.authenticated && !document.hidden && ongoing()) poll = setTimeout(() => refresh(), 3000);
}
function render() {
  const admin = session?.admin === true;
  $('updates-loading').hidden = true;
  $('update-login').hidden = true;
  $('updates-content').hidden = !admin;
  $('update-sign-out').hidden = !session?.authenticated;
  if (!admin) { error('App updates are available to this installation’s administrator.'); return; }
  if (!status) return;
  $('update-installed').textContent = status.current?.version ? `Installed version: ${status.current.version}` : 'Installed version is not available.';
  $('update-owner').hidden = !status.reason && status.supported === true && status.owner === 'standalone';
  $('update-owner').textContent = status.reason || (status.owner === 'enrolled'
    ? 'Updates are managed by this installation’s existing updater. Use its update controls.'
    : 'App updates are not available for this installation.');
  const recovery = status.mode === 'recovery';
  $('return-to-companion').hidden = recovery;
  const operation = status.operation;
  $('update-progress').hidden = !operation && !pending && !recovery;
  if (operation) {
    const [title,detail] = progress(operation);
    $('update-progress-title').textContent = title;
    // Render the saved phase/outcome in user-facing language. Internal worker
    // diagnostics remain in the operation receipt, not in the main flow.
    $('update-progress-detail').textContent = detail;
    $('update-reference-value').textContent = operation.transaction_id || operation.id || '';
    $('update-reference').hidden = !operation.id;
  } else if (pending) {
    $('update-progress-title').textContent = 'Checking your update request';
    $('update-progress-detail').textContent = 'The reply was interrupted. Checking the saved request without starting another update.';
    $('update-reference-value').textContent = pending.request_id;
    $('update-reference').hidden = false;
  } else if (recovery) {
    $('update-progress-title').textContent = 'Companion needs recovery';
    $('update-progress-detail').textContent = 'Ordinary calls and work remain stopped while this installation is checked.';
    $('update-reference').hidden = true;
  }
  const actions = status.actions || {}, writable = status.supported === true && status.owner === 'standalone';
  const blocked = busy || Boolean(pending);
  $('update-package-form').hidden = !writable || recovery;
  $('update-check').disabled = blocked || actions.check !== true;
  $('update-package-path').disabled = blocked || actions.check !== true;
  const check = status.check;
  $('update-candidate').hidden = !check || !writable || recovery;
  if (check) {
    $('update-candidate-title').textContent = check.available ? `Version ${check.version || 'ready to install'}` : 'This package is already installed';
    $('update-candidate-summary').textContent = typeof check.summary === 'string' ? check.summary : 'The complete package has been checked.';
  }
  $('update-apply').hidden = !writable || actions.apply !== true || !check?.available || recovery;
  $('update-apply').disabled = blocked;
  $('update-apply-note').hidden = !check?.available;
  $('update-resume').hidden = !writable || actions.recover !== true;
  $('update-resume').disabled = blocked || !operation?.id;
  $('update-rollback').hidden = !writable || actions.rollback !== true;
  $('update-rollback').disabled = blocked || !operation?.id;
  $('update-refresh').disabled = busy;
  $('update-retry-request').hidden = !writable || !pending || status.request?.id !== pending.request_id || status.request.state !== 'not_recorded'
    || recovery && pending.action === 'apply';
  $('update-retry-request').disabled = busy;
  $('update-confirm-rollback').disabled = blocked || actions.rollback !== true;
  if (!writable || actions.rollback !== true || blocked) $('update-rollback-confirmation').hidden = true;
  schedule();
}
function signedOut() {
  generation++; clearTimeout(poll); status = null; busy = false;
  $('updates-loading').hidden = true; $('updates-content').hidden = true; $('update-sign-out').hidden = true;
  $('update-login').hidden = Boolean(session?.first_account_needed);
  const local = session?.auth_mode === 'local_password';
  $('update-username-label').hidden = local; $('update-username').hidden = local;
  $('update-username').value = session?.owner || ''; $('update-password').value = '';
  $('update-login-description').textContent = local ? 'Use the password you chose for Companion.' : 'Use your account on this service’s host.';
  // A signed-out page cannot yet establish whether normal contacts are served.
  // Initial account setup is the only known normal flow before authentication.
  $('return-to-companion').hidden = !session?.first_account_needed;
  if (session?.first_account_needed) error('Open Companion on this computer to finish setting up your account.');
}
async function refresh() {
  if (busy || !session?.authenticated) return;
  const expected = generation; busy = true; render();
  try {
    const path = root + (pending ? '?request_id='+encodeURIComponent(pending.request_id) : '');
    const next = await api(path);
    if (expected !== generation) return;
    status = next;
    if (pending && (next.operation?.request_id === pending.request_id || next.request?.state === 'recorded' && next.request.id === pending.request_id)) remember(null);
    error('');
  } catch (e) {
    if (expected !== generation) return;
    if (e.status === 401) { session = {...session,authenticated:false}; signedOut(); return; }
    if (e.status === 404) { status = {supported:false,reason:'This installation does not expose native update controls.'}; }
    else error(ongoing() ? 'Companion is temporarily unavailable. This page will keep checking the same operation. If it does not return, reopen Companion on this computer to continue recovery.' : e.message);
  } finally { if (expected === generation) { busy = false; render(); schedule(); } }
}
async function mutate(action) {
  if (busy || pending || !session?.admin || status?.owner !== 'standalone') return;
  if (action === 'apply' ? status.actions?.apply !== true || !status.check?.id : status.actions?.[action === 'resume' ? 'recover' : 'rollback'] !== true || !status.operation?.id) return;
  const request = action === 'apply'
    ? {request_id:crypto.randomUUID(),action,check_id:status.check.id}
    : {request_id:crypto.randomUUID(),action,operation_id:status.operation.id};
  return dispatch(request);
}
async function dispatch(request) {
  // A deliberate retry retains the original UUID and immutable payload. The
  // installation deduplicates it even if the first request arrives late.
  const wasUncertain = pending?.request_id === request.request_id;
  remember(request); busy = true; error(''); render();
  const expected = generation;
  try {
    const {action:choice,...body} = request;
    const next = await api(root+(choice === 'apply' ? '/apply' : '/recover'),'POST', choice === 'apply' ? body : {...body,action:choice});
    if (expected !== generation) return;
    status = next; remember(null);
  } catch (e) {
    if (expected !== generation) return;
    // A rejection of this retry cannot establish what happened to the original
    // unknown request. Retain its identity until saved-state reconciliation.
    if (!e.uncertain && !wasUncertain) remember(null);
    if (e.status === 401) { session = {...session,authenticated:false}; signedOut(); error('Sign in to check the saved update request.'); return; }
    error(e.uncertain ? 'The request may have been accepted. Checking its saved state; it will not be sent again automatically.' : e.message);
  } finally {
    if (expected === generation) { busy = false; render(); if (pending) await refresh(); }
  }
}
$('update-package-form').onsubmit = async event => {
  event.preventDefault(); if (busy || pending || status?.actions?.check !== true) return;
  const expected = generation; busy = true; error(''); render();
  try {
    const next = await api(root+'/check','POST',{package_path:$('update-package-path').value});
    if (expected === generation) { status = next; $('update-package-path').value = ''; }
  } catch (e) { if (expected === generation) error(e.uncertain ? 'The package check was interrupted. Check status before choosing it again.' : e.message); }
  finally { if (expected === generation) { busy = false; render(); } }
};
$('update-refresh').onclick = () => refresh();
$('update-retry-request').onclick = () => {
  if (!busy && pending && status?.owner === 'standalone' && status?.request?.id === pending.request_id && status.request.state === 'not_recorded') dispatch(pending);
};
$('update-apply').onclick = () => mutate('apply');
$('update-resume').onclick = () => mutate('resume');
$('update-rollback').onclick = () => { $('update-rollback-confirmation').hidden = false; $('update-confirm-rollback').focus(); };
$('update-cancel-rollback').onclick = () => { $('update-rollback-confirmation').hidden = true; $('update-rollback').focus(); };
$('update-rollback-confirmation').onsubmit = event => { event.preventDefault(); mutate('rollback'); };
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

// Setup never opens media, starts a call, or stores credentials in browser state.
const $ = id => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
let setupToken = fragment.get('setup') || '';
if (fragment.has('setup')) {
  fragment.delete('setup');
  const rest = fragment.toString();
  history.replaceState(history.state, '', location.pathname + location.search + (rest ? '#' + rest : ''));
}

export function installSetup({api, getSession, signedIn, refreshContacts, showLogin, notice}) {
  let setupGeneration = 0;
  const error = (id, message) => { $(id).textContent = message; $(id).hidden = !message; };
  function signedOut(session) {
    setupGeneration++;
    $('contact-setup').hidden = true;
    $('setup-openai-key').value = '';
    $('setup-agent').replaceChildren();
    $('first-account').hidden = !session?.first_account_needed;
    if (session?.first_account_needed) {
      $('login').hidden = true;
      $('app').hidden = true;
      $('loading').hidden = true;
      $('create-account').disabled = !setupToken;
      error('first-account-error', setupToken ? '' : 'Reopen Companion on this computer to create your account.');
    } else {
      const local = session?.auth_mode === 'local_password';
      $('login-description').textContent = local ? 'Sign in with the password you chose for Companion.' : 'Sign in with your account on this service’s host.';
      $('username-label').hidden = local; $('username').hidden = local;
      $('username').value = session?.owner || '';
    }
  }
  $('first-account-form').onsubmit = async event => {
    event.preventDefault();
    const password = $('new-password').value;
    if (password !== $('confirm-password').value) { error('first-account-error','The passwords do not match.'); return; }
    if (!setupToken) { error('first-account-error','Reopen Companion to continue setup.'); return; }
    $('create-account').disabled = true; error('first-account-error','');
    try {
      const result = await api('/auth/first-account','POST',{setup_token:setupToken,password,start_at_signin:$('start-at-signin').checked});
      setupToken = '';
      $('first-account').hidden = true;
      await signedIn(result);
      if (result.startup_error || result.startup?.error) notice('Your account is ready. Automatic startup could not be enabled; you can still open Companion normally.');
    } catch (failure) {
      // An unknown reply is reconciled through the existing session, never by
      // automatically repeating account creation or overwriting its password.
      try {
        const session = await api('/auth/session');
        if (session.authenticated) { setupToken = ''; $('first-account').hidden = true; await signedIn(session); return; }
        if (!session.first_account_needed) { setupToken = ''; showLogin(session); return; }
      } catch { /* Preserve the initial failure, with no automatic resend. */ }
      error('first-account-error',failure.uncertain ? 'Account creation could not be confirmed. Reopen Companion to check its saved state.' : failure.message);
      if (failure.uncertain) setupToken = '';
    } finally {
      $('new-password').value = ''; $('confirm-password').value = '';
      $('create-account').disabled = !setupToken;
    }
  };

  async function refresh() {
    const generation = ++setupGeneration;
    if (!getSession()?.admin) { $('contact-setup').hidden = true; return; }
    try {
      const setup = await api('/api/v1/companion/setup');
      if (generation !== setupGeneration || !getSession()?.admin) return;
      $('contact-setup').hidden = false;
      const agents = Array.isArray(setup.agents) ? setup.agents : [];
      $('setup-agent').replaceChildren(...agents.map(agent => new Option(agent.label || agent.id,agent.id)));
      $('setup-voice').replaceChildren(...(setup.voices || []).map(voice => new Option(voice,voice)));
      if ((setup.voices || []).includes('marin')) $('setup-voice').value = 'marin';
      const selectionChanged = () => {
        const saved = agents.find(agent => agent.id === $('setup-agent').value)?.voice_configured === true;
        $('setup-voice-fields').hidden = saved;
        $('setup-voice').disabled = saved;
        $('save-contact').textContent = saved ? 'Done' : 'Add contact';
        $('setup-agent-note').textContent = saved ? 'Already added. Use Voice on its contact to change settings.' : agents.length ? 'Choose a voice for this new contact.' : 'Connect an existing agent below. You can leave setup and come back later.';
      };
      $('setup-agent').onchange = selectionChanged;
      selectionChanged();
      $('save-contact').disabled = !agents.length;
      $('setup-key-fields').hidden = setup.credential.configured || setup.credential.environment_override;
      $('setup-openai-key').required = !setup.credential.configured && !setup.credential.environment_override;
      $('setup-key-note').textContent = setup.credential.environment_override
        ? (setup.credential.configured ? 'This installation already supplies a key through its environment.' : 'An empty environment key overrides saved keys. Update the installation environment before calling.')
        : setup.credential.configured ? 'Your OpenAI key is saved privately on this computer.' : 'The key is saved privately on this computer and is never shown again. Voice calls use your OpenAI account.';
      if (!agents.some(agent => agent.voice_configured)) $('contact-setup').open = true;
    } catch (failure) {
      if (generation === setupGeneration) { $('contact-setup').hidden = false; error('contact-setup-status',failure.message); }
    }
  }
  $('contact-setup-form').onsubmit = async event => {
    event.preventDefault();
    const generation = setupGeneration;
    const body = {agent_id:$('setup-agent').value, voice:$('setup-voice').value};
    if ($('setup-openai-key').value) body.openai_key = $('setup-openai-key').value;
    $('setup-openai-key').value = ''; $('save-contact').disabled = true;
    error('contact-setup-status','Saving contact…');
    try {
      const result = await api('/api/v1/companion/setup','POST',body);
      if (generation !== setupGeneration) return;
      error('contact-setup-status', result.credential.configured ? 'Contact saved. Choose Call when you’re ready.' : 'Contact saved. Voice access is still missing.');
      $('contact-setup').open = false;
      notice(result.credential.configured ? 'Contact saved. Choose Call when you’re ready.' : 'Contact saved. Voice access is still missing.');
      await refreshContacts();
    } catch (failure) {
      if (generation === setupGeneration) error('contact-setup-status', failure.uncertain ? 'The reply was interrupted. Refresh the contacts before saving again.' : failure.message);
    } finally { delete body.openai_key; $('save-contact').disabled = !$('setup-agent').options.length; }
  };
  $('local-agent-form').onsubmit = async event => {
    event.preventDefault();
    const generation = setupGeneration;
    $('connect-local-agent').disabled = true; error('local-agent-status','Checking the connection…');
    try {
      await api('/api/v1/companion/local-agent','POST',{id:$('local-agent-id').value,label:$('local-agent-label').value,home:$('local-agent-home').value});
      if (generation !== setupGeneration) return;
      error('local-agent-status','Connection saved. Select it above to add a voice contact.');
      await refresh(); $('setup-agent').value = $('local-agent-id').value; $('setup-agent').onchange(); $('setup-agent').focus();
    } catch (failure) {
      if (generation === setupGeneration) error('local-agent-status',failure.uncertain ? 'The connection check could not be confirmed. Refresh the saved list before trying again.' : failure.message);
    } finally { $('connect-local-agent').disabled = false; }
  };
  return {signedOut,refresh,clear:()=>{setupGeneration++;$('setup-openai-key').value='';}};
}

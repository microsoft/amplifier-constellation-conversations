import './markdown.js';

// Same-origin content is restricted to attachments belonging to this result's
// conversation. External links never authorize fetching a media preview.
export function safeCardLink(value, conversationId, origin) {
  if (typeof value !== 'string' || /[\u0000-\u0020\u007f]/.test(value)) return null;
  try {
    const url = new URL(value, origin);
    if (!['http:','https:'].includes(url.protocol) || url.username || url.password) return null;
    if (url.origin === origin) {
      const prefix = `/api/v1/conversations/${encodeURIComponent(conversationId)}/attachments/`;
      if (!conversationId || !url.pathname.startsWith(prefix) || !/^[^/]+$/.test(url.pathname.slice(prefix.length)) || url.search || url.hash) return null;
    } else if (!/^https?:\/\//i.test(value)) return null;
    return url.href;
  } catch { return null; }
}

export function renderCardText(container, text, conversationId, origin) {
  const body = document.createElement('div'); body.className = 'markdown card-text';
  // This is the locally bundled parser/sanitizer used by conversation messages.
  // No source rewriting: saved text, qualifiers and the full-result path remain.
  body.innerHTML = globalThis.renderMarkdown(text, {resolveLink: value => safeCardLink(value, conversationId, origin)});
  container.append(body);
  return [...body.querySelectorAll('a')].map(link => ({url:link.href,name:link.textContent || 'Linked media'}));
}

export function savedCardTime(value) {
  const date = typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? new Date(value)
    : typeof value === 'string' && /^\d{4}-\d\d-\d\dT/.test(value) ? new Date(value) : null;
  const label = document.createElement('small');
  if (!date || !Number.isFinite(date.getTime())) { label.textContent = 'Saved time unavailable'; return label; }
  const time = document.createElement('time'); time.dateTime = date.toISOString();
  time.textContent = `Saved ${date.toLocaleString([], {month:'short',day:'numeric',hour:'numeric',minute:'2-digit'})}`;
  time.title = date.toLocaleString(); label.append(time);
  return label;
}

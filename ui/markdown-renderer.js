/* Model output is content, never application markup. Dependencies are bundled locally. */
(() => {
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const parser = new marked.Marked({
    gfm: true,
    breaks: true,
    renderer: {
      html({text}) { return escape(text); },
      // Do not let model-written URLs silently fetch images in the user's browser.
      image({href, text}) { return `<a href="${escape(href)}">${escape(text || 'Image')}</a>`; },
    },
  });
  globalThis.renderMarkdown = text => {
    const fragment = DOMPurify.sanitize(parser.parse(String(text ?? '')), {
      RETURN_DOM_FRAGMENT: true,
      ALLOWED_TAGS: ['p','br','strong','em','del','code','pre','blockquote','ul','ol','li','h1','h2','h3','h4','h5','h6','hr','a','table','thead','tbody','tr','th','td','input'],
      ALLOWED_ATTR: ['href','title','start','type','checked','disabled'],
      ALLOW_DATA_ATTR: false,
      ALLOW_ARIA_ATTR: false,
    });
    for (const link of fragment.querySelectorAll('a')) {
      const href = link.getAttribute('href') || '';
      let safe = false;
      try { safe = /^(https?:|mailto:)/i.test(href) && ['https:','http:','mailto:'].includes(new URL(href).protocol); } catch {}
      if (!safe) { link.replaceWith(...link.childNodes); continue; }
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
    }
    for (const input of fragment.querySelectorAll('input')) {
      if (input.type !== 'checkbox') input.remove();
      else input.disabled = true;
    }
    const output = document.createElement('div');
    output.append(fragment);
    return output.innerHTML;
  };
})();

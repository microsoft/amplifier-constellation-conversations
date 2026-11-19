// Shared by Conversations and Companion. Only a same-room attachment is fetched,
// after the reader opens it. Author HTML always stays source text.
(() => {
  'use strict';
  const NS = 'http://www.w3.org/2000/svg';
  const SOURCE_LIMIT = 200_000, DIAGRAM_LIMIT = 32_768;
  const types = new Map([
    ['text/plain','text'], ['text/markdown','markdown'], ['text/html','html'],
    ['image/svg+xml','svg'], ['text/vnd.mermaid','mermaid'],
  ]);
  const svgTags = new Set('svg g rect circle ellipse line polyline polygon path text tspan title desc'.split(' '));
  const svgAttrs = new Set('viewBox width height x y x1 x2 y1 y2 dx dy cx cy r rx ry points d transform fill stroke stroke-width stroke-linecap stroke-linejoin opacity fill-opacity stroke-opacity text-anchor font-size font-weight preserveAspectRatio'.split(' '));

  function svgNode(name, attrs={}, text) {
    const node=document.createElementNS(NS,name);
    for (const [key,value] of Object.entries(attrs)) node.setAttribute(key,String(value));
    if (text!==undefined) node.textContent=text;
    return node;
  }
  function staticSvg(source) {
    if (source.length>DIAGRAM_LIMIT || /<!DOCTYPE|<!ENTITY|<\?(?!xml\s)/i.test(source)) return null;
    const root=new DOMParser().parseFromString(source,'image/svg+xml').documentElement;
    if (root.localName!=='svg' || root.namespaceURI!==NS || root.querySelectorAll('*').length>256) return null;
    // A static, bounded geometry vocabulary. No resource references, styles,
    // selectors, SMIL, event attributes, foreign documents or embedded images.
    for (const node of [root,...root.querySelectorAll('*')]) {
      if (node.namespaceURI!==NS || !svgTags.has(node.localName)) return null;
      for (const {name,value} of node.attributes) {
        if (name==='xmlns' && node===root && value===NS) continue;
        if (!svgAttrs.has(name) || /url\s*\(|[\\&;:<>]/i.test(value)) return null;
        if (name==='fill' || name==='stroke') {
          if (!/^(none|#[0-9a-f]{3,8}|[a-z]+)$/i.test(value)) return null;
        } else if (!/^[\w\s.,%()+-]*$/.test(value)) return null;
        if (name==='width' || name==='height') {
          if (!/^\d{1,4}(?:\.\d{1,3})?(?:px|%)?$/.test(value) || parseFloat(value)>4096) return null;
        }
      }
    }
    return root;
  }
  // Supported Mermaid subset: directed acyclic flowcharts, named rectangle
  // nodes, plain labels and solid arrows. Any other syntax retains its source.
  function flowchart(source) {
    if (source.length>DIAGRAM_LIMIT) return null;
    const lines=source.trim().split(/[;\n]/).map(s=>s.trim()).filter(s=>s && !s.startsWith('%%'));
    const header=/^(?:flowchart|graph)\s+(TD|TB|LR|BT|RL)$/.exec(lines.shift());
    if (!header || !lines.length || lines.length>48) return null;
    const nodes=new Map(),edges=[];
    for (const line of lines) {
      let previous;
      for (const token of line.split('-->')) {
        const match=/^([A-Za-z][\w-]{0,31})(?:\[(?:"([^"\[\]]{1,54})"|([^"\[\]]{1,54}))\])?$/.exec(token.trim());
        if (!match) return null;
        const [,id,quoted,plain]=match;
        if (id==='end' || /[<>]/.test(quoted||plain||'')) return null;
        if (!nodes.has(id)) nodes.set(id,{label:id,level:0,incoming:0,next:[]});
        const node=nodes.get(id);
        if (quoted || plain) node.label=quoted||plain;
        if (previous) { previous.next.push(node); node.incoming++; edges.push([previous,node]); }
        previous=node;
        if (nodes.size>24 || edges.length>48) return null;
      }
    }
    const order=[...nodes.values()].filter(node=>!node.incoming);
    for (const node of order) for (const next of node.next) {
      next.level=Math.max(next.level,node.level+1);
      if (!--next.incoming) order.push(next);
    }
    if (order.length!==nodes.size) return null;
    const maxLevel=Math.max(...order.map(n=>n.level)), slots=new Map();
    const horizontal=['LR','RL'].includes(header[1]), reversed=['RL','BT'].includes(header[1]);
    const labelWidth=Math.max(84,...order.map(n=>n.label.length*8+20)), height=44;
    let wide=0,tall=0;
    for (const n of order) {
      const slot=slots.get(n.level)||0; slots.set(n.level,slot+1);
      const level=reversed?maxLevel-n.level:n.level;
      n.x=8+(horizontal?level:slot)*(labelWidth+34);
      n.y=8+(horizontal?slot:level)*(height+34);
      wide=Math.max(wide,n.x+labelWidth+8); tall=Math.max(tall,n.y+height+8);
    }
    const svg=svgNode('svg',{viewBox:`0 0 ${wide} ${tall}`,width:wide,height:tall});
    for (const [a,b] of edges) {
      const ax=a.x+labelWidth/2, ay=a.y+height/2, bx=b.x+labelWidth/2, by=b.y+height/2;
      const dx=bx-ax,dy=by-ay, portion=Math.min(Math.abs(labelWidth/2/dx),Math.abs(height/2/dy));
      const x=bx-dx*portion,y=by-dy*portion, length=Math.hypot(dx,dy),ux=dx/length,uy=dy/length;
      svg.append(svgNode('line',{x1:ax+dx*portion,y1:ay+dy*portion,x2:x,y2:y,stroke:'#42574b','stroke-width':2}),
        svgNode('polygon',{points:`${x},${y} ${x-9*ux+4*uy},${y-9*uy-4*ux} ${x-9*ux-4*uy},${y-9*uy+4*ux}`,fill:'#42574b'}));
    }
    for (const n of order) svg.append(
      svgNode('rect',{x:n.x,y:n.y,width:labelWidth,height,rx:6,fill:'#f4f7f4',stroke:'#486451'}),
      svgNode('text',{x:n.x+labelWidth/2,y:n.y+height/2+5,'text-anchor':'middle','font-size':15,fill:'#172a1d'},n.label));
    return svg;
  }
  function diagram(kind,source) {
    const svg=kind==='svg'?staticSvg(source):flowchart(source);
    if (!svg) return null;
    const img=document.createElement('img');
    img.className='artifact-diagram'; img.alt=svg.querySelector('title')?.textContent || (kind==='svg'?'SVG diagram':'Mermaid flowchart');
    img.src='data:image/svg+xml;charset=utf-8,'+encodeURIComponent(new XMLSerializer().serializeToString(svg));
    return img;
  }
  function renderSource(kind,source) {
    const container=document.createElement('div');
    const pre=document.createElement('pre'),code=document.createElement('code');
    code.textContent=source; pre.append(code);
    if (kind==='mermaid' || kind==='svg') {
      const img=diagram(kind,source);
      if (img) {
        const details=document.createElement('details'),summary=document.createElement('summary');
        summary.textContent='Diagram source'; details.append(summary,pre); container.append(img,details);
      } else {
        const notice=document.createElement('p');
        notice.textContent='Diagram preview unavailable. Original source follows.'; container.append(notice,pre);
      }
    } else container.append(pre);
    return container;
  }
  function attachment(file,url,room) {
    const kind=types.get(file?.mime_type);
    const idPattern=/^[A-Za-z0-9][A-Za-z0-9_.-]{0,159}$/;
    if (!kind || !idPattern.test(room ?? '') || !idPattern.test(file.id ?? '')) return null;
    const expected=new URL('/api/v1/conversations/'+encodeURIComponent(room)+'/attachments/'+encodeURIComponent(file.id),location.origin);
    try { if (new URL(url,location.href).href!==expected.href) return null; } catch { return null; }
    const details=document.createElement('details'),summary=document.createElement('summary');
    details.className='artifact-preview';
    summary.textContent=kind==='html'?'View HTML source':'Preview '+file.name;
    details.append(summary);
    let requested=false;
    details.addEventListener('toggle',async()=>{
      if (!details.open || requested) return; requested=true;
      const report=document.createElement('p'); report.textContent='Loading preview…'; details.append(report);
      try {
        const response=await fetch(expected,{credentials:'same-origin',cache:'no-store',redirect:'error'});
        if (!response.ok || !response.body) throw new Error();
        const reader=response.body.getReader(), decoder=new TextDecoder('utf-8',{fatal:true});
        let total=0,source='';
        try {
          while (true) {
            const {value,done}=await reader.read(); if (done) break;
            total+=value.byteLength; if (total>SOURCE_LIMIT) throw new Error();
            source+=decoder.decode(value,{stream:true});
          }
          source+=decoder.decode();
        } finally { await reader.cancel(); }
        if (kind==='markdown') {
          const content=document.createElement('div'); content.className='markdown';
          content.innerHTML=globalThis.renderMarkdown(source,{resolveLink:raw=>{
            let u; try { u=new URL(raw,location.origin); } catch { return null; }
            if (u.username || u.password) return null;
            if (u.origin===location.origin) {
              const prefix=expected.pathname.slice(0,expected.pathname.lastIndexOf('/')+1);
              return u.pathname.startsWith(prefix) && idPattern.test(u.pathname.slice(prefix.length)) && !u.search && !u.hash ? u.href : null;
            }
            return /^https?:\/\//i.test(raw)?u.href:null;
          }});
          report.replaceWith(content);
        } else report.replaceWith(renderSource(kind,source));
      } catch { report.textContent='Preview unavailable. The original download link is below.'; }
    });
    return details;
  }
  globalThis.roomArtifacts={renderSource,attachment};
})();

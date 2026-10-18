// Render document text through DOM nodes; file contents never become HTML.
export function renderDocument(text, element) {
  const document = element('article', 'document-preview');
  const lines = text.split(/\r?\n/);
  let code = null, list = null;
  function inline(node, value) {
    for (const part of value.split(/(\*\*[^*]+\*\*|`[^`]+`)/g)) {
      if (part.startsWith('**') && part.endsWith('**')) node.append(element('strong', '', part.slice(2,-2)));
      else if (part.startsWith('`') && part.endsWith('`')) node.append(element('code', '', part.slice(1,-1)));
      else node.append(documentNode(part));
    }
  }
  function documentNode(text) { return globalThis.document.createTextNode(text); }
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (line.startsWith('```')) {
      if (code) { code = null; } else { code = element('pre', 'document-code'); document.append(code); }
      list = null; continue;
    }
    if (code) { code.append(documentNode(line+'\n')); continue; }
    if (!line.trim()) { list = null; continue; }
    if (/^\|.*\|$/.test(line.trim()) && /^\|[\s:|\-]+\|$/.test(lines[index+1]?.trim() || '')) {
      const table = element('table', 'document-table');
      const add = (value, tag) => { const row = element('tr'); for (const cell of value.trim().slice(1,-1).split('|')) { const node = element(tag); inline(node, cell.trim()); row.append(node); } table.append(row); };
      add(line, 'th'); index++;
      while (/^\|.*\|$/.test(lines[index+1]?.trim() || '')) add(lines[++index], 'td');
      const wrap = element('div', 'document-table-wrap'); wrap.append(table); document.append(wrap); list = null; continue;
    }
    const heading = /^(#{1,6})\s+(.+)$/.exec(line);
    if (heading) { const node = element(`h${heading[1].length}`); inline(node, heading[2]); document.append(node); list = null; continue; }
    const item = /^(?:[-*]|\d+\.)\s+(.+)$/.exec(line);
    if (item) { if (!list) { list = element('ul'); document.append(list); } const node = element('li'); inline(node, item[1].replace(/^\[x\]\s*/i,'✓ ').replace(/^\[ \]\s*/,'□ ')); list.append(node); continue; }
    list = null;
    const paragraph = element(line.startsWith('> ') ? 'blockquote' : 'p'); inline(paragraph, line.replace(/^> /,'')); document.append(paragraph);
  }
  return document;
}

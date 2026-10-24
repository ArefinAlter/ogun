'use strict';
// Finds the text range of the JSON value a JSON Pointer (RFC 6901) names, so validator
// diagnostics land on the right characters. A small recursive-descent scanner: no dependencies,
// tolerant of anything after the first complete value.

function locate(text, pointer) {
  const tokens = pointer === '' ? [] : pointer.slice(1).split('/').map((t) => t.replace(/~1/g, '/').replace(/~0/g, '~'));
  let i = 0;
  const ws = () => { while (i < text.length && ' \t\r\n'.includes(text[i])) i++; };
  const str = () => {
    const start = i++;
    let out = '';
    while (i < text.length && text[i] !== '"') {
      if (text[i] === '\\') { out += JSON.parse('"' + text.slice(i, i + (text[i + 1] === 'u' ? 6 : 2)) + '"'); i += text[i + 1] === 'u' ? 6 : 2; }
      else out += text[i++];
    }
    i++;
    return { start, end: i, value: out };
  };
  // Scans one value. `path` is null when skipping, [] when this value is the target, and the
  // remaining tokens when the target is inside it.
  const deeper = (path) => Array.isArray(path) && path.length > 0;
  const value = (path) => {
    ws();
    const start = i;
    const c = text[i];
    if (c === '{') {
      i++; let found = null;
      for (;;) {
        ws();
        if (text[i] === '}') { i++; break; }
        const key = str(); ws(); i++; // ':'
        const hit = deeper(path) && key.value === path[0];
        const r = value(hit ? path.slice(1) : null);
        if (hit) found = r;
        ws();
        if (text[i] === ',') i++;
        else if (text[i] === '}') { i++; break; }
        else break;
      }
      return deeper(path) ? found : { start, end: i };
    }
    if (c === '[') {
      i++; let found = null; let n = 0;
      for (;;) {
        ws();
        if (text[i] === ']') { i++; break; }
        const hit = deeper(path) && String(n) === path[0];
        const r = value(hit ? path.slice(1) : null);
        if (hit) found = r;
        n++; ws();
        if (text[i] === ',') i++;
        else if (text[i] === ']') { i++; break; }
        else break;
      }
      return deeper(path) ? found : { start, end: i };
    }
    if (c === '"') { const s = str(); return deeper(path) ? null : { start: s.start, end: s.end }; }
    while (i < text.length && !',}] \t\r\n'.includes(text[i])) i++;
    return deeper(path) ? null : { start, end: i };
  };
  try {
    return value(tokens) || null;
  } catch (_) {
    return null;
  }
}

// Falls back to the longest prefix of the pointer that exists, then to the whole document.
function locateNearest(text, pointer) {
  let p = pointer;
  for (;;) {
    const r = locate(text, p);
    if (r) return r;
    if (p === '') return { start: 0, end: 0 };
    p = p.slice(0, p.lastIndexOf('/'));
  }
}

module.exports = { locate, locateNearest };

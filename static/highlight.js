// Graftline — a small syntax highlighter for diff lines.
// It colors one line at a time, without the lines around it: a diff shows fragments of a
// file, so a full parser has nothing to hold on to. Comments, strings, numbers, keywords,
// types and calls are enough to make code readable. Colors come from the theme (--t-*).

const esc = (s) => s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
const span = (cls, text) => `<span class="${cls}">${esc(text)}</span>`;
const words = (s) => new Set(s.split(' '));

const KEYWORDS = {
  rust: words('as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while'),
  js: words('as async await break case catch class const continue debugger default delete do else enum export extends finally for from function get if implements import in instanceof interface let new of private protected public readonly return set static super switch this throw try type typeof var void while with yield declare namespace abstract keyof'),
  python: words('and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case self'),
  go: words('break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var'),
  c: words('abstract auto break case catch class const constexpr continue default delete do else enum explicit extends extern final finally for friend goto if implements import inline instanceof interface namespace new operator override package private protected public register return sizeof static struct switch template this throw throws try typedef typename union using virtual void volatile while fun val var when internal data object companion sealed open func let guard defer protocol extension echo function use'),
  shell: words('if then else elif fi for while do done case esac function in return export local readonly set unset echo exit source'),
  sql: words('select from where and or not in is null like between as on join left right inner outer full cross group by order having limit offset insert into values update set delete create alter drop table index view primary key foreign references default unique constraint union all distinct case when then else end with returning'),
  ruby: words('alias and begin break case class def defined do else elsif end ensure for if in module next not or redo rescue retry return self super then undef unless until when while yield require attr_accessor attr_reader'),
  lua: words('and break do else elseif end for function goto if in local not or repeat return then until while'),
};
const CONSTANTS = words('true false null undefined None True False nil NULL nullptr NaN Infinity');

// One pattern per family. Groups: 1 comment, 2 string, 3 number, 4 identifier, 5 punctuation, 6 operator.
const STR = String.raw`"(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\])*'?`;
const TAIL = String.raw`|(\b\d[\w.]*)|([A-Za-z_$][\w$]*)|([{}()\[\];,.])|([-+*/%=<>!&|^~?:@]+)`;
const FAMILY = {
  slash: new RegExp(String.raw`(//.*$|/\*.*?(?:\*/|$))|(${STR}|` + '`(?:\\\\.|[^`\\\\])*`?)' + TAIL, 'g'),
  // Rust: 'a is a lifetime, 'x' is a character.
  rust: new RegExp(String.raw`(//.*$|/\*.*?(?:\*/|$))|("(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\])')` + TAIL, 'g'),
  hash: new RegExp(String.raw`(#.*$)|(${STR})` + TAIL, 'g'),
  dash: new RegExp(String.raw`(--.*$)|(${STR})` + TAIL, 'g'),
};
const LANG = {
  rust: ['rust', KEYWORDS.rust], js: ['slash', KEYWORDS.js], go: ['slash', KEYWORDS.go], c: ['slash', KEYWORDS.c], css: ['slash', new Set()],
  python: ['hash', KEYWORDS.python], shell: ['hash', KEYWORDS.shell], ruby: ['hash', KEYWORDS.ruby], yaml: ['hash', new Set()], toml: ['hash', new Set()],
  sql: ['dash', KEYWORDS.sql], lua: ['dash', KEYWORDS.lua],
};

function code(text, family, keywords, caseless) {
  const re = FAMILY[family];
  re.lastIndex = 0;
  let out = '';
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) out += esc(text.slice(last, m.index));
    last = re.lastIndex;
    if (m[1] != null) out += span('t-com', m[1]);
    else if (m[2] != null) out += span('t-str', m[2]);
    else if (m[3] != null) out += span('t-num', m[3]);
    else if (m[4] != null) {
      const w = m[4];
      if (keywords.has(caseless ? w.toLowerCase() : w)) out += span('t-kw', w);
      else if (CONSTANTS.has(w)) out += span('t-const', w);
      else if (text[last] === '(' || (text[last] === '!' && family === 'rust')) out += span('t-fn', w);
      else if (/^[A-Z]/.test(w) && /[a-z]/.test(w)) out += span('t-type', w);
      else out += esc(w);
    } else if (m[5] != null) out += span('t-punct', m[5]);
    else out += span('t-op', m[6]);
    if (m[0] === '') re.lastIndex++;
  }
  return out + esc(text.slice(last));
}

function json(text) {
  return text.replace(/("(?:\\.|[^"\\])*")(\s*:)?|(-?\b\d[\d.eE+-]*)|\b(true|false|null)\b|([{}\[\],])|[&<>]/g, (m, str, colon, num, lit, punct) => {
    if (str) return span(colon ? 't-id' : 't-str', str) + (colon || '');
    if (num) return span('t-num', num);
    if (lit) return span('t-const', lit);
    if (punct) return span('t-punct', punct);
    return esc(m);
  });
}

function markup(text) {
  return text.replace(/(<!--.*?(?:-->|$))|(<\/?)([\w:-]+)|("[^"]*"|'[^']*')|([\w:-]+)(=)|(\/?>)|[&<>]/g, (m, com, open, tag, str, attr, eq, close) => {
    if (com) return span('t-com', com);
    if (tag) return span('t-punct', open) + span('t-kw', tag);
    if (str) return span('t-str', str);
    if (attr) return span('t-id', attr) + span('t-op', eq);
    if (close) return span('t-punct', close);
    return esc(m);
  });
}

function markdown(text) {
  if (/^#{1,6}\s/.test(text)) return span('t-kw', text);
  if (/^\s*(```|~~~)/.test(text)) return span('t-com', text);
  return text.replace(/(`[^`]+`)|(\*\*[^*]+\*\*)|(\[[^\]]+\]\([^)]+\))|[&<>]/g, (m, codeSpan, bold, link) => {
    if (codeSpan) return span('t-str', codeSpan);
    if (bold) return span('t-type', bold);
    if (link) return span('t-id', link);
    return esc(m);
  });
}

/** Returns HTML for one line of code, or null when the language is unknown or the line is very long. */
export function highlightLine(text, lang) {
  if (!lang || text.length > 2000) return null;
  if (lang === 'json') return json(text);
  if (lang === 'html') return markup(text);
  if (lang === 'md') return markdown(text);
  const def = LANG[lang];
  return def ? code(text, def[0], def[1], lang === 'sql') : null;
}

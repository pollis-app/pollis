/**
 * doc-text.mjs — the text a public-copy checker matches against (#1722, #1723).
 *
 * Shared by check-facts.mjs and check-claims.mjs so a sentence that "still
 * appears" means the same thing to both: HTML loses its tags and entities,
 * Markdown its `**`, and every run of whitespace becomes one space, so a
 * sentence that rewraps or gains a <strong> still matches.
 */

export const ENTITIES = { "&nbsp;": " ", "&amp;": "&", "&lt;": "<", "&gt;": ">", "&quot;": '"', "&#39;": "'",
  "&mdash;": "—", "&ndash;": "–", "&rsquo;": "’", "&lsquo;": "‘", "&hellip;": "…", "&times;": "×", "&plusmn;": "±" };

/**
 * The text an assertion pattern is matched against, with a map back to the
 * original offset (for `file:line`). HTML loses its tags and entities, Markdown
 * its `**`, and every run of whitespace becomes one space, so a sentence that
 * wraps differently, or gains a <strong>, still matches.
 */
export function normalize(raw, isHtml) {
  let out = "";
  const map = [];
  let i = 0;
  let pendingSpace = false;
  const push = (s, at) => {
    if (pendingSpace && out.length && !out.endsWith(" ")) {
      out += " ";
      map.push(at);
    }
    pendingSpace = false;
    for (const ch of s) {
      out += ch;
      map.push(at);
    }
  };
  while (i < raw.length) {
    const c = raw[i];
    if (isHtml && c === "<") {
      const close = raw.indexOf(">", i);
      if (close !== -1) {
        const tag = raw.slice(i, close + 1);
        if (/^<(script|style)\b/i.test(tag)) {
          const end = raw.toLowerCase().indexOf(`</${tag.slice(1).match(/^\w+/)[0].toLowerCase()}`, close);
          i = end === -1 ? raw.length : end;
          continue;
        }
        // Block-level boundaries read as whitespace; inline tags (<strong>) vanish.
        if (/^<\/?(p|div|li|ul|ol|br|h\d|tr|td|th|table|section|figure|figcaption|pre|blockquote)\b/i.test(tag)) {
          pendingSpace = true;
        }
        i = close + 1;
        continue;
      }
    }
    if (isHtml && c === "&") {
      const m = /^&(#\d+|#x[0-9a-f]+|[a-z]+);/i.exec(raw.slice(i, i + 10));
      if (m) {
        let ch = ENTITIES[m[0]];
        if (ch === undefined && m[1][0] === "#") {
          ch = String.fromCodePoint(m[1][1] === "x" ? parseInt(m[1].slice(2), 16) : parseInt(m[1].slice(1), 10));
        }
        if (ch !== undefined) {
          if (/\s/.test(ch)) {
            pendingSpace = true;
          } else {
            push(ch, i);
          }
          i += m[0].length;
          continue;
        }
      }
    }
    if (!isHtml && c === "*" && raw[i + 1] === "*") {
      i += 2;
      continue;
    }
    if (/\s/.test(c)) {
      pendingSpace = true;
      i += 1;
      continue;
    }
    push(c, i);
    i += 1;
  }
  return { text: out, map };
}

export function lineOf(raw, offset) {
  let n = 1;
  for (let i = 0; i < offset && i < raw.length; i++) {
    if (raw[i] === "\n") {
      n++;
    }
  }
  return n;
}

/** A workflow `paths:` glob (`*` within a segment, `**` across them) as a RegExp. */
export function globToRe(g) {
  let re = "";
  for (let i = 0; i < g.length; i++) {
    if (g[i] === "*" && g[i + 1] === "*") {
      re += ".*";
      i++;
    } else if (g[i] === "*") {
      re += "[^/]*";
    } else if (g[i] === "?") {
      re += "[^/]";
    } else {
      re += g[i].replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }
  }
  return new RegExp(`^${re}$`);
}

/** The `pull_request: paths:` globs of a workflow file's text (empty when the shape is unrecognised). */
export function pullRequestPaths(workflowText) {
  const block = /pull_request:\s*\n([\s\S]*?)\n\s{2}push:/.exec(workflowText);
  return block ? [...block[1].matchAll(/^\s*-\s*"([^"]+)"/gm)].map((m) => m[1]) : [];
}

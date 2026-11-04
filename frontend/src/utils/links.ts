/*
 * The one URL finder for message bodies, shared by the linkifier and the media unfurl (#874).
 * mobile/lib/links.ts is a byte-for-byte copy (mobile imports no frontend TypeScript);
 * mobile/tests/links.test.ts fails if the two differ. Edit both together.
 *
 * Message text is attacker-controlled. A token becomes a link only when what it shows is
 * what opens: http(s) only, no credentials in the authority, no invisible or bidi controls,
 * a host that parses. Pure: no DOM, no `URL` global (React Native's is a regex stub).
 */

/** Tokens longer than this stay plain text. */
export const MAX_URL_LENGTH = 2048;

// Whitespace without U+FEFF (JS `\s` counts it), so a zero-width no-break space stays inside the token and refuses it.
const SPACE = "\\t\\n\\v\\f\\r \\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000";
const TOKEN = `[^${SPACE}<>"'\`]+`;
// http(s):// and www. URLs. A source string, compiled per scan, so no shared `lastIndex` can leak.
const URL_PATTERN = `(?:https?://|www\\.)${TOKEN}`;

// Bidi embeddings/overrides/isolates and marks, zero-width characters, word joiners, BOM, and C0/C1 controls.
const INVISIBLE = /[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u2069\ufeff]/;
// A link may not start mid-word ("awww.", "x@www.", "xhttps://").
const GLUED_BEFORE = /[\p{L}\p{N}_.@/+-]/u;
const TRAILING_PUNCT = ".,;:!?";
// Ideographic and fullwidth full stops, which IDNA reads as dots.
const DOTS = /[\u3002\uff0e\uff61]/g;
// RFC 3986 unreserved + reserved + `%`; anything else in the path is percent-encoded for the OS.
const URI_SAFE = /[A-Za-z0-9\-._~:/?#[\]@!$&'()*+,;=%]/;

export interface UrlMatch {
  /** Index of the first character of `url` within the scanned text. */
  start: number;
  /** The text exactly as written, which is also the link's visible label. */
  url: string;
}

export interface LinkTarget {
  /** What gets opened: lowercased scheme, ASCII (punycode) host, path percent-encoded. */
  href: string;
  /** The ASCII host, as `href` carries it. */
  host: string;
  /** The host is IDN / punycode, so it can imitate another: show `host` and ask before opening. */
  needsConfirm: boolean;
}

/** URLs in `text` that `parseLink` accepts, or `null` (the common case) when there are none. */
export function findUrls(text: string): UrlMatch[] | null {
  const re = new RegExp(URL_PATTERN, "gi");
  let found: UrlMatch[] | null = null;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    const start = match.index;
    if (start > 0 && GLUED_BEFORE.test(text[start - 1])) {
      continue;
    }
    // Over the cap before trimming: plain text, and never fed to the trimmer.
    if (match[0].length > MAX_URL_LENGTH) {
      continue;
    }
    const url = trimTrailing(match[0]);
    if (parseLink(url) === null) {
      continue;
    }
    if (found === null) {
      found = [];
    }
    found.push({ start, url });
  }
  return found;
}

/** Add `https://` to a `www.` URL; http(s) is left alone. */
export function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}

/** The destination for a message link, or `null` when it must not be opened. Re-run before every open. */
export function parseLink(raw: string): LinkTarget | null {
  if (raw.length === 0 || raw.length > MAX_URL_LENGTH || INVISIBLE.test(raw)) {
    return null;
  }
  const isWww = /^www\./i.test(raw);
  const withProtocol = ensureProtocol(raw);
  const scheme = /^(https?):\/\//i.exec(withProtocol);
  if (!scheme) {
    return null;
  }
  const rest = withProtocol.slice(scheme[0].length);
  const authorityEnd = rest.search(/[/?#]/);
  const authority = authorityEnd === -1 ? rest : rest.slice(0, authorityEnd);
  const tail = authorityEnd === -1 ? "" : rest.slice(authorityEnd);
  // Credentials (`user:pass@host`) read as one host and open another; a backslash is a path separator to browsers.
  if (authority.length === 0 || authority.includes("@") || authority.includes("\\")) {
    return null;
  }
  const hostPort = splitPort(authority);
  if (hostPort === null) {
    return null;
  }
  const host = toAsciiHost(hostPort.host);
  if (host === null) {
    return null;
  }
  if (isWww && host.ascii.split(".").filter((l) => l.length > 0).length < 3) {
    return null;
  }
  const port = hostPort.port === null ? "" : `:${hostPort.port}`;
  return {
    href: `${scheme[1].toLowerCase()}://${host.ascii}${port}${encodeTail(tail)}`,
    host: host.ascii,
    needsConfirm: host.idn,
  };
}

function trimTrailing(url: string): string {
  let end = url.length;
  let open = 0;
  let close = 0;
  let openSquare = 0;
  let closeSquare = 0;
  for (const c of url) {
    if (c === "(") {
      open += 1;
    } else if (c === ")") {
      close += 1;
    } else if (c === "[") {
      openSquare += 1;
    } else if (c === "]") {
      closeSquare += 1;
    }
  }
  while (end > 0) {
    const last = url[end - 1];
    if (TRAILING_PUNCT.includes(last)) {
      end -= 1;
    } else if (last === ")" && close > open) {
      // A closing bracket belongs to the URL only when the URL opened it (Wikipedia's `Foo_(bar)`).
      close -= 1;
      end -= 1;
    } else if (last === "]" && closeSquare > openSquare) {
      closeSquare -= 1;
      end -= 1;
    } else {
      break;
    }
  }
  return url.slice(0, end);
}

function splitPort(authority: string): { host: string; port: string | null } | null {
  if (authority.startsWith("[")) {
    const close = authority.indexOf("]");
    if (close === -1) {
      return null;
    }
    const after = authority.slice(close + 1);
    if (after !== "" && !/^:\d{1,5}$/.test(after)) {
      return null;
    }
    return { host: authority.slice(0, close + 1), port: after === "" ? null : validPort(after.slice(1)) };
  }
  const colon = authority.lastIndexOf(":");
  if (colon === -1) {
    return { host: authority, port: null };
  }
  const port = authority.slice(colon + 1);
  if (!/^\d{1,5}$/.test(port) || validPort(port) === null) {
    return null;
  }
  return { host: authority.slice(0, colon), port };
}

function validPort(port: string): string | null {
  return Number(port) <= 65535 ? port : null;
}

function toAsciiHost(raw: string): { ascii: string; idn: boolean } | null {
  if (raw.startsWith("[")) {
    return /^\[[0-9a-f:.]+\]$/i.test(raw) ? { ascii: raw.toLowerCase(), idn: false } : null;
  }
  const nonAscii = /[^\x00-\x7f]/.test(raw);
  let mapped = raw.replace(DOTS, ".");
  try {
    mapped = mapped.normalize("NFKC");
  } catch {
    // No normalizer: keep the host as written; it is still encoded and confirmed.
  }
  mapped = mapped.toLowerCase();
  if (mapped.length === 0 || mapped.startsWith(".")) {
    return null;
  }
  const labels = mapped.split(".");
  const ascii: string[] = [];
  let idn = nonAscii;
  for (let i = 0; i < labels.length; i += 1) {
    const label = labels[i];
    if (label.length === 0) {
      // Only a single trailing dot (`example.com.`) may leave an empty label.
      if (i === labels.length - 1 && i > 0) {
        ascii.push("");
        continue;
      }
      return null;
    }
    let encoded = label;
    if (/[^\x00-\x7f]/.test(label)) {
      // Letters, marks and digits only: no lookalike slashes, dots or symbols (`∕`, `⁄`, `／` after NFKC).
      if (!/^[\p{L}\p{M}\p{N}-]+$/u.test(label)) {
        return null;
      }
      encoded = `xn--${punycodeEncode(label)}`;
    } else if (!/^[a-z0-9_-]+$/.test(label)) {
      return null;
    }
    if (encoded.startsWith("xn--")) {
      idn = true;
    }
    if (encoded.length > 63) {
      return null;
    }
    ascii.push(encoded);
  }
  const joined = ascii.join(".");
  if (joined.length > 253) {
    return null;
  }
  return { ascii: joined, idn };
}

function encodeTail(tail: string): string {
  let out = "";
  for (const ch of tail) {
    out += URI_SAFE.test(ch) ? ch : encodeURIComponent(ch);
  }
  return out;
}

/** RFC 3492 Punycode for one label (no `xn--` prefix). */
function punycodeEncode(label: string): string {
  const base = 36;
  const tMin = 1;
  const tMax = 26;
  const codePoints = Array.from(label, (c) => c.codePointAt(0) as number);
  let n = 128;
  let delta = 0;
  let bias = 72;
  let output = codePoints
    .filter((c) => c < 0x80)
    .map((c) => String.fromCharCode(c))
    .join("");
  const basic = output.length;
  let handled = basic;
  if (basic > 0) {
    output += "-";
  }
  const digit = (d: number) => String.fromCharCode(d + 22 + (d < 26 ? 75 : 0));
  const adapt = (d: number, points: number, first: boolean) => {
    let k = 0;
    d = first ? Math.floor(d / 700) : d >> 1;
    d += Math.floor(d / points);
    while (d > ((base - tMin) * tMax) >> 1) {
      d = Math.floor(d / (base - tMin));
      k += base;
    }
    return k + Math.floor(((base - tMin + 1) * d) / (d + 38));
  };
  while (handled < codePoints.length) {
    let m = Number.MAX_SAFE_INTEGER;
    for (const c of codePoints) {
      if (c >= n && c < m) {
        m = c;
      }
    }
    delta += (m - n) * (handled + 1);
    n = m;
    for (const c of codePoints) {
      if (c < n) {
        delta += 1;
      }
      if (c === n) {
        let q = delta;
        for (let k = base; ; k += base) {
          const t = k <= bias ? tMin : k >= bias + tMax ? tMax : k - bias;
          if (q < t) {
            break;
          }
          output += digit(t + ((q - t) % (base - t)));
          q = Math.floor((q - t) / (base - t));
        }
        output += digit(q);
        bias = adapt(delta, handled + 1, handled === basic);
        delta = 0;
        handled += 1;
      }
    }
    delta += 1;
    n += 1;
  }
  return output;
}

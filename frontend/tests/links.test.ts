/* Message links: what becomes a link and what it opens. mobile/tests/links.test.ts runs the same cases against mobile's copy. */
import test from "node:test";
import assert from "node:assert/strict";
import { domainToASCII } from "node:url";

import { MAX_URL_LENGTH, ensureProtocol, findUrls, parseLink } from "../src/utils/links.ts";

/** The visible link texts found in `text`. */
const urls = (text: string) => (findUrls(text) ?? []).map((m) => m.url);
const href = (raw: string) => parseLink(raw)?.href ?? null;

test("plain text has no links", () => {
  assert.equal(findUrls("hello there"), null);
  assert.equal(findUrls(""), null);
});

test("http, https and www. are links; positions are exact", () => {
  const text = "a https://example.com/x b http://example.org c www.example.net/p";
  assert.deepEqual(findUrls(text), [
    { start: 2, url: "https://example.com/x" },
    { start: 26, url: "http://example.org" },
    { start: 47, url: "www.example.net/p" },
  ]);
});

test("a link at the very start is kept (#874: no stale lastIndex)", () => {
  for (let i = 0; i < 3; i += 1) {
    assert.deepEqual(urls("https://example.com/first then https://example.com/second"), [
      "https://example.com/first",
      "https://example.com/second",
    ]);
  }
});

test("bare www. gets https://; http(s) is left alone", () => {
  assert.equal(ensureProtocol("www.example.com"), "https://www.example.com");
  assert.equal(ensureProtocol("http://example.com"), "http://example.com");
  assert.equal(href("www.example.com/a"), "https://www.example.com/a");
  assert.equal(href("HTTPS://Example.COM/Path"), "https://example.com/Path");
});

test("bare domains without www. are not links (desktop's rule)", () => {
  assert.equal(findUrls("visit example.com today"), null);
});

test("www. needs a real domain after it", () => {
  assert.equal(findUrls("www.x"), null);
  assert.equal(findUrls("www."), null);
});

test("trailing punctuation is not part of the link", () => {
  assert.deepEqual(urls("See https://example.com."), ["https://example.com"]);
  assert.deepEqual(urls("https://example.com, then"), ["https://example.com"]);
  assert.deepEqual(urls("Really? https://example.com/a?!"), ["https://example.com/a"]);
  assert.deepEqual(urls("https://example.com/a;"), ["https://example.com/a"]);
  assert.deepEqual(urls("https://example.com:"), ["https://example.com"]);
});

test("parentheses: balanced ones stay (Wikipedia), an enclosing one goes", () => {
  assert.deepEqual(urls("https://en.wikipedia.org/wiki/Foo_(bar)"), [
    "https://en.wikipedia.org/wiki/Foo_(bar)",
  ]);
  assert.deepEqual(urls("(see https://example.com/a)"), ["https://example.com/a"]);
  assert.deepEqual(urls("(https://en.wikipedia.org/wiki/Foo_(bar))."), [
    "https://en.wikipedia.org/wiki/Foo_(bar)",
  ]);
  assert.deepEqual(urls("[https://example.com/a]"), ["https://example.com/a"]);
});

test("quotes and angle brackets end a link", () => {
  assert.deepEqual(urls('"https://example.com/a"'), ["https://example.com/a"]);
  assert.deepEqual(urls("<https://example.com/a>"), ["https://example.com/a"]);
  assert.deepEqual(urls("`https://example.com/a`"), ["https://example.com/a"]);
});

test("only http and https ever open", () => {
  for (const raw of [
    "javascript:alert(1)",
    "data:text/html,<b>x</b>",
    "file:///etc/passwd",
    "intent://scan/#Intent;scheme=zxing;end",
    "content://media/external",
    "tel:+15555550100",
    "sms:+15555550100",
    "pollis://m/abc/def",
    "ftp://example.com",
    "mailto:someone@example.com",
  ]) {
    assert.equal(href(raw), null, raw);
    assert.equal(findUrls(raw), null, raw);
  }
});

test("javascript: inside text never becomes a link; a real link beside it still does", () => {
  assert.equal(findUrls("click javascript:alert(document.cookie) now"), null);
  assert.deepEqual(urls("javascript:https://example.com/x"), ["https://example.com/x"]);
  assert.equal(href("https://example.com/x"), "https://example.com/x");
});

test("credentials in the authority are refused (the label would not be the host)", () => {
  assert.equal(findUrls("https://google.com@evil.example/login"), null);
  assert.equal(findUrls("https://user:pass@example.com/"), null);
  assert.equal(findUrls("www.example.com@evil.example"), null);
  // `@` in the path is fine: it is not the authority.
  assert.deepEqual(urls("https://medium.com/@someone/post"), ["https://medium.com/@someone/post"]);
});

test("a backslash in the authority is refused (browsers read it as a slash)", () => {
  assert.equal(findUrls("https://evil.example\\@good.example/"), null);
  assert.equal(findUrls("https://evil.example\\good.example"), null);
});

test("email addresses are not links, and neither is www. glued to one", () => {
  assert.equal(findUrls("write to someone@example.com"), null);
  assert.equal(findUrls("write to someone@www.example.com"), null);
  assert.equal(findUrls("awww.example.com"), null);
  assert.equal(findUrls("xhttps://example.com"), null);
});

test("mentions next to links: neither swallows the other", () => {
  const text = "@alice https://example.com/a @bob";
  assert.deepEqual(findUrls(text), [{ start: 7, url: "https://example.com/a" }]);
  assert.deepEqual(urls("@alice,https://example.com"), ["https://example.com"]);
});

test("bidi controls and invisible characters make the token plain text", () => {
  for (const ch of [
    "\u202a", "\u202b", "\u202c", "\u202d", "\u202e",
    "\u2066", "\u2067", "\u2068", "\u2069",
    "\u200e", "\u200f", "\u061c", "\u200b", "\u200c", "\u200d", "\u2060", "\ufeff",
    "\u0000", "\u0007", "\u007f", "\u0085",
  ]) {
    const text = `see https://example.com/${ch}gnp.exe here`;
    assert.equal(findUrls(text), null, `U+${ch.codePointAt(0)!.toString(16)}`);
    assert.equal(href(`https://example.com/${ch}x`), null);
    assert.equal(href(`https://exa${ch}mple.com/`), null);
  }
  // The text around a refused token keeps its other links.
  assert.deepEqual(urls("https://ok.example a https://bad.example/\u202ex"), ["https://ok.example"]);
});

test("IDN hosts open as punycode and ask first; the encoding matches WHATWG's", () => {
  for (const host of ["bücher.example", "аpple.com", "例え.テスト", "mañana.com", "παράδειγμα.δοκιμή"]) {
    const target = parseLink(`https://${host}/path`);
    assert.ok(target, host);
    assert.equal(target.host, domainToASCII(host), host);
    assert.equal(target.href, `https://${domainToASCII(host)}/path`);
    assert.equal(target.needsConfirm, true, host);
    assert.deepEqual(urls(`go to https://${host}/path now`), [`https://${host}/path`]);
  }
});

test("a written punycode host also asks first", () => {
  const target = parseLink("https://xn--80ak6aa92e.com/");
  assert.ok(target);
  assert.equal(target.host, "xn--80ak6aa92e.com");
  assert.equal(target.needsConfirm, true);
});

test("ASCII hosts open directly", () => {
  const target = parseLink("https://sub.example.com:8443/a?b=c#d");
  assert.deepEqual(target, {
    href: "https://sub.example.com:8443/a?b=c#d",
    host: "sub.example.com",
    needsConfirm: false,
  });
});

test("fullwidth letters fold to ASCII but still ask, because the label differed", () => {
  const target = parseLink("https://ｇｏｏｇｌｅ.com/");
  assert.ok(target);
  assert.equal(target.host, "google.com");
  assert.equal(target.needsConfirm, true);
});

test("lookalike slashes and dots in a host are refused", () => {
  assert.equal(findUrls("https://evil.example∕good.example/"), null);
  assert.equal(findUrls("https://evil.example⁄good.example/"), null);
  assert.equal(findUrls("https://evil.example／good.example/"), null);
  assert.equal(findUrls("https://ex%61mple.com/"), null);
  assert.equal(findUrls("https://exa_mple..com/"), null);
});

test("ports and IPv6 literals", () => {
  assert.equal(href("http://[::1]:8080/a"), "http://[::1]:8080/a");
  assert.equal(href("http://example.com:65535/"), "http://example.com:65535/");
  assert.equal(href("http://example.com:65536/"), null);
  assert.equal(href("http://example.com:abc/"), null);
  assert.equal(href("http://[::1/"), null);
});

test("non-ASCII paths are percent-encoded for the OS, the label is unchanged", () => {
  assert.deepEqual(urls("https://example.com/путь?q=ü"), ["https://example.com/путь?q=ü"]);
  assert.equal(
    href("https://example.com/путь?q=ü"),
    "https://example.com/%D0%BF%D1%83%D1%82%D1%8C?q=%C3%BC",
  );
  assert.equal(href("https://example.com/a|b{c}"), "https://example.com/a%7Cb%7Bc%7D");
  assert.equal(href("https://example.com/%7E"), "https://example.com/%7E");
});

test("very long tokens stay plain text; up to the cap they link", () => {
  const prefix = "https://example.com/";
  const atCap = prefix + "a".repeat(MAX_URL_LENGTH - prefix.length);
  const overCap = atCap + "a";
  assert.deepEqual(urls(`x ${atCap} y`), [atCap]);
  assert.equal(findUrls(`x ${overCap} y`), null);
});

test("scanning a large hostile body stays fast", () => {
  const body = `${"https://example.com/(((((".repeat(400)} ${"www.".repeat(2000)} https://example.com/${")".repeat(200_000)}`;
  const t0 = Date.now();
  findUrls(body);
  findUrls(`https://example.com/${")".repeat(2000)}`);
  assert.ok(Date.now() - t0 < 500);
});

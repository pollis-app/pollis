#!/usr/bin/env node
/**
 * check-learn-sync.mjs — every /learn topic says the same words in all three places (#1723).
 *
 * The narration script `learn/manim/scripts/<slug>.md` is the single source of
 * truth for a topic's words. They are copied into the caption track
 * (`website/learn/media/<slug>.vtt`, one cue per beat, same timings) and the
 * on-page transcript (`website/learn.html`, `#<slug>-transcript-body`, one <p>
 * per beat). A claim fixed in one copy and not the others leaves the site saying
 * two things at once (the audit in #1723 found all three copies had drifted
 * apart), so this fails on any beat whose words or timing differ.
 *
 * Words are compared after removing markup (`*em*`, `<em>`, VTT tags),
 * decoding entities, folding curly quotes to straight ones and collapsing
 * whitespace.
 *
 * Usage:
 *   node scripts/check-learn-sync.mjs            check (scripts-check.yml)
 *   node scripts/check-learn-sync.mjs --write    rewrite every .vtt cue and transcript <p> from the scripts
 *   --root <dir>                                 check another tree (the self-test)
 *
 * The .vtt files are served from pollis.com with the page (website-deploy.yml);
 * only the video, poster and webm stay on cdn.pollis.com (#697).
 */

import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { normalize } from "./lib/doc-text.mjs";

const args = process.argv.slice(2);
const ri = args.indexOf("--root");
const ROOT = resolve(ri !== -1 ? args[ri + 1] : join(dirname(fileURLToPath(import.meta.url)), ".."));
const WRITE = args.includes("--write");
const SCRIPTS = "learn/manim/scripts";
const PAGE = "website/learn.html";
const MEDIA = "website/learn/media";

const errors = [];

function fold(s) {
  return s.replace(/[‘’]/g, "'").replace(/[“”]/g, '"').replace(/\s+/g, " ").trim();
}

function toSeconds(t) {
  return t.split(":").map(Number).reduce((acc, n) => acc * 60 + n, 0);
}

function stamp(sec) {
  const m = String(Math.floor(sec / 60)).padStart(2, "0");
  const s = String(sec % 60).padStart(2, "0");
  return `${m}:${s}.000`;
}

const BEAT = /^\*\*\[(\d+:\d+)\s*[–-]\s*(\d+:\d+)\].*?\*\*(?=\s|$)(.*)$/;
const EMPHASIS = /(^|[\s("'])\*([^*\s][^*]*?)\*(?=[\s.,;:!?)"']|$)/g;

/** Script beats: `**[mm:ss–mm:ss] optional label**`, then narration until the next beat. */
function scriptBeats(raw) {
  const beats = [];
  let cur = null;
  for (const line of raw.split("\n")) {
    const m = BEAT.exec(line);
    if (m) {
      cur = { start: toSeconds(m[1]), end: toSeconds(m[2]), md: m[3] };
      beats.push(cur);
      continue;
    }
    if (!cur) {
      continue;
    }
    if (/^(---|#)/.test(line.trim())) {
      cur = null;
      continue;
    }
    cur.md += " " + line;
  }
  return beats.map((b) => {
    const md = b.md.replace(/\s+/g, " ").trim();
    return { ...b, md, text: fold(md.replace(EMPHASIS, "$1$2")) };
  });
}

function vttCues(raw) {
  const cues = [];
  for (const block of raw.replace(/\r/g, "").split(/\n\s*\n/)) {
    const lines = block.split("\n");
    const ti = lines.findIndex((l) => l.includes("-->"));
    if (ti === -1) {
      continue;
    }
    const [a, b] = lines[ti].split("-->").map((x) => x.trim().split(/\s/)[0].replace(/\.\d+$/, ""));
    const text = lines.slice(ti + 1).join(" ").replace(/<[^>]+>/g, "");
    cues.push({ start: toSeconds(a), end: toSeconds(b), text: fold(normalize(text, true).text) });
  }
  return cues;
}

function transcriptRange(page, slug) {
  const open = page.indexOf(`id="${slug}-transcript-body"`);
  if (open === -1) {
    return null;
  }
  const start = page.indexOf(">", open) + 1;
  return { start, end: page.indexOf("</div>", start) };
}

function transcriptParas(page, slug) {
  const r = transcriptRange(page, slug);
  if (r === null) {
    return null;
  }
  return [...page.slice(r.start, r.end).matchAll(/<p\b[^>]*>([\s\S]*?)<\/p>/g)].map((m) => ({ text: fold(normalize(m[1], true).text) }));
}

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function wrap(text, width, first, rest) {
  const out = [];
  let line = first;
  let empty = true;
  for (const w of text.split(" ")) {
    if (!empty && line.length + 1 + w.length > width) {
      out.push(line);
      line = rest + w;
    } else {
      line += (empty ? "" : " ") + w;
    }
    empty = false;
  }
  out.push(line);
  return out.join("\n");
}

function writeVtt(path, beats) {
  const old = existsSync(path) ? readFileSync(path, "utf8").replace(/\r/g, "") : "WEBVTT\n";
  const header = old.split(/\n\s*\n/).filter((b) => !b.includes("-->")).join("\n\n").trim();
  const cues = beats.map((b) => `${stamp(b.start)} --> ${stamp(b.end)}\n${b.md.replace(EMPHASIS, "$1$2")}`);
  writeFileSync(path, `${header}\n\n${cues.join("\n\n")}\n`);
}

function writeTranscript(page, slug, beats) {
  const r = transcriptRange(page, slug);
  const indent = (/\n([ \t]*)<p/.exec(page.slice(r.start, r.end)) ?? [, "            "])[1];
  const closeIndent = (/\n([ \t]*)$/.exec(page.slice(r.start, r.end)) ?? [, indent.slice(2)])[1];
  const paras = beats.map((b) => {
    const html = escapeHtml(b.md).replace(EMPHASIS, "$1<em>$2</em>");
    return wrap(html, 96, `${indent}<p>`, `${indent}  `) + "</p>";
  });
  return page.slice(0, r.start) + "\n" + paras.join("\n") + "\n" + closeIndent + page.slice(r.end);
}

/** Where two strings first differ, with a little context from each. */
function diffAt(a, b) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) {
    i++;
  }
  const from = Math.max(0, i - 30);
  return `script: "…${a.slice(from, i + 50)}…"\n        copy:   "…${b.slice(from, i + 50)}…"`;
}

let page = readFileSync(join(ROOT, PAGE), "utf8");
const slugs = readdirSync(join(ROOT, SCRIPTS)).filter((f) => f.endsWith(".md")).map((f) => f.slice(0, -3)).sort();
if (slugs.length === 0) {
  console.error(`${SCRIPTS}: no narration scripts found — refusing to pass on nothing.`);
  process.exit(1);
}

let beatsChecked = 0;
for (const slug of slugs) {
  const beats = scriptBeats(readFileSync(join(ROOT, SCRIPTS, `${slug}.md`), "utf8"));
  if (beats.length === 0) {
    errors.push(`${SCRIPTS}/${slug}.md: no **[mm:ss–mm:ss]** beats found — has the script format changed?`);
    continue;
  }
  const vttPath = `${MEDIA}/${slug}.vtt`;
  if (WRITE) {
    writeVtt(join(ROOT, vttPath), beats);
    if (transcriptRange(page, slug) !== null) {
      page = writeTranscript(page, slug, beats);
    }
  }
  const copies = [];
  if (!existsSync(join(ROOT, vttPath))) {
    errors.push(`${vttPath}: missing — run node scripts/check-learn-sync.mjs --write`);
  } else {
    copies.push([vttPath, vttCues(readFileSync(join(ROOT, vttPath), "utf8"))]);
  }
  if (!page.includes(`src="learn/media/${slug}.vtt"`)) {
    errors.push(`${PAGE}: the ${slug} <track> does not load learn/media/${slug}.vtt — the page must serve the caption file this repo checks`);
  }
  const paras = transcriptParas(page, slug);
  if (paras === null) {
    errors.push(`${PAGE}: no #${slug}-transcript-body — every scripted topic has an on-page transcript`);
  } else {
    copies.push([`${PAGE}#${slug}-transcript-body`, paras]);
  }
  for (const [where, items] of copies) {
    if (items.length !== beats.length) {
      errors.push(`${where}: ${items.length} cue(s)/paragraph(s), but ${SCRIPTS}/${slug}.md has ${beats.length} beat(s) — run node scripts/check-learn-sync.mjs --write`);
      continue;
    }
    beats.forEach((b, i) => {
      beatsChecked++;
      if (items[i].text !== b.text) {
        errors.push(`${where}: beat ${i + 1} differs from ${SCRIPTS}/${slug}.md — edit the script, then run node scripts/check-learn-sync.mjs --write\n        ${diffAt(b.text, items[i].text)}`);
      }
      if (items[i].start !== undefined && (items[i].start !== b.start || items[i].end !== b.end)) {
        errors.push(`${where}: cue ${i + 1} is ${items[i].start}s–${items[i].end}s, but the script beat is ${b.start}s–${b.end}s`);
      }
    });
  }
}
if (WRITE) {
  writeFileSync(join(ROOT, PAGE), page);
}

if (errors.length) {
  console.error(`\n/learn copies disagree with their narration scripts (${errors.length}):\n`);
  for (const e of errors) {
    console.error(`  ✗ ${e}`);
  }
  console.error(`\nThe script in ${SCRIPTS}/ is the source of truth; see learn/README.md.`);
  process.exit(1);
}
console.log(`/learn sync OK — ${slugs.length} topic(s), ${beatsChecked} beat copies match their scripts.`);

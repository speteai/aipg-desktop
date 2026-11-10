// Finds private values in text that is about to go to the grid (API keys,
// passwords, e-mail addresses, IBANs, card numbers, phone numbers) and swaps
// them for placeholders, so the machine that answers never sees the real
// values. The placeholders are put back in the answer.
//
// The same rules exist in src-tauri/src/privacy.rs for the connector; keep
// the two in step. This is a safety net for clearly shaped values, not a
// guarantee: confidential prose is not recognised.
(function (root) {
  const RULES = [
    ["secret", /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, 0],
    ["secret", /\bgrid_[A-Za-z0-9_-]{16,}/g, 0],
    ["secret", /\bsk-[A-Za-z0-9_-]{20,}/g, 0],
    ["secret", /\bgh[pousr]_[A-Za-z0-9]{30,}/g, 0],
    ["secret", /\bgithub_pat_[A-Za-z0-9_]{30,}/g, 0],
    ["secret", /\bAKIA[0-9A-Z]{16}\b/g, 0],
    ["secret", /\bxox[baprs]-[A-Za-z0-9-]{10,}/g, 0],
    ["secret", /\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/g, 0],
    ["secret", /\b0x[0-9a-fA-F]{64}\b/g, 0],
    ["secret", /\b(?:pass(?:word|wort)?|pwd|kennwort|secret|api[_-]?key|token)\b\s*[:=]\s*["']?([^\s"',;]{4,})/gi, 1],
    ["iban", /\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b/g, 0],
    ["card", /\b\d(?:[ -]?\d){12,18}\b/g, 0],
    ["email", /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g, 0],
    ["phone", /\+\d{1,3}[\d \-/()]{7,18}\d/g, 0],
  ];
  const TAG = { secret: "SECRET", iban: "IBAN", card: "CARD", email: "EMAIL", phone: "PHONE" };
  const LABEL = {
    secret: ["key or password", "keys or passwords"],
    iban: ["IBAN", "IBANs"],
    card: ["card number", "card numbers"],
    email: ["e-mail address", "e-mail addresses"],
    phone: ["phone number", "phone numbers"],
  };
  const NOTE = "Tokens such as [[EMAIL_1]] or [[SECRET_1]] in the text stand for private values that were removed. Use them exactly as written wherever the value belongs; do not alter, explain or guess them.";

  function ibanOk(s) {
    const c = s.replace(/\s/g, "");
    if (c.length < 15 || c.length > 34) return false;
    let rem = 0;
    for (const ch of c.slice(4) + c.slice(0, 4)) {
      let v;
      if (ch >= "0" && ch <= "9") v = ch.charCodeAt(0) - 48;
      else if (ch >= "A" && ch <= "Z") v = ch.charCodeAt(0) - 55;
      else return false;
      rem = v >= 10 ? (rem * 100 + v) % 97 : (rem * 10 + v) % 97;
    }
    return rem === 1;
  }
  function luhnOk(s) {
    const d = s.replace(/\D/g, "");
    if (d.length < 13 || d.length > 19) return false;
    let sum = 0;
    for (let i = 0; i < d.length; i++) {
      let x = d.charCodeAt(d.length - 1 - i) - 48;
      if (i % 2 === 1) { x *= 2; if (x > 9) x -= 9; }
      sum += x;
    }
    return sum % 10 === 0;
  }

  // Private values in `text` as {s, e, kind, value}, in order, not overlapping.
  // Where rules overlap, the earlier rule in the list wins.
  function find(text) {
    text = String(text == null ? "" : text);
    const hits = [];
    RULES.forEach(([kind, re, group], prio) => {
      re.lastIndex = 0;
      let m;
      while ((m = re.exec(text)) !== null) {
        if (m[0] === "") { re.lastIndex++; continue; }
        const value = m[group];
        if (value == null) continue;
        const s = m.index + m[0].indexOf(value, group ? 1 : 0);
        if (kind === "iban" && !ibanOk(value)) continue;
        if (kind === "card" && !luhnOk(value)) continue;
        hits.push({ s, e: s + value.length, kind, value, prio });
      }
    });
    hits.sort((a, b) => a.prio - b.prio || a.s - b.s);
    const kept = [];
    for (const h of hits) if (!kept.some((k) => h.s < k.e && k.s < h.e)) kept.push(h);
    kept.sort((a, b) => a.s - b.s);
    return kept;
  }

  // "2 e-mail addresses, 1 key or password"
  function summary(hits) {
    return ["secret", "iban", "card", "email", "phone"].map((k) => {
      const n = hits.filter((h) => h.kind === k).length;
      return n ? `${n} ${LABEL[k][n === 1 ? 0 : 1]}` : "";
    }).filter(Boolean).join(", ");
  }

  // Placeholders for one request. `skip` is a Set of values the user chose to
  // send as they are.
  function makeMask(skip) {
    const pairs = []; // [placeholder, original]
    return {
      get count() { return pairs.length; },
      apply(text) {
        text = String(text == null ? "" : text);
        const hits = find(text).filter((h) => !(skip && skip.has(h.value)));
        if (!hits.length) return text;
        let out = "", pos = 0;
        for (const h of hits) {
          out += text.slice(pos, h.s);
          let p = pairs.find((x) => x[1] === h.value);
          if (!p) {
            const n = pairs.filter((x) => x[0].startsWith(`[[${TAG[h.kind]}_`)).length + 1;
            p = [`[[${TAG[h.kind]}_${n}]]`, h.value];
            pairs.push(p);
          }
          out += p[0];
          pos = h.e;
        }
        return out + text.slice(pos);
      },
      // Models sometimes drop a bracket, so [EMAIL_1] is accepted as well.
      restore(text) {
        let out = String(text == null ? "" : text);
        for (const [ph, value] of pairs) {
          out = out.split(ph).join(value);
          out = out.split(ph.slice(1, -1)).join(value);
        }
        return out;
      },
    };
  }

  const api = { find, summary, makeMask, NOTE };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.Privacy = api;
})(typeof window !== "undefined" ? window : globalThis);

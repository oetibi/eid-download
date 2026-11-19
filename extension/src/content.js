// Eid Download content script: the "Download this video" button over <video> players.
//
// Design rules (docs/EXTENSION.md):
// * one closed Shadow DOM host, position: fixed, so page CSS cannot restyle it and it never
//   changes the page layout; it lives in <html>, or inside the fullscreen element while a
//   player is fullscreen;
// * no polling: media events (captured on the document), pointermove (rAF-throttled),
//   ResizeObserver, IntersectionObserver, fullscreenchange and YouTube's yt-navigate-finish;
// * nothing is sent anywhere until the user clicks a download choice, and then only the page
//   (or media file) address goes to the background script, which talks to the desktop app;
// * protected media (EME/DRM) never gets the button;
// * no innerHTML: every node is built with createElement and text goes through textContent.
(function () {
  "use strict";
  if (window.__eidDownloadLoaded) return;
  window.__eidDownloadLoaded = true;

  const api = typeof browser !== "undefined" ? browser : chrome;
  const Core = window.EidCore;
  if (!Core || !api || !api.runtime) return;

  const t = (key, subs) => {
    try {
      return api.i18n.getMessage(key, subs) || key;
    } catch (_) {
      return key;
    }
  };
  const uiLang = (() => {
    try {
      return api.i18n.getUILanguage() || "en";
    } catch (_) {
      return "en";
    }
  })();
  const RTL = /^(ar|fa|he|ur)\b/i.test(uiLang);

  // ------------------------------------------------------------------ state
  let settings = Core.normalizeSettings(null);
  let active = null; // the <video> the button belongs to
  let shown = false;
  let menuOpen = false;
  let hideTimer = 0;
  let rafPending = false;
  let lastPointer = null;
  const candidates = new Set();
  const encrypted = new WeakSet();
  const visibleRatio = new WeakMap();
  const probeCache = new Map(); // url -> Promise<{ok,result|code}>

  // ------------------------------------------------------------------ DOM helpers
  function h(tag, attrs, kids) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k of Object.keys(attrs)) {
        const v = attrs[k];
        if (v === undefined || v === null || v === false) continue;
        if (k === "text") el.textContent = v;
        else if (k === "on") for (const ev of Object.keys(v)) el.addEventListener(ev, v[ev]);
        else el.setAttribute(k, v === true ? "" : String(v));
      }
    }
    for (const c of kids || []) if (c) el.appendChild(c);
    return el;
  }

  function icon() {
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("class", "ico");
    const p = document.createElementNS(NS, "path");
    p.setAttribute("d", "M12 3v11m0 0l-4.5-4.5M12 14l4.5-4.5M4.5 17.5v2h15v-2");
    p.setAttribute("fill", "none");
    p.setAttribute("stroke", "currentColor");
    p.setAttribute("stroke-width", "2.2");
    p.setAttribute("stroke-linecap", "round");
    p.setAttribute("stroke-linejoin", "round");
    svg.appendChild(p);
    return svg;
  }

  const CSS = `
:host { all: initial; }
* { box-sizing: border-box; }
.wrap { position: fixed; inset: 0 auto auto 0; width: 0; height: 0; pointer-events: none;
  font: 13px/1.35 system-ui, "Segoe UI", Tahoma, Arial, sans-serif; color: #111827; }
.btn { position: fixed; pointer-events: auto; display: none; align-items: center; gap: 6px;
  border: 0; margin: 0; cursor: pointer; color: #fff; background: var(--c);
  height: var(--px); min-width: var(--px); padding: 0 calc(var(--px) * .32);
  border-radius: var(--r); opacity: var(--o); font: 600 var(--fs)/1 inherit;
  font-family: inherit; box-shadow: 0 2px 8px rgba(0,0,0,.35); white-space: nowrap;
  transition: opacity .15s ease, transform .15s ease; }
.btn.icon-only { padding: 0; justify-content: center; width: var(--px); }
.btn.show { display: inline-flex; }
.btn:hover, .btn:focus-visible { opacity: 1; transform: translateY(-1px); }
.btn:focus-visible { outline: 2px solid #fff; outline-offset: 2px; }
.ico { width: calc(var(--px) * .56); height: calc(var(--px) * .56); flex: none; }
.menu { position: fixed; pointer-events: auto; display: none; width: 300px; max-width: calc(100vw - 16px);
  max-height: min(420px, calc(100vh - 16px)); overflow: auto; background: #fff; color: #111827;
  border-radius: 10px; box-shadow: 0 8px 28px rgba(0,0,0,.35); padding: 6px 0; }
.menu.open { display: block; }
.head { display: flex; align-items: center; gap: 8px; padding: 6px 12px 8px; border-bottom: 1px solid #e5e7eb; }
.title { flex: 1; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.x { border: 0; background: transparent; cursor: pointer; font: 18px/1 inherit; color: #6b7280; padding: 2px 6px; border-radius: 6px; }
.x:hover { background: #f3f4f6; color: #111827; }
.sec { padding: 6px 0; }
.sec + .sec { border-top: 1px solid #e5e7eb; }
.lbl { padding: 2px 12px 4px; color: #6b7280; font-size: 12px; font-weight: 600; }
.it { display: flex; width: 100%; align-items: center; gap: 8px; border: 0; background: transparent;
  text-align: start; padding: 7px 12px; cursor: pointer; font: inherit; color: inherit; }
.it:hover, .it:focus-visible { background: #f0fdfa; outline: none; }
.it[disabled] { opacity: .5; cursor: default; }
.muted { padding: 6px 12px; color: #6b7280; }
.err { padding: 6px 12px; color: #b91c1c; }
.ok { padding: 6px 12px; color: #15803d; }
.row { display: flex; align-items: center; gap: 8px; padding: 4px 12px; }
select { font: inherit; padding: 3px 6px; border: 1px solid #d1d5db; border-radius: 6px; background: #fff; color: inherit; }
.link { border: 0; background: transparent; color: #0f766e; cursor: pointer; font: inherit; padding: 2px 4px; text-decoration: underline; }
.list { max-height: 220px; overflow: auto; border-top: 1px solid #e5e7eb; border-bottom: 1px solid #e5e7eb; }
.pick { display: flex; align-items: center; gap: 8px; padding: 5px 12px; cursor: pointer; }
.pick:hover { background: #f9fafb; }
.pick .n { color: #6b7280; font-variant-numeric: tabular-nums; min-width: 2.5em; }
.pick .tt { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pick .d { color: #6b7280; font-variant-numeric: tabular-nums; }
.primary { margin: 8px 12px 4px; width: calc(100% - 24px); justify-content: center; background: #0f766e; color: #fff; border-radius: 8px; font-weight: 600; }
.primary:hover, .primary:focus-visible { background: #115e59; }
.spin { display: inline-block; width: 12px; height: 12px; border: 2px solid #d1d5db; border-top-color: #0f766e; border-radius: 50%; animation: s 0.8s linear infinite; vertical-align: -2px; margin-inline-end: 6px; }
@keyframes s { to { transform: rotate(360deg); } }
@media (prefers-color-scheme: dark) {
  .menu { background: #1f2937; color: #f3f4f6; }
  .head, .sec + .sec, .list { border-color: #374151; }
  .x:hover, .pick:hover { background: #374151; color: #f3f4f6; }
  .it:hover, .it:focus-visible { background: #134e4a; }
  .lbl, .muted, .pick .n, .pick .d { color: #9ca3af; }
  select { background: #111827; border-color: #4b5563; }
  .link { color: #5eead4; }
  .err { color: #fca5a5; } .ok { color: #86efac; }
}
@media (prefers-reduced-motion: reduce) { .btn, .spin { transition: none; animation: none; } }
`;

  // ------------------------------------------------------------------ shadow host
  let hostEl = null;
  let ui = null;

  function ensureUi() {
    if (ui) return ui;
    hostEl = document.createElement("eid-download-overlay");
    // inline !important beats any page rule aimed at our element
    for (const [k, v] of [
      ["all", "initial"],
      ["position", "fixed"],
      ["top", "0"],
      ["left", "0"],
      ["width", "0"],
      ["height", "0"],
      ["z-index", "2147483647"],
      ["display", "block"],
    ]) {
      hostEl.style.setProperty(k, v, "important");
    }
    const root = hostEl.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent = CSS;
    const btn = h("button", { class: "btn", type: "button", title: t("btnTitle") });
    const btnText = h("span", { class: "txt" });
    btn.appendChild(icon());
    btn.appendChild(btnText);
    const menu = h("div", { class: "menu", role: "dialog", "aria-label": t("menuTitle") });
    const wrap = h("div", { class: "wrap", dir: RTL ? "rtl" : "ltr", lang: RTL ? "ar" : "en" }, [
      btn,
      menu,
    ]);
    root.appendChild(style);
    root.appendChild(wrap);
    // nothing we do may reach the page's own handlers (YouTube toggles play on click, and has
    // keyboard shortcuts on the document)
    for (const ev of [
      "click",
      "dblclick",
      "mousedown",
      "mouseup",
      "pointerdown",
      "pointerup",
      "keydown",
      "keyup",
      "keypress",
      "wheel",
      "contextmenu",
    ]) {
      wrap.addEventListener(ev, (e) => e.stopPropagation());
    }
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      if (menuOpen) closeMenu();
      else openMenu();
    });
    wrap.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        closeMenu();
        btn.focus();
      }
    });
    wrap.addEventListener("pointerenter", () => clearTimeout(hideTimer), true);
    ui = { root, wrap, btn, btnText, menu };
    applyStyle();
    attachHost();
    return ui;
  }

  function attachHost() {
    if (!hostEl) return;
    const fs = document.fullscreenElement || document.webkitFullscreenElement || null;
    const parent =
      fs && fs !== active && active && fs.contains(active) ? fs : document.documentElement;
    if (hostEl.parentNode !== parent) parent.appendChild(hostEl);
  }

  function applyStyle() {
    if (!ui) return;
    const st = Core.buttonStyle(settings);
    const b = ui.btn;
    b.style.setProperty("--px", st.px + "px");
    b.style.setProperty("--c", st.color);
    b.style.setProperty("--r", st.radius + "px");
    b.style.setProperty("--o", String(st.opacity));
    b.style.setProperty("--fs", Math.max(12, Math.round(st.px * 0.38)) + "px");
    b.classList.toggle("icon-only", st.iconOnly);
    ui.btnText.textContent = st.iconOnly ? "" : labelFor(active);
    b.setAttribute("aria-label", labelFor(active));
  }

  function labelFor(v) {
    const cls = Core.classifyUrl(location.href);
    return v && cls.kind === "web" ? t("btnLabelFile") : t("btnLabel");
  }

  // ------------------------------------------------------------------ eligibility
  function snapshot(v) {
    const r = v.getBoundingClientRect();
    let src = "";
    try {
      src = v.currentSrc || v.src || "";
    } catch (_) {
      src = "";
    }
    return {
      src,
      mediaKeys: !!v.mediaKeys,
      encrypted: encrypted.has(v),
      width: r.width,
      height: r.height,
      isAd: !!(v.closest && v.closest(".ad-showing, .ad-interrupting")),
    };
  }

  function verdict(v) {
    if (!v || !v.isConnected) return "none";
    return Core.eligibility(snapshot(v), Core.classifyUrl(location.href));
  }

  function siteAllowed() {
    if (!settings.enabled) return false;
    if (Core.isExcluded(location.hostname, settings)) return false;
    // inside an embed, honour the exclusion of the page that embeds us (Chrome/Edge only:
    // Firefox has no ancestorOrigins)
    const anc = location.ancestorOrigins;
    if (anc) {
      for (let i = 0; i < anc.length; i++) {
        try {
          if (Core.isExcluded(new URL(anc[i]).hostname, settings)) return false;
        } catch (_) {
          /* opaque origin */
        }
      }
    }
    return true;
  }

  // ------------------------------------------------------------------ observers
  const ro = typeof ResizeObserver === "function" ? new ResizeObserver(() => schedule()) : null;
  const io =
    typeof IntersectionObserver === "function"
      ? new IntersectionObserver(
          (entries) => {
            for (const e of entries) visibleRatio.set(e.target, e.intersectionRatio);
            schedule();
          },
          { threshold: [0, 0.25, 0.5, 0.75, 1] },
        )
      : null;

  function track(v) {
    if (!(v instanceof HTMLVideoElement) || candidates.has(v)) return;
    candidates.add(v);
    if (io) io.observe(v);
    v.addEventListener("encrypted", () => {
      encrypted.add(v);
      schedule();
    });
  }

  function untrackGone() {
    for (const v of candidates) {
      if (!v.isConnected) {
        candidates.delete(v);
        if (io) io.unobserve(v);
        if (v === active) setActive(null);
      }
    }
  }

  function setActive(v) {
    if (active === v) return;
    if (active && ro) ro.unobserve(active);
    active = v;
    if (active && ro) ro.observe(active);
    if (menuOpen) closeMenu();
    if (ui) applyStyle();
    attachHost();
  }

  function pointIn(r, p) {
    return p && p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom;
  }

  /** Pick the video the button belongs to, given the pointer / visibility mode. */
  function chooseVideo() {
    untrackGone();
    let best = null;
    let bestArea = 0;
    for (const v of candidates) {
      if ((visibleRatio.get(v) ?? 1) < 0.25) continue;
      const r = v.getBoundingClientRect();
      if (settings.visibility === "hover" && !pointIn(r, lastPointer)) continue;
      const ok = verdict(v);
      if (ok !== "youtube" && ok !== "file") continue;
      const area = r.width * r.height;
      if (area > bestArea) {
        best = v;
        bestArea = area;
      }
    }
    return best;
  }

  function schedule() {
    if (rafPending) return;
    rafPending = true;
    requestAnimationFrame(() => {
      rafPending = false;
      update();
    });
  }

  function update() {
    if (!siteAllowed()) {
      hide(true);
      return;
    }
    if (!menuOpen) {
      const v = chooseVideo();
      if (v) {
        clearTimeout(hideTimer);
        setActive(v);
        show();
      } else if (shown && !overUi()) {
        // hover mode: linger a little so the user can reach the button
        clearTimeout(hideTimer);
        hideTimer = setTimeout(() => {
          if (!menuOpen && !overUi()) hide(false);
        }, settings.visibility === "hover" ? 1200 : 0);
      }
    } else if (active && verdict(active) === "drm") {
      hide(true);
      return;
    }
    position();
  }

  function overUi() {
    if (!ui || !lastPointer) return false;
    const r = ui.btn.getBoundingClientRect();
    if (shown && pointIn(r, lastPointer)) return true;
    return menuOpen && pointIn(ui.menu.getBoundingClientRect(), lastPointer);
  }

  function show() {
    ensureUi();
    attachHost();
    if (!shown) {
      shown = true;
      ui.btn.classList.add("show");
      applyStyle();
    }
  }

  function hide(force) {
    if (!ui) return;
    if (menuOpen && !force) return;
    if (menuOpen) closeMenu();
    shown = false;
    ui.btn.classList.remove("show");
  }

  function position() {
    if (!ui || !shown || !active) return;
    const fs = document.fullscreenElement || document.webkitFullscreenElement;
    if (fs === active) {
      // the <video> itself is fullscreen: nothing else can be drawn over it
      ui.btn.classList.remove("show");
      return;
    }
    const r = active.getBoundingClientRect();
    const bw = ui.btn.offsetWidth || 34;
    const bh = ui.btn.offsetHeight || 34;
    const pos = Core.placeButton(r, settings, Core.classifyUrl(location.href), bw, bh);
    if (!pos || r.bottom < 0 || r.top > innerHeight) {
      ui.btn.classList.remove("show");
      if (menuOpen) closeMenu();
      return;
    }
    ui.btn.classList.add("show");
    ui.btn.style.top = pos.top + "px";
    ui.btn.style.left = pos.left + "px";
    if (menuOpen) placeMenu();
  }

  function placeMenu() {
    const b = ui.btn.getBoundingClientRect();
    const m = ui.menu;
    const mw = m.offsetWidth || 300;
    const mh = m.offsetHeight || 200;
    let left = settings.position === "top-left" ? b.left : b.right - mw;
    left = Math.max(8, Math.min(left, innerWidth - mw - 8));
    let top = b.bottom + 6;
    if (top + mh > innerHeight - 8) top = Math.max(8, innerHeight - mh - 8);
    m.style.left = Math.round(left) + "px";
    m.style.top = Math.round(top) + "px";
  }

  // ------------------------------------------------------------------ messaging
  function send(msg) {
    msg.eid = 1;
    try {
      return Promise.resolve(api.runtime.sendMessage(msg)).then(
        (r) => r || { ok: false, code: "failed" },
        () => ({ ok: false, code: "failed" }),
      );
    } catch (_) {
      // the extension was reloaded/updated under this page
      return Promise.resolve({ ok: false, code: "failed" });
    }
  }

  function probe(url) {
    let p = probeCache.get(url);
    if (!p) {
      p = send({ cmd: "probe", url }).then((r) => {
        if (!r.ok) probeCache.delete(url); // failures are not cached: "Try again" re-asks
        return r;
      });
      probeCache.set(url, p);
    }
    return p;
  }

  const ERR_KEYS = {
    not_installed: "errNotInstalled",
    not_allowed: "errNotAllowed",
    app_not_running: "errAppNotRunning",
    app_too_old: "errAppTooOld",
    tools_missing: "errToolsMissing",
    drm: "errDrm",
    unsupported: "errUnsupported",
    timeout: "errTimeout",
    failed: "errFailed",
    no_qualities: "errNoQualities",
  };
  const errText = (code) => t(ERR_KEYS[code] || "errFailed");

  const Q_KEYS = {
    best: "qBest",
    audio: "qAudio",
    "2160p": "q2160",
    "1440p": "q1440",
    "1080p": "q1080",
    "720p": "q720",
    "480p": "q480",
    "360p": "q360",
  };
  // the menu lists "best" first, then heights from tall to short, then audio
  function menuOrder(qs) {
    const f = Core.filterQualities(qs);
    const out = [];
    if (f.indexOf("best") !== -1) out.push("best");
    for (const q of f) if (q !== "best" && q !== "audio") out.push(q);
    if (f.indexOf("audio") !== -1) out.push("audio");
    return out;
  }

  function fmtDuration(s) {
    if (typeof s !== "number" || !Number.isFinite(s) || s < 0) return "";
    s = Math.round(s);
    const hh = Math.floor(s / 3600);
    const mm = Math.floor((s % 3600) / 60);
    const ss = String(s % 60).padStart(2, "0");
    return hh ? `${hh}:${String(mm).padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
  }

  // ------------------------------------------------------------------ menu
  let status = null; // the status line element of the current view
  let menuToken = 0; // invalidates late probe answers after the menu closed/changed

  function setStatus(kind, text, retry) {
    if (!status) return;
    status.textContent = "";
    status.className = kind;
    if (kind === "muted" && text) status.appendChild(h("span", { class: "spin" }));
    status.appendChild(document.createTextNode(text || ""));
    if (retry) {
      status.appendChild(document.createTextNode(" "));
      status.appendChild(h("button", { class: "link", type: "button", text: t("retry"), on: { click: retry } }));
    }
  }

  async function act(msg) {
    setStatus("muted", t("sending"));
    const r = await send(msg);
    if (r.ok) setStatus("ok", t("sent"));
    else setStatus("err", errText(r.code));
    return r;
  }

  function header(title) {
    const ttl = h("div", { class: "title", text: title || t("menuTitle") });
    const x = h("button", {
      class: "x",
      type: "button",
      "aria-label": t("menuClose"),
      title: t("menuClose"),
      text: "×",
      on: { click: () => closeMenu() },
    });
    return { el: h("div", { class: "head" }, [ttl, x]), ttl };
  }

  function openMenu() {
    ensureUi();
    menuOpen = true;
    clearTimeout(hideTimer);
    ui.btn.setAttribute("aria-expanded", "true");
    renderMain();
    ui.menu.classList.add("open");
    placeMenu();
    const first = ui.menu.querySelector("button.it, button.x");
    if (first) first.focus({ preventScroll: true });
  }

  function closeMenu() {
    if (!ui) return;
    menuOpen = false;
    menuToken++;
    status = null;
    ui.menu.classList.remove("open");
    ui.menu.textContent = "";
    ui.btn.setAttribute("aria-expanded", "false");
  }

  function renderMain() {
    const token = ++menuToken;
    const m = ui.menu;
    m.textContent = "";
    const cls = Core.classifyUrl(location.href);
    const v = active;
    const hd = header(t("menuTitle"));
    m.appendChild(hd.el);
    status = h("div", { class: "muted" });

    if (cls.kind === "web") {
      // a plain HTML5 video file: hand the file address to the download engine
      const src = v ? snapshot(v).src : "";
      const sec = h("div", { class: "sec" }, [
        h("button", {
          class: "it",
          type: "button",
          text: t("menuDownloadFile"),
          on: { click: () => act({ cmd: "add", url: src }) },
        }),
      ]);
      m.appendChild(sec);
      m.appendChild(status);
      setStatus("", "");
      return;
    }

    if (!cls.videoUrl) {
      m.appendChild(h("div", { class: "err", text: t("errUnsupported") }));
      return;
    }

    const vsec = h("div", { class: "sec" }, [h("div", { class: "lbl", text: t("menuThisVideo") })]);
    const vbody = h("div");
    vsec.appendChild(vbody);
    m.appendChild(vsec);

    let psec = null;
    let pbody = null;
    if (cls.playlistUrl) {
      pbody = h("div");
      psec = h("div", { class: "sec" }, [h("div", { class: "lbl", text: t("menuPlaylist") }), pbody]);
      m.appendChild(psec);
    }
    m.appendChild(status);
    setStatus("", "");

    const loadVideo = () => {
      vbody.textContent = "";
      const ld = h("div", { class: "muted" }, [h("span", { class: "spin" })]);
      ld.appendChild(document.createTextNode(t("menuLoading")));
      vbody.appendChild(ld);
      probe(cls.videoUrl).then((r) => {
        if (token !== menuToken) return;
        vbody.textContent = "";
        if (!r.ok) {
          const e = h("div", { class: "err", text: errText(r.code) + " " });
          e.appendChild(h("button", { class: "link", type: "button", text: t("retry"), on: { click: loadVideo } }));
          vbody.appendChild(e);
          placeMenu();
          return;
        }
        if (r.result.title) hd.ttl.textContent = r.result.title;
        const qs = menuOrder(r.result.qualities);
        if (!qs.length) vbody.appendChild(h("div", { class: "err", text: errText("no_qualities") }));
        for (const q of qs) {
          vbody.appendChild(
            h("button", {
              class: "it",
              type: "button",
              "data-q": q,
              text: t(Q_KEYS[q]),
              on: { click: () => act({ cmd: "add_video", url: cls.videoUrl, quality: q }) },
            }),
          );
        }
        placeMenu();
      });
    };
    loadVideo();

    if (pbody) {
      const loadList = () => {
        pbody.textContent = "";
        const ld = h("div", { class: "muted" }, [h("span", { class: "spin" })]);
        ld.appendChild(document.createTextNode(t("menuPlaylistLoading")));
        pbody.appendChild(ld);
        probe(cls.playlistUrl).then((r) => {
          if (token !== menuToken) return;
          pbody.textContent = "";
          if (!r.ok || r.result.kind !== "playlist") {
            if (!r.ok) {
              const e = h("div", { class: "err", text: errText(r.code) + " " });
              e.appendChild(h("button", { class: "link", type: "button", text: t("retry"), on: { click: loadList } }));
              pbody.appendChild(e);
            } else {
              psec.remove(); // not a real playlist after all: offer nothing
            }
            placeMenu();
            return;
          }
          const pl = r.result;
          const qs = menuOrder(pl.qualities);
          if (!qs.length || !pl.items.length) {
            psec.remove();
            placeMenu();
            return;
          }
          const sel = h("select", { "aria-label": t("menuPlaylistQuality") });
          for (const q of qs) sel.appendChild(h("option", { value: q, text: t(Q_KEYS[q]) }));
          pbody.appendChild(
            h("div", { class: "row" }, [
              h("span", { class: "tt", text: (pl.title ? pl.title + " · " : "") + t("menuPlaylistCount", [String(pl.items.length)]) }),
            ]),
          );
          pbody.appendChild(
            h("div", { class: "row" }, [h("span", { text: t("menuPlaylistQuality") }), sel]),
          );
          pbody.appendChild(
            h("button", {
              class: "it",
              type: "button",
              "data-act": "whole",
              text: t("menuWholePlaylist"),
              on: {
                click: () => act({ cmd: "add_playlist", url: cls.playlistUrl, quality: sel.value }),
              },
            }),
          );
          pbody.appendChild(
            h("button", {
              class: "it",
              type: "button",
              "data-act": "choose",
              text: t("menuChooseItems"),
              on: { click: () => renderPicker(cls.playlistUrl, pl, sel.value) },
            }),
          );
          placeMenu();
        });
      };
      loadList();
    }
  }

  function renderPicker(url, pl, quality) {
    ++menuToken;
    const m = ui.menu;
    m.textContent = "";
    const hd = header(t("pickTitle"));
    m.appendChild(hd.el);
    const chosen = new Set(pl.items.map((i) => i.index));
    const count = h("span", { class: "muted" });
    const boxes = [];
    const refresh = () => {
      count.textContent = t("pickSelected", [String(chosen.size), String(pl.items.length)]);
      go.disabled = chosen.size === 0;
    };
    const setAll = (on) => {
      for (const b of boxes) {
        b.checked = on;
        if (on) chosen.add(Number(b.value));
        else chosen.delete(Number(b.value));
      }
      refresh();
    };
    m.appendChild(
      h("div", { class: "row" }, [
        h("button", { class: "link", type: "button", "data-act": "all", text: t("pickAll"), on: { click: () => setAll(true) } }),
        h("button", { class: "link", type: "button", "data-act": "none", text: t("pickNone"), on: { click: () => setAll(false) } }),
        count,
      ]),
    );
    const list = h("div", { class: "list" });
    for (const it of pl.items) {
      const cb = h("input", { type: "checkbox", value: String(it.index), checked: true });
      cb.addEventListener("change", () => {
        if (cb.checked) chosen.add(it.index);
        else chosen.delete(it.index);
        refresh();
      });
      boxes.push(cb);
      list.appendChild(
        h("label", { class: "pick" }, [
          cb,
          h("span", { class: "n", text: String(it.index).padStart(3, "0") }),
          h("span", { class: "tt", text: it.title || t("pickUntitled", [String(it.index)]) }),
          h("span", { class: "d", text: fmtDuration(it.duration) }),
        ]),
      );
    }
    m.appendChild(list);
    const go = h("button", {
      class: "it primary",
      type: "button",
      "data-act": "download-selected",
      text: t("pickDownload"),
      on: {
        click: () => {
          const items = Array.from(chosen).sort((a, b) => a - b);
          // all selected = the whole playlist (lets the app also pick up new items later)
          const msg = { cmd: "add_playlist", url, quality };
          if (items.length !== pl.items.length) msg.items = items;
          act(msg);
        },
      },
    });
    m.appendChild(go);
    m.appendChild(
      h("div", { class: "row" }, [
        h("button", { class: "link", type: "button", text: t("pickBack"), on: { click: () => renderMain() } }),
      ]),
    );
    status = h("div", { class: "muted" });
    m.appendChild(status);
    setStatus("", "");
    refresh();
    placeMenu();
  }

  // ------------------------------------------------------------------ events
  const onMedia = (e) => {
    if (e.target instanceof HTMLVideoElement) {
      track(e.target);
      schedule();
    }
  };
  for (const ev of ["loadedmetadata", "play", "playing", "emptied", "loadstart"]) {
    document.addEventListener(ev, onMedia, true);
  }
  // `encrypted` is not composed/bubbling either, so capture catches every <video>
  document.addEventListener(
    "encrypted",
    (e) => {
      if (e.target instanceof HTMLVideoElement) {
        encrypted.add(e.target);
        schedule();
      }
    },
    true,
  );

  document.addEventListener(
    "pointermove",
    (e) => {
      lastPointer = { x: e.clientX, y: e.clientY };
      schedule();
    },
    { capture: true, passive: true },
  );
  document.addEventListener(
    "pointerdown",
    (e) => {
      if (menuOpen && hostEl && !e.composedPath().includes(hostEl)) closeMenu();
    },
    true,
  );
  window.addEventListener("scroll", schedule, { capture: true, passive: true });
  window.addEventListener("resize", schedule, { passive: true });
  const onFs = () => {
    attachHost();
    schedule();
  };
  document.addEventListener("fullscreenchange", onFs);
  document.addEventListener("webkitfullscreenchange", onFs);
  const onNavigate = () => {
    closeMenu();
    probeCache.clear();
    hide(true);
    for (const v of document.getElementsByTagName("video")) track(v);
    schedule();
  };
  // YouTube is a single-page app: its own event marks the end of a navigation
  document.addEventListener("yt-navigate-finish", onNavigate);
  window.addEventListener("popstate", onNavigate);

  // settings (storage.sync) and live updates from the options page
  try {
    Promise.resolve(api.storage.sync.get(null)).then(
      (s) => {
        settings = Core.normalizeSettings(s);
        applyStyle();
        schedule();
      },
      () => {},
    );
    api.storage.onChanged.addListener((_changes, area) => {
      if (area !== "sync") return;
      Promise.resolve(api.storage.sync.get(null)).then((s) => {
        settings = Core.normalizeSettings(s);
        applyStyle();
        schedule();
      });
    });
  } catch (_) {
    /* storage unavailable: defaults */
  }

  for (const v of document.getElementsByTagName("video")) track(v);
  schedule();
})();

// Eid Download browser extension: pure logic shared by the content script, the background
// script and the options page. No DOM, no browser APIs: everything here is unit-tested with
// `node --test` (extension/test/core.test.js).
//
// Loaded as a classic script (content scripts and the Chrome service worker cannot import ES
// modules declaratively) and as a CommonJS module by the tests.
(function (root) {
  "use strict";

  /** Native Messaging host name registered by Eid Download (crates/eid-native-host). */
  const HOST_NAME = "com.eid.download";
  /** eid-ipc protocol version (crates/eid-ipc/src/proto.rs `VERSION`). */
  const PROTOCOL_VERSION = 1;
  /** Bounds mirrored from crates/eid-ipc/src/proto.rs. */
  const MAX_URL_LEN = 8192;
  const MAX_ID_LEN = 64;
  /** Upper bound on playlist items we display or send (a sanity limit, not a policy). */
  const MAX_ITEMS = 5000;
  const MAX_TEXT = 300;

  /**
   * Owner's quality policy (2026-11-20): fixed choices from 144p to 4320p (8K), nothing taller.
   * Order is the display order (tallest first).
   */
  const QUALITIES = ["4320p", "2160p", "1440p", "1080p", "720p", "480p", "360p", "240p", "144p"];
  const SPECIAL = ["best", "m4a", "audio"];
  /** v0.2: exact format ids from the app's own probe (`299+140`), the protocol's rule. */
  const FORMAT_SPEC = /^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,63}(\+[A-Za-z0-9_.][A-Za-z0-9_.-]{0,63})?$/;
  const ALL_QUALITIES = QUALITIES.concat(SPECIAL);
  const MAX_HEIGHT = 4320;

  const YT_HOSTS = new Set([
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
  ]);
  const VIDEO_ID = /^[A-Za-z0-9_-]{11}$/;
  const LIST_ID = /^[A-Za-z0-9_-]{2,64}$/;

  function parseUrl(s) {
    if (typeof s !== "string" || s.length === 0 || s.length > MAX_URL_LEN) return null;
    try {
      return new URL(s);
    } catch (_) {
      return null;
    }
  }

  /**
   * Playlists we offer: real, public-by-link playlists. Not offered: Watch Later / Liked
   * (`WL`, `LL`: private, would need the user's cookies, which we never send) and Mixes
   * (`RD…`: generated, endless and personal).
   */
  function isOfferablePlaylist(list) {
    if (!list || !LIST_ID.test(list)) return false;
    if (list === "WL" || list === "LL" || list === "LM") return false;
    if (list.startsWith("RD")) return false;
    return true;
  }

  /**
   * Classify a page (or frame) URL.
   * Returns one of:
   *   { kind: "youtube", type: "watch"|"shorts"|"embed", videoId, videoUrl, playlistId?, playlistUrl? }
   *   { kind: "youtube", type: "playlist", playlistId, playlistUrl }
   *   { kind: "youtube", type: "other" }        (home, search, channel…: no single video)
   *   { kind: "web" }                           (any other http(s) page)
   *   { kind: "unsupported" }                   (not http(s), credentials, too long…)
   */
  function classifyUrl(s) {
    const u = parseUrl(s);
    if (!u || (u.protocol !== "https:" && u.protocol !== "http:")) return { kind: "unsupported" };
    if (u.username || u.password) return { kind: "unsupported" };
    const host = u.hostname.toLowerCase();
    if (!YT_HOSTS.has(host) && host !== "youtu.be") return { kind: "web" };

    let videoId = null;
    let type = "other";
    const parts = u.pathname.split("/").filter(Boolean);
    if (host === "youtu.be") {
      if (parts.length >= 1 && VIDEO_ID.test(parts[0])) {
        videoId = parts[0];
        type = "watch";
      }
    } else if (parts[0] === "watch") {
      const v = u.searchParams.get("v");
      if (v && VIDEO_ID.test(v)) {
        videoId = v;
        type = "watch";
      }
    } else if (parts[0] === "shorts" || parts[0] === "live") {
      if (parts[1] && VIDEO_ID.test(parts[1])) {
        videoId = parts[1];
        type = parts[0] === "shorts" ? "shorts" : "watch";
      }
    } else if (parts[0] === "embed") {
      if (parts[1] && VIDEO_ID.test(parts[1])) {
        videoId = parts[1];
        type = "embed";
      }
    } else if (parts[0] === "playlist") {
      type = "playlist";
    }

    const list = u.searchParams.get("list");
    const playlistId = isOfferablePlaylist(list) ? list : null;
    const out = { kind: "youtube", type };
    if (videoId) {
      out.videoId = videoId;
      // canonical, tracking-free form; the desktop app gets nothing else from the page
      out.videoUrl = "https://www.youtube.com/watch?v=" + videoId;
    }
    if (playlistId && type !== "shorts") {
      out.playlistId = playlistId;
      out.playlistUrl = "https://www.youtube.com/playlist?list=" + playlistId;
    }
    if (type === "playlist" && !out.playlistId) out.type = "other";
    if (type !== "other" && type !== "playlist" && !videoId) out.type = "other";
    return out;
  }

  /**
   * v0.2: the address "Open in Eid Download" sends for a classified page: the video with its
   * playlist (`watch?v=X&list=Y`, so the app offers "this video" or "the whole playlist"), the
   * video alone, or the playlist alone; null for anything else. Canonical, tracking-free.
   */
  function openInAppUrl(cls) {
    if (!cls || cls.kind !== "youtube") return null;
    if (cls.videoId && cls.playlistId) {
      return "https://www.youtube.com/watch?v=" + cls.videoId + "&list=" + cls.playlistId;
    }
    return cls.videoUrl || cls.playlistUrl || null;
  }

  /** Height of a quality label ("1080p" -> 1080), or null for best/audio/garbage. */
  function heightOf(q) {
    const m = /^(\d{3,4})p$/.exec(q);
    return m ? Number(m[1]) : null;
  }

  /**
   * Keep only policy qualities, in policy order, without duplicates. Anything taller than
   * 4320p ("8640p"…), a height outside the fixed list ("2880p") or unknown is dropped. `best` and `audio` are kept only if
   * the app offered them (the app knows whether audio extraction is possible).
   */
  function filterQualities(list) {
    if (!Array.isArray(list)) return [];
    const have = new Set();
    for (const q of list) {
      if (typeof q !== "string") continue;
      if (ALL_QUALITIES.indexOf(q) === -1) continue;
      const h = heightOf(q);
      if (h !== null && h > MAX_HEIGHT) continue;
      have.add(q);
    }
    return ALL_QUALITIES.filter((q) => have.has(q));
  }

  function isPolicyQuality(q) {
    return typeof q === "string" && ALL_QUALITIES.indexOf(q) !== -1;
  }

  function cleanText(s, max) {
    if (typeof s !== "string") return "";
    // drop control characters and bidi overrides (U+202A..U+202E, U+2066..U+2069)
    // eslint-disable-next-line no-control-regex
    const t = s.replace(/[\u0000-\u001f\u007f-\u009f‪-‮⁦-⁩]/g, "").trim();
    return t.length > max ? t.slice(0, max - 1) + "…" : t;
  }

  function finiteNonNeg(n) {
    return typeof n === "number" && Number.isFinite(n) && n >= 0 ? n : undefined;
  }

  /**
   * Validate and normalise a `probe` result from the app. Returns null when the shape is
   * wrong (the UI then shows a generic error instead of trusting the data).
   */
  function normalizeProbe(r) {
    if (!r || typeof r !== "object") return null;
    if (r.kind !== "video" && r.kind !== "playlist") return null;
    const out = {
      kind: r.kind,
      title: cleanText(r.title, MAX_TEXT),
      qualities: filterQualities(r.qualities),
    };
    const up = cleanText(r.uploader, MAX_TEXT);
    if (up) out.uploader = up;
    const d = finiteNonNeg(r.duration);
    if (d !== undefined) out.duration = d;
    if (r.kind === "playlist") {
      const items = [];
      const seen = new Set();
      if (Array.isArray(r.items)) {
        for (const it of r.items) {
          if (items.length >= MAX_ITEMS) break;
          if (!it || typeof it !== "object") continue;
          const index = it.index;
          if (!Number.isInteger(index) || index < 1 || index > 0xffffffff || seen.has(index)) {
            continue;
          }
          seen.add(index);
          const item = { index, title: cleanText(it.title, MAX_TEXT) };
          const dd = finiteNonNeg(it.duration);
          if (dd !== undefined) item.duration = dd;
          items.push(item);
        }
      }
      items.sort((a, b) => a.index - b.index);
      out.items = items;
    }
    if (r.kind === "video") {
      const o = normalizeOptions(r.options);
      if (o.length) out.options = o;
      const a = normalizeAudioOptions(r.audio_options);
      if (a.length) out.audioOptions = a;
    }
    return out;
  }

  function cleanFormat(f) {
    return typeof f === "string" && FORMAT_SPEC.test(f) ? f : null;
  }
  function posInt(n, max) {
    return typeof n === "number" && Number.isFinite(n) && n > 0 && n <= max ? Math.round(n) : undefined;
  }

  /**
   * v0.2 the download menu: the app's exact video options, validated; tallest first, one per
   * policy height, only real details (anything malformed is dropped, never guessed).
   */
  function normalizeOptions(list) {
    if (!Array.isArray(list)) return [];
    const seen = new Set();
    const out = [];
    for (const o of list.slice(0, 32)) {
      if (!o || typeof o !== "object") continue;
      if (QUALITIES.indexOf(o.quality) === -1 || seen.has(o.quality)) continue;
      const format = cleanFormat(o.format);
      if (!format) continue;
      seen.add(o.quality);
      const x = { quality: o.quality, format, audio: o.audio === true };
      const fps = posInt(o.fps, 1000);
      if (fps) x.fps = fps;
      const codec = cleanText(o.codec, 12);
      if (codec) x.codec = codec;
      const hdr = cleanText(o.hdr, 16);
      if (hdr) x.hdr = hdr;
      const size = finiteNonNeg(o.size);
      if (size) x.size = size;
      out.push(x);
    }
    return out.sort((a, b) => QUALITIES.indexOf(a.quality) - QUALITIES.indexOf(b.quality));
  }

  /** v0.2: the audio options (`m4a` original, `audio` MP3 converted), validated. */
  function normalizeAudioOptions(list) {
    if (!Array.isArray(list)) return [];
    const out = [];
    for (const q of ["m4a", "audio"]) {
      const o = list.find((x) => x && typeof x === "object" && x.quality === q);
      if (!o) continue;
      const format = o.format == null ? null : cleanFormat(o.format);
      if (q === "m4a" && (!format || format.indexOf("+") !== -1)) continue;
      const x = { quality: q, original: q === "m4a", codec: q === "m4a" ? "AAC" : "MP3" };
      if (format) x.format = format;
      const kbps = posInt(o.kbps, 10000);
      if (kbps) x.kbps = kbps;
      const size = finiteNonNeg(o.size);
      if (size && q === "m4a") x.size = size;
      out.push(x);
    }
    return out;
  }

  /** "52.4 MB" (decimal units, as download sizes are usually shown). */
  function fmtBytes(n) {
    if (typeof n !== "number" || !Number.isFinite(n) || n <= 0) return "";
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    let v = n;
    while (v >= 1000 && i < u.length - 1) {
      v /= 1000;
      i++;
    }
    return (i === 0 ? String(Math.round(v)) : v.toFixed(v < 10 ? 2 : 1)) + " " + u[i];
  }

  /** A fresh request id (idempotency key, see proto.rs `Request::id`). */
  function newRequestId(rand) {
    const bytes = new Uint8Array(12);
    if (rand) rand(bytes);
    else root.crypto.getRandomValues(bytes);
    let hex = "";
    for (const b of bytes) hex += b.toString(16).padStart(2, "0");
    return "ext-" + hex;
  }

  function validId(id) {
    return (
      typeof id === "string" &&
      id.length > 0 &&
      id.length <= MAX_ID_LEN &&
      /^[A-Za-z0-9._-]+$/.test(id)
    );
  }

  function validHttpUrl(s) {
    const u = parseUrl(s);
    if (!u) return false;
    if (u.protocol !== "https:" && u.protocol !== "http:") return false;
    if (u.username || u.password) return false;
    // eslint-disable-next-line no-control-regex
    return !/[\u0000-\u001f\u007f]/.test(s);
  }

  /**
   * Build the Native Messaging request for one user action, or throw an Error whose message
   * is a stable code ("bad_action", "bad_url", "bad_quality", "bad_items", "bad_id").
   * Only commands allowed from the browser are buildable (proto.rs `allowed_from_browser`).
   */
  function buildRequest(action, id) {
    if (!validId(id)) throw new Error("bad_id");
    if (!action || typeof action !== "object") throw new Error("bad_action");
    let command;
    switch (action.cmd) {
      case "ping":
        command = { cmd: "ping" };
        break;
      case "probe":
        if (!validHttpUrl(action.url)) throw new Error("bad_url");
        command = { cmd: "probe", url: action.url };
        break;
      case "add":
        if (!validHttpUrl(action.url)) throw new Error("bad_url");
        command = { cmd: "add", url: action.url };
        break;
      case "open_video":
        // v0.2: the desktop app's Video window with this page (the user chooses there)
        if (!validHttpUrl(action.url)) throw new Error("bad_url");
        command = { cmd: "open_video", url: action.url };
        // v0.2 (review of PR #9): the quality chosen in the menu, so the window opens with it
        if (action.quality !== undefined && action.quality !== null) {
          if (!isPolicyQuality(action.quality)) throw new Error("bad_quality");
          command.quality = action.quality;
        }
        break;
      case "add_video":
        if (!validHttpUrl(action.url)) throw new Error("bad_url");
        if (!isPolicyQuality(action.quality)) throw new Error("bad_quality");
        command = { cmd: "add_video", url: action.url, quality: action.quality };
        // v0.2: the exact formats the menu line showed (only with a fixed height or m4a)
        if (action.format !== undefined && action.format !== null) {
          const f = cleanFormat(action.format);
          const fixed = /^\d+p$/.test(action.quality);
          if (!f || !(fixed || (action.quality === "m4a" && f.indexOf("+") === -1))) throw new Error("bad_format");
          command.format = f;
        }
        break;
      case "add_playlist": {
        if (!validHttpUrl(action.url)) throw new Error("bad_url");
        if (!isPolicyQuality(action.quality)) throw new Error("bad_quality");
        command = { cmd: "add_playlist", url: action.url, quality: action.quality };
        // v0.2: sent only when not the default, so an older app (which knows no such field)
        // still accepts the usual request
        if (action.if_missing !== undefined && action.if_missing !== null) {
          if (IF_MISSING.indexOf(action.if_missing) === -1) throw new Error("bad_if_missing");
          if (action.if_missing !== "lower" && /^\d+p$/.test(action.quality)) command.if_missing = action.if_missing;
        }
        if (action.items !== undefined && action.items !== null) {
          const it = action.items;
          if (!Array.isArray(it) || it.length === 0 || it.length > MAX_ITEMS) {
            throw new Error("bad_items");
          }
          const seen = new Set();
          for (const i of it) {
            if (!Number.isInteger(i) || i < 1 || i > 0xffffffff || seen.has(i)) {
              throw new Error("bad_items");
            }
            seen.add(i);
          }
          command.items = it.slice().sort((a, b) => a - b);
        }
        break;
      }
      default:
        throw new Error("bad_action");
    }
    return { v: PROTOCOL_VERSION, id, command };
  }

  /**
   * Map a failure (a `runtime.lastError` message or the app's `error` string) to a stable
   * error code the UI translates. Unknown text maps to "failed"; the raw text is never shown
   * (it could contain a URL).
   */
  function classifyError(msg) {
    const m = typeof msg === "string" ? msg : "";
    if (/^TOOLS_MISSING:/.test(m)) return "tools_missing";
    if (/native messaging host not found|no such native application/i.test(m)) {
      return "not_installed";
    }
    if (/forbidden|access to the specified native messaging host is forbidden/i.test(m)) {
      return "not_allowed";
    }
    if (/is not running|cannot reach eid download|lost the connection/i.test(m)) {
      return "app_not_running";
    }
    if (/command not allowed from the browser|malformed request|unsupported protocol/i.test(m)) {
      return "app_too_old";
    }
    if (/no window/i.test(m)) return "no_window";
    if (/drm|protected/i.test(m)) return "drm";
    if (/unsupported url|unsupported site|no video/i.test(m)) return "unsupported";
    if (/timed? ?out/i.test(m)) return "timeout";
    return "failed";
  }

  /** Validate a response frame for request `id`. Returns {ok:true,result} or {ok:false,code}. */
  function parseResponse(resp, id) {
    if (!resp || typeof resp !== "object" || resp.v !== PROTOCOL_VERSION) {
      return { ok: false, code: "failed" };
    }
    if (resp.id !== id && resp.id !== "") return { ok: false, code: "failed" };
    if (resp.ok === true) return { ok: true, result: resp.result };
    return { ok: false, code: classifyError(resp.error) };
  }

  /**
   * `probe` over IPC answers `{pending:true, retry_ms}` while yt-dlp runs in the app (the
   * IPC handler cannot block); the caller asks again with a FRESH id after `retry_ms`.
   * Returns the clamped delay in ms, or null when `result` is a final answer.
   */
  function pendingDelay(result) {
    if (!result || typeof result !== "object" || result.pending !== true) return null;
    const ms = result.retry_ms;
    return Math.round(clamp(ms, 200, 5000, 500));
  }

  /** Total time we keep polling one probe before giving up (a big playlist takes a while). */
  const PROBE_DEADLINE_MS = 180000;

  // ---------------------------------------------------------------- settings

  const SIZES = { small: 28, medium: 34, large: 42 };
  const COLORS = {
    teal: "#0f766e",
    blue: "#1d4ed8",
    red: "#b91c1c",
    purple: "#6d28d9",
    dark: "#1f2937",
  };
  const DEFAULT_SETTINGS = Object.freeze({
    enabled: true,
    position: "top-right", // "top-left" | "top-right"
    size: "medium", // "small" | "medium" | "large" | "custom"
    customSize: 34, // px, 20..64
    color: "teal", // a COLORS key or "custom"
    customColor: "#0f766e",
    shape: "rounded", // "rounded" | "square" | "pill"
    label: "icon-text", // "icon" | "icon-text"
    opacity: 0.92, // 0.3..1
    visibility: "hover", // "hover" | "always"
    excludedSites: [], // host names; "example.com" also excludes sub-domains
    ifMissing: "lower", // v0.2, playlists: "lower" | "higher" | "skip" when an item lacks the quality
  });
  const IF_MISSING = ["lower", "higher", "skip"];

  function clamp(n, lo, hi, dflt) {
    if (typeof n !== "number" || !Number.isFinite(n)) return dflt;
    return Math.min(hi, Math.max(lo, n));
  }

  function oneOf(v, allowed, dflt) {
    return allowed.indexOf(v) !== -1 ? v : dflt;
  }

  /** Normalise a host entered by the user ("https://www.Example.com/x" -> "www.example.com"). */
  function normalizeHost(s) {
    if (typeof s !== "string") return null;
    let t = s.trim().toLowerCase();
    if (!t) return null;
    if (/^[a-z][a-z0-9+.-]*:\/\//.test(t)) {
      const u = parseUrl(t);
      if (!u) return null;
      t = u.hostname;
    } else {
      t = t.split(/[/?#]/)[0];
    }
    t = t.replace(/^\*\./, "").replace(/\.$/, "");
    if (!/^[a-z0-9.-]{1,253}$/.test(t) || t.startsWith(".") || t.includes("..")) return null;
    return t;
  }

  function normalizeSettings(s) {
    const d = DEFAULT_SETTINGS;
    const o = s && typeof s === "object" ? s : {};
    const hosts = [];
    if (Array.isArray(o.excludedSites)) {
      for (const h of o.excludedSites) {
        const n = normalizeHost(h);
        if (n && hosts.indexOf(n) === -1 && hosts.length < 500) hosts.push(n);
      }
    }
    return {
      enabled: typeof o.enabled === "boolean" ? o.enabled : d.enabled,
      position: oneOf(o.position, ["top-left", "top-right"], d.position),
      size: oneOf(o.size, ["small", "medium", "large", "custom"], d.size),
      customSize: Math.round(clamp(o.customSize, 20, 64, d.customSize)),
      color: oneOf(o.color, Object.keys(COLORS).concat(["custom"]), d.color),
      customColor:
        typeof o.customColor === "string" && /^#[0-9a-fA-F]{6}$/.test(o.customColor)
          ? o.customColor.toLowerCase()
          : d.customColor,
      shape: oneOf(o.shape, ["rounded", "square", "pill"], d.shape),
      label: oneOf(o.label, ["icon", "icon-text"], d.label),
      opacity: Math.round(clamp(o.opacity, 0.3, 1, d.opacity) * 100) / 100,
      visibility: oneOf(o.visibility, ["hover", "always"], d.visibility),
      excludedSites: hosts,
      ifMissing: oneOf(o.ifMissing, IF_MISSING, d.ifMissing),
    };
  }

  /** Resolved visual parameters for the button. */
  function buttonStyle(settings) {
    const s = normalizeSettings(settings);
    const px = s.size === "custom" ? s.customSize : SIZES[s.size];
    const color = s.color === "custom" ? s.customColor : COLORS[s.color];
    const radius = s.shape === "square" ? 3 : s.shape === "pill" ? px / 2 : 8;
    return { px, color, radius, opacity: s.opacity, iconOnly: s.label === "icon" };
  }

  /** Whether `hostname` is excluded: exact match or a sub-domain of an excluded host. */
  function isExcluded(hostname, settings) {
    const h = typeof hostname === "string" ? hostname.toLowerCase().replace(/\.$/, "") : "";
    if (!h) return false;
    const list = normalizeSettings(settings).excludedSites;
    return list.some((e) => h === e || h.endsWith("." + e));
  }

  /**
   * Vertical inset (px) from the top of the player so the button never sits on the player's
   * own top-bar controls (YouTube embed title/share bar, Shorts' play/mute/menu row).
   */
  function topInset(cls) {
    if (cls && cls.kind === "youtube") {
      if (cls.type === "embed") return 60;
      if (cls.type === "shorts") return 64;
      return 12;
    }
    return 10;
  }

  /**
   * Compute the fixed-position box of the button for a player rect.
   * Returns null if the player is too small to host the button without covering it.
   */
  function placeButton(rect, settings, cls, btnW, btnH) {
    if (!rect || rect.width < 200 || rect.height < 120) return null;
    const s = normalizeSettings(settings);
    const inset = 10;
    const top = rect.top + topInset(cls);
    if (top + btnH > rect.top + rect.height * 0.6) return null;
    const left =
      s.position === "top-left" ? rect.left + inset : rect.left + rect.width - inset - btnW;
    return { top: Math.round(top), left: Math.round(left) };
  }

  /**
   * Decide whether a <video> may get the button. `v` is a plain snapshot so this stays pure:
   * { src, mediaKeys, encrypted, width, height, isAd }
   * Protected media (EME/DRM) is never offered. On non-YouTube sites a blob:/MSE-only player
   * (no real http(s) source) is not offered either: we would have nothing honest to send.
   */
  function eligibility(v, cls) {
    if (!v) return "none";
    if (v.mediaKeys || v.encrypted) return "drm";
    if (v.isAd) return "ad";
    if (!(v.width >= 200 && v.height >= 120)) return "small";
    if (cls.kind === "youtube") {
      return cls.videoId ? "youtube" : "none";
    }
    if (cls.kind !== "web") return "none";
    const src = typeof v.src === "string" ? v.src : "";
    if (!validHttpUrl(src)) return "no_source"; // blob:, data:, mediastream, empty
    if (/\.m3u8(\?|$)|\.mpd(\?|$)/i.test(src)) return "no_source"; // streams need the app's probe
    return "file";
  }

  const api = {
    HOST_NAME,
    PROTOCOL_VERSION,
    MAX_URL_LEN,
    MAX_ITEMS,
    QUALITIES,
    ALL_QUALITIES,
    MAX_HEIGHT,
    COLORS,
    SIZES,
    DEFAULT_SETTINGS,
    classifyUrl,
    openInAppUrl,
    normalizeOptions,
    normalizeAudioOptions,
    fmtBytes,
    heightOf,
    filterQualities,
    isPolicyQuality,
    cleanText,
    normalizeProbe,
    newRequestId,
    buildRequest,
    classifyError,
    parseResponse,
    pendingDelay,
    PROBE_DEADLINE_MS,
    normalizeHost,
    normalizeSettings,
    buttonStyle,
    isExcluded,
    topInset,
    placeButton,
    eligibility,
    validHttpUrl,
  };
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.EidCore = api;
})(typeof globalThis !== "undefined" ? globalThis : this);

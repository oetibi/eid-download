// Options page: button settings in storage.sync (normalised by EidCore.normalizeSettings).
"use strict";
(function () {
  const api = typeof browser !== "undefined" ? browser : chrome;
  const Core = window.EidCore;
  const t = (k, s) => api.i18n.getMessage(k, s) || k;
  const lang = (api.i18n.getUILanguage && api.i18n.getUILanguage()) || "en";
  const rtl = /^(ar|fa|he|ur)\b/i.test(lang);
  document.documentElement.lang = rtl ? "ar" : "en";
  document.documentElement.dir = rtl ? "rtl" : "ltr";
  for (const el of document.querySelectorAll("[data-i18n]")) el.textContent = t(el.dataset.i18n);
  document.title = t("optTitle");

  const f = document.getElementById("f");
  const msg = document.getElementById("msg");
  const colors = document.getElementById("colors");
  const COLOR_KEYS = { teal: "colorTeal", blue: "colorBlue", red: "colorRed", purple: "colorPurple", dark: "colorDark" };
  for (const [name, hex] of Object.entries(Core.COLORS)) {
    const lab = document.createElement("label");
    lab.className = "swatch";
    lab.title = t(COLOR_KEYS[name]);
    const r = document.createElement("input");
    r.type = "radio";
    r.name = "color";
    r.value = name;
    r.setAttribute("aria-label", t(COLOR_KEYS[name]));
    const dot = document.createElement("span");
    dot.className = "dot";
    dot.style.background = hex;
    lab.append(r, dot);
    colors.appendChild(lab);
  }

  function setRadio(name, value) {
    for (const r of f.querySelectorAll(`input[name="${name}"]`)) r.checked = r.value === value;
  }

  function fill(s) {
    f.enabled.checked = s.enabled;
    for (const k of ["visibility", "position", "size", "color", "shape", "label", "ifMissing"]) setRadio(k, s[k]);
    f.customSize.value = s.customSize;
    f.customColor.value = s.customColor;
    f.opacity.value = s.opacity;
    f.excludedSites.value = s.excludedSites.join("\n");
    preview();
  }

  function radio(name) {
    const r = f.querySelector(`input[name="${name}"]:checked`);
    return r ? r.value : undefined;
  }

  function read() {
    return Core.normalizeSettings({
      enabled: f.enabled.checked,
      visibility: radio("visibility"),
      position: radio("position"),
      size: radio("size"),
      customSize: Number(f.customSize.value),
      color: radio("color"),
      customColor: f.customColor.value,
      shape: radio("shape"),
      label: radio("label"),
      opacity: Number(f.opacity.value),
      excludedSites: f.excludedSites.value.split(/[\n,]+/),
      ifMissing: radio("ifMissing"),
    });
  }

  function preview() {
    const s = read();
    const st = Core.buttonStyle(s);
    const b = document.getElementById("pv");
    b.style.height = st.px + "px";
    b.style.minWidth = st.px + "px";
    b.style.background = st.color;
    b.style.borderRadius = st.radius + "px";
    b.style.opacity = String(st.opacity);
    b.style.fontSize = Math.max(12, Math.round(st.px * 0.38)) + "px";
    b.style.padding = st.iconOnly ? "0" : `0 ${Math.round(st.px * 0.32)}px`;
    b.style.width = st.iconOnly ? st.px + "px" : "auto";
    const svg = b.querySelector("svg");
    svg.style.width = svg.style.height = Math.round(st.px * 0.56) + "px";
    document.getElementById("pvt").hidden = st.iconOnly;
    document.getElementById("opv").textContent = Math.round(st.opacity * 100) + "%";
  }

  f.addEventListener("input", () => {
    if (document.activeElement === f.customColor) setRadio("color", "custom");
    if (document.activeElement === f.customSize) setRadio("size", "custom");
    preview();
  });
  f.addEventListener("submit", async (e) => {
    e.preventDefault();
    const s = read();
    await api.storage.sync.set(s);
    fill(s); // show the normalised values (e.g. cleaned site list)
    msg.textContent = t("optSaved");
  });
  document.getElementById("reset").addEventListener("click", async () => {
    const s = Core.normalizeSettings(null);
    await api.storage.sync.set(s);
    fill(s);
    msg.textContent = t("optSaved");
  });
  document.getElementById("test").addEventListener("click", async () => {
    const out = document.getElementById("testmsg");
    out.textContent = "…";
    let r;
    try {
      r = await api.runtime.sendMessage({ eid: 1, cmd: "ping" });
    } catch (_) {
      r = { ok: false, code: "failed" };
    }
    const keys = {
      not_installed: "errNotInstalled",
      not_allowed: "errNotAllowed",
      app_not_running: "errAppNotRunning",
      app_too_old: "errAppTooOld",
    };
    out.textContent = r && r.ok ? t("optTestOk") : t(keys[r && r.code] || "errFailed");
    out.className = r && r.ok ? "ok" : "err";
  });

  Promise.resolve(api.storage.sync.get(null)).then((s) => fill(Core.normalizeSettings(s)));
})();

// In-memory stand-in for the Rust side, used only when the page is opened outside the app (a
// plain browser, for layout checks). Never loaded inside Eid Download.
export function mockApi() {
  let next = 6;
  const items = [
    { id: 5, status: "running", name: "ubuntu-24.04-desktop-amd64.iso", url: "https://releases.example.com/ubuntu-24.04-desktop-amd64.iso", folder: "C:\\Users\\me\\Downloads", attempts: 1, error_kind: null, error: null, rate: { done: 1932735283, total: 6114770944, speed: 12582912, eta: 332 } },
    { id: 4, status: "paused", name: "تقرير 2026 النهائي - final report.pdf", url: "https://example.com/files/report.pdf", folder: "C:\\Users\\me\\Downloads", attempts: 1, error_kind: null, error: null, rate: { done: 734003, total: 2097152, speed: 0, eta: null } },
    { id: 3, status: "failed", name: "missing.zip", url: "https://example.com/missing.zip", folder: "C:\\Users\\me\\Downloads", attempts: 3, error_kind: "not_found", error: "server error: HTTP 404 for https://example.com/missing.zip", rate: null },
    { id: 2, status: "done", name: "setup.exe", url: "https://example.com/setup.exe", folder: "D:\\Apps", attempts: 1, error_kind: null, error: null, rate: { done: 48234496, total: 48234496, speed: 0, eta: null } },
    { id: 1, status: "pending", name: "music.flac", url: "https://example.com/music.flac", folder: "C:\\Users\\me\\Downloads", attempts: 0, error_kind: null, error: null, rate: null },
  ];
  const find = (id) => items.find((i) => i.id === id);
  const handlers = {
    list: () => {
      const r = find(5);
      if (r && r.status === "running") r.rate.done = Math.min(r.rate.total, r.rate.done + 8 * 1024 * 1024);
      return { items: structuredClone(items), default_dir: "C:\\Users\\me\\Downloads" };
    },
    add: ({ links, name }) => {
      const ls = links.split(/\s+/).filter(Boolean);
      if (!ls.length) throw "no_links";
      const added = [], refused = [];
      for (const l of ls) {
        if (!/^https?:\/\/[^/]+/i.test(l)) { refused.push([l, "bad_url"]); continue; }
        const id = next++;
        items.unshift({ id, status: "pending", name: name || l.split("/").pop() || l, url: l, folder: "C:\\Users\\me\\Downloads", attempts: 0, error_kind: null, error: null, rate: null });
        added.push(id);
      }
      return { added, refused };
    },
    pause: ({ id }) => { find(id).status = "paused"; },
    resume: ({ id }) => { find(id).status = "pending"; },
    remove: ({ id }) => {
      if (find(id).status === "running") throw "pause_first";
      items.splice(items.indexOf(find(id)), 1);
    },
    clear_finished: () => {
      const before = items.length;
      for (let i = items.length - 1; i >= 0; i--) if (items[i].status === "done") items.splice(i, 1);
      return before - items.length;
    },
    pick_folder: () => "D:\\Downloads\\مجلد",
    reveal: () => {},
  };
  return { call: async (cmd, args) => handlers[cmd](args ?? {}) };
}

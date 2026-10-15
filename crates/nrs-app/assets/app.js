// @ts-check

const app = document.querySelector("#app");
if (!(app instanceof HTMLElement)) throw new Error("missing #app");

const sections = {
  profile: ["Profile", "Baseline craft and progress: what kind of entry this is and how much of it was consumed."],
  responses: ["Responses", "Specific moments and aftereffects. Repeated NEI/AEI kinds are folded into one glanceable group."],
  resonance: ["Personal resonance", "Attachments, memes, gateways, and other effects that escaped the work itself."],
  adjustments: ["Adjustments", "Explicit score corrections kept separate from observed responses."],
  other: ["Other", "Custom or legacy impacts without a standards category."],
};

// Each section lists its labels in a fixed order (container before
// contained, original before derived) so groups don't reshuffle
// depending on which relation happens to come back first.
const relationSections = {
  structure: ["Structure", "Collections, albums, franchises, and their members.", ["Contains", "Contained by"]],
  authorship: ["Authorship", "Who made it and where their work appears.", ["Credited on", "Contributors"]],
  lineage: ["Lineage", "Originals, remixes, and derived versions.", ["Remix of", "Remixed into"]],
  influence: ["Influence", "Music carried into another entry, or value displaced by another work.", ["Features music", "Featured in", "Killed by", "Kills"]],
};

let requestController = new AbortController();
let currentEntryData = null;
let searchTimer;
let routeVersion = 0;
// Per-page hooks, reset on every navigation: `pageTeardown` releases
// whatever the page set up (observers), `pageKeys` handles shortcuts.
let pageTeardown = null;
let pageKeys = null;

// [kind, label, section] -- section mirrors the server's
// `impact_section`, and groups the kind picker the same way the entry
// page groups impacts.
const impactKinds = [
  ["cry", "Cry", "responses"],
  ["nei", "Noticeable emotional impact (NEI)", "responses"],
  ["aei", "Appreciable emotional impact (AEI)", "responses"],
  ["ehi", "Exceptional humor impact (EHI)", "responses"],
  ["epi", "Exceptional plot impact (EPI)", "responses"],
  ["jumpscare", "Jumpscare", "responses"],
  ["sleepless_night", "Sleepless night", "responses"],
  ["politics", "Politics", "resonance"],
  ["interest_field", "New interest field (gateway)", "resonance"],
  ["additional", "Additional (buff/nerf)", "adjustments"],
  ["music", "Music", "profile"],
  ["visual", "Visual", "profile"],
  ["writing", "Writing quality", "profile"],
  ["osu_song", "osu! song", "resonance"],
  ["waifu", "Waifu", "resonance"],
  ["meme", "Meme", "resonance"],
  ["pads", "PADS", "responses"],
  ["consumed", "Consumed (boredom)", "profile"],
  ["anime_consumed", "Anime consumed (boredom, by episode)", "profile"],
  ["dropped", "Dropped", "profile"],
  ["music_consumed_progress", "Music consumed progress", "profile"],
];

const relationKinds = [
  ["contains", "Contains"],
  ["remix", "Remix"],
  ["feature_music", "Feature music"],
  ["killed_by", "Killed-by"],
  ["credit", "Credit (contributor → work)"],
];

const roleNames = ["compose", "arrange", "lyrics", "vocal", "inst_perform", "image", "image_feat", "mv", "albumart", "prod", "inst", "perform", "inst_writing", "inst_total", "vocal_lyrics", "music_total", "image_total", "total"];

const anchors = [
  "comedy", "hype", "tragedy", "wistful", "cozy", "heartwarming", "hate",
  "discomfort", "waifu", "meme", "hooked", "worldview", "gateway", "writing",
  "visual", "music", "boredom", "additional",
];

// The server's `entry_kind` slugs, in chip order, with display labels.
const entryKinds = {
  anime: "anime",
  album: "album",
  track: "track",
  artist: "artist",
  franchise: "franchise",
  game: "game",
  vn: "visual novel",
  literature: "literature",
  other: "other",
};

// A type store's own `kind` (`resolve`'s) as an entry kind, like the
// server's `kind_from_store`.
const storeKinds = {
  anime: "anime",
  anime_movie: "anime",
  manga: "literature",
  novel: "literature",
  light_novel: "literature",
  one_shot: "literature",
  visual_novel: "vn",
  release: "album",
  release_group: "album",
  track: "track",
  artist: "artist",
};

function storeKindTag(kind) {
  return storeKinds[kind] ? kindTag(storeKinds[kind]) : `<span class="kind-tag">${escapeHtml(kind)}</span>`;
}

function kindTag(kind) {
  return `<span class="kind-tag" data-kind="${escapeHtml(kind)}">${escapeHtml(entryKinds[kind] || kind)}</span>`;
}

// The entry page's kind tag, as a picker: "auto" keeps the inferred kind.
function kindSelect(data) {
  const current = data.kind_stored ? data.entry.kind : "";
  const option = (value, label) => `<option value="${escapeHtml(value)}" ${value === current ? "selected" : ""}>${escapeHtml(label)}</option>`;
  return `<select class="kind-tag kind-select" data-kind="${escapeHtml(data.entry.kind)}" aria-label="entry type" title="entry type">
    ${option("", `auto: ${entryKinds[data.inferred_kind] || data.inferred_kind}`)}
    ${Object.entries(entryKinds).map(([kind, label]) => option(kind, label)).join("")}
  </select>`;
}

function bindKindSelect(data) {
  const select = app.querySelector(".kind-select");
  if (!(select instanceof HTMLSelectElement)) return;
  select.addEventListener("change", async () => {
    select.disabled = true;
    try {
      await sendJson(`/api/v1/entries/${encodeURIComponent(data.entry.id)}/kind`, "PUT", { kind: select.value || null });
      await renderEntry(data.entry.id, { quiet: true });
    } catch (reason) {
      showToast(reason?.message || String(reason), "error");
      select.disabled = false;
    }
  });
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function pathForEntry(id) {
  return `/entries/${encodeURIComponent(id)}`;
}

async function getJson(path) {
  const version = routeVersion;
  requestController.abort();
  requestController = new AbortController();
  const response = await fetch(path, {
    signal: requestController.signal,
    headers: { Accept: "application/json" },
  });
  const body = await response.json().catch(() => ({}));
  if (version !== routeVersion) throw new DOMException("Navigation changed", "AbortError");
  if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
  return body;
}

async function sendJson(path, method, body) {
  const response = await fetch(path, {
    method,
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || `${response.status} ${response.statusText}`);
  return payload;
}

function showToast(message, tone = "ok") {
  const toast = document.querySelector("#toast");
  if (!(toast instanceof HTMLElement)) return;
  toast.textContent = message;
  toast.dataset.tone = tone;
  toast.classList.add("visible");
  setTimeout(() => toast.classList.remove("visible"), 2200);
}

function navigate(url) {
  history.pushState(null, "", url);
  route();
}

document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) return;
  const action = event.target.closest("[data-action]");
  if (action instanceof HTMLElement) {
    const name = action.dataset.action;
    if (name === "add-impact") openImpactEditor(null, action);
    if (name === "edit-impact") {
      const impact = currentEntryData?.impacts.find((item) => item.id === action.dataset.id);
      if (impact) {
        if (impact.kind) openImpactEditor(impact, action);
        else if (!impact.shared) openRawImpactEditor(impact, action);
      }
    }
    if (name === "confirm-impact") confirmImpact(action.dataset.id);
    if (name === "share-impact") shareImpact(action.dataset.id);
    if (name === "add-relation") openRelationEditor(null, action);
    if (name === "edit-relation") {
      const relation = currentEntryData?.relations.find((item) => item.id === action.dataset.id);
      if (relation) openRelationEditor(relation, action);
    }
    if (name === "delete-relation") deleteRelation(action.dataset.id);
    if (name === "close-editor") closeEditor(action);
    return;
  }
  const anchor = event.target.closest("a[data-link]");
  if (!(anchor instanceof HTMLAnchorElement)) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  if (anchor.origin !== location.origin) return;
  event.preventDefault();
  navigate(anchor.href);
});

window.addEventListener("popstate", route);

document.addEventListener("keydown", (event) => {
  if (!pageKeys || event.metaKey || event.ctrlKey || event.altKey) return;
  const target = event.target;
  const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement;
  pageKeys(event, typing);
});

app.addEventListener("toggle", (event) => {
  const details = event.target;
  if (!(details instanceof HTMLDetailsElement) || !details.classList.contains("card-details") || !details.open || !details.isConnected || details.querySelector(".inline-editor-panel")) return;
  const button = details.querySelector('[data-action="edit-impact"]');
  if (button instanceof HTMLButtonElement) button.click();
}, true);

function setActiveNav() {
  const segment = location.pathname.split("/")[1];
  const active = ["palette", "calibrate", "kfeat", "review", "entries", "seasons", "progress"].includes(segment)
    ? segment
    : "scores";
  document.querySelectorAll(".rail nav a[data-nav]").forEach((anchor) => {
    anchor.classList.toggle("active", anchor.getAttribute("data-nav") === active);
  });
}

function renderError(error) {
  if (error?.name === "AbortError") return;
  app.innerHTML = `<p class="error">${escapeHtml(error?.message || error)}</p>`;
}

const SCORE_PAGE_SIZE = 100;

function deltaMarkup(delta) {
  if (delta == null) return `<span class="dim">new</span>`;
  if (delta > 0) return `<span class="delta-up">▲${delta}</span>`;
  if (delta < 0) return `<span class="delta-down">▼${-delta}</span>`;
  return `<span class="dim">–</span>`;
}

/** `(+3)`-style overall Δ, shown beside the filtered one. */
function overallDelta(delta) {
  if (delta == null) return "new";
  return delta > 0 ? `+${delta}` : delta < 0 ? `−${-delta}` : "0";
}

/**
 * `filtered`: the view is narrowed by a filter, so rank and Δ are shown
 * among the matches (1..N) with the overall figures beside them.
 */
function scoreRow(row, comparing, filtered) {
  const rank = filtered ? `${row.filtered_rank}<small>(${row.rank})</small>` : row.rank;
  const delta = filtered
    ? `${deltaMarkup(row.filtered_delta)}<small>(${overallDelta(row.delta)})</small>`
    : deltaMarkup(row.delta);
  return `<tr data-id="${escapeHtml(row.id)}" data-kind="${escapeHtml(row.kind)}">
    <td class="col-rank">${rank}</td>
    ${comparing ? `<td class="col-delta">${delta}</td>` : ""}
    <td class="col-title"><a href="${pathForEntry(row.id)}" data-link>${escapeHtml(row.title || row.id)}</a></td>
    <td class="col-type">${kindTag(row.kind)}</td>
    <td class="col-store">${escapeHtml(row.store)}</td>
    <td class="col-value">${Number(row.value).toFixed(4)}</td>
  </tr>`;
}

/**
 * The ranking, paged: rows arrive SCORE_PAGE_SIZE at a time as the
 * end of the table scrolls into view. Sorting and filtering happen on
 * the server against a cached ranking, so they only refetch the first
 * page. A rerun with new parameters compares ranks against the run
 * before it.
 */
async function renderScores() {
  currentEntryData = null;
  document.title = "Scores — NRS";
  const params = new URLSearchParams(location.search);
  const view = {
    sort: params.get("sort") || "value",
    dir: params.get("dir") || "desc",
    q: params.get("q") || "",
    shared: params.get("shared") || "",
    length: params.get("length") || "",
    cmp_gen: params.get("cmp_gen") || "",
    cmp_shared: params.get("cmp_shared") || "",
    cmp_length: params.get("cmp_length") || "",
  };
  const comparing = Boolean(view.cmp_gen || view.cmp_shared || view.cmp_length);
  const compareLabel = [
    view.cmp_gen ? `version ${view.cmp_gen}` : "",
    view.cmp_shared ? `λ ${view.cmp_shared}` : "",
    view.cmp_length ? `L ${view.cmp_length} days` : "",
  ].filter(Boolean).join(" · ");
  const columns = [
    ["rank", "#", "value"],
    ...(comparing ? [["delta", "Δ", "delta"]] : []),
    ["title", "title", "title"],
    ["type", "type", "type"],
    ["store", "store", "id"],
    ["value", "V (raw)", "value"],
  ];
  app.innerHTML = `
    <h1>scores</h1>
    <p class="muted">Uncalibrated V (nested pooling over <a href="/kfeat">K</a>) -- not a public score yet.</p>
    <form class="score-controls">
      ${textField(view, "shared", "time: shared λ (1 = ignore dates)", "", "number", 'min="0" max="1" step="0.05"')}
      ${textField(view, "length", "time: length L (days)", "", "number", 'min="1" step="1"')}
      <button class="button primary" type="submit">rerun</button>
      <button class="button" type="button" data-save-time>save as default</button>
    </form>
    <div class="ledger-meta">
      <span class="score-count">Calculating…</span>
      ${comparing ? `<span class="compare-note">Δ vs ${escapeHtml(compareLabel)} <button class="edit-link" type="button" data-clear-compare>clear</button></span>` : `<span class="compare-note" hidden><button class="edit-link" type="button" data-start-compare>compare with the previous version</button></span>`}
      <a class="csv-link">download csv</a>
    </div>
    <div class="toolbar score-toolbar">
      <input class="search" type="search" placeholder="filter… try  type:anime v>10  ( / )" value="${escapeHtml(view.q)}" spellcheck="false" autocomplete="off">
      <details class="filter-help"><summary>syntax</summary><div class="filter-help-body">
        <p>Space-separated terms; every one must match. A leading <code>-</code> negates any term.</p>
        <dl>
          <dt><code>word</code> <code>"a phrase"</code></dt><dd>title, id, or store contains it</dd>
          <dt><code>type:anime</code></dt><dd>${Object.keys(entryKinds).join(", ")}, or <code>music</code> for album + track + artist — several mean any of them (click a chip; shift-click excludes)</dd>
          <dt><code>store:mal</code></dt><dd>store contains it</dd>
          <dt><code>v>10</code> <code>rank<=100</code></dt><dd>compare with <code>&gt; &gt;= &lt; &lt;= =</code></dd>
          <dt><code>delta>=5</code> <code>is:up</code> <code>is:down</code> <code>is:moved</code></dt><dd>rank changes since the compared run (after an edit or a rerun)</dd>
        </dl>
      </div></details>
    </div>
    <div class="kind-chips" aria-label="filter by type"></div>
    <p class="filter-warnings" hidden></p>
    <div class="score-table-wrap">
      <table class="score-table">
        <thead><tr>${columns.map(([key, label, sort]) => `<th class="col-${key}">${sort && key !== "rank" ? `<button class="sort-button" type="button" data-sort="${sort}">${label}</button>` : label}</th>`).join("")}</tr></thead>
        <tbody></tbody>
      </table>
      <div class="score-sentinel"></div>
    </div>
    <p class="key-hint dim">j / k to move · enter to open · / to filter</p>`;
  const controls = app.querySelector(".score-controls");
  const count = app.querySelector(".score-count");
  const csv = app.querySelector(".csv-link");
  const search = app.querySelector(".search");
  const table = app.querySelector(".score-table");
  const tbody = app.querySelector(".score-table tbody");
  const sentinel = app.querySelector(".score-sentinel");
  const chips = app.querySelector(".kind-chips");
  const warnings = app.querySelector(".filter-warnings");
  if (!(controls instanceof HTMLFormElement) || !(count instanceof HTMLElement) || !(csv instanceof HTMLAnchorElement) || !(search instanceof HTMLInputElement) || !(table instanceof HTMLElement) || !(tbody instanceof HTMLElement) || !(sentinel instanceof HTMLElement) || !(chips instanceof HTMLElement) || !(warnings instanceof HTMLElement)) return;

  const defaults = { sort: "value", dir: "desc" };
  const query = (extra = {}) => new URLSearchParams(Object.entries({ ...view, ...extra }).filter(([, value]) => value !== ""));
  // The address bar only carries what differs from the defaults.
  const pageUrl = (extra = {}) => `/?${new URLSearchParams([...query(extra)].filter(([key, value]) => defaults[key] !== value))}`.replace(/\?$/, "");
  let token = 0;
  let loaded = 0;
  let total = Infinity;
  let loading = false;
  let selected = -1;

  const syncView = () => {
    history.replaceState(null, "", pageUrl());
    const { cmp_gen, cmp_shared, cmp_length, ...exported } = view;
    csv.href = `/api/v1/scores.csv?${new URLSearchParams(Object.entries(exported).filter(([, value]) => value !== ""))}`;
    app.querySelectorAll(".sort-button").forEach((button) => {
      const active = button.getAttribute("data-sort") === view.sort;
      button.textContent = button.textContent.replace(/ [↓↑]$/, "") + (active ? (view.dir === "desc" ? " ↓" : " ↑") : "");
      button.closest("th")?.setAttribute("aria-sort", active ? (view.dir === "desc" ? "descending" : "ascending") : "none");
    });
  };

  const nearViewport = () => sentinel.getBoundingClientRect().top < window.innerHeight + 800;

  async function loadMore() {
    if (loading || loaded >= total) return;
    loading = true;
    const mine = token;
    if (!loaded) sentinel.innerHTML = `<p class="loading">Calculating scores…</p>`;
    try {
      const page = await getJson(`/api/v1/scores?${query({ offset: String(loaded), limit: String(SCORE_PAGE_SIZE) })}`);
      if (mine !== token) return;
      total = page.total;
      loaded += page.rows.length;
      const filtered = page.total < page.database_total;
      table.classList.toggle("filtered", filtered);
      tbody.insertAdjacentHTML("beforeend", page.rows.map((row) => scoreRow(row, comparing, filtered)).join(""));
      if (page.offset === 0) {
        paintFilters(page);
        noteGeneration(page);
        effective = page.time;
        stored = page.stored_time;
        const sharedInput = controls.elements.namedItem("shared");
        const lengthInput = controls.elements.namedItem("length");
        if (sharedInput instanceof HTMLInputElement) sharedInput.placeholder = String(stored.shared);
        if (lengthInput instanceof HTMLInputElement) lengthInput.placeholder = String(stored.length_days);
      }
      count.textContent = total === page.database_total ? `${total} entries` : `${total} of ${page.database_total} entries`;
      sentinel.innerHTML = loaded < total ? `<p class="loading">Loading more…</p>` : total ? "" : `<p class="empty">No matching entries.</p>`;
    } catch (error) {
      if (error?.name === "AbortError" || mine !== token) return;
      sentinel.innerHTML = `<p class="error">${escapeHtml(error?.message || error)}</p>`;
      total = loaded;
    } finally {
      if (mine === token) loading = false;
    }
    // A page shorter than the viewport leaves the sentinel in view, and
    // the observer only fires on changes.
    if (mine === token && nearViewport()) loadMore();
  }

  // Chips edit the query text itself, so the search box and URL stay
  // the single description of what's shown.
  let kinds = [];
  const statusTerms = [["is:moved", "moved"], ["is:up", "up"], ["is:down", "down"]];
  const termsOf = (q) => q.split(/\s+/).filter(Boolean);
  function paintFilters(page) {
    kinds = page.kinds;
    const present = new Set(termsOf(view.q));
    chips.innerHTML = kinds.map(({ kind, count, state }) => `<button class="kind-chip" type="button" data-kind="${escapeHtml(kind)}" data-state="${state || ""}" ${count || state ? "" : "disabled"} title="click: only ${escapeHtml(entryKinds[kind] || kind)} · shift-click: hide it">${escapeHtml(entryKinds[kind] || kind)}<small>${count}</small></button>`).join("")
      + (comparing ? `<span class="chip-divider"></span>${statusTerms.map(([term, label]) => `<button class="status-chip" type="button" data-term="${term}" data-state="${present.has(term) ? "include" : ""}">${label}</button>`).join("")}` : "");
    warnings.hidden = !page.warnings.length;
    warnings.textContent = page.warnings.join(" · ");
  }
  // The time settings the current ranking used, and the stored defaults.
  let effective = null;
  let stored = null;
  // The last ranking version this tab saw, so an edit made elsewhere
  // (an impact, a relation, K) can be compared against what it replaced.
  const GENERATION_KEY = "nrs.scores.generation";
  let previousGeneration = null;
  function noteGeneration(page) {
    let seen = null;
    try {
      seen = sessionStorage.getItem(GENERATION_KEY);
      sessionStorage.setItem(GENERATION_KEY, String(page.generation));
    } catch {}
    const note = app.querySelector(".compare-note");
    if (comparing) {
      if (!page.comparing && note) note.textContent = `version ${view.cmp_gen} is no longer cached -- nothing to compare against`;
      return;
    }
    if (seen !== null && seen !== String(page.generation) && note instanceof HTMLElement) {
      previousGeneration = seen;
      note.hidden = false;
    }
  }
  function setQuery(q) {
    clearTimeout(searchTimer);
    view.q = q.trim();
    // Rewriting the box mid-typing would eat a just-typed space.
    if (document.activeElement !== search) search.value = view.q;
    reload();
  }
  chips.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target.closest("button") : null;
    if (!(target instanceof HTMLButtonElement)) return;
    const terms = termsOf(view.q);
    const term = target.getAttribute("data-term");
    if (term) {
      setQuery((terms.includes(term) ? terms.filter((item) => item !== term) : [...terms, term]).join(" "));
      return;
    }
    const info = kinds.find((item) => item.kind === target.getAttribute("data-kind"));
    if (!info) return;
    const wanted = event.shiftKey ? "exclude" : "include";
    const kept = terms.filter((item) => !info.terms.includes(item));
    if (info.state !== wanted) kept.push(`${wanted === "exclude" ? "-" : ""}type:${info.kind}`);
    setQuery(kept.join(" "));
  });

  function reload() {
    token++;
    loading = false;
    loaded = 0;
    total = Infinity;
    selected = -1;
    tbody.innerHTML = "";
    syncView();
    loadMore();
  }

  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) loadMore();
  }, { rootMargin: "800px 0px" });
  observer.observe(sentinel);
  pageTeardown = () => observer.disconnect();

  app.querySelector("thead")?.addEventListener("click", (event) => {
    const button = event.target instanceof Element ? event.target.closest(".sort-button") : null;
    if (!(button instanceof HTMLElement)) return;
    const column = button.getAttribute("data-sort") || "value";
    view.dir = view.sort === column && view.dir === "desc" ? "asc" : "desc";
    view.sort = column;
    reload();
  });

  search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => setQuery(search.value), 250);
  });
  search.addEventListener("keydown", (event) => {
    if (event.key === "Enter") setQuery(search.value);
  });

  app.querySelector("[data-clear-compare]")?.addEventListener("click", () => {
    navigate(pageUrl({ cmp_gen: "" }));
  });
  app.querySelector("[data-start-compare]")?.addEventListener("click", () => {
    if (previousGeneration !== null) navigate(pageUrl({ cmp_gen: previousGeneration }));
  });

  const timeInputs = () => {
    const form = new FormData(controls);
    return { shared: String(form.get("shared") || "").trim(), length: String(form.get("length") || "").trim() };
  };
  controls.addEventListener("submit", (event) => {
    event.preventDefault();
    const { shared, length } = timeInputs();
    const before = effective;
    const next = { shared: shared === "" ? stored?.shared : Number(shared), length: length === "" ? stored?.length_days : Number(length) };
    const changed = before && (next.shared !== before.shared || next.length !== before.length_days);
    // Compare against the run being replaced, so Δ answers "what did
    // this change do?".
    navigate(pageUrl({ shared, length, cmp_gen: "", ...(changed ? { cmp_shared: String(before.shared), cmp_length: String(before.length_days) } : { cmp_shared: "", cmp_length: "" }) }));
  });
  app.querySelector("[data-save-time]")?.addEventListener("click", async () => {
    if (!effective) return;
    const { shared, length } = timeInputs();
    const payload = { shared: shared === "" ? effective.shared : Number(shared), length_days: length === "" ? effective.length_days : Number(length) };
    try {
      await sendJson("/api/v1/calibration/time", "PUT", payload);
      showToast(`time saved: λ ${payload.shared} · L ${payload.length_days} days`);
      navigate(pageUrl({ shared: "", length: "" }));
    } catch (error) {
      showToast(error?.message || String(error), "error");
    }
  });

  const select = (index) => {
    const rows = tbody.querySelectorAll("tr");
    if (!rows.length) return;
    rows[selected]?.classList.remove("selected");
    selected = Math.max(0, Math.min(rows.length - 1, index));
    rows[selected].classList.add("selected");
    rows[selected].scrollIntoView({ block: "nearest" });
  };
  pageKeys = (event, typing) => {
    if (typing) {
      if (event.key === "Escape" && event.target === search) search.blur();
      return;
    }
    if (event.key === "/") {
      event.preventDefault();
      search.focus();
      search.select();
    } else if (event.key === "j") {
      select(selected + 1);
    } else if (event.key === "k") {
      select(selected - 1);
    } else if (event.key === "Enter" && selected >= 0) {
      const id = tbody.querySelectorAll("tr")[selected]?.getAttribute("data-id");
      if (id) navigate(pathForEntry(id));
    }
  };

  syncView();
  await loadMore();
}

async function renderEntries() {
  currentEntryData = null;
  document.title = "Entries — NRS";
  app.innerHTML = `
    <h1>entries</h1>
    <p class="muted entry-subtitle">Paste an entry URL to ingest it through the anime or music store, or filter to search NRS and both stores.</p>
    <form class="ingest-form">
      ${textField({}, "query", "anime, manga, novel, visual novel, or music URL", "", "url", 'placeholder="https://myanimelist.net/anime/5114 or https://vgmdb.net/album/104418" required')}
      <button class="button primary" type="submit">ingest</button>
      <p class="form-error" hidden></p>
    </form>
    <div class="toolbar">
      <input class="search" type="search" placeholder="filter by title…">
    </div>
    <p class="entry-count">…</p>
    <div class="entry-list"><p class="loading">Loading entries…</p></div>
    <section class="store-hits" hidden>
      <h2>From the stores <span class="count"></span></h2>
      <p class="muted entry-subtitle">What the anime and music stores already hold. Add one to make it an NRS entry.</p>
      <div class="entry-list store-hit-list"></div>
      <p class="dim store-hit-errors" hidden></p>
    </section>
    <section class="store-hits discover-hits" hidden>
      <h2>On AniList <span class="count"></span></h2>
      <p class="muted entry-subtitle">Anime and manga the anime store hasn't fetched yet. Adding one fetches it, then makes it an NRS entry.</p>
      <div class="entry-list discover-list"></div>
    </section>
  `;
  const ingest = app.querySelector(".ingest-form");
  const input = app.querySelector(".search");
  const list = app.querySelector(".entry-list");
  const count = app.querySelector(".entry-count");
  if (!(ingest instanceof HTMLFormElement) || !(input instanceof HTMLInputElement) || !(list instanceof HTMLElement) || !(count instanceof HTMLElement)) return;

  ingest.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = ingest.querySelector("button");
    const error = ingest.querySelector(".form-error");
    if (button instanceof HTMLButtonElement) button.disabled = true;
    if (error instanceof HTMLElement) error.hidden = true;
    try {
      const payload = Object.fromEntries(new FormData(ingest).entries());
      const result = await sendJson("/api/v1/entries/ingest", "POST", payload);
      navigate(pathForEntry(result.id));
    } catch (reason) {
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    } finally {
      if (button instanceof HTMLButtonElement) button.disabled = false;
    }
  });

  async function load() {
    try {
      const query = new URLSearchParams({ limit: "5000" });
      if (input.value.trim()) query.set("q", input.value.trim());
      const data = await getJson(`/api/v1/entries?${query}`);
      count.textContent = `${data.total} of ${data.database_total} entries`;
      list.innerHTML = data.entries.length
        ? data.entries.map(entryRow).join("")
        : `<p class="empty">No matching entries.</p>`;
    } catch (error) {
      renderError(error);
    }
  }
  input.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      load();
      loadStoreHits();
    }, 250);
  });

  const hits = app.querySelector(".store-hits");
  const hitList = app.querySelector(".store-hit-list");
  const hitErrors = app.querySelector(".store-hit-errors");
  if (!(hits instanceof HTMLElement) || !(hitList instanceof HTMLElement) || !(hitErrors instanceof HTMLElement)) return;
  const discoverHits = app.querySelector(".discover-hits");
  const discoverList = app.querySelector(".discover-list");
  if (!(discoverHits instanceof HTMLElement) || !(discoverList instanceof HTMLElement)) return;
  let storeSearch = null;
  let discoverSearch = null;
  let hitData = [];
  let discoverData = [];
  pageTeardown = () => {
    storeSearch?.abort();
    discoverSearch?.abort();
  };
  // Separate from getJson, which would abort the entry list's request.
  async function loadStoreHits() {
    const q = input.value.trim();
    storeSearch?.abort();
    loadDiscover(q);
    if (q.length < 2) {
      hits.hidden = true;
      return;
    }
    const controller = (storeSearch = new AbortController());
    try {
      const response = await fetch(`/api/v1/stores/search?${new URLSearchParams({ q })}`, {
        signal: controller.signal,
        headers: { Accept: "application/json" },
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
      hitData = body.results;
      const fresh = hitData.filter((hit) => !hit.entry_id).length;
      hits.querySelector("h2 .count").textContent = `${fresh} not in NRS`;
      hitList.innerHTML = hitData.length ? hitData.map(storeHitRow).join("") : `<p class="empty">Neither store has a match.</p>`;
      hitErrors.hidden = !body.errors.length;
      hitErrors.textContent = body.errors.join("; ");
      hits.hidden = false;
      renderDiscover();
    } catch (error) {
      if (error?.name === "AbortError") return;
      hitList.innerHTML = `<p class="empty">${escapeHtml(error?.message || String(error))}</p>`;
      hits.hidden = false;
    }
  }
  // AniList through the anime store's discover: slower, so it fills in
  // on its own.
  async function loadDiscover(q) {
    discoverSearch?.abort();
    discoverData = [];
    if (q.length < 3) {
      discoverHits.hidden = true;
      return;
    }
    const controller = (discoverSearch = new AbortController());
    try {
      const response = await fetch(`/api/v1/stores/anime/discover?${new URLSearchParams({ q })}`, {
        signal: controller.signal,
        headers: { Accept: "application/json" },
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
      discoverData = body.candidates;
      renderDiscover();
    } catch (error) {
      if (error?.name === "AbortError") return;
      discoverList.innerHTML = `<p class="empty">${escapeHtml(error?.message || String(error))}</p>`;
      discoverHits.hidden = false;
    }
  }
  // Candidates the store search above doesn't already list.
  function renderDiscover() {
    const listed = new Set(hitData.filter((hit) => hit.store === "anime").map((hit) => hit.store_id));
    const shown = discoverData.map((candidate, index) => [candidate, index]).filter(([candidate]) => !listed.has(candidate.store_id));
    discoverHits.hidden = !discoverData.length;
    discoverHits.querySelector("h2 .count").textContent = `${shown.filter(([candidate]) => !candidate.entry_id).length} not in NRS`;
    discoverList.innerHTML = shown.length
      ? shown.map(([candidate, index]) => candidateRow(candidate, index)).join("")
      : `<p class="empty">Everything AniList found is listed above.</p>`;
  }
  discoverList.addEventListener("click", async (event) => {
    const button = event.target instanceof Element ? event.target.closest("[data-ingest]") : null;
    if (!(button instanceof HTMLButtonElement)) return;
    const candidate = discoverData[Number(button.dataset.ingest)];
    const id = await ingestCandidate(candidate, button);
    if (id) navigate(pathForEntry(id));
  });
  hitList.addEventListener("click", async (event) => {
    const button = event.target instanceof Element ? event.target.closest("[data-adopt]") : null;
    if (!(button instanceof HTMLButtonElement)) return;
    const hit = hitData[Number(button.dataset.adopt)];
    button.disabled = true;
    try {
      const result = await sendJson("/api/v1/entries/adopt", "POST", { store: hit.store, store_id: hit.store_id });
      navigate(pathForEntry(result.id));
    } catch (reason) {
      showToast(reason?.message || String(reason), "error");
      button.disabled = false;
    }
  });

  await load();
}

/**
 * Ingests a discover candidate through the anime or music store (which
 * fetches it from its providers) into an NRS entry; the entry's id, or
 * null after showing why not.
 */
async function ingestCandidate(candidate, button) {
  button.disabled = true;
  button.textContent = "adding…";
  try {
    return (await sendJson("/api/v1/entries/ingest", "POST", { query: candidate.query })).id;
  } catch (reason) {
    showToast(reason?.message || String(reason), "error");
    button.disabled = false;
    button.textContent = "add";
    return null;
  }
}

/** A discover candidate as an entries-page row (see `storeHitRow`). */
function candidateRow(candidate, index) {
  return storeHitRow({ ...candidate, store: "anilist" }, index).replace(`data-adopt="${index}"`, `data-ingest="${index}"`);
}

/** A store search result: "in NRS" when bound, else an add button. */
function storeHitRow(hit, index) {
  const editions = hit.other_editions ? ` · +${hit.other_editions} edition${hit.other_editions === 1 ? "" : "s"}` : "";
  const subtitle = hit.subtitle || editions ? `<span class="hit-subtitle">${escapeHtml((hit.subtitle || "") + editions)}</span>` : "";
  let host = hit.store;
  try {
    if (hit.canonical_url) host = new URL(hit.canonical_url).hostname.replace(/^www\./, "");
  } catch {}
  const source = hit.canonical_url
    ? `<a class="entry-ref" href="${escapeHtml(hit.canonical_url)}" target="_blank" rel="noopener">${escapeHtml(host)} ↗</a>`
    : `<span class="entry-ref">${escapeHtml(host)}</span>`;
  const action = hit.entry_id
    ? `<a class="link-button" href="${pathForEntry(hit.entry_id)}" data-link>in NRS</a>`
    : `<button class="button" type="button" data-adopt="${index}">add</button>`;
  return `<div class="entry-row store-hit${hit.entry_id ? " adopted" : ""}">
    <span class="entry-title">${storeKindTag(hit.kind)}${escapeHtml(hit.title)}${subtitle}</span>
    <span class="hit-actions">${source}${action}</span>
  </div>`;
}

function entryRow(entry) {
  return `
    <a class="entry-row" href="${pathForEntry(entry.id)}" data-link>
      <span class="entry-title">${kindTag(entry.kind)}${escapeHtml(entry.title)}</span>
      <span class="entry-ref">${escapeHtml(entry.store)}:${escapeHtml(entry.store_id)}</span>
    </a>`;
}

function anchorChips(anchors) {
  if (!anchors?.length) return `<span class="dim">no anchor composition</span>`;
  return anchors
    .map(({ anchor, share }) => `<span class="chip">${escapeHtml(anchor)}<small>${Math.round(share * 100)}%</small></span>`)
    .join("");
}

function periodMarkup(period) {
  if (!period) return "";
  return `
    <div class="period">
      <div class="period-line">
        <span>${escapeHtml(period.from)}</span><span class="period-rule"></span><span>${escapeHtml(period.to)}</span>
      </div>
      <p class="period-caption">${escapeHtml(period.caption)}</p>
    </div>`;
}

function paramsMarkup(params) {
  const ignored = new Set(["kind", "source", "note", "dah_factor"]);
  const rows = Object.entries(params || {}).filter(([key]) => !ignored.has(key));
  if (!rows.length) return "";
  return `
    <dl class="detail-grid">
      ${rows.map(([key, value]) => `<dt>${escapeHtml(key)}</dt><dd class="mono">${escapeHtml(value)}</dd>`).join("")}
    </dl>`;
}

function impactCard(impact) {
  const note = impact.note || impact.source_comment;
  const source = impact.atoms?.[0]?.source_entry;
  const sourceMarkup = source
    ? `<p class="source-line">from <a href="${pathForEntry(source.id)}" data-link>${escapeHtml(source.title)}</a> ${kindTag(source.kind)}</p>`
    : "";
  return `
    <article class="impact-card" data-category="${escapeHtml(impact.category)}" data-impact-kind="${escapeHtml(impact.kind || "")}">
      <div class="card-head"><div class="category-badge">${escapeHtml(impact.category)}</div>
        <div class="card-copy">
          <h3 class="impact-title">${escapeHtml(impact.title)}${impact.shared ? `<span class="share-pill" title="shared between ${impact.shared.members.length} entries">shared <span class="mono">${Number(impact.contributor_share ?? 0).toFixed(2)}</span></span>` : ""}${impact.needs_review ? '<span class="review-flag">needs review</span>' : ""}</h3>
          <p class="impact-description">${escapeHtml(impact.description)}</p>
          <div class="chips">${anchorChips(impact.anchors)}</div>
          ${sourceMarkup}
          ${periodMarkup(impact.period)}
          ${note ? `<p class="note">${escapeHtml(note)}</p>` : ""}
        </div>
        ${impact.period ? "" : `<div class="metric"><div class="metric-value">${escapeHtml(impact.metric.value)}</div><div class="metric-label">${escapeHtml(impact.metric.label)}</div>${impact.metric.progress != null ? `<div class="metric-bar"><span style="width:${Math.max(0, Math.min(1, Number(impact.metric.progress))) * 100}%"></span></div>` : ""}</div>`}
      </div>
      <details class="card-details" data-disclosure="${escapeHtml(impact.id)}"><summary>details &amp; edit</summary><div class="details-body">
        <p class="raw-id"><span class="mono">${escapeHtml(impact.id)}</span>${!impact.shared && impact.contributor_share != null && Math.abs(impact.contributor_share - 1) > .005 ? `<span>contributor share <span class="mono">${Number(impact.contributor_share).toFixed(2)}</span></span>` : ""}</p>
        ${impact.shared ? `<div class="card-split" data-inline-split="${escapeHtml(impact.id)}"></div>` : ""}
        ${impact.kind ? "" : paramsMarkup(impact.params)}
        <div class="detail-actions">
          ${(impact.needs_review || (!impact.note && impact.source_comment)) ? `<button class="button primary" type="button" data-action="confirm-impact" data-id="${escapeHtml(impact.id)}">confirm</button>` : ""}
          ${impact.shared ? "" : `<button class="link-button" type="button" data-action="share-impact" data-id="${escapeHtml(impact.id)}" title="split this impact between this entry and others">share with other entries</button>`}
          <button hidden type="button" data-action="edit-impact" data-id="${escapeHtml(impact.id)}">edit impact</button>
        </div>
      </div></details>
    </article>`;
}

function groupedImpactCard(items) {
  const first = items[0];
  const anchors = [...new Set(items.flatMap((impact) => impact.anchors.map(({ anchor }) => anchor)))];
  const reviews = items.filter((impact) => impact.needs_review).length;
  return `<details class="impact-group" data-category="${escapeHtml(first.category)}" data-impact-kind="${escapeHtml(first.kind || "")}" data-disclosure="group-${escapeHtml(first.kind)}"><summary>
    <span class="disclosure">▶</span><span class="category-badge">${escapeHtml(first.category)}</span>
    <span class="group-copy"><strong>${escapeHtml(first.title)}</strong><span class="group-description">${escapeHtml(first.description)}</span></span>
    <span class="group-count">${items.length} <small>moments</small>${reviews ? ` <em>${reviews} need review</em>` : ""}</span>
    <span class="group-anchors">${anchors.slice(0, 4).map((anchor) => `<span class="chip">${escapeHtml(anchor)}</span>`).join("")}${anchors.length > 4 ? `<span class="dim">+${anchors.length - 4}</span>` : ""}</span>
  </summary><div class="group-body">${items.map(impactCard).join("")}</div></details>`;
}

function impactSections(impacts) {
  return Object.entries(sections)
    .map(([key, [title, description]]) => {
      const items = impacts.filter((impact) => impact.section === key);
      if (!items.length) return "";
      const groups = items.reduce((result, impact) => {
        const groupKey = impact.kind || impact.id;
        (result[groupKey] ||= []).push(impact);
        return result;
      }, {});
      const content = key === "responses"
        ? Object.values(groups).map((group) => group.length > 1 ? groupedImpactCard(group) : impactCard(group[0])).join("")
        : items.map(impactCard).join("");
      return `
        <section class="subsection">
          <h3 class="subsection-title">${title}<span class="count">${items.length}</span></h3>
          <p class="section-intro">${description}</p>
          <div class="card-grid">${content}</div>
        </section>`;
    })
    .join("");
}

// Past this many anchor weights/routes an inline list is noise, so it
// moves behind a disclosure instead.
const TRANSFORM_INLINE_LIMIT = 4;

function transformParts(transform) {
  if (transform.type === "regional") return ["anchor weights", transform.weights.map(({ anchor, weight }) => `${anchor} ${Math.round(weight * 100)}%`)];
  if (transform.type === "transport") return ["anchor routes", transform.routes.map(({ from, to, weight }) => `${from}→${to} ${Math.round(weight * 100)}%`)];
  return [transform.type, []];
}

function transformList(parts) {
  return `<div class="transform-list">${parts.map((part) => `<div>${escapeHtml(part)}</div>`).join("")}</div>`;
}

function transformMarkup(transform) {
  if (!transform) return "";
  if (transform.type === "scalar") {
    return Math.abs(transform.factor - 1) < .005 ? "full weight" : `${Math.round(transform.factor * 100)}% weight`;
  }
  const [label, parts] = transformParts(transform);
  if (parts.length <= TRANSFORM_INLINE_LIMIT) return escapeHtml(parts.join(" · "));
  return `<details class="inline-disclosure"><summary>${escapeHtml(label)} · ${parts.length}</summary>${transformList(parts)}</details>`;
}

// A credit is shown as the roles expression someone actually wrote
// (`compose*0.6+arrange*0.4`), with the computed weights one click
// away for checking the math.
function relationWeightMarkup(relation) {
  if (relation.kind === "credit" && relation.params.roles) {
    const [, parts] = transformParts(relation.transform);
    const matrix = parts.length ? transformList(parts) : `<div class="transform-list">${transformMarkup(relation.transform)}</div>`;
    return `<span>${escapeHtml(relation.params.roles)}</span><details class="inline-disclosure"><summary>matrix</summary>${matrix}</details>`;
  }
  return transformMarkup(relation.transform);
}

/** `position` (from `bindTracklist`) numbers the row as an album track. */
function relationRow(relation, position = null) {
  return `<div class="relation-row${position == null ? "" : " track-row"}">
    ${position == null ? "" : `<span class="track-no">${escapeHtml(position)}</span>`}
    <a class="relation-title" href="${pathForEntry(relation.other.id)}" data-link>${escapeHtml(relation.other.title)}</a>
    <span class="relation-kind">${kindTag(relation.other.kind)}</span>
    <div class="relation-weight">${relationWeightMarkup(relation)}</div>
    <div class="relation-actions">${relation.editable ? `<button class="edit-link" type="button" data-action="edit-relation" data-id="${escapeHtml(relation.id)}">edit</button>` : ""}<button class="edit-link danger-button" type="button" data-action="delete-relation" data-id="${escapeHtml(relation.id)}">delete</button></div>
  </div>`;
}

/** A store track no `contains` from this entry binds: one dim line. */
function unlinkedTrackRow(position, title) {
  return `<div class="relation-row track-row unlinked">
    <span class="track-no">${escapeHtml(position)}</span>
    <span class="relation-title">${escapeHtml(title || "untitled track")}</span>
    <span class="relation-kind">not linked</span>
  </div>`;
}

/**
 * An album's Contains group in the music store's track order: linked
 * tracks numbered, the store's other tracks as dim lines in between,
 * relations the tracklist doesn't have last. The plain list stays when
 * the store has no tracklist for the entry or can't be reached.
 */
async function bindTracklist(data) {
  const group = app.querySelector('details.relation-group[data-disclosure="relations-Contains"]');
  if (data.entry.store !== "url" || !(group instanceof HTMLDetailsElement)) return;
  let tracks;
  try {
    const response = await fetch(`/api/v1/entries/${encodeURIComponent(data.entry.id)}/tracklist`);
    if (!response.ok) return;
    ({ tracks } = await response.json());
  } catch {
    return;
  }
  if (!tracks?.length || currentEntryData !== data || !group.isConnected) return;

  const pending = new Map(data.relations.filter((relation) => relation.label === "Contains").map((relation) => [relation.id, relation]));
  const discs = new Set(tracks.map((track) => track.disc ?? 1));
  const position = (track) =>
    track.track == null ? "·" : discs.size > 1 ? `${track.disc ?? 1}-${String(track.track).padStart(2, "0")}` : String(track.track);
  const rows = [];
  let linkedTracks = 0;
  for (const track of tracks) {
    const linked = track.relation_ids.map((id) => pending.get(id)).filter(Boolean);
    if (linked.length) linkedTracks++;
    for (const relation of linked) {
      pending.delete(relation.id);
      rows.push(relationRow(relation, position(track)));
    }
    if (!linked.length) rows.push(unlinkedTrackRow(position(track), track.title));
  }
  for (const relation of pending.values()) rows.push(relationRow(relation, "·"));

  const count = group.querySelector(":scope > summary .count");
  if (count) {
    count.textContent = `${linkedTracks}/${tracks.length} tracks`;
    count.setAttribute("title", "tracks linked by contains / tracks on the store's tracklist");
  }
  group.querySelectorAll(":scope > .relation-row").forEach((row) => row.remove());
  group.insertAdjacentHTML("beforeend", rows.join(""));
  group.classList.add("tracklist");
}

/**
 * What the entry's store suggests around it that NRS doesn't record yet
 * (a track's album, an album's tracks), as a checklist: picked rows add
 * the other entry when NRS lacks it, then the relation. The albums a
 * track is on start checked; an album's tracks don't.
 */
async function bindRelated(data) {
  const slot = app.querySelector(".related-slot");
  if (data.entry.store !== "url" || !(slot instanceof HTMLElement)) return;
  let suggestions;
  try {
    const response = await fetch(`/api/v1/entries/${encodeURIComponent(data.entry.id)}/related`);
    if (!response.ok) return;
    ({ suggestions } = await response.json());
  } catch {
    return;
  }
  if (currentEntryData !== data || !slot.isConnected) return;
  const open = suggestions.filter((suggestion) => !suggestion.relation_exists);
  if (!open.length) return;
  slot.innerHTML = `
    <details class="relation-group related-group" open>
      <summary>Suggested by the ${escapeHtml(open[0].store)} store <span class="count">${open.length}</span></summary>
      <form class="related-form">
        ${open.map(relatedRow).join("")}
        <div class="related-actions">
          ${open.length > 3 ? `<button class="link-button" type="button" data-related-all>select all</button>` : ""}
          <button class="button primary" type="submit">add selected</button>
        </div>
      </form>
    </details>`;
  const form = slot.querySelector(".related-form");
  if (!(form instanceof HTMLFormElement)) return;
  const boxes = () => [...form.querySelectorAll('input[name="pick"]')];
  form.addEventListener("click", (event) => {
    if (!(event.target instanceof Element) || !event.target.closest("[data-related-all]")) return;
    const all = boxes().every((box) => box.checked);
    boxes().forEach((box) => (box.checked = !all));
  });
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const items = boxes()
      .filter((box) => box.checked)
      .map((box) => {
        const suggestion = open[Number(box.value)];
        const edition = form.querySelector(`select[name="edition-${box.value}"]`);
        const storeId = edition instanceof HTMLSelectElement ? edition.value : suggestion.target.store_id;
        return { store_id: storeId, kind: suggestion.kind, direction: suggestion.direction };
      });
    if (!items.length) return;
    const button = form.querySelector('button[type="submit"]');
    if (button instanceof HTMLButtonElement) button.disabled = true;
    try {
      const result = await sendJson(`/api/v1/entries/${encodeURIComponent(data.entry.id)}/related`, "POST", { items });
      const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
      showToast(`Added ${plural(result.relations_added, "relation", "relations")}, ${plural(result.entries_added, "new entry", "new entries")}`);
      await refreshEntry(data.entry.id);
    } catch (reason) {
      showToast(reason?.message || String(reason), "error");
      if (button instanceof HTMLButtonElement) button.disabled = false;
    }
  });
}

/** An edition in a picker: its title, where it's from, and its date. */
function editionLabel(edition) {
  let host = "";
  try {
    host = new URL(edition.canonical_url).hostname.replace(/^www\./, "");
  } catch {}
  const parts = [edition.title, host, edition.release_date?.slice(0, 10), `${edition.urls?.length || 0} links`];
  return parts.filter(Boolean).join(" · ");
}

// How the target relates to this entry, as the relation sections word it.
function relatedPhrase(suggestion) {
  const incoming = suggestion.direction === "incoming";
  if (suggestion.kind === "contains") return incoming ? "on" : "contains";
  // Remix relations run from the source to the derived work.
  if (suggestion.kind === "remix") return incoming ? "remix of" : "remixed into";
  return incoming ? `${suggestion.kind} of` : suggestion.kind;
}

function relatedRow(suggestion, index) {
  const target = suggestion.target;
  const title = suggestion.target_entry_id
    ? `<a class="relation-title" href="${pathForEntry(suggestion.target_entry_id)}" data-link>${escapeHtml(target.title)}</a>`
    : `<span class="relation-title">${escapeHtml(target.title)}</span>`;
  const details = [suggestion.evidence, target.subtitle, target.release_date?.slice(0, 10)].filter(Boolean);
  const editions = suggestion.editions || [];
  // Several editions of one work: NRS's one is already chosen, else pick.
  let editionPicker = "";
  if (editions.length > 1 && suggestion.target_entry_id) {
    editionPicker = `<span class="related-editions">${editions.length} editions</span>`;
  } else if (editions.length > 1) {
    editionPicker = `<select class="related-editions" name="edition-${index}" aria-label="edition to add">
      ${editions.map(({ target: edition }) => `<option value="${escapeHtml(edition.store_id)}">${escapeHtml(editionLabel(edition))}</option>`).join("")}
    </select>`;
  }
  return `<label class="relation-row related-row">
    <input type="checkbox" name="pick" value="${index}" ${suggestion.direction === "incoming" ? "checked" : ""}>
    <span class="related-what"><span class="related-phrase">${escapeHtml(relatedPhrase(suggestion))}</span>${title}</span>
    <span class="relation-kind">${storeKindTag(target.kind)} ${suggestion.target_entry_id ? "in NRS" : "new entry"}</span>
    <span class="related-details">${escapeHtml(details.join(" · "))}${editionPicker}</span>
  </label>`;
}

function formatContribution(value) {
  const rounded = Math.abs(value) < .005 ? 0 : value;
  return rounded.toFixed(2);
}

// A bar of |value| against the largest |value| shown beside it;
// negatives read in oxblood.
function contributionBar(value, scale, parts = null) {
  const width = scale > 0 ? Math.min(1, Math.abs(value) / scale) * 100 : 0;
  if (value < 0) return `<span class="score-bar"><span class="negative" style="width:${width}%"></span></span>`;
  const segments = parts
    ? parts.filter(([, part]) => part > 0).map(([kind, part]) => `<span class="${kind}" style="width:${(part / value) * width}%"></span>`).join("")
    : `<span class="own" style="width:${width}%"></span>`;
  return `<span class="score-bar">${segments}</span>`;
}

function anchorStrip(anchors) {
  const positive = anchors.filter((anchor) => anchor.contribution > 0);
  const total = positive.reduce((sum, anchor) => sum + anchor.contribution, 0);
  if (!(total > 0)) return "";
  return `<div class="score-strip" aria-hidden="true">${positive.map((anchor, index) => {
    const share = anchor.contribution / total;
    return `<span style="width:${share * 100}%;background:${splitColors[index % splitColors.length]}" title="${escapeHtml(anchor.anchor)} ${formatContribution(anchor.contribution)}">${share >= .08 ? escapeHtml(anchor.anchor) : ""}</span>`;
  }).join("")}</div>`;
}

function scoreAnchorRows(anchors, scale) {
  return anchors.map((anchor) => {
    const parts = [["own", anchor.own], ["borrowed", anchor.borrowed]];
    const title = `mass ${anchor.mass.toFixed(3)} · embedded ${anchor.embedded.toFixed(3)} · own ${formatContribution(anchor.own)} · borrowed ${formatContribution(anchor.borrowed)}`;
    return `<div class="score-row" title="${escapeHtml(title)}">
      <span class="score-label mono">${escapeHtml(anchor.anchor)}</span>
      ${contributionBar(anchor.contribution, scale, parts)}
      <span class="score-amount">${formatContribution(anchor.contribution)}</span>
    </div>`;
  }).join("");
}

function scoreImpactRow(impact, scale, kind = "own") {
  const anchors = impact.anchors.map(({ anchor, contribution }) => `<span class="chip">${escapeHtml(anchor)}<small>${formatContribution(contribution)}</small></span>`).join("");
  return `<div class="score-row" title="${escapeHtml(impact.id)}">
    <span class="score-label"><span>${escapeHtml(impact.title)}</span>${impact.anchors.length > 1 ? `<span class="score-chips">${anchors}</span>` : `<span class="dim mono">${escapeHtml(impact.anchors[0]?.anchor || "")}</span>`}</span>
    ${contributionBar(impact.contribution, scale, kind === "own" ? null : [[kind, impact.contribution]])}
    <span class="score-amount">${formatContribution(impact.contribution)}</span>
  </div>`;
}

function scoreLender(lender, scale, openDisclosures) {
  const key = `score-lender-${lender.entry.id}`;
  const innerScale = Math.max(...lender.impacts.map((impact) => Math.abs(impact.contribution)), 0);
  return `<details class="score-lender" data-disclosure="${escapeHtml(key)}"${openDisclosures.has(key) ? " open" : ""}>
    <summary class="score-row">
      <span class="score-label"><span><span class="disclosure">▶</span> <a href="${pathForEntry(lender.entry.id)}" data-link>${escapeHtml(lender.entry.title)}</a></span> ${kindTag(lender.entry.kind)}</span>
      ${contributionBar(lender.contribution, scale, [["borrowed", lender.contribution]])}
      <span class="score-amount">${formatContribution(lender.contribution)}</span>
    </summary>
    <div class="score-lender-body">${lender.impacts.map((impact) => scoreImpactRow(impact, innerScale, "borrowed")).join("")}</div>
  </details>`;
}

// Past this many lenders the rest fold behind one disclosure.
const SCORE_LENDER_LIMIT = 8;

function scorePanelMarkup(score, openDisclosures) {
  const ownTotal = score.own.reduce((sum, impact) => sum + impact.contribution, 0);
  const borrowedTotal = score.borrowed.reduce((sum, lender) => sum + lender.contribution, 0);
  const anchorScale = Math.max(...score.anchors.map((anchor) => Math.abs(anchor.contribution)), 0);
  const sourceScale = Math.max(...score.own.map((impact) => Math.abs(impact.contribution)), ...score.borrowed.map((lender) => Math.abs(lender.contribution)), 0);
  const rank = (rank, total, label) => `<div><dt>${escapeHtml(label)}</dt><dd>${rank == null ? "—" : `#${rank}`}<small> / ${total}</small></dd></div>`;
  const lenders = score.borrowed.map((lender) => scoreLender(lender, sourceScale, openDisclosures));
  const moreKey = "score-lenders-more";
  const lenderMarkup = lenders.length > SCORE_LENDER_LIMIT
    ? `${lenders.slice(0, SCORE_LENDER_LIMIT).join("")}<details class="score-more" data-disclosure="${moreKey}"${openDisclosures.has(moreKey) ? " open" : ""}><summary>${lenders.length - SCORE_LENDER_LIMIT} more entries</summary>${lenders.slice(SCORE_LENDER_LIMIT).join("")}</details>`
    : lenders.join("");
  return `
    <h2>Score</h2>
    <div class="score-head">
      <div class="score-value"><span class="mono">${score.value.toFixed(2)}</span><small>V</small></div>
      <dl class="score-ranks">
        ${rank(score.rank, score.total, "overall")}
        ${rank(score.kind_rank, score.kind_total, score.kind)}
        <div><dt>time</dt><dd>λ ${score.time.shared.toFixed(2)}<small> · L ${Math.round(score.time.length_days)}d</small></dd></div>
      </dl>
    </div>
    ${score.anchors.length ? anchorStrip(score.anchors) : `<p class="empty">No scored mass yet.</p>`}
    ${score.anchors.length ? `<div class="score-columns">
      <section>
        <h3 class="subsection-title">By anchor<span class="count">${score.anchors.length}</span></h3>
        <p class="section-intro">Each anchor's share of V after pooling. <span class="legend own"></span>own <span class="legend borrowed"></span>borrowed</p>
        <div class="score-rows">${scoreAnchorRows(score.anchors, anchorScale)}</div>
      </section>
      <section>
        <h3 class="subsection-title">By source</h3>
        <p class="section-intro">Each anchor's share split across the impacts whose mass reaches it. Borrowed mass is listed under the entry that holds the impact, however many relations it passed through.</p>
        ${score.own.length ? `<h4 class="score-group">Own impacts <span class="score-amount">${formatContribution(ownTotal)}</span></h4><div class="score-rows">${score.own.map((impact) => scoreImpactRow(impact, sourceScale)).join("")}</div>` : ""}
        ${score.borrowed.length ? `<h4 class="score-group">Borrowed from ${score.borrowed.length} ${score.borrowed.length === 1 ? "entry" : "entries"} <span class="score-amount">${formatContribution(borrowedTotal)}</span></h4><div class="score-rows">${lenderMarkup}</div>` : ""}
      </section>
    </div>` : ""}`;
}

async function bindScore(data, openDisclosures) {
  const panel = app.querySelector("[data-score]");
  if (!(panel instanceof HTMLElement)) return;
  let score;
  try {
    const response = await fetch(`/api/v1/entries/${encodeURIComponent(data.entry.id)}/score`);
    if (!response.ok) throw new Error((await response.json().catch(() => null))?.error || `HTTP ${response.status}`);
    score = await response.json();
  } catch (error) {
    if (currentEntryData === data && panel.isConnected) panel.innerHTML = `<h2>Score</h2><p class="error">${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`;
    return;
  }
  if (currentEntryData !== data || !panel.isConnected) return;
  panel.innerHTML = scorePanelMarkup(score, openDisclosures);
}

function relationSectionsMarkup(relations) {
  return Object.entries(relationSections)
    .map(([section, [title, description, order]]) => {
      const sectionRelations = relations.filter((relation) => relation.section === section);
      if (!sectionRelations.length) return "";
      const labels = [...new Set(sectionRelations.map((relation) => relation.label))]
        .sort((a, b) => order.indexOf(a) - order.indexOf(b));
      return `
        <section class="subsection relation-section" data-section="${section}">
          <h3 class="subsection-title">${title}</h3>
          <p class="section-intro">${description}</p>
          <div class="relation-groups">
            ${labels.map((label) => {
              const rows = sectionRelations.filter((relation) => relation.label === label);
              return `<details class="relation-group" ${rows.length <= 6 ? "open" : ""} data-disclosure="relations-${escapeHtml(label)}">
                <summary>${escapeHtml(label)}<span class="count">${rows.length}</span></summary>
                ${rows.map((relation) => relationRow(relation)).join("")}
              </details>`;
            }).join("")}
          </div>
        </section>`;
    })
    .join("");
}

function fieldValue(fields, name, fallback = "") {
  return fields?.[name] ?? fallback;
}

function field(label, control, hint = "") {
  return `<label class="form-field"><span>${escapeHtml(label)}</span>${control}${hint ? `<small>${escapeHtml(hint)}</small>` : ""}</label>`;
}

function textField(fields, name, label, fallback = "", type = "text", extra = "") {
  return field(label, `<input name="${name}" type="${type}" value="${escapeHtml(fieldValue(fields, name, fallback))}" ${extra}>`);
}

function rangeField(fields, name, label, fallback = "0.5", min = "0", max = "1", step = "0.05") {
  const value = fieldValue(fields, name, fallback);
  return field(label, `<div class="range-control"><input name="${name}" type="range" min="${min}" max="${max}" step="${step}" value="${escapeHtml(value)}"><output>${escapeHtml(value)}</output><div class="range-bounds"><span>${min}</span><span>${max}</span></div></div>`);
}

function selectField(fields, name, label, choices, fallback = choices[0]?.[0] || "") {
  const value = fieldValue(fields, name, fallback);
  return field(label, `<select name="${name}">${choices.map(([key, text]) => `<option value="${escapeHtml(key)}" ${String(key) === String(value) ? "selected" : ""}>${escapeHtml(text)}</option>`).join("")}</select>`);
}

function impactKindField(value) {
  const groups = Object.entries(sections)
    .filter(([section]) => section !== "other")
    .map(([section, [title]]) => `<optgroup label="${escapeHtml(title)}">${impactKinds
      .filter(([, , kindSection]) => kindSection === section)
      .map(([key, text]) => `<option value="${escapeHtml(key)}" ${key === value ? "selected" : ""}>${escapeHtml(text)}</option>`)
      .join("")}</optgroup>`)
    .join("");
  return field("kind", `<select name="kind">${groups}</select>`);
}

function formatDurationInput(value) {
  if (value == null || value === "") return "";
  const minutes = Number(value);
  if (!Number.isFinite(minutes)) return String(value ?? "");
  const seconds = Math.round(minutes * 60);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

function durationField(fields, name, label, fallback) {
  const value = formatDurationInput(fieldValue(fields, name, fallback));
  return field(label, `<input name="${name}" type="text" value="${escapeHtml(value)}" placeholder="3:41">`, "M:SS, H:MM:SS, or numeric minutes");
}

/**
 * Emotion blend as a split: each anchor is a segment of one bar summing to
 * 1 (every stored blend does). Dragging a boundary trades weight between
 * the two neighbours; the form still submits anchor1..4 / weight1..4.
 */
function blendFields(fields) {
  const slots = [1, 2, 3, 4]
    .map((number) => ({ anchor: fieldValue(fields, `anchor${number}`), weight: Number(fieldValue(fields, `weight${number}`, "0")) || 0 }))
    .filter((slot) => slot.anchor);
  const hidden = [1, 2, 3, 4].map((number) => `<input type="hidden" name="anchor${number}" value=""><input type="hidden" name="weight${number}" value="">`).join("");
  return `<div class="form-field full emotion-split" data-emotion-split="${escapeHtml(JSON.stringify(slots))}">
    <span>emotions</span>
    <div class="emo-body">
      <div class="split-bar editable" data-bar></div>
      <div class="emo-rows"><span data-rows class="emo-chips"></span><select data-add-anchor aria-label="add anchor"><option value="">+ anchor</option>${anchors.map((anchor) => `<option value="${anchor}">${anchor}</option>`).join("")}</select><button class="link-button" type="button" data-even>split evenly</button><span class="dim" data-total></span></div>
    </div>
    ${hidden}
  </div>`;
}

function bindEmotionSplits(root) {
  root.querySelectorAll("[data-emotion-split]").forEach((editor) => {
    if (!(editor instanceof HTMLElement) || editor.dataset.bound) return;
    editor.dataset.bound = "true";
    const slots = JSON.parse(editor.dataset.emotionSplit || "[]");
    const bar = editor.querySelector("[data-bar]");
    const rows = editor.querySelector("[data-rows]");
    const add = editor.querySelector("[data-add-anchor]");
    const round = (value) => Math.round(value * 100) / 100;
    const total = () => slots.reduce((sum, slot) => sum + slot.weight, 0);
    const paint = () => {
      let left = 0;
      slots.forEach((slot, index) => {
        const seg = editor.querySelector(`[data-seg="${index}"]`);
        if (seg instanceof HTMLElement) {
          seg.style.left = `${left * 100}%`;
          seg.style.width = `${slot.weight * 100}%`;
          seg.textContent = slot.weight >= .12 ? slot.anchor : "";
        }
        left += slot.weight;
        const handle = editor.querySelector(`[data-handle="${index}"]`);
        if (handle instanceof HTMLElement) handle.style.left = `${left * 100}%`;
        const input = editor.querySelector(`[data-weight="${index}"]`);
        if (input instanceof HTMLInputElement && document.activeElement !== input) input.value = slot.weight.toFixed(2);
      });
      [1, 2, 3, 4].forEach((number) => {
        const slot = slots[number - 1];
        const anchorInput = editor.querySelector(`input[name="anchor${number}"]`);
        const weightInput = editor.querySelector(`input[name="weight${number}"]`);
        if (anchorInput instanceof HTMLInputElement) anchorInput.value = slot ? slot.anchor : "";
        if (weightInput instanceof HTMLInputElement) weightInput.value = slot ? String(slot.weight) : "";
      });
      const sum = editor.querySelector("[data-total]");
      if (sum) sum.textContent = slots.length && Math.abs(total() - 1) > .005 ? `total ${total().toFixed(2)} (should be 1)` : "";
      if (add instanceof HTMLSelectElement) {
        add.disabled = slots.length >= 4;
        [...add.options].forEach((option) => option.disabled = !!option.value && slots.some((slot) => slot.anchor === option.value));
      }
    };
    // Takes `amount` from (or gives it to) every slot but `keep`, in
    // proportion to their weights, so the total stays put.
    const rebalance = (keep, amount) => {
      const others = slots.filter((_, index) => index !== keep);
      const pool = others.reduce((sum, slot) => sum + slot.weight, 0);
      others.forEach((slot) => {
        slot.weight = round(pool > 0 ? slot.weight - amount * (slot.weight / pool) : Math.max(0, -amount / others.length));
      });
    };
    const render = () => {
      if (bar instanceof HTMLElement) {
        bar.innerHTML = slots.map((slot, index) => `<span class="split-seg" data-seg="${index}" style="background:${splitColors[index % splitColors.length]}"></span>`).join("")
          + slots.slice(0, -1).map((_, index) => `<span class="split-handle" data-handle="${index}" role="slider" tabindex="0" aria-label="boundary ${index + 1}"></span>`).join("");
      }
      if (rows instanceof HTMLElement) {
        rows.innerHTML = slots.map((slot, index) => `<span class="emo-chip"><span class="split-swatch" style="background:${splitColors[index % splitColors.length]}"></span>${escapeHtml(slot.anchor)}<input type="number" min="0" max="1" step="0.05" data-weight="${index}" aria-label="${escapeHtml(slot.anchor)} weight"><button type="button" class="chip-x" data-remove="${index}" aria-label="remove ${escapeHtml(slot.anchor)}">×</button></span>`).join("")
          || `<span class="dim">no anchors yet</span>`;
      }
      rows?.querySelectorAll("[data-weight]").forEach((input) => input.addEventListener("input", () => {
        const index = Number(input.dataset.weight);
        const value = Number(input.value);
        if (!Number.isFinite(value) || slots.length < 2) return;
        const next = Math.min(1, Math.max(0, value));
        rebalance(index, next - slots[index].weight);
        slots[index].weight = round(next);
        paint();
      }));
      rows?.querySelectorAll("[data-remove]").forEach((button) => button.addEventListener("click", () => {
        const index = Number(button.dataset.remove);
        const freed = slots[index].weight;
        slots.splice(index, 1);
        const pool = total();
        slots.forEach((slot) => slot.weight = round(pool > 0 ? slot.weight + freed * (slot.weight / pool) : 1 / slots.length));
        render();
      }));
      bar?.querySelectorAll("[data-handle]").forEach((handle) => {
        if (!(handle instanceof HTMLElement) || !(bar instanceof HTMLElement)) return;
        const index = Number(handle.dataset.handle);
        const move = (at) => {
          const lower = slots.slice(0, index).reduce((sum, slot) => sum + slot.weight, 0);
          const upper = lower + slots[index].weight + slots[index + 1].weight;
          const boundary = Math.min(upper, Math.max(lower, Math.round(at * 100) / 100));
          slots[index + 1].weight = round(upper - boundary);
          slots[index].weight = round(boundary - lower);
          paint();
        };
        handle.addEventListener("pointerdown", (event) => {
          event.preventDefault();
          handle.setPointerCapture(event.pointerId);
          const onMove = (moveEvent) => {
            const rect = bar.getBoundingClientRect();
            move((moveEvent.clientX - rect.left) / rect.width);
          };
          const onUp = () => {
            handle.removeEventListener("pointermove", onMove);
            handle.removeEventListener("pointerup", onUp);
          };
          handle.addEventListener("pointermove", onMove);
          handle.addEventListener("pointerup", onUp);
        });
        handle.addEventListener("keydown", (event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          const edge = slots.slice(0, index + 1).reduce((sum, slot) => sum + slot.weight, 0);
          move(edge + (event.key === "ArrowRight" ? 1 : -1) * (event.shiftKey ? .1 : .01));
        });
      });
      paint();
    };
    add?.addEventListener("change", () => {
      const anchor = add.value;
      add.value = "";
      if (!anchor || slots.length >= 4 || slots.some((slot) => slot.anchor === anchor)) return;
      // A first anchor takes everything; later ones start at 0 to be dragged in.
      slots.push({ anchor, weight: slots.length ? 0 : 1 });
      render();
    });
    editor.querySelector("[data-even]")?.addEventListener("click", () => {
      slots.forEach((slot) => slot.weight = round(1 / slots.length));
      render();
    });
    render();
  });
}

function periodFields(fields, fallbackDays) {
  const fromExpression = fieldValue(fields, "period_from_expression");
  const toExpression = fieldValue(fields, "period_to_expression");
  return `
    <div class="form-field full"><span>calendar range</span><div class="date-range">
      <input name="period_from" type="date" value="${escapeHtml(fieldValue(fields, "period_from"))}"><span>→</span>
      <input name="period_to" type="date" value="${escapeHtml(fieldValue(fields, "period_to"))}">
    </div><div class="expression-row">
      <label><input name="period_from_expression" type="checkbox" value="${escapeHtml(fromExpression || "today")}" ${fromExpression ? "checked" : ""}> start follows ${escapeHtml(fromExpression || "today")}</label>
      <label><input name="period_to_expression" type="checkbox" value="${escapeHtml(toExpression || "today")}" ${toExpression ? "checked" : ""}> end follows ${escapeHtml(toExpression || "today")}</label>
    </div><small>Optional. Dynamic endpoints are resolved whenever this is saved or reimported.</small></div>
    ${textField(fields, "days", "fallback days", fallbackDays, "number", 'min="0" step="1"')}`;
}

function impactKindFields(kind, fields) {
  if (["cry", "ehi"].includes(kind)) return blendFields(fields);
  if (["nei", "aei"].includes(kind)) return `${rangeField(fields, "factor", "intensity")}${selectField(fields, "sign", "sign", [["1", "positive"], ["-1", "negative"]], "1")}${blendFields(fields)}`;
  if (kind === "epi") return `${rangeField(fields, "factor", "intensity")}${blendFields(fields)}`;
  if (kind === "pads") return `${periodFields(fields, "3")}${blendFields(fields)}`;
  if (["jumpscare", "sleepless_night", "politics", "dropped"].includes(kind)) return "";
  if (kind === "interest_field") return selectField(fields, "new_field", "new field?", [["true", "yes — genuinely new interest"], ["false", "no — same field as before"]], "false");
  if (kind === "additional") return rangeField(fields, "value", "signed adjustment", "0.5", "-2", "2");
  if (kind === "music") return rangeField(fields, "music_base", "music quality");
  if (kind === "waifu") return periodFields(fields, "90");
  if (kind === "music_consumed_progress") return durationField(fields, "duration_minutes", "duration", "");
  if (kind === "meme") return `${rangeField(fields, "strength", "strength", "1", "0", "2")}${periodFields(fields, "30")}`;
  if (kind === "osu_song") return `${rangeField(fields, "personal", "personal impact")}${rangeField(fields, "community", "community impact", "0.3")}`;
  if (kind === "consumed") return `${rangeField(fields, "boredom", "boredom")}${durationField(fields, "duration_minutes", "duration", "180")}`;
  if (kind === "anime_consumed") return `${rangeField(fields, "boredom", "boredom")}${textField(fields, "episodes", "episodes", "", "number", 'min="0" step="0.01" placeholder="12"')}${durationField(fields, "episode_duration_minutes", "duration per episode", "")}`;
  if (kind === "visual") return `${selectField(fields, "visual_kind", "visual kind", [["1.0", "Anime / RPG 3D game (1.0)"], ["0.8", "Animated short / MV / VN / manga (0.8)"], ["0.7", "Anime PV / animated gacha art (0.7)"], ["0.6", "Gacha card art, static (0.6)"], ["0.5", "Light novel visual / semi-animated MV (0.5)"], ["0.3", "Static music video (0.3)"], ["0.25", "Album art / anime promo art (0.25)"]], "1.0")}${rangeField(fields, "base", "quality")}${rangeField(fields, "unique", "uniqueness")}`;
  if (kind === "writing") return `${rangeField(fields, "character_complexity", "character complexity")}${rangeField(fields, "story_quality", "story quality")}${rangeField(fields, "pacing", "pacing")}${rangeField(fields, "originality", "originality")}`;
  return `<p class="error full">Unknown impact kind.</p>`;
}

function closeEditor(action) {
  const panel = action?.closest(".inline-editor-panel");
  const details = panel?.closest(".card-details");
  panel?.remove();
  if (details instanceof HTMLDetailsElement) details.open = false;
}

function bindRangeOutputs(root) {
  root.addEventListener("input", (event) => {
    if (!(event.target instanceof HTMLInputElement) || event.target.type !== "range") return;
    const output = event.target.nextElementSibling;
    if (output instanceof HTMLOutputElement) output.value = event.target.value;
  });
}

function showEditor(host, title, formMarkup, onSubmit, onDelete) {
  document.querySelectorAll(".inline-editor-panel").forEach((panel) => panel.remove());
  const editor = document.createElement("section");
  editor.className = "inline-editor-panel";
  editor.innerHTML = `<form class="editor-shell"><header><h3>${escapeHtml(title)}</h3><button class="icon-button" type="button" data-action="close-editor" aria-label="Close">×</button></header><div class="editor-form">${formMarkup}<p class="form-error" hidden></p><div class="editor-actions">${onDelete ? '<button class="danger-button" type="button" data-delete>delete</button>' : "<span></span>"}<span></span><button class="button" type="button" data-action="close-editor">cancel</button><button class="button primary" type="submit">save</button></div></div></form>`;
  if (host.classList.contains("details-body")) {
    host.append(editor);
  } else {
    host.insertAdjacentElement("afterend", editor);
  }
  const shell = editor.querySelector(".editor-shell");
  const form = editor.querySelector(".editor-form");
  if (!(shell instanceof HTMLFormElement) || !(form instanceof HTMLElement)) return;
  bindRangeOutputs(form);
  shell.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (shell.dataset.busy) return;
    shell.dataset.busy = "true";
    shell.querySelectorAll("button").forEach((button) => button.disabled = true);
    const error = form.querySelector(".form-error");
    try {
      const payload = Object.fromEntries(new FormData(shell).entries());
      await onSubmit(payload);
      editor.remove();
    } catch (reason) {
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    } finally {
      delete shell.dataset.busy;
      shell.querySelectorAll("button").forEach((button) => button.disabled = false);
    }
  });
  const deleteButton = editor.querySelector("[data-delete]");
  deleteButton?.addEventListener("click", async () => {
    if (!onDelete || !confirm("Delete this permanently?")) return;
    try {
      await onDelete();
      editor.remove();
    } catch (reason) {
      const error = form.querySelector(".form-error");
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
    }
  });
  if (!host.closest(".card-details")) editor.scrollIntoView({ behavior: "smooth", block: "nearest" });
  return editor;
}

// Re-fetches the entry after a write and repaints it in place -- no
// loading placeholder, scroll position and open groups kept -- so a
// save doesn't throw the page back to the top.
async function refreshEntry(entryId, options = {}) {
  if (location.pathname === pathForEntry(entryId)) await renderEntry(entryId, { ...options, quiet: true });
}

async function confirmImpact(impactId) {
  if (!currentEntryData || !impactId) return;
  const entryId = currentEntryData.entry.id;
  try {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impactId)}/confirm`, "POST");
    await refreshEntry(entryId);
    showToast("Impact confirmed");
  } catch (error) {
    showToast(error?.message || String(error), "error");
  }
}

/** Converts an ordinary impact into a shared one; its split editor opens. */
async function shareImpact(impactId) {
  if (!currentEntryData || !impactId) return;
  const entryId = currentEntryData.entry.id;
  try {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impactId)}/share`, "POST");
    await refreshEntry(entryId, { keepEditorFor: impactId });
    showToast("Now shared — add entries to the split");
  } catch (error) {
    showToast(error?.message || String(error), "error");
  }
}

async function deleteRelation(relationId) {
  if (!currentEntryData || !relationId || !confirm("Delete this relation?")) return;
  const entryId = currentEntryData.entry.id;
  try {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relationId)}`, "DELETE");
    await refreshEntry(entryId);
    showToast("Relation deleted");
  } catch (error) {
    showToast(error?.message || String(error), "error");
  }
}

/** An API atom in the shape the raw-edit endpoint takes back. */
function atomPayload(atom, blend = atom.blend.map(({ anchor, weight }) => [anchor, weight])) {
  return {
    source: atom.source,
    magnitude: atom.magnitude,
    blend,
    time: atom.time.type === "moment" ? "Moment" : { Tail: { peak: atom.time.peak, lambda: atom.time.lambda, cutoff: atom.time.cutoff } },
  };
}

/**
 * One-click reclassification: `− anchor` drops an anchor from this
 * atom's blend, `+ anchor` adds it at weight 1. Each click saves
 * immediately.
 */
function blendChips(atom, index) {
  const active = atom.blend.map(({ anchor, weight }) => `<button class="blend-chip active" type="button" data-chip data-atom="${index}" data-anchor="${escapeHtml(anchor)}" data-op="remove">− ${escapeHtml(anchor)} <small>${Number(weight).toFixed(2)}</small></button>`);
  const inactive = anchors
    .filter((anchor) => !atom.blend.some((entry) => entry.anchor === anchor))
    .map((anchor) => `<button class="blend-chip" type="button" data-chip data-atom="${index}" data-anchor="${anchor}" data-op="add">+ ${anchor}</button>`);
  return `<div class="blend-chips">${[...active, ...inactive].join("")}</div>`;
}

function openRawImpactEditor(impact, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const rawPath = `/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}/raw`;
  const formMarkup = `${impact.atoms.map((atom, index) => {
    const weights = Object.fromEntries(atom.blend.map(({ anchor, weight }) => [anchor, weight]));
    const choices = [...new Set([...anchors, ...Object.keys(weights)])];
    const source = atom.source_entry
      ? `<a href="${pathForEntry(atom.source_entry.id)}" data-link>${escapeHtml(atom.source_entry.title)}</a> ${kindTag(atom.source_entry.kind)}`
      : `<span class="mono">${escapeHtml(atom.source)}</span>`;
    return `<fieldset class="raw-atom"><legend>source atom ${index + 1}</legend>
      <p class="atom-source">source ${source} <span class="mono dim">· magnitude ${Number(atom.magnitude).toFixed(2)}</span></p>
      ${blendChips(atom, index)}
      <details class="raw-advanced"><summary>edit weights &amp; magnitude</summary>
        <div class="form-grid">${textField({ source: atom.source }, `source${index}`, "source entry", atom.source)}${textField({}, `magnitude${index}`, "magnitude", String(atom.magnitude), "number", 'step="any" required')}</div>
        <div class="raw-blend">${choices.map((anchor) => field(anchor, `<input type="number" name="blend${index}:${escapeHtml(anchor)}" step="any" value="${escapeHtml(weights[anchor] ?? 0)}">`)).join("")}</div>
      </details>
    </fieldset>`;
  }).join("")}${field("note", `<textarea name="note" rows="3">${escapeHtml(impact.note || impact.source_comment || "")}</textarea>`)}`;
  const editor = showEditor(action.closest(".details-body") || action, "Edit impact", formMarkup, async (payload) => {
    const atoms = impact.atoms.map((original, index) => atomPayload(
      { ...original, source: payload[`source${index}`], magnitude: Number(payload[`magnitude${index}`]) },
      Object.entries(payload).filter(([key, value]) => key.startsWith(`blend${index}:`) && Number(value) !== 0).map(([key, value]) => [key.slice(key.indexOf(":") + 1), Number(value)]),
    ));
    await sendJson(rawPath, "PUT", { note: payload.note || "", atoms });
    await refreshEntry(entryId);
    showToast("Impact saved");
  }, async () => {
    await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}`, "DELETE");
    await refreshEntry(entryId);
    showToast("Impact deleted");
  });
  editor?.addEventListener("click", async (event) => {
    const chip = event.target instanceof Element ? event.target.closest("[data-chip]") : null;
    if (!(chip instanceof HTMLButtonElement) || editor.dataset.busy) return;
    editor.dataset.busy = "true";
    const chips = editor.querySelectorAll("[data-chip]");
    chips.forEach((button) => { if (button instanceof HTMLButtonElement) button.disabled = true; });
    const atomIndex = Number(chip.dataset.atom);
    const anchor = chip.dataset.anchor;
    const atoms = impact.atoms.map((atom, index) => {
      const blend = atom.blend.map((entry) => [entry.anchor, entry.weight]);
      if (index !== atomIndex) return atomPayload(atom, blend);
      return atomPayload(atom, chip.dataset.op === "remove" ? blend.filter(([name]) => name !== anchor) : [...blend, [anchor, 1]]);
    });
    // The note rides along so a chip click doesn't drop an unsaved
    // edit to it.
    const note = editor.querySelector('textarea[name="note"]');
    try {
      await sendJson(rawPath, "PUT", { note: note instanceof HTMLTextAreaElement ? note.value : "", atoms });
      // Keep this card's editor open so several chips can be
      // clicked in a row.
      await refreshEntry(entryId, { keepEditorFor: impact.id });
    } catch (reason) {
      const error = editor.querySelector(".form-error");
      if (error instanceof HTMLElement) {
        error.hidden = false;
        error.textContent = reason?.message || String(reason);
      }
      chips.forEach((button) => { if (button instanceof HTMLButtonElement) button.disabled = false; });
    } finally {
      delete editor.dataset.busy;
    }
  });
}

const splitColors = ["var(--brass)", "var(--verdigris)", "var(--cobalt)", "var(--heather)", "var(--oxblood)", "var(--kind-anime)", "var(--kind-album)", "var(--kind-track)"];

function memberTitle(member) {
  return member.entry?.title || member.id;
}

/**
 * Tree editor for a shared impact's split. A group divides its own amount
 * among its children by one rule: explicit fractions (any rest unassigned),
 * even parts, or weights. A member's share of the impact is the product of
 * portions down its path. One level is shown at a time; groups open with a
 * breadcrumb back. Saving sends the whole tree.
 */
function splitPortions(group) {
  const children = group.children;
  if (group.rule === "explicit") return children.map((child) => Number(child.share) || 0);
  if (group.rule === "even") return children.map(() => 1 / Math.max(1, children.length));
  const total = children.reduce((sum, child) => sum + (Number(child.weight) || 0), 0);
  return children.map((child) => (total > 0 ? (Number(child.weight) || 0) / total : 0));
}

function formatShare(value) {
  return value >= 0.1 || value === 0 ? value.toFixed(2) : value.toFixed(4);
}

function bindSplitTree(container, impact, entryId) {
  const original = JSON.stringify(impact.shared.split);
  let tree = JSON.parse(original);
  let path = [];
  let filter = "";
  let showAll = false;
  let selected = new Set();
  const titles = new Map(impact.shared.members.map((member) => [member.id, member.entry?.title || member.id]));
  const nodeAt = (at) => at.reduce((node, index) => node.children[index], tree);
  const current = () => nodeAt(path);
  const amountAt = (at) => {
    let amount = 1;
    let node = tree;
    for (const index of at) {
      amount *= splitPortions(node)[index];
      node = node.children[index];
    }
    return amount;
  };
  const nameOf = (node) => node.type === "member" ? (titles.get(node.entry) || node.entry) : (node.label || `${node.rule} group`);
  const countOf = (node) => node.type === "member" ? 1 : node.children.reduce((sum, child) => sum + countOf(child), 0);
  const entriesOf = (node, out = new Set()) => {
    if (node.type === "member") out.add(node.entry);
    else node.children.forEach((child) => entriesOf(child, out));
    return out;
  };
  const round = (value) => Math.round(value * 10000) / 10000;
  const resetView = () => {
    selected = new Set();
    filter = "";
    showAll = false;
  };

  const render = () => {
    const group = current();
    const portions = splitPortions(group);
    const amount = amountAt(path);
    const assigned = group.rule === "explicit" ? portions.reduce((a, b) => a + b, 0) : 1;
    const crumbs = [`<button type="button" class="link-button" data-crumb="0">whole impact</button>`];
    path.forEach((_, depth) => crumbs.push(`<button type="button" class="link-button" data-crumb="${depth + 1}">${escapeHtml(nameOf(nodeAt(path.slice(0, depth + 1))))}</button>`));
    const needle = filter.trim().toLowerCase();
    const rows = group.children
      .map((child, index) => ({ child, index }))
      .filter(({ child }) => !needle || nameOf(child).toLowerCase().includes(needle));
    const visible = showAll || needle ? rows : rows.slice(0, 15);
    let left = 0;
    const segments = group.children.map((child, index) => {
      const width = portions[index];
      const markup = `<span class="split-seg${child.type === "member" && child.entry === entryId ? " current" : ""}${child.type === "group" ? " group" : ""}" data-seg="${index}" style="left:${left * 100}%;width:${width * 100}%;background:${splitColors[index % splitColors.length]}" title="${escapeHtml(nameOf(child))} ${formatShare(width)}"></span>`;
      left += width;
      return markup;
    }).join("");
    let edge = 0;
    const handles = group.rule === "explicit" ? group.children.map((_, index) => {
      edge += portions[index];
      return `<span class="split-handle" data-handle="${index}" role="slider" tabindex="0" aria-label="boundary ${index + 1}" style="left:${edge * 100}%"></span>`;
    }).join("") : "";
    const valueField = (child, index) => {
      if (group.rule === "explicit") return `<input type="number" min="0" max="1" step="0.01" data-share="${index}" value="${round(Number(child.share) || 0)}" aria-label="share">`;
      if (group.rule === "weighted") return `<input type="number" min="0" step="any" data-weight="${index}" value="${round(Number(child.weight) || 0)}" aria-label="weight">`;
      return `<span class="dim split-equal">equal</span>`;
    };
    const ruleNames = { explicit: "explicit shares", even: "evenly", weighted: "by weight" };
    container.innerHTML = `<div class="split-editor">
      ${path.length ? `<nav class="split-crumbs">${crumbs.join('<span class="dim"> › </span>')}</nav>` : ""}
      <div class="split-head">
        <label class="split-rule">split <select data-rule>${Object.entries(ruleNames).map(([rule, label]) => `<option value="${rule}" ${rule === group.rule ? "selected" : ""}>${label}</option>`).join("")}</select></label>
        ${path.length ? `<label class="split-label">label <input data-label value="${escapeHtml(group.label || "")}" placeholder="${escapeHtml(group.rule)} group"></label>` : ""}
        <span class="dim split-worth">${path.length ? `worth <span class="mono">${formatShare(amount)}</span> of the impact · ` : ""}${countOf(group)} entries</span>
      </div>
      <div class="split-bar editable" data-split-bar>${segments}${handles}</div>
      ${group.children.length > 10 ? `<input class="split-filter" type="search" data-filter placeholder="filter ${group.children.length} parts…" value="${escapeHtml(filter)}">` : ""}
      <div class="split-legend">${visible.map(({ child, index }) => `<div class="split-row tree">
        <input type="checkbox" data-select="${index}" ${selected.has(index) ? "checked" : ""} aria-label="select">
        <span class="split-swatch" style="background:${splitColors[index % splitColors.length]}"></span>
        <span class="split-name">${child.type === "group" ? `<button type="button" class="link-button" data-open="${index}">${escapeHtml(nameOf(child))} ›</button> <small class="dim">${escapeHtml(ruleNames[child.rule])} · ${countOf(child)}</small>` : `${escapeHtml(nameOf(child))}${child.entry === entryId ? ' <small class="dim">(this entry)</small>' : ""}`}</span>
        ${valueField(child, index)}
        <span class="mono dim split-eff" data-eff="${index}" title="share of the whole impact">${formatShare(amount * portions[index])}</span>
        ${child.type === "group" ? `<button type="button" class="link-button" data-ungroup="${index}">ungroup</button>` : `<button type="button" class="chip-x" data-remove="${index}" aria-label="remove">×</button>`}
      </div>`).join("")}</div>
      ${visible.length < rows.length ? `<button type="button" class="link-button" data-show-all>show all ${rows.length}</button>` : ""}
      <div class="split-tools">
        <span class="split-add"><input type="search" data-add placeholder="add an entry…" autocomplete="off"><span class="split-results" data-results></span></span>
        <button class="link-button" type="button" data-group-selected ${selected.size ? "" : "disabled"}>group selected</button>
        ${group.rule === "explicit" ? `<button class="link-button" type="button" data-split-even>split evenly</button><button class="link-button" type="button" data-split-normalize>normalize to 1</button>` : ""}
      </div>
      <div class="split-footer">
        <span class="split-total" data-split-total>${group.rule === "explicit" ? `assigned <span class="mono">${formatShare(assigned)}</span>${assigned < .9995 ? ` · unassigned <span class="mono">${formatShare(1 - assigned)}</span>` : ""}` : ""}</span>
        <span class="split-save" ${JSON.stringify(tree) === original ? "hidden" : ""}><button class="link-button" type="button" data-split-reset>reset</button><button class="button primary" type="button" data-split-save>save split</button></span>
      </div>
    </div>`;
    bind();
  };

  // While dragging or typing, only positions and numbers move.
  const paint = () => {
    const group = current();
    const portions = splitPortions(group);
    const amount = amountAt(path);
    let left = 0;
    portions.forEach((portion, index) => {
      const seg = container.querySelector(`[data-seg="${index}"]`);
      if (seg instanceof HTMLElement) {
        seg.style.left = `${left * 100}%`;
        seg.style.width = `${portion * 100}%`;
      }
      left += portion;
      const handle = container.querySelector(`[data-handle="${index}"]`);
      if (handle instanceof HTMLElement) handle.style.left = `${left * 100}%`;
      const input = container.querySelector(`[data-share="${index}"]`);
      if (input instanceof HTMLInputElement && document.activeElement !== input) input.value = String(round(portion));
      const eff = container.querySelector(`[data-eff="${index}"]`);
      if (eff) eff.textContent = formatShare(amount * portion);
    });
    const total = container.querySelector("[data-split-total]");
    if (total && group.rule === "explicit") total.innerHTML = `assigned <span class="mono">${formatShare(left)}</span>${left < .9995 ? ` · unassigned <span class="mono">${formatShare(1 - left)}</span>` : ""}`;
    const save = container.querySelector(".split-save");
    if (save instanceof HTMLElement) save.hidden = JSON.stringify(tree) === original;
  };

  // Switching a group's rule keeps every child's current portion.
  const setRule = (group, rule) => {
    const portions = splitPortions(group);
    group.children.forEach((child, index) => {
      child.share = round(portions[index]);
      child.weight = round(portions[index]);
    });
    group.rule = rule;
  };

  let searchTimer = 0;
  const bind = () => {
    const group = current();
    container.querySelectorAll("[data-crumb]").forEach((button) => button.addEventListener("click", () => {
      path = path.slice(0, Number(button.dataset.crumb));
      resetView();
      render();
    }));
    container.querySelector("[data-rule]")?.addEventListener("change", (event) => {
      setRule(group, event.target.value);
      render();
    });
    container.querySelector("[data-label]")?.addEventListener("input", (event) => {
      group.label = event.target.value.trim() || undefined;
      paint();
    });
    container.querySelector("[data-filter]")?.addEventListener("input", (event) => {
      filter = event.target.value;
      const caret = event.target.selectionStart;
      render();
      const field = container.querySelector("[data-filter]");
      if (field instanceof HTMLInputElement) {
        field.focus();
        field.setSelectionRange(caret, caret);
      }
    });
    container.querySelector("[data-show-all]")?.addEventListener("click", () => {
      showAll = true;
      render();
    });
    container.querySelectorAll("[data-open]").forEach((button) => button.addEventListener("click", () => {
      path = [...path, Number(button.dataset.open)];
      resetView();
      render();
    }));
    container.querySelectorAll("[data-select]").forEach((box) => box.addEventListener("change", () => {
      const index = Number(box.dataset.select);
      if (box.checked) selected.add(index);
      else selected.delete(index);
      const button = container.querySelector("[data-group-selected]");
      if (button instanceof HTMLButtonElement) button.disabled = selected.size < 1;
    }));
    container.querySelectorAll("[data-share]").forEach((input) => input.addEventListener("input", () => {
      const index = Number(input.dataset.share);
      const others = splitPortions(group).reduce((a, b) => a + b, 0) - (Number(group.children[index].share) || 0);
      const value = Number(input.value);
      if (!Number.isFinite(value)) return;
      group.children[index].share = round(Math.min(Math.max(0, value), 1 - others));
      paint();
    }));
    container.querySelectorAll("[data-weight]").forEach((input) => input.addEventListener("input", () => {
      const value = Number(input.value);
      if (!Number.isFinite(value) || value < 0) return;
      group.children[Number(input.dataset.weight)].weight = value;
      paint();
    }));
    container.querySelectorAll("[data-remove]").forEach((button) => button.addEventListener("click", () => {
      if (countOf(tree) <= 1) {
        showToast("A shared impact needs at least one entry; delete the impact instead", "error");
        return;
      }
      group.children.splice(Number(button.dataset.remove), 1);
      // A group left empty goes away with its last member.
      while (path.length && current().children.length === 0) {
        const index = path[path.length - 1];
        path = path.slice(0, -1);
        current().children.splice(index, 1);
      }
      selected = new Set();
      render();
    }));
    container.querySelectorAll("[data-ungroup]").forEach((button) => button.addEventListener("click", () => {
      const index = Number(button.dataset.ungroup);
      const inner = group.children[index];
      const innerPortions = splitPortions(inner);
      const lifted = inner.children.map((child, i) => ({
        ...child,
        share: round((Number(inner.share) || 0) * innerPortions[i]),
        weight: round((Number(inner.weight) || 0) * innerPortions[i]),
      }));
      group.children.splice(index, 1, ...lifted);
      selected = new Set();
      render();
    }));
    container.querySelector("[data-group-selected]")?.addEventListener("click", () => {
      const indices = [...selected].sort((a, b) => a - b);
      if (!indices.length) return;
      const portions = splitPortions(group);
      const sum = indices.reduce((total, index) => total + portions[index], 0);
      const inner = sum > 0 ? (index) => round(portions[index] / sum) : () => 0;
      const node = {
        type: "group",
        rule: "even",
        share: round(indices.reduce((total, index) => total + (Number(group.children[index].share) || 0), 0)),
        weight: round(indices.reduce((total, index) => total + (Number(group.children[index].weight) || 0), 0)),
        children: indices.map((index) => ({ ...group.children[index], share: inner(index), weight: inner(index) })),
      };
      for (const index of [...indices].reverse()) group.children.splice(index, 1);
      group.children.splice(indices[0], 0, node);
      selected = new Set();
      render();
    });
    container.querySelector("[data-split-even]")?.addEventListener("click", () => {
      const assigned = splitPortions(group).reduce((a, b) => a + b, 0);
      group.children.forEach((child) => child.share = round(assigned / group.children.length));
      render();
    });
    container.querySelector("[data-split-normalize]")?.addEventListener("click", () => {
      const assigned = splitPortions(group).reduce((a, b) => a + b, 0);
      if (assigned <= 0) return;
      group.children.forEach((child) => child.share = round((Number(child.share) || 0) / assigned));
      render();
    });
    const add = container.querySelector("[data-add]");
    const results = container.querySelector("[data-results]");
    add?.addEventListener("input", () => {
      clearTimeout(searchTimer);
      const query = add.value.trim();
      if (!query) {
        if (results) results.innerHTML = "";
        return;
      }
      searchTimer = setTimeout(async () => {
        try {
          const found = await getJson(`/api/v1/entries?q=${encodeURIComponent(query)}&limit=8`);
          if (!results) return;
          results.innerHTML = found.entries.map((entry) => `<button type="button" class="split-result" data-pick="${escapeHtml(entry.id)}" data-title="${escapeHtml(entry.title)}">${escapeHtml(entry.title)} ${kindTag(entry.kind)}</button>`).join("") || `<span class="dim">no match</span>`;
          results.querySelectorAll("[data-pick]").forEach((pick) => pick.addEventListener("click", () => {
            const id = pick.dataset.pick;
            if (entriesOf(tree).has(id)) {
              showToast("That entry is already in the split", "error");
              return;
            }
            titles.set(id, pick.dataset.title || id);
            const weights = group.children.map((child) => Number(child.weight) || 0);
            const mean = weights.length ? weights.reduce((a, b) => a + b, 0) / weights.length : 1;
            group.children.push({ type: "member", entry: id, share: 0, weight: round(mean || 1) });
            render();
          }));
        } catch (error) {
          showToast(error?.message || String(error), "error");
        }
      }, 200);
    });
    const bar = container.querySelector("[data-split-bar]");
    container.querySelectorAll("[data-handle]").forEach((handle) => {
      if (!(handle instanceof HTMLElement) || !(bar instanceof HTMLElement)) return;
      const index = Number(handle.dataset.handle);
      // Boundary `index` sits after child `index`; it trades between that
      // child and the next, or the last child and the unassigned rest.
      const move = (at) => {
        const portions = splitPortions(group);
        const lower = portions.slice(0, index).reduce((a, b) => a + b, 0);
        const upper = index < portions.length - 1 ? lower + portions[index] + portions[index + 1] : 1;
        const boundary = Math.min(upper, Math.max(lower, Math.round(at * 100) / 100));
        if (index < portions.length - 1) group.children[index + 1].share = round(upper - boundary);
        group.children[index].share = round(boundary - lower);
        paint();
      };
      handle.addEventListener("pointerdown", (event) => {
        event.preventDefault();
        handle.setPointerCapture(event.pointerId);
        const onMove = (moveEvent) => {
          const rect = bar.getBoundingClientRect();
          move((moveEvent.clientX - rect.left) / rect.width);
        };
        const onUp = () => {
          handle.removeEventListener("pointermove", onMove);
          handle.removeEventListener("pointerup", onUp);
        };
        handle.addEventListener("pointermove", onMove);
        handle.addEventListener("pointerup", onUp);
      });
      handle.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
        event.preventDefault();
        const step = event.shiftKey ? .1 : .01;
        const edgeAt = splitPortions(group).slice(0, index + 1).reduce((a, b) => a + b, 0);
        move(edgeAt + (event.key === "ArrowRight" ? step : -step));
      });
    });
    container.querySelector("[data-split-reset]")?.addEventListener("click", () => {
      tree = JSON.parse(original);
      path = [];
      resetView();
      render();
    });
    container.querySelector("[data-split-save]")?.addEventListener("click", async (event) => {
      const button = event.currentTarget;
      if (button instanceof HTMLButtonElement) button.disabled = true;
      try {
        await sendJson(`/api/v1/entries/${encodeURIComponent(currentEntryData.entry.id)}/impacts/${encodeURIComponent(impact.id)}/shares`, "PUT", { split: tree });
        await refreshEntry(currentEntryData.entry.id, { keepEditorFor: impact.id });
        showToast("Split saved");
      } catch (error) {
        if (button instanceof HTMLButtonElement) button.disabled = false;
        showToast(error?.message || String(error), "error");
      }
    });
  };
  render();
}

/** Each shared impact's "details & edit" carries its split editor. */
function bindInlineSplits(data) {
  app.querySelectorAll("[data-inline-split]").forEach((container) => {
    if (!(container instanceof HTMLElement)) return;
    const impact = data.impacts.find((item) => item.id === container.dataset.inlineSplit);
    if (impact?.shared?.split) bindSplitTree(container, impact, data.entry.id);
  });
}

const progressBasisCache = new Map();

/** What the stores say for this entry's progress (length, episodes). */
function progressBasis(entryId) {
  if (!progressBasisCache.has(entryId)) {
    progressBasisCache.set(entryId, fetch(`/api/v1/entries/${encodeURIComponent(entryId)}/progress-basis`)
      .then((response) => (response.ok ? response.json() : {}))
      .catch(() => ({})));
  }
  return progressBasisCache.get(entryId);
}

/**
 * Progress fields default to the store's values (as the importer does for
 * XML that leaves them out): blank fields are filled, and a field that
 * differs from the store shows the store's value with a "use" link.
 */
async function bindProgressBasis(root, entryId) {
  const kind = root.closest(".impact-form")?.querySelector('select[name="kind"]')?.value;
  const targets = {
    music_consumed_progress: [["duration_minutes", "length_minutes", formatDurationInput]],
    anime_consumed: [
      ["episodes", "episodes", (value) => String(value)],
      ["episode_duration_minutes", "episode_minutes", formatDurationInput],
    ],
  }[kind];
  if (!targets) return;
  const basis = await progressBasis(entryId);
  // A TV episode counts 20 minutes (OP/ED excluded), not the store's length.
  const tvDefault = basis?.store_episode_minutes == null || basis.episode_minutes !== basis.store_episode_minutes;
  for (const [name, key, format] of targets) {
    const source = key === "episode_minutes" && tvDefault
      ? (basis?.store_episode_minutes != null ? `TV default · store ${formatDurationInput(basis.store_episode_minutes)} incl. OP/ED` : "TV default")
      : "from store";
    const input = root.querySelector(`input[name="${name}"]`);
    const value = basis?.[key];
    if (!(input instanceof HTMLInputElement) || value == null) continue;
    const stored = format(value);
    if (!input.value.trim()) input.value = stored;
    const hint = document.createElement("small");
    hint.className = "store-hint";
    const sync = () => {
      const same = input.value.trim() === stored || Math.abs(Number(input.value) - Number(stored)) < 1e-6;
      hint.innerHTML = same
        ? escapeHtml(source)
        : `${escapeHtml(source)}: ${escapeHtml(stored)} <button class="link-button" type="button">use</button>`;
      hint.querySelector("button")?.addEventListener("click", () => {
        input.value = stored;
        sync();
      });
    };
    input.addEventListener("input", sync);
    input.closest(".form-field")?.append(hint);
    sync();
  }
}

function openImpactEditor(impact = null, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const fields = { ...(impact?.params || {}) };
  fields.kind = impact?.kind || "nei";
  fields.source ||= entryId;
  fields.dah_factor = String(impact?.contributor_share ?? 1);
  fields.note = impact?.note || impact?.source_comment || "";
  // A shared impact edits its whole group: values plus every share.
  // A shared impact's split has its own editor above this form; the form keeps it.
  const shareMarkup = impact?.shared
    ? `<input type="hidden" name="shares" value="${escapeHtml(JSON.stringify(impact.shared.members.map((member) => ({ contributor: member.id, factor: member.factor }))))}">`
    : `<input type="hidden" name="dah_factor" value="${escapeHtml(fields.dah_factor)}">`;
  const body = `
    ${impact ? "" : `<p class="form-note editor-intro">Per DAH_standards — pick the kind, fill in its own parameters, and the score vector is computed from the spec's formula rather than picked by hand.</p>`}
    ${impact?.shared ? `<p class="form-note editor-intro">Shared impact — these values apply to every entry in the split; the split itself is edited above.</p>` : ""}
    <div class="impact-form">
      <input type="hidden" name="source" value="${escapeHtml(fields.source)}">
      ${impactKindField(fields.kind)}
      <div id="impact-kind-fields" class="form-grid">${impactKindFields(fields.kind, fields)}</div>
      ${shareMarkup}
      ${field("note", `<textarea name="note" rows="2">${escapeHtml(fields.note)}</textarea>`)}
    </div>`;
  const refresh = async (message) => {
    await refreshEntry(entryId);
    showToast(message);
  };
  const host = impact ? action.closest(".details-body") || action : action.closest(".section-actions") || action;
  const editor = showEditor(
    host,
    impact ? "Edit impact" : "Add impact",
    body,
    async (payload) => {
      const path = impact ? `/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}` : `/api/v1/entries/${encodeURIComponent(entryId)}/impacts`;
      await sendJson(path, impact ? "PUT" : "POST", payload);
      await refresh(impact ? "Impact saved" : "Impact added");
    },
    impact ? async () => {
      await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/impacts/${encodeURIComponent(impact.id)}`, "DELETE");
      await refresh("Impact deleted");
    } : null,
  );
  bindEmotionSplits(editor);
  const kindFields = editor.querySelector("#impact-kind-fields");
  if (kindFields instanceof HTMLElement) bindProgressBasis(kindFields, entryId);
  const kindSelect = editor.querySelector('select[name="kind"]');
  kindSelect?.addEventListener("change", () => {
    const target = editor.querySelector("#impact-kind-fields");
    if (target instanceof HTMLElement && kindSelect instanceof HTMLSelectElement) {
      target.innerHTML = impactKindFields(kindSelect.value, {});
      bindRangeOutputs(target);
      bindEmotionSplits(target);
      bindProgressBasis(target, entryId);
    }
  });
}

/**
 * An entry field you fill by searching titles. The hidden input carries the
 * id; typing an exact raw id still works, since the text is used as is until
 * a result is picked.
 */
// ---------------------------------------------------------------
// Progress: where an entry stands (planned, watching, finished...).
// ---------------------------------------------------------------

const progressStatuses = ["active", "paused", "planned", "completed", "dropped"];

const progressUnits = {
  episode: ["episode", "ep"],
  chapter: ["chapter", "ch"],
  volume: ["volume", "vol"],
  minute: ["minute", "min"],
};

function progressStatusLabel(status, kind) {
  if (status !== "active") return status;
  if (kind === "literature") return "reading";
  if (kind === "game" || kind === "vn") return "playing";
  if (kind === "album" || kind === "track") return "listening";
  return "watching";
}

function defaultProgressUnit(kind) {
  if (kind === "anime") return "episode";
  if (kind === "literature") return "chapter";
  return "minute";
}

// The store's total for `unit`, from `/progress-basis`.
function basisTotal(basis, unit) {
  const key = { episode: "episodes", chapter: "chapters", volume: "volumes", minute: "length_minutes" }[unit];
  // A still-airing show or running series has a count of 0: unknown.
  return basis?.[key] || null;
}

function formatCount(value) {
  return Number.isInteger(value) ? String(value) : String(Math.round(value * 10) / 10);
}

function progressCount(progress) {
  const unit = progressUnits[progress.unit]?.[1] || progress.unit;
  const total = progress.total ? formatCount(progress.total) : "?";
  return `${formatCount(progress.current)} / ${total} ${unit}`;
}

function progressBar(progress) {
  if (!progress.total) return "";
  const share = Math.min(1, progress.current / progress.total);
  return `<div class="progress-bar" data-status="${escapeHtml(progress.status)}"><span style="width: ${(share * 100).toFixed(1)}%"></span></div>`;
}

function progressPath(entryId, suffix = "") {
  return `/api/v1/entries/${encodeURIComponent(entryId)}/progress${suffix}`;
}

function todayIso() {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

function progressPayload(progress, changes = {}) {
  const { status, unit, current, total, started, finished, rewatch } = { ...progress, ...changes };
  return { status, unit, current, total, started, finished, rewatch };
}

function progressPanelMarkup(data, progress) {
  if (!progress) {
    return `<h2>Progress</h2>
      <p class="empty">Not tracked.</p>
      <div class="section-actions progress-actions">
        <button class="button primary" type="button" data-progress-start="active">start ${escapeHtml(progressStatusLabel("active", data.entry.kind))}</button>
        <button class="button" type="button" data-progress-start="planned">plan</button>
      </div>`;
  }
  const kind = data.entry.kind;
  const open = !["completed", "dropped"].includes(progress.status);
  const dates = [
    progress.started ? `started ${escapeHtml(progress.started)}` : "",
    progress.finished ? `finished ${escapeHtml(progress.finished)}` : "",
    progress.rewatch ? `rewatch ×${progress.rewatch}` : "",
  ].filter(Boolean).join(" · ");
  return `<h2>Progress</h2>
    <div class="progress-line">
      <select class="progress-status" data-status="${escapeHtml(progress.status)}" aria-label="progress status">
        ${progressStatuses.map((status) => `<option value="${status}" ${status === progress.status ? "selected" : ""}>${escapeHtml(progressStatusLabel(status, kind))}</option>`).join("")}
      </select>
      <div class="progress-counter">
        ${open ? `<button class="icon-button" type="button" data-progress-step="-1" aria-label="one less">−</button>` : ""}
        <span class="mono">${escapeHtml(progressCount(progress))}</span>
        ${open ? `<button class="icon-button" type="button" data-progress-step="1" aria-label="one more">+</button>` : ""}
      </div>
    </div>
    ${progressBar(progress)}
    ${dates ? `<p class="muted progress-dates">${dates}</p>` : ""}
    <div class="section-actions progress-actions">
      ${progress.status !== "completed" ? `<button class="button primary" type="button" data-progress-finish="completed">finish</button>` : `<button class="button" type="button" data-progress-finish="completed">re-score</button>`}
      ${progress.status !== "dropped" && progress.status !== "completed" ? `<button class="button" type="button" data-progress-finish="dropped">drop</button>` : ""}
      <button class="button" type="button" data-progress-edit>edit</button>
      <button class="button subtle" type="button" data-progress-clear>stop tracking</button>
    </div>`;
}

async function bindProgress(data) {
  const panel = app.querySelector("[data-progress]");
  if (!(panel instanceof HTMLElement)) return;
  const entryId = data.entry.id;
  let progress;
  try {
    const response = await fetch(progressPath(entryId));
    if (!response.ok) throw new Error((await response.json().catch(() => null))?.error || `HTTP ${response.status}`);
    progress = (await response.json()).progress;
  } catch (error) {
    if (currentEntryData === data && panel.isConnected) panel.innerHTML = `<h2>Progress</h2><p class="error">${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`;
    return;
  }
  if (currentEntryData !== data || !panel.isConnected) return;
  panel.innerHTML = progressPanelMarkup(data, progress);

  const repaint = (next) => {
    progress = next;
    panel.innerHTML = progressPanelMarkup(data, progress);
    bindPanel();
  };
  const guard = (action) => async (event) => {
    const button = event.currentTarget;
    if (button instanceof HTMLButtonElement || button instanceof HTMLSelectElement) button.disabled = true;
    try {
      await action(event);
    } catch (reason) {
      showToast(reason?.message || String(reason), "error");
      if (button instanceof HTMLButtonElement || button instanceof HTMLSelectElement) button.disabled = false;
    }
  };
  const bindPanel = () => {
    panel.querySelectorAll("[data-progress-start]").forEach((button) => button.addEventListener("click", guard(async () => {
      const status = button.getAttribute("data-progress-start");
      const unit = defaultProgressUnit(data.entry.kind);
      const basis = await progressBasis(entryId);
      const body = { status, unit, current: 0, total: basisTotal(basis, unit), started: status === "active" ? todayIso() : null, finished: null, rewatch: 0 };
      repaint((await sendJson(progressPath(entryId), "PUT", body)).progress);
    })));
    panel.querySelectorAll("[data-progress-step]").forEach((button) => button.addEventListener("click", guard(async () => {
      const by = Number(button.getAttribute("data-progress-step"));
      const next = (await sendJson(progressPath(entryId, "/step"), "POST", { by })).progress;
      repaint(next);
      if (by > 0 && next.total != null && next.current >= next.total) openProgressFinish(data, next, "completed", panel);
    })));
    panel.querySelector(".progress-status")?.addEventListener("change", guard(async (event) => {
      const select = event.currentTarget;
      if (!(select instanceof HTMLSelectElement)) return;
      if (select.value === "completed" || select.value === "dropped") {
        const status = select.value;
        select.value = progress.status;
        select.disabled = false;
        openProgressFinish(data, progress, status, panel);
        return;
      }
      const changes = { status: select.value };
      if (select.value === "active" && !progress.started) changes.started = todayIso();
      repaint((await sendJson(progressPath(entryId), "PUT", progressPayload(progress, changes))).progress);
    }));
    panel.querySelectorAll("[data-progress-finish]").forEach((button) => button.addEventListener("click", () => {
      openProgressFinish(data, progress, button.getAttribute("data-progress-finish"), panel);
    }));
    panel.querySelector("[data-progress-edit]")?.addEventListener("click", () => openProgressEditor(data, progress, panel, repaint));
    panel.querySelector("[data-progress-clear]")?.addEventListener("click", guard(async () => {
      if (!confirm("Stop tracking this entry? Its impacts are kept.")) return;
      await sendJson(progressPath(entryId), "DELETE");
      repaint(null);
    }));
  };
  bindPanel();
}

function progressFields(progress) {
  const unitChoices = Object.entries(progressUnits).map(([key, [label]]) => [key, label]);
  return `<div class="form-grid">
    ${selectField(progress, "status", "status", progressStatuses.map((status) => [status, status]))}
    ${selectField(progress, "unit", "counted in", unitChoices)}
    ${textField(progress, "current", "progress", "0", "number", 'step="any" min="0" required')}
    ${textField(progress, "total", "total", "", "number", 'step="any" min="0" placeholder="unknown"')}
    ${textField(progress, "started", "started", "", "date")}
    ${textField(progress, "finished", "finished", "", "date")}
    ${textField(progress, "rewatch", "earlier rewatches", "0", "number", 'step="1" min="0"')}
  </div>`;
}

function numberOrNull(value) {
  return value === undefined || String(value).trim() === "" ? null : Number(value);
}

function openProgressEditor(data, progress, host, repaint) {
  showEditor(host, "Edit progress", progressFields(progress), async (payload) => {
    const body = {
      status: payload.status,
      unit: payload.unit,
      current: Number(payload.current),
      total: numberOrNull(payload.total),
      started: payload.started || null,
      finished: payload.finished || null,
      rewatch: Number(payload.rewatch || 0),
    };
    repaint((await sendJson(progressPath(data.entry.id), "PUT", body)).progress);
  });
}

/**
 * Finishing or dropping: confirms the count and asks for what the
 * consumed impact needs (boredom, minutes per unit). The linked impact,
 * if any, supplies the defaults; unchecking "score" just records it.
 */
async function openProgressFinish(data, progress, status, host) {
  const entryId = data.entry.id;
  const linked = data.impacts.find((impact) => impact.id === progress.consumed_impact);
  const params = linked?.params || {};
  let unitMinutes = "";
  if (progress.unit === "episode") {
    unitMinutes = params.episode_duration_minutes || (await progressBasis(entryId))?.episode_minutes || 20;
  } else if (progress.unit !== "minute" && params.duration_minutes && progress.current) {
    unitMinutes = Number(params.duration_minutes) / progress.current;
  }
  const current = status === "completed" && progress.total != null ? Math.max(progress.current, progress.total) : progress.current;
  const unitLabel = progressUnits[progress.unit]?.[0] || progress.unit;
  const fields = {
    current: formatCount(current),
    finished: progress.finished || todayIso(),
    boredom: params.boredom ?? "1",
    unit_minutes: unitMinutes === "" ? "" : formatCount(Number(unitMinutes)),
  };
  const lead = linked
    ? "Updates this entry's consumed impact instead of adding another."
    : "Adds a consumed impact from the count.";
  const body = `
    <p class="form-note editor-intro">${escapeHtml(lead)}${status === "dropped" ? " A dropped impact is added too." : ""}</p>
    <div class="form-grid">
      ${textField(fields, "current", `${unitLabel}s done`, "", "number", 'step="any" min="0" required')}
      ${textField(fields, "finished", status === "dropped" ? "dropped on" : "finished on", "", "date")}
      ${rangeField(fields, "boredom", "boredom (1 = never bored)", "1")}
      ${progress.unit === "minute" ? "" : textField(fields, "unit_minutes", `minutes per ${unitLabel}`, "", "number", 'step="any" min="0"')}
    </div>
    <label class="form-check"><input type="checkbox" name="score" checked> score it (write the consumed impact)</label>`;
  showEditor(host, status === "dropped" ? "Drop" : "Finish", body, async (payload) => {
    const request = progressPayload(progress, {
      status,
      current: Number(payload.current),
      finished: payload.finished || null,
    });
    if (status === "completed" && request.total == null) request.total = request.current;
    if (payload.score) request.score = { boredom: Number(payload.boredom), unit_minutes: numberOrNull(payload.unit_minutes) };
    await sendJson(progressPath(entryId), "PUT", request);
    showToast(status === "dropped" ? "Dropped" : "Finished");
    await refreshEntry(entryId);
  });
}

async function renderProgress() {
  currentEntryData = null;
  document.title = "Progress — NRS";
  const lead = `<h1>progress</h1><p class="muted">What's being watched, read, and played, and what's next.</p>`;
  app.innerHTML = `${lead}<p class="loading">Loading progress…</p>`;
  let items;
  try {
    items = (await getJson("/api/v1/progress")).items;
  } catch (error) {
    renderError(error);
    return;
  }
  const row = ({ entry, progress }) => `<div class="progress-row" data-entry="${escapeHtml(entry.id)}">
      <a href="${pathForEntry(entry.id)}" data-link>${escapeHtml(entry.title)}</a>
      ${kindTag(entry.kind)}
      <span class="mono muted">${escapeHtml(progressCount(progress))}</span>
      ${["completed", "dropped"].includes(progress.status) ? "<span></span>" : `<button class="button" type="button" data-step aria-label="one more">+1</button>`}
      ${progressBar(progress)}
    </div>`;
  const groups = [
    ["active", "In progress", true],
    ["paused", "Paused", true],
    ["planned", "Planned", true],
    ["completed", "Completed", false],
    ["dropped", "Dropped", false],
  ];
  const groupMarkup = ([status, title, open]) => {
    const rows = items.filter((item) => item.progress.status === status);
    if (!rows.length) return "";
    const list = `<div class="progress-list">${rows.map(row).join("")}</div>`;
    return open
      ? `<section class="page-section"><h2>${title} <span class="count">${rows.length}</span></h2>${list}</section>`
      : `<details class="page-section progress-group"><summary><h2>${title} <span class="count">${rows.length}</span></h2></summary>${list}</details>`;
  };
  app.innerHTML = items.length
    ? `${lead}<div class="progress-page">${groups.map(groupMarkup).join("")}</div>`
    : `${lead}<p class="empty">Nothing tracked yet. Start tracking from an entry's page.</p>`;
  app.querySelector(".progress-page")?.addEventListener("click", async (event) => {
    const button = event.target instanceof Element ? event.target.closest("[data-step]") : null;
    const rowElement = button?.closest(".progress-row");
    const item = items.find(({ entry }) => entry.id === rowElement?.getAttribute("data-entry"));
    if (!(button instanceof HTMLButtonElement) || !rowElement || !item) return;
    button.disabled = true;
    try {
      item.progress = (await sendJson(progressPath(item.entry.id, "/step"), "POST", { by: 1 })).progress;
      rowElement.outerHTML = row(item);
      if (item.progress.total != null && item.progress.current >= item.progress.total) {
        showToast(`${item.entry.title}: all done. Finish it from its page.`);
      }
    } catch (reason) {
      showToast(reason?.message || String(reason), "error");
      button.disabled = false;
    }
  });
}

function entryPickerField(name, label, id, title, placeholder = "search entries…") {
  return `<div class="form-field entry-picker" data-entry-picker>
    <span>${escapeHtml(label)}</span>
    <div class="entry-picker-body">
      <input type="hidden" name="${name}" value="${escapeHtml(id)}">
      <input type="search" data-entry-query value="${escapeHtml(title || id)}" placeholder="${escapeHtml(placeholder)}" autocomplete="off" ${id ? "" : "required"}>
      <small class="dim mono" data-entry-id>${escapeHtml(id)}</small>
      <div class="entry-picker-results" data-entry-results></div>
    </div>
  </div>`;
}

function bindEntryPickers(root) {
  root.querySelectorAll("[data-entry-picker]").forEach((picker) => {
    const hidden = picker.querySelector('input[type="hidden"]');
    const query = picker.querySelector("[data-entry-query]");
    const idLabel = picker.querySelector("[data-entry-id]");
    const results = picker.querySelector("[data-entry-results]");
    if (!(hidden instanceof HTMLInputElement) || !(query instanceof HTMLInputElement) || !results || !idLabel) return;
    let timer;
    let controller;
    const choose = (id, title) => {
      hidden.value = id;
      query.value = title || id;
      idLabel.textContent = id;
      results.innerHTML = "";
    };
    query.addEventListener("input", () => {
      hidden.value = query.value.trim();
      idLabel.textContent = "";
      clearTimeout(timer);
      const text = query.value.trim();
      if (!text) {
        results.innerHTML = "";
        return;
      }
      timer = setTimeout(async () => {
        controller?.abort();
        controller = new AbortController();
        try {
          const response = await fetch(`/api/v1/entries?q=${encodeURIComponent(text)}&limit=8`, { signal: controller.signal });
          const found = await response.json();
          results.innerHTML = (found.entries || []).map((entry) => `<button type="button" class="split-result" data-pick="${escapeHtml(entry.id)}" data-title="${escapeHtml(entry.title)}">${escapeHtml(entry.title)} ${kindTag(entry.kind)} <span class="dim mono">${escapeHtml(entry.id)}</span></button>`).join("") || `<span class="dim">no match — the text is used as a raw id</span>`;
          results.querySelectorAll("[data-pick]").forEach((pick) => pick.addEventListener("click", () => choose(pick.dataset.pick, pick.dataset.title)));
        } catch (error) {
          if (error?.name !== "AbortError") showToast(error?.message || String(error), "error");
        }
      }, 200);
    });
    query.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      const first = results.querySelector("[data-pick]");
      if (first instanceof HTMLElement) {
        event.preventDefault();
        first.click();
      }
    });
  });
}

function relationKindFields(kind, fields) {
  if (kind === "contains") return rangeField(fields, "factor", "borrowed weight", "1");
  if (kind === "killed_by") return `${rangeField(fields, "potential", "lost potential")}${rangeField(fields, "effect", "actual effect")}`;
  if (kind === "remix") return `<p class="form-note full">Fixed 0.2 scalar borrow — no extra parameters.</p>`;
  if (kind === "feature_music") return `<p class="form-note full">Fixed 0.2 on the Music anchor only — no extra parameters.</p>`;
  if (kind === "credit") return `${field("roles", `<input name="roles" type="text" value="${escapeHtml(fieldValue(fields, "roles"))}" placeholder="vocal+image/2" required list="role-names" autocomplete="off"><datalist id="role-names">${roleNames.map((name) => `<option value="${name}">`).join("")}</datalist>`, `from = contributor, to = work. Roles join with +, scale with * or /: ${roleNames.slice(0, 9).join(", ")}…`)}`;
  return `<p class="error full">Unknown relation kind.</p>`;
}

function openRelationEditor(relation = null, action) {
  if (!currentEntryData) return;
  const entryId = currentEntryData.entry.id;
  const fields = { ...(relation?.params || {}) };
  fields.kind = relation?.kind || "contains";
  fields.from_entry = relation?.from || entryId;
  fields.to_entry = relation?.to || "";
  const titleOf = (id) => (id === entryId ? currentEntryData.entry.title : id && relation?.other?.id === id ? relation.other.title : "");
  const body = `
  ${relation ? "" : `<p class="form-note editor-intro">The relation's kind already implies its transform under DAH_standards — no separate scalar/regional/transport choice to make.</p>`}
  <div class="form-grid">
    ${selectField(fields, "kind", "relation kind", relationKinds, fields.kind)}
    ${entryPickerField("from_entry", "from entry", fields.from_entry, titleOf(fields.from_entry))}
    ${entryPickerField("to_entry", "to entry", fields.to_entry, titleOf(fields.to_entry))}
    <div id="relation-kind-fields" class="form-grid full">${relationKindFields(fields.kind, fields)}</div>
  </div>`;
  const refresh = async (message) => {
    await refreshEntry(entryId);
    showToast(message);
  };
  const host = relation ? action.closest(".relation-row") || action : action.closest(".section-actions") || action;
  const editor = showEditor(
    host,
    relation ? "Edit relation" : "Add relation",
    body,
    async (payload) => {
      const path = relation ? `/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relation.id)}` : `/api/v1/entries/${encodeURIComponent(entryId)}/relations`;
      await sendJson(path, relation ? "PUT" : "POST", payload);
      await refresh(relation ? "Relation saved" : "Relation added");
    },
    relation ? async () => {
      await sendJson(`/api/v1/entries/${encodeURIComponent(entryId)}/relations/${encodeURIComponent(relation.id)}`, "DELETE");
      await refresh("Relation deleted");
    } : null,
  );
  bindEntryPickers(editor);
  const kindSelect = editor.querySelector('select[name="kind"]');
  kindSelect?.addEventListener("change", () => {
    const target = editor.querySelector("#relation-kind-fields");
    if (target instanceof HTMLElement && kindSelect instanceof HTMLSelectElement) {
      target.innerHTML = relationKindFields(kindSelect.value, {});
      bindRangeOutputs(target);
    }
  });
}

async function renderPalette() {
  currentEntryData = null;
  document.title = "Palette — NRS";
  app.innerHTML = `<h1>palette</h1><p class="muted">A provisional geometry prior -- not fitted psychology, and not active in the score view yet.</p><p class="loading">Loading palette…</p>`;
  try {
    const data = await getJson("/api/v1/palette");
    const cellValues = new Map(data.cells.map((cell) => [`${cell.row}\u0000${cell.column}`, cell.value]));
    app.innerHTML = `
      <h1>palette</h1>
      <p class="muted">A provisional geometry prior -- not fitted psychology, and not active in the score view yet.</p>
      <section class="page-section"><h2>Anchors</h2>
        <div class="data-table-wrap"><table class="data-table"><thead><tr><th>id</th><th>label</th><th>family</th></tr></thead><tbody>
          ${data.anchors.map((anchor) => `<tr><td class="mono brass">${escapeHtml(anchor.id)}</td><td>${escapeHtml(anchor.label)}</td><td class="muted">${escapeHtml(anchor.family || "—")}</td></tr>`).join("")}
        </tbody></table></div>
      </section>
      <section class="page-section"><h2>Feature similarity heatmap</h2><p class="muted section-copy">Brighter cells mean more double-counting. Hover a cell for the exact value.</p>
        <div class="heatmap-wrap"><table class="heatmap"><thead><tr><th></th>${data.anchors.map((anchor) => `<th title="${escapeHtml(anchor.label)}">${escapeHtml(anchor.id)}</th>`).join("")}</tr></thead><tbody>
          ${data.anchors.map((row) => `<tr><th title="${escapeHtml(row.label)}">${escapeHtml(row.id)}</th>${data.anchors.map((column) => { const value = Number(cellValues.get(`${row.id}\u0000${column.id}`) || 0); return `<td title="${escapeHtml(row.label)} × ${escapeHtml(column.label)} = ${value.toFixed(3)}" style="--cell:${Math.round((.08 + .84 * Math.max(0, Math.min(1, value))) * 100)}%">${value.toFixed(2)}</td>`; }).join("")}</tr>`).join("")}
        </tbody></table></div>
      </section>`;
  } catch (error) {
    renderError(error);
  }
}

async function renderCalibrate() {
  currentEntryData = null;
  document.title = "Calibrate — NRS";
  app.innerHTML = `<h1>calibrate</h1><div id="calibration-pair"><p class="loading">Choosing a pair…</p></div>`;
  const target = app.querySelector("#calibration-pair");
  if (!(target instanceof HTMLElement)) return;
  const paint = (pair) => {
    target.innerHTML = `<p class="calibration-prompt">Which of these do you actually think is better?</p><div class="calibration-options">
      ${[pair.left, pair.right].map((option, index) => `<button class="calibration-option" type="button" data-index="${index}"><strong>${escapeHtml(option.title)}</strong><span>current V: ${Number(option.value).toFixed(3)}</span></button>`).join("")}
    </div>`;
    target.querySelectorAll(".calibration-option").forEach((button) => button.addEventListener("click", async () => {
      const winner = Number(button.getAttribute("data-index")) === 0 ? pair.left : pair.right;
      const loser = winner === pair.left ? pair.right : pair.left;
      target.classList.add("busy");
      try {
        const next = await sendJson("/api/v1/calibration/pair", "POST", { winner: winner.id, loser: loser.id });
        paint(next);
        showToast("Preference recorded");
      } catch (error) {
        showToast(error?.message || String(error), "error");
      } finally {
        target.classList.remove("busy");
      }
    }));
  };
  try {
    paint(await getJson("/api/v1/calibration/pair"));
  } catch (error) {
    renderError(error);
  }
}

async function renderKfeat() {
  currentEntryData = null;
  document.title = "K — NRS";
  const intro = `<h1>K</h1><p class="muted">Pooling exponents. An anchor's own exponent <var>w</var> sets how repeats grow (<var>n</var> equal hits score like <var>n<sup>w</sup></var>; 1 = they simply add). A pair's exponent is where the two pool together: 1 = independent (they add), equal to both anchors' <var>w</var> = one box. Scoring merges pairs tightest-first, so only the hierarchy matters.</p>`;
  app.innerHTML = `${intro}<p class="loading">Loading K…</p>`;
  const row = (pair) => `<form class="kfeat-row"><span>${pair.a === pair.b ? escapeHtml(pair.a) : `${escapeHtml(pair.a)} ↔ ${escapeHtml(pair.b)}`}</span><input type="hidden" name="a" value="${escapeHtml(pair.a)}"><input type="hidden" name="b" value="${escapeHtml(pair.b)}"><input type="number" name="value" min="0.01" max="1" step="0.05" value="${escapeHtml(pair.value)}"><button type="submit">save</button></form>`;
  try {
    const data = await getJson("/api/v1/calibration/kfeat");
    const corrections = data.corrections.length
      ? `<h2>implied by the hierarchy</h2><p class="muted">These pairs already pool tighter than written, through a chain of tighter links. Scoring uses the tighter value; loosen a link in the chain to undo it.</p><ul class="kfeat-corrections">${data.corrections.map((c) => `<li>${escapeHtml(c.a)} ↔ ${escapeHtml(c.b)}: ${escapeHtml(c.from)} → <strong>${escapeHtml(c.to)}</strong> <span class="dim">via ${c.via.map(escapeHtml).join(" → ")}</span></li>`).join("")}</ul>`
      : "";
    app.innerHTML = `${intro}
      <h2>anchor exponents</h2><div class="kfeat-list">${data.exponents.map(row).join("")}</div>
      ${corrections}
      <h2>pair exponents</h2><div class="kfeat-list">${data.pairs.map(row).join("")}</div>`;
    app.querySelectorAll(".kfeat-row").forEach((form) => form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!(form instanceof HTMLFormElement)) return;
      const payload = Object.fromEntries(new FormData(form).entries());
      payload.value = Number(payload.value);
      try {
        await sendJson("/api/v1/calibration/kfeat", "PUT", payload);
        showToast(payload.a === payload.b ? `${payload.a} saved` : `${payload.a} ↔ ${payload.b} saved`);
        renderKfeat();
      } catch (error) {
        showToast(error?.message || String(error), "error");
      }
    }));
  } catch (error) {
    renderError(error);
  }
}

async function renderReview() {
  currentEntryData = null;
  document.title = "Review — NRS";
  app.innerHTML = `<h1>review</h1><p class="muted">Entries with something left to confirm.</p><p class="loading">Loading review queue…</p>`;
  try {
    const data = await getJson("/api/v1/review");
    app.innerHTML = `<h1>review</h1><p class="muted">Entries with something left to confirm.</p>${data.entries.length ? `<div class="review-list">${data.entries.map((entry) => `<a href="${pathForEntry(entry.id)}" data-link>${escapeHtml(entry.title)}</a>`).join("")}</div>` : `<p class="empty">Nothing left to review.</p>`}`;
  } catch (error) {
    renderError(error);
  }
}

/**
 * `quiet` repaints in place after a write (see `refreshEntry`): open
 * groups survive, the scroll position is kept, and errors go back to
 * the caller instead of replacing the page. Impact cards' own
 * "details & edit" panels close again -- except `keepEditorFor`'s,
 * whose editor reopens.
 */
async function renderEntry(id, { quiet = false, keepEditorFor = null } = {}) {
  const disclosures = quiet ? [...app.querySelectorAll("details[data-disclosure]")] : [];
  const seenDisclosures = new Set(disclosures.map((element) => element.getAttribute("data-disclosure")));
  const openDisclosures = new Set(disclosures.filter((element) => element instanceof HTMLDetailsElement && element.open).map((element) => element.getAttribute("data-disclosure")));
  const scrollY = window.scrollY;
  if (!quiet) app.innerHTML = `<p class="loading">Loading entry…</p>`;
  try {
    const data = await getJson(`/api/v1/entries/${encodeURIComponent(id)}`);
    currentEntryData = data;
    document.title = `${data.entry.title} — NRS`;
    app.innerHTML = `
      <a class="back-link" href="/entries" data-link>← all entries</a>
      <header class="entry-header">
        <h1>${escapeHtml(data.entry.title)}</h1>
        <p class="muted mono">${kindSelect(data)} ${escapeHtml(data.entry.store)}:${escapeHtml(data.entry.store_id)}</p>
      </header>
      <section class="page-section progress-panel" data-progress>
        <h2>Progress</h2>
        <p class="loading">Loading progress…</p>
      </section>
      <section class="page-section score-panel" data-score>
        <h2>Score</h2>
        <p class="loading">Scoring…</p>
      </section>
      <section class="page-section">
        <h2>Impacts <span class="count">${data.impacts.length}</span></h2>
        <p class="section-lead">What this entry did, why it mattered, and which feeling or craft anchor carried it.</p>
        ${data.impacts.length ? impactSections(data.impacts) : `<p class="empty">No impacts yet.</p>`}
        <div class="section-actions"><button class="button primary" type="button" data-action="add-impact">add impact</button></div>
      </section>
      <section class="page-section">
        <h2>Relations <span class="count">${data.relations.length}</span></h2>
        <p class="section-lead">Structure, lineage, credits, and influence. Open only the branch you need.</p>
        ${data.relations.length ? `<div class="relation-sections">${relationSectionsMarkup(data.relations)}</div>` : `<p class="empty">No relations yet.</p>`}
        <div class="related-slot"></div>
        <div class="section-actions"><button class="button primary" type="button" data-action="add-relation">add relation</button></div>
      </section>`;
    bindKindSelect(data);
    bindInlineSplits(data);
    bindTracklist(data);
    bindRelated(data);
    bindScore(data, openDisclosures);
    bindProgress(data);
    app.querySelectorAll("details[data-disclosure]").forEach((element) => {
      if (!(element instanceof HTMLDetailsElement)) return;
      const key = element.getAttribute("data-disclosure");
      if (element.classList.contains("card-details")) element.open = key === keepEditorFor;
      else if (seenDisclosures.has(key)) element.open = openDisclosures.has(key);
    });
    if (quiet) window.scrollTo(0, scrollY);
    else app.focus({ preventScroll: true });
  } catch (error) {
    if (quiet) throw error;
    renderError(error);
  }
}

const seasonNames = ["winter", "spring", "summer", "fall"];

function currentSeason(date = new Date()) {
  return `${date.getFullYear()}-${seasonNames[Math.floor(date.getMonth() / 3)]}`;
}

function shiftSeason(season, by) {
  const [year, name] = season.split("-");
  const index = seasonNames.indexOf(name) + by;
  return `${Number(year) + Math.floor(index / 4)}-${seasonNames[((index % 4) + 4) % 4]}`;
}

function seasonLabel(season) {
  const [year, name] = season.split("-");
  return `${name} ${year}`;
}

/**
 * A season's anime from AniList (the anime store's discover), most
 * popular first, each marked in NRS or addable in place.
 */
async function renderSeasons() {
  currentEntryData = null;
  const asked = new URLSearchParams(location.search).get("season") || "";
  const season = /^\d{4}-(winter|spring|summer|fall)$/.test(asked) ? asked : currentSeason();
  const hideKey = "nrs.seasons.hideAdopted";
  let hideAdopted = false;
  try {
    hideAdopted = localStorage.getItem(hideKey) === "1";
  } catch {}
  document.title = `${seasonLabel(season)} — NRS`;
  const link = (target, label) => `<a class="button" href="/seasons?season=${target}" data-link>${label}</a>`;
  app.innerHTML = `
    <h1>seasons</h1>
    <p class="muted entry-subtitle">A season's anime on AniList, most popular first. Adding one fetches it into the anime store and makes it an NRS entry.</p>
    <div class="toolbar season-bar">
      ${link(shiftSeason(season, -1), `‹ ${seasonLabel(shiftSeason(season, -1))}`)}
      <h2 class="season-name">${escapeHtml(seasonLabel(season))}</h2>
      ${link(shiftSeason(season, 1), `${seasonLabel(shiftSeason(season, 1))} ›`)}
      ${season === currentSeason() ? "" : link(currentSeason(), "now")}
      <label class="season-toggle"><input type="checkbox" class="hide-adopted" ${hideAdopted ? "checked" : ""}> hide entries in NRS</label>
    </div>
    <p class="entry-count">…</p>
    <div class="season-grid"><p class="loading">Loading ${escapeHtml(seasonLabel(season))}…</p></div>
    <div class="section-actions"><button class="button season-more" type="button" hidden>load more</button></div>
  `;
  const grid = app.querySelector(".season-grid");
  const count = app.querySelector(".entry-count");
  const more = app.querySelector(".season-more");
  const toggle = app.querySelector(".hide-adopted");
  if (!(grid instanceof HTMLElement) || !(count instanceof HTMLElement) || !(more instanceof HTMLButtonElement) || !(toggle instanceof HTMLInputElement)) return;
  const shows = [];
  let nextPage = 1;

  const paint = () => {
    const visible = shows.map((show, index) => [show, index]).filter(([show]) => !(hideAdopted && show.entry_id));
    const adopted = shows.filter((show) => show.entry_id).length;
    count.textContent = `${shows.length}${nextPage ? "+" : ""} shows · ${adopted} in NRS`;
    grid.innerHTML = visible.length
      ? visible.map(([show, index]) => seasonCard(show, index)).join("")
      : `<p class="empty">${shows.length ? "Every show loaded so far is in NRS." : "AniList lists nothing for this season."}</p>`;
    more.hidden = !nextPage;
  };
  async function loadPage() {
    more.disabled = true;
    try {
      const data = await getJson(`/api/v1/stores/anime/discover?${new URLSearchParams({ season, page: String(nextPage), limit: "50" })}`);
      shows.push(...data.candidates);
      nextPage = data.next_page;
      paint();
    } catch (error) {
      renderError(error);
    } finally {
      more.disabled = false;
    }
  }
  more.addEventListener("click", loadPage);
  toggle.addEventListener("change", () => {
    hideAdopted = toggle.checked;
    try {
      localStorage.setItem(hideKey, hideAdopted ? "1" : "0");
    } catch {}
    paint();
  });
  grid.addEventListener("click", async (event) => {
    const button = event.target instanceof Element ? event.target.closest("[data-ingest]") : null;
    if (!(button instanceof HTMLButtonElement)) return;
    const show = shows[Number(button.dataset.ingest)];
    const id = await ingestCandidate(show, button);
    if (!id) return;
    show.entry_id = id;
    showToast(`Added ${show.title}`);
    paint();
  });
  await loadPage();
}

function seasonCard(show, index) {
  const cover = show.cover_url
    ? `<img class="season-cover" src="${escapeHtml(show.cover_url)}" alt="" loading="lazy">`
    : `<div class="season-cover"></div>`;
  const action = show.entry_id
    ? `<a class="link-button" href="${pathForEntry(show.entry_id)}" data-link>in NRS</a>`
    : `<button class="button" type="button" data-ingest="${index}">add</button>`;
  return `<article class="season-card${show.entry_id ? " adopted" : ""}">
    ${cover}
    <div class="season-copy">
      <a class="season-title" href="${escapeHtml(show.canonical_url || show.query)}" target="_blank" rel="noopener">${escapeHtml(show.title)}</a>
      ${show.subtitle ? `<p class="hit-subtitle">${escapeHtml(show.subtitle)}</p>` : ""}
      <div class="season-action">${storeKindTag(show.kind)}${action}</div>
    </div>
  </article>`;
}

function route() {
  routeVersion++;
  pageTeardown?.();
  pageTeardown = null;
  pageKeys = null;
  clearTimeout(searchTimer);
  requestController.abort();
  setActiveNav();
  const match = location.pathname.match(/^\/entries\/([^/]+)\/?$/);
  if (match) {
    renderEntry(decodeURIComponent(match[1]));
  } else if (/^\/entries\/?$/.test(location.pathname)) {
    renderEntries();
  } else if (/^\/seasons\/?$/.test(location.pathname)) {
    renderSeasons();
  } else if (/^\/progress\/?$/.test(location.pathname)) {
    renderProgress();
  } else if (/^\/palette\/?$/.test(location.pathname)) {
    renderPalette();
  } else if (/^\/calibrate\/?$/.test(location.pathname)) {
    renderCalibrate();
  } else if (/^\/kfeat\/?$/.test(location.pathname)) {
    renderKfeat();
  } else if (/^\/review\/?$/.test(location.pathname)) {
    renderReview();
  } else {
    renderScores();
  }
}

route();

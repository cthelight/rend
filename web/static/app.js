let state = null;
let settingsKey = "";
const cards = new Map();
const tabs = new Map();
// The drive index whose card is visible; null until the first snapshot.
let activeTab = null;
// Card indices with a request in flight; their action buttons are disabled.
const pending = new Set();
// Monotonically increasing; only the newest snapshot may be applied, so an
// out-of-order response can never restore a stale view.
let snapshotSeq = 0;

const DISC_SVG = `<svg viewBox="0 0 64 64" fill="none" aria-hidden="true">
  <circle cx="32" cy="32" r="29" stroke="currentColor" stroke-width="2.5"/>
  <circle cx="32" cy="32" r="9" stroke="currentColor" stroke-width="2.5"/>
  <circle cx="32" cy="32" r="2.5" fill="currentColor"/>
  <path d="M11 41a24 24 0 0 1-1-17" stroke="currentColor" stroke-width="1.5" opacity="0.5"/>
</svg>`;

function $(selector) {
  return document.querySelector(selector);
}

async function post(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body ?? {}),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new Error(payload.error || response.statusText);
  }
  return response.status === 204 ? null : response.json();
}

async function refresh() {
  const seq = ++snapshotSeq;
  const response = await fetch("/api/state");
  if (!response.ok) throw new Error(response.statusText);
  applySnapshot(seq, await response.json());
}

function applySnapshot(seq, snapshot) {
  if (seq !== snapshotSeq) return;
  state = snapshot;
  render();
}

function timeStamp() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function addLog(message) {
  const item = document.createElement("li");
  const time = document.createElement("span");
  time.className = "log-time";
  time.textContent = timeStamp();
  const msg = document.createElement("span");
  msg.className = "log-msg";
  msg.textContent = message;
  item.append(time, msg);
  if (/(failed|error|died|not found|denied)/i.test(message)) {
    item.classList.add("log--err");
  }
  const list = $("#log-list");
  list.prepend(item);
  while (list.children.length > 100) list.lastChild.remove();
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  })[ch]);
}

function duration(secs) {
  if (secs == null) return "";
  const minutes = Math.floor(secs / 60);
  const seconds = secs % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function humanBytes(bytes) {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(1)} ${units[i]}`;
}

function progress(trackState) {
  if (trackState.status === "ripping" && trackState.total) {
    return `${(trackState.done / 1024 / 1024).toFixed(1)} / ${(
      trackState.total /
      1024 /
      1024
    ).toFixed(1)} MiB`;
  }
  return trackState.detail || "";
}

// Overall rip progress across the selected tracks: finished tracks count
// fully, the track being ripped counts its byte fraction, pending count 0.
function ripProgress(rip) {
  const selected = new Set(rip.tracks);
  let units = 0;
  let done = 0;
  let finished = 0;
  for (const trackState of rip.states) {
    if (!selected.has(trackState.number)) continue;
    units += 1;
    if (trackState.status === "ripping" && trackState.total) {
      done += trackState.done / trackState.total;
    } else if (trackState.status !== "pending") {
      done += 1;
      finished += 1;
    }
  }
  return { finished, units, fraction: units ? done / units : 0 };
}

function drivePill(status) {
  switch (status) {
    case "disc present":
      return "pill--ok";
    case "tray open":
      return "pill--warn";
    case "not ready":
      return "pill--warn";
    case "unreadable":
      return "pill--err";
    default:
      return "pill--muted";
  }
}

function lookupPill(status) {
  if (/…$/.test(status)) return "pill--busy";
  if (/failed/i.test(status)) return "pill--err";
  if (/no match/i.test(status)) return "pill--warn";
  if (/^found$|^loaded$|^demo disc$/i.test(status)) return "pill--ok";
  if (/match \d+ of \d+/i.test(status)) return "pill--info";
  if (/manual/i.test(status)) return "pill--warn";
  return "pill--muted";
}

function statePill(status) {
  switch (status) {
    case "ripping":
      return "busy";
    case "done":
      return "ok";
    case "failed":
      return "err";
    default:
      return "muted";
  }
}

function renderSettings() {
  const key = [
    state.format,
    state.quality,
    state.force,
    state.outDir,
    state.formats
      .map((format) => `${format.id}:${format.qualityOptions.map((o) => o.value).join(",")}`)
      .join("|"),
  ].join(":");
  if (settingsKey === key) return;
  settingsKey = key;

  const currentFormat = state.formats.find((format) => format.id === state.format);
  const qualityOptions = currentFormat?.qualityOptions ?? [];
  const settings = $("#settings");
  settings.innerHTML = `
    <span class="chip" title="output directory">
      <span>out</span>
      <span class="chip-text">${escapeHtml(state.outDir)}</span>
    </span>
    <div class="seg" role="group" aria-label="output format">
      ${state.formats
        .map(
          (format) => `
      <button type="button" class="seg-btn ${state.format === format.id ? "active" : ""}" data-format="${format.id}">
        ${escapeHtml(format.label)}
      </button>`
        )
        .join("")}
    </div>
    ${
      qualityOptions.length
        ? `
    <label class="quality">
      <span>quality</span>
      <select id="quality">
        ${qualityOptions
          .map(
            (option) => `
        <option value="${option.value}" ${state.quality === option.value ? "selected" : ""}>
          ${escapeHtml(option.label)}
        </option>`
          )
          .join("")}
      </select>
    </label>`
        : ""
    }
    <label class="switch" title="overwrite existing files">
      <input type="checkbox" id="force" ${state.force ? "checked" : ""}>
      <span class="switch-track" aria-hidden="true"></span>
      <span>force</span>
    </label>
  `;

  const force = $("#force");
  force.addEventListener("change", async (event) => {
    const previous = state.force;
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { force: event.target.checked }));
    } catch (error) {
      addLog(error.message);
      event.target.checked = previous;
    }
  });

  const quality = $("#quality");
  if (quality) {
    quality.addEventListener("change", async (event) => {
      const previous = state.quality;
      const seq = ++snapshotSeq;
      try {
        applySnapshot(
          seq,
          await post("/api/settings", { quality: Number(event.target.value) })
        );
      } catch (error) {
        addLog(error.message);
        event.target.value = previous;
      }
    });
  }
}

function buildCard(drive) {
  const card = document.createElement("section");
  card.className = "drive";
  card.dataset.index = drive.index;
  card.innerHTML = `
    <header class="drive-head">
      <div class="drive-id">
        <h2 class="drive-label"></h2>
        <div class="drive-status"></div>
        <div class="path drive-path"></div>
      </div>
      <div class="actions">
        <button type="button" class="btn" data-action="load">Read TOC</button>
        <button type="button" class="btn" data-action="eject">Eject</button>
        <button type="button" class="btn btn--danger" data-action="stop" disabled>Stop</button>
        <button type="button" class="btn btn--primary" data-action="rip">Rip selected</button>
      </div>
    </header>
    <div class="drive-body">
      <aside class="disc">
        <div class="meta-slot"></div>
        <div class="matches-slot"></div>
      </aside>
      <div class="tracks">
        <div class="toc-slot"></div>
        <div class="tags-slot"></div>
        <div class="rip-slot"></div>
      </div>
    </div>
  `;
  return card;
}

function tocTable(audioTracks, checked, active, titles) {
  const all = audioTracks.length > 0 && audioTracks.every((t) => checked.has(t.number));
  return `
    <div class="toc">
      <h3 class="section-title">Tracks</h3>
      <div class="toc-head">
        <label class="selectall">
          <input type="checkbox" data-select-all ${all ? "checked" : ""} ${active ? "disabled" : ""}>
          <span>Select all</span>
        </label>
        <span class="toc-count"></span>
      </div>
      <table>
        <thead>
          <tr><th class="col-check"></th><th class="col-num">#</th><th>Title</th><th class="col-dur">Time</th></tr>
        </thead>
        <tbody>
          ${audioTracks
            .map((track, index) => {
              const title = titles && titles[index] ? titles[index].title : "";
              return `
            <tr data-track-row="${track.number}">
              <td class="col-check">
                <input
                  type="checkbox"
                  data-track="${track.number}"
                  aria-label="select track ${track.number}"
                  ${checked.has(track.number) ? "checked" : ""}
                  ${active ? "disabled" : ""}
                >
              </td>
              <td class="col-num">${track.number}</td>
              <td class="title">${
                title
                  ? escapeHtml(title)
                  : `<span class="track-fallback">Track ${track.number}</span>`
              }</td>
              <td class="col-dur">${duration(track.durationSecs)}</td>
            </tr>`;
            })
            .join("")}
        </tbody>
      </table>
    </div>`;
}

function syncTocSlot(slot, audioTracks, selected, active) {
  for (const track of audioTracks) {
    const row = slot.querySelector(`[data-track-row="${track.number}"]`);
    if (!row) continue;
    const checkbox = row.querySelector('input[data-track]');
    checkbox.checked = selected.has(track.number);
    checkbox.disabled = active;
  }
  const selectedCount = audioTracks.filter((track) => selected.has(track.number)).length;
  const selectAll = slot.querySelector("[data-select-all]");
  if (selectAll) {
    selectAll.checked = selectedCount === audioTracks.length && selectedCount > 0;
    selectAll.indeterminate = selectedCount > 0 && selectedCount < audioTracks.length;
    selectAll.disabled = active;
  }
  const count = slot.querySelector(".toc-count");
  if (count) {
    count.textContent = `${selectedCount} of ${audioTracks.length} selected`;
  }
}

function updateTocSlot(record, drive, active) {
  const slot = record.card.querySelector(".toc-slot");
  const tracks = drive.toc ? drive.toc.tracks : [];
  const audioTracks = tracks.filter((track) => track.kind === "audio");
  const titles = drive.meta ? drive.meta.tracks : null;
  const numbersKey = audioTracks.map((track) => track.number).join(",");
  const titleKey = titles ? titles.map((track) => track.title).join("|") : "";

  if (!drive.toc) {
    record.tocKey = null;
    record.tocRenderKey = null;
    slot.innerHTML = '<div class="slot-empty">No disc loaded.</div>';
    return;
  }
  if (record.tocKey !== numbersKey) {
    record.tocKey = numbersKey;
    record.checked = new Set(audioTracks.map((track) => track.number));
    record.tocRenderKey = null;
  }
  const selected = active ? new Set(drive.rip.tracks) : record.checked;
  const renderKey = `${numbersKey}|${titleKey}`;
  if (record.tocRenderKey !== renderKey) {
    record.tocRenderKey = renderKey;
    slot.innerHTML = tocTable(audioTracks, selected, active, titles);
  }
  syncTocSlot(slot, audioTracks, selected, active);
}

function updateMetaSlot(record, drive) {
  // Rebuilding here re-assigns the cover's data URL, making the browser
  // re-decode it; skip the rebuild when none of the inputs changed.
  const meta = drive.meta;
  const metaKey = [
    drive.lookupStatus,
    drive.mcn || "",
    drive.registerUrl || "",
    drive.coverFetching,
    Boolean(drive.toc),
    meta ? `${meta.artist}\u0000${meta.album}\u0000${meta.year || ""}` : "",
  ].join("\u0000");
  if (metaKey === record.metaKey && drive.cover === record.metaCover) return;
  record.metaKey = metaKey;
  record.metaCover = drive.cover;
  const album = meta ? meta.album : "";
  const artist = meta ? meta.artist : "";
  const year = meta ? meta.year : null;
  record.card.querySelector(".meta-slot").innerHTML = `
    <div class="cover">
      <img src="${escapeHtml(drive.cover || "")}" alt="cover art" ${
    drive.cover ? "" : "hidden"
  }>
      <div class="cover-fallback" ${drive.cover ? "hidden" : ""}>${DISC_SVG}</div>
    </div>
    <div class="disc-title">
      <h3 class="disc-album">${
    album
      ? escapeHtml(album)
      : drive.toc
        ? "Unidentified disc"
        : "No disc loaded"
  }</h3>
      <div class="disc-artist">${
    artist
      ? `${escapeHtml(artist)}${year ? ` <span class="disc-year">· ${escapeHtml(year)}</span>` : ""}`
      : ""
  }</div>
    </div>
    <div class="disc-lookup">
      <span class="pill ${lookupPill(drive.lookupStatus)}"><span class="pill-dot"></span>${escapeHtml(
        drive.lookupStatus || "idle"
      )}</span>
      ${
    drive.coverFetching
      ? '<span class="pill pill--muted"><span class="pill-dot"></span>fetching cover</span>'
      : ""
  }
    </div>
    <div class="disc-extra">
      ${
    drive.mcn
      ? `<div class="mcn"><span>MCN</span><code>${escapeHtml(drive.mcn)}</code></div>`
      : ""
  }
      ${
    drive.registerUrl
      ? `<a class="register" href="${escapeHtml(drive.registerUrl)}" target="_blank" rel="noopener">register on MusicBrainz ↗</a>`
      : ""
  }
    </div>`;
}

function updateMatchesSlot(card, drive, active) {
  const slot = card.querySelector(".matches-slot");
  if (!drive.matches.length) {
    slot.innerHTML = "";
    return;
  }
  slot.innerHTML = `<div class="matches">
    ${drive.matches.length > 1 ? '<h3 class="section-title">Matches</h3>' : ""}
    ${drive.matches
      .map(
        (match, index) => `
      <button
        type="button"
        class="match ${index === drive.matchSel ? "selected" : ""}"
        data-match="${index}"
        ${active ? "disabled" : ""}
      >
        <span class="match-radio" aria-hidden="true"></span>
        <span class="match-text">
          <span class="match-line1">${escapeHtml(match.artist)} — ${escapeHtml(match.album)}</span>
          <span class="match-line2">${
            [
              match.year ? escapeHtml(match.year) : "",
              match.discPosition ? `disc ${escapeHtml(match.discPosition)}` : "",
            ]
              .filter(Boolean)
              .join(" · ")
          }</span>
        </span>
      </button>`
      )
      .join("")}
  </div>`;
}

function updateTagsSlot(record, drive, active) {
  const slot = record.card.querySelector(".tags-slot");
  const meta = drive.meta;
  if (!meta) {
    if (record.formKey !== "none") {
      slot.innerHTML = "";
      record.formKey = "none";
    }
    return;
  }
  const formKey = `meta:${drive.matchSel}:${meta.tracks.length}`;
  if (record.formKey !== formKey) {
    slot.innerHTML = `<form class="tags" data-tags="${drive.index}">
      <h3 class="section-title">Tags</h3>
      <div class="tags-grid">
        <label>Artist <input name="artist" value="${escapeHtml(meta.artist)}"></label>
        <label>Album <input name="album" value="${escapeHtml(meta.album)}"></label>
        <label>Album artist <input name="albumArtist" value="${escapeHtml(
          meta.albumArtist || ""
        )}"></label>
        <label>Year <input name="year" value="${escapeHtml(meta.year || "")}"></label>
      </div>
      <table>
        <thead>
          <tr><th class="col-num">#</th><th>Title</th><th>Track artist</th></tr>
        </thead>
        <tbody>
          ${meta.tracks
            .map(
              (track, index) => `
            <tr>
              <td class="col-num">${index + 1}</td>
              <td><input name="title-${index}" value="${escapeHtml(track.title)}"></td>
              <td><input name="artist-${index}" value="${escapeHtml(
                track.artist || ""
              )}"></td>
            </tr>`
            )
            .join("")}
        </tbody>
      </table>
      <div class="tags-actions">
        <button type="submit" class="btn btn--primary">Save tags</button>
      </div>
    </form>`;
    record.formKey = formKey;
  }
  const submit = slot.querySelector('button[type="submit"]');
  if (submit) submit.disabled = active;
}

function updateRipSlot(record, drive) {
  const slot = record.card.querySelector(".rip-slot");
  const rip = drive.rip;
  if (!rip) {
    slot.innerHTML = "";
    record.ripRows = null;
    return;
  }
  const rows = rip.states.map((trackState) => trackState.number);
  const sameRows =
    record.ripRows &&
    record.ripRows.length === rows.length &&
    rows.every((number, i) => number === record.ripRows[i]);
  if (!sameRows) {
    record.ripRows = rows;
    slot.innerHTML = `
      <div class="rip">
        <h3 class="section-title">Rip</h3>
        <div class="rip-overall">
          <span class="bar bar--lg"><span class="fill"></span></span>
          <span class="rip-fraction"></span>
        </div>
        <div class="rip-meta"></div>
        <table>
          <thead>
            <tr><th class="col-num">#</th><th>state</th><th class="col-prog">progress</th></tr>
          </thead>
          <tbody>
            ${rip.states
              .map(
                (trackState) => `
              <tr data-state-number="${trackState.number}">
                <td class="col-num">${trackState.number}</td>
                <td class="state"></td>
                <td class="prog"></td>
              </tr>`
              )
              .join("")}
          </tbody>
        </table>
        <div class="summary"></div>
      </div>`;
  }
  slot.querySelector(".rip-meta").textContent = [
    `${rip.format} → ${rip.outDir}`,
    rip.force ? "(force)" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const { finished, units, fraction } = ripProgress(rip);
  const overall = slot.querySelector(".rip-overall");
  overall.querySelector(".fill").style.width = `${Math.round(fraction * 100)}%`;
  overall.querySelector(".rip-fraction").textContent = `${finished} of ${units} tracks`;
  for (const trackState of rip.states) {
    const row = slot.querySelector(`[data-state-number="${trackState.number}"]`);
    if (!row) continue;
    row.querySelector(".state").innerHTML = `
      <span class="pill pill--sm pill--${statePill(trackState.status)}"><span class="pill-dot"></span>${trackState.status}</span>`;
    let width = 0;
    let barClass = "";
    if (trackState.status === "done") {
      width = 100;
      barClass = "bar--done";
    } else if (trackState.status === "failed") {
      barClass = "bar--fail";
    }
    if (trackState.total) {
      width = Math.max(width, Math.round((trackState.done / trackState.total) * 100));
    }
    const text = progress(trackState);
    row.querySelector(".prog").innerHTML = `
      <span class="prog-wrap">
        <span class="bar ${barClass}"><span class="fill" style="width:${width}%"></span></span>
        ${
      text
        ? `<span class="prog-text${
            trackState.status === "failed" ? " prog-text--err" : ""
          }">${escapeHtml(text)}</span>`
        : ""
    }
      </span>`;
  }
  const summary = slot.querySelector(".summary");
  summary.textContent = rip.summary || "";
  summary.style.display = rip.summary ? "" : "none";
}

function updateCard(record, drive) {
  const card = record.card;
  const rip = drive.rip;
  const active = Boolean(rip && rip.active);

  card.querySelector(".drive-label").textContent = drive.label;
  card.querySelector(".drive-status").innerHTML = [
    `<span class="pill ${drivePill(drive.status)}"><span class="pill-dot"></span>${escapeHtml(
      drive.status || "unknown"
    )}</span>`,
    drive.disc
      ? `<span class="pill pill--muted">${escapeHtml(drive.disc)} disc</span>`
      : "",
    drive.demo ? '<span class="pill pill--violet">demo</span>' : "",
  ]
    .filter(Boolean)
    .join("");
  card.querySelector(".drive-path").textContent = drive.path;
  const busy = active || pending.has(drive.index);
  card.querySelector('[data-action="load"]').disabled =
    busy || drive.demo || !drive.disc;
  card.querySelector('[data-action="rip"]').disabled = busy || !drive.toc;
  card.querySelector('[data-action="stop"]').disabled = !active;
  card.querySelector('[data-action="eject"]').disabled = busy || drive.demo;

  updateTocSlot(record, drive, active);
  updateMetaSlot(record, drive);
  updateMatchesSlot(card, drive, active);
  updateTagsSlot(record, drive, active);
  updateRipSlot(record, drive);
}

function tabIndicator(drive) {
  const rip = drive.rip;
  if (rip && rip.active) {
    const { finished, units, fraction } = ripProgress(rip);
    return {
      classes: "tab--ripping",
      html: `<span class="bar"><span class="fill" style="width:${Math.round(
        fraction * 100
      )}%"></span></span><span class="tab-count">${finished}/${units}</span>`,
    };
  }
  if (drive.loadingToc || drive.coverFetching || /…$/.test(drive.lookupStatus)) {
    return { classes: "tab--busy", html: '<span class="spinner"></span>' };
  }
  if (rip) {
    const failed = rip.states.filter((track) => track.status === "failed").length;
    return failed
      ? {
          classes: "tab--failed",
          html: '<span class="dot dot--failed"></span>',
        }
      : { classes: "tab--done", html: '<span class="dot dot--done"></span>' };
  }
  return { classes: "", html: "" };
}

function updateTab(tab, drive) {
  tab.querySelector(".tab-label").textContent = drive.label;
  const indicator = tabIndicator(drive);
  tab.className = `tab ${indicator.classes}`.trim() + (tab.classList.contains("active") ? " active" : "");
  tab.querySelector(".tab-indicator").innerHTML = indicator.html;
  tab.title = [drive.status, drive.disc]
    .filter(Boolean)
    .join(" / ");
}

function renderTabs() {
  const bar = $("#tabs");
  bar.classList.toggle("hidden", !state.drives.length);
  const seen = new Set();
  state.drives.forEach((drive) => {
    seen.add(drive.index);
    let tab = tabs.get(drive.index);
    if (!tab) {
      tab = document.createElement("button");
      tab.type = "button";
      tab.className = "tab";
      tab.dataset.index = drive.index;
      tab.setAttribute("role", "tab");
      tab.innerHTML = `<span class="tab-label"></span><span class="tab-indicator"></span>`;
      tabs.set(drive.index, tab);
      bar.appendChild(tab);
    }
    updateTab(tab, drive);
  });
  for (const [index, tab] of tabs) {
    if (!seen.has(index)) {
      tab.remove();
      tabs.delete(index);
    }
  }
  if (!seen.has(activeTab)) activeTab = null;
  if (activeTab === null && state.drives.length) {
    activeTab = state.drives[0].index;
  }
  for (const [index, tab] of tabs) {
    const selected = index === activeTab;
    tab.classList.toggle("active", selected);
    tab.setAttribute("aria-selected", selected ? "true" : "false");
  }
}

function showActiveCard() {
  for (const [index, record] of cards) {
    record.card.classList.toggle("hidden", index !== activeTab);
  }
}

function applyRipEvent(driveId, event) {
  if (!state) return;
  const drive = state.drives.find(
    (candidate) => candidate.path === driveId || candidate.label === driveId
  );
  if (!drive || !drive.rip) return;
  const track = drive.rip.states.find((candidate) => candidate.number === event.number);
  if (!track) return;

  switch (event.event) {
    case "trackStarted":
      track.status = "ripping";
      track.detail = null;
      break;
    case "progress":
      track.status = "ripping";
      track.done = event.bytesDone;
      track.total = event.bytesTotal;
      break;
    case "trackDone":
      track.status = "done";
      track.detail = humanBytes(event.bytes);
      break;
    case "trackFailed":
      track.status = "failed";
      track.detail = event.error;
      break;
    case "trackSkipped":
      track.status = "skipped";
      track.detail = event.reason;
      break;
    case "finished": {
      drive.rip.active = false;
      const succeeded = event.total - event.failed;
      drive.rip.summary = event.stopped
        ? `stopped after ${succeeded} of ${event.total}`
        : `finished: ${succeeded} of ${event.total}${
            event.failed ? `, ${event.failed} failed` : ""
          }`;
      break;
    }
    default:
      break;
  }
}

function render() {
  if (!state) return;
  renderSettings();
  renderTabs();
  $("#empty").classList.toggle("hidden", state.drives.length > 0);
  const main = $("#drives");
  const seen = new Set();
  state.drives.forEach((drive) => {
    seen.add(drive.index);
    let record = cards.get(drive.index);
    if (!record) {
      record = {
        card: buildCard(drive),
        checked: new Set(),
        tocKey: null,
        tocRenderKey: null,
        metaKey: null,
        metaCover: null,
        formKey: null,
        ripRows: null,
      };
      cards.set(drive.index, record);
      main.appendChild(record.card);
    }
    updateCard(record, drive);
  });
  for (const [index, record] of cards) {
    if (!seen.has(index)) {
      record.card.remove();
      cards.delete(index);
    }
  }
  showActiveCard();
}

function bindDelegated() {
  $("#tabs").addEventListener("click", (event) => {
    const tab = event.target.closest(".tab");
    if (!tab) return;
    const index = Number(tab.dataset.index);
    if (index === activeTab) return;
    activeTab = index;
    showActiveCard();
    for (const [tabIndex, element] of tabs) {
      const selected = tabIndex === activeTab;
      element.classList.toggle("active", selected);
      element.setAttribute("aria-selected", selected ? "true" : "false");
    }
  });
  const settings = $("#settings");
  settings.addEventListener("click", async (event) => {
    const button = event.target.closest(".seg-btn");
    if (!button) return;
    const format = button.dataset.format;
    if (format === state.format) return;
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { format }));
    } catch (error) {
      addLog(error.message);
    }
  });
  const main = $("#drives");
  main.addEventListener("click", async (event) => {
    const actionButton = event.target.closest("[data-action]");
    if (actionButton) {
      const card = actionButton.closest(".drive");
      const index = Number(card.dataset.index);
      if (pending.has(index)) return;
      const action = actionButton.dataset.action;
      pending.add(index);
      try {
        if (action === "load") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/load`));
        } else if (action === "rip") {
          const tracks = [...card.querySelectorAll('input[data-track]:checked')].map(
            (input) => Number(input.dataset.track)
          );
          if (!tracks.length) {
            addLog("select at least one track to rip");
            return;
          }
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/rip`, { tracks }));
        } else if (action === "stop") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/stop`));
        } else if (action === "eject") {
          const seq = ++snapshotSeq;
          applySnapshot(seq, await post(`/api/drives/${index}/eject`));
        }
      } catch (error) {
        addLog(error.message);
      } finally {
        pending.delete(index);
        render();
      }
      return;
    }
    const matchButton = event.target.closest("[data-match]");
    if (matchButton) {
      const card = matchButton.closest(".drive");
      const index = Number(card.dataset.index);
      if (pending.has(index)) return;
      pending.add(index);
      try {
        const seq = ++snapshotSeq;
        applySnapshot(
          seq,
          await post(`/api/drives/${index}/match`, {
            index: Number(matchButton.dataset.match),
          })
        );
      } catch (error) {
        addLog(error.message);
      } finally {
        pending.delete(index);
        render();
      }
    }
  });

  main.addEventListener("change", (event) => {
    const checkbox = event.target.closest("input[data-track]");
    if (checkbox) {
      const card = checkbox.closest(".drive");
      const record = cards.get(Number(card.dataset.index));
      if (!record || !record.checked) return;
      const number = Number(checkbox.dataset.track);
      if (checkbox.checked) record.checked.add(number);
      else record.checked.delete(number);
      syncTocSlot(
        card.querySelector(".toc-slot"),
        [...card.querySelectorAll("input[data-track]")].map((box) => ({
          number: Number(box.dataset.track),
        })),
        record.checked,
        false
      );
      return;
    }
    const selectAll = event.target.closest("[data-select-all]");
    if (selectAll) {
      const card = selectAll.closest(".drive");
      const record = cards.get(Number(card.dataset.index));
      if (!record) return;
      const boxes = card.querySelectorAll("input[data-track]");
      for (const box of boxes) {
        box.checked = selectAll.checked;
        const number = Number(box.dataset.track);
        if (selectAll.checked) record.checked.add(number);
        else record.checked.delete(number);
      }
      syncTocSlot(
        card.querySelector(".toc-slot"),
        [...boxes].map((box) => ({ number: Number(box.dataset.track) })),
        record.checked,
        false
      );
    }
  });

  main.addEventListener("submit", async (event) => {
    const form = event.target.closest("form[data-tags]");
    if (!form) return;
    event.preventDefault();
    const card = form.closest(".drive");
    const index = Number(card.dataset.index);
    if (pending.has(index)) return;
    const data = new FormData(form);
    const trackCount = form.querySelectorAll('input[name^="title-"]').length;
    const tracks = [];
    for (let i = 0; i < trackCount; i += 1) {
      tracks.push({
        title: data.get(`title-${i}`),
        artist: data.get(`artist-${i}`),
      });
    }
    pending.add(index);
    try {
      const seq = ++snapshotSeq;
      applySnapshot(
        seq,
        await post(`/api/drives/${index}/tags`, {
          artist: data.get("artist"),
          album: data.get("album"),
          albumArtist: data.get("albumArtist"),
          year: data.get("year"),
          tracks,
        })
      );
    } catch (error) {
      addLog(error.message);
    } finally {
      pending.delete(index);
      render();
    }
  });
}

function setConn(on) {
  const conn = $("#conn");
  conn.classList.toggle("on", on);
  conn.classList.toggle("off", !on);
  conn.querySelector(".conn-text").textContent = on ? "live" : "reconnecting";
}

function connect() {
  const source = new EventSource("/api/events");
  source.onopen = () => {
    setConn(true);
    refresh().catch((error) => addLog(error.message));
  };
  source.onerror = () => {
    setConn(false);
  };
  source.onmessage = (message) => {
    let event;
    try {
      event = JSON.parse(message.data);
    } catch {
      return;
    }
    if (event.type === "driveChanged") {
      if (!state) {
        refresh().catch((error) => addLog(error.message));
        return;
      }
      // Locate the drive by its logical index (not array position) so a
      // re- or removal of devices can't shift the wrong card.
      const slot = state.drives.findIndex((drive) => drive.index === event.drive.index);
      if (slot === -1) {
        refresh().catch((error) => addLog(error.message));
      } else {
        state.drives[slot] = event.drive;
        render();
      }
    } else if (event.type === "rip") {
      applyRipEvent(event.drive, event.event);
      render();
    } else if (event.type === "log") {
      addLog(event.msg);
    } else if (event.type === "resync") {
      refresh().catch((error) => addLog(error.message));
    }
  };
}

(async function init() {
  await refresh().catch((error) => addLog(error.message));
  bindDelegated();
  connect();
})();

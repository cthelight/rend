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

function addLog(message) {
  const item = document.createElement("li");
  item.textContent = message;
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

function renderSettings() {
  const key = `${state.format}:${state.force}`;
  if (settingsKey === key) return;
  settingsKey = key;

  const settings = $("#settings");
  settings.innerHTML = `
    <label>
      format
      <select id="format">
        <option value="flac" ${state.format === "flac" ? "selected" : ""}>flac</option>
        <option value="wav" ${state.format === "wav" ? "selected" : ""}>wav</option>
      </select>
    </label>
    <label>
      <input type="checkbox" id="force" ${state.force ? "checked" : ""}>
      force
    </label>
    ${state.ffmpeg ? "" : '<span class="warn">ffmpeg not found; flac output is unavailable</span>'}
  `;

  $("#format").addEventListener("change", async (event) => {
    const previous = state.format;
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { format: event.target.value }));
    } catch (error) {
      addLog(error.message);
      event.target.value = previous;
    }
  });

  $("#force").addEventListener("change", async (event) => {
    const previous = state.force;
    const seq = ++snapshotSeq;
    try {
      applySnapshot(seq, await post("/api/settings", { force: event.target.checked }));
    } catch (error) {
      addLog(error.message);
      event.target.checked = previous;
    }
  });
}

function buildCard(drive) {
  const card = document.createElement("section");
  card.className = "drive";
  card.dataset.index = drive.index;
  card.innerHTML = `
    <header>
      <h2 class="drive-label"></h2>
      <span class="status drive-status"></span>
    </header>
    <div class="path drive-path"></div>
    <div class="actions">
      <button data-action="load">Read TOC</button>
      <button data-action="rip">Rip selected</button>
      <button data-action="stop" disabled>Stop</button>
      <button data-action="eject">Eject</button>
    </div>
    <div class="toc-slot"></div>
    <div class="meta-slot"></div>
    <div class="matches-slot"></div>
    <div class="tags-slot"></div>
    <div class="rip-slot"></div>
  `;
  return card;
}

function tocTable(audioTracks, checked, active) {
  return `<table>
    <thead>
      <tr><th></th><th>#</th><th>type</th><th>start</th><th>duration</th></tr>
    </thead>
    <tbody>
      ${audioTracks
        .map(
          (track) => `
        <tr data-track-row="${track.number}">
          <td>
            <input
              type="checkbox"
              data-track="${track.number}"
              aria-label="select track ${track.number}"
              ${checked.has(track.number) ? "checked" : ""}
              ${active ? "disabled" : ""}
            >
          </td>
          <td>${track.number}</td>
          <td>${escapeHtml(track.kind)}</td>
          <td>${escapeHtml(track.startMsf || "")}</td>
          <td>${duration(track.durationSecs)}</td>
        </tr>`
        )
        .join("")}
    </tbody>
  </table>`;
}

function updateTocSlot(record, drive, active) {
  const slot = record.card.querySelector(".toc-slot");
  const tracks = drive.toc ? drive.toc.tracks : [];
  const audioTracks = tracks.filter((track) => track.kind === "audio");
  const tocKey = audioTracks.map((track) => track.number).join(",");

  if (!drive.toc) {
    record.tocKey = null;
    slot.innerHTML = '<p class="muted">No disc loaded.</p>';
    return;
  }
  if (record.tocKey !== tocKey) {
    record.tocKey = tocKey;
    record.checked = new Set(audioTracks.map((track) => track.number));
    const shown = active ? new Set(drive.rip.tracks) : record.checked;
    slot.innerHTML = tocTable(audioTracks, shown, active);
    return;
  }
  const selected = active ? new Set(drive.rip.tracks) : record.checked;
  for (const track of audioTracks) {
    const row = slot.querySelector(`[data-track-row="${track.number}"]`);
    if (!row) continue;
    const checkbox = row.querySelector('input[data-track]');
    checkbox.checked = selected.has(track.number);
    checkbox.disabled = active;
  }
}

function updateMetaSlot(record, drive) {
  // Rebuilding here re-assigns the cover's data URL, making the browser
  // re-decode it; skip the rebuild when none of the inputs changed.
  const metaKey = [
    drive.lookupStatus,
    drive.mcn || "",
    drive.registerUrl || "",
    drive.coverFetching,
  ].join("\u0000");
  if (metaKey === record.metaKey && drive.cover === record.metaCover) return;
  record.metaKey = metaKey;
  record.metaCover = drive.cover;
  record.card.querySelector(".meta-slot").innerHTML = `
    <div class="meta">
      <div>
        ${escapeHtml(drive.lookupStatus)}
        ${drive.coverFetching ? ' <span class="warn">(fetching cover)</span>' : ""}
      </div>
      ${drive.mcn ? `<div>MCN: ${escapeHtml(drive.mcn)}</div>` : ""}
      ${
        drive.registerUrl
          ? `<div><a href="${escapeHtml(drive.registerUrl)}" target="_blank" rel="noopener">register on MusicBrainz</a></div>`
          : ""
      }
      <div class="cover">
        <img src="${escapeHtml(drive.cover || "")}" alt="" ${
    drive.cover ? "" : "hidden"
  }>
      </div>
    </div>`;
}

function updateMatchesSlot(card, drive, active) {
  const slot = card.querySelector(".matches-slot");
  if (!drive.matches.length) {
    slot.innerHTML = "";
    return;
  }
  slot.innerHTML = `<div class="matches">
    ${drive.matches
      .map(
        (match, index) => `
      <button
        class="match ${index === drive.matchSel ? "selected" : ""}"
        data-match="${index}"
        ${active ? "disabled" : ""}
      >
        ${escapeHtml(match.artist)} — ${escapeHtml(match.album)}
        ${match.year ? ` (${escapeHtml(match.year)})` : ""}
        ${match.discPosition ? ` disc ${escapeHtml(match.discPosition)}` : ""}
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
      slot.innerHTML = '<p class="muted">No metadata.</p>';
      record.formKey = "none";
    }
    return;
  }
  const formKey = `meta:${drive.matchSel}:${meta.tracks.length}`;
  if (record.formKey !== formKey) {
    slot.innerHTML = `<form class="tags" data-tags="${drive.index}">
      <h3>Tags</h3>
      <label>Artist <input name="artist" value="${escapeHtml(meta.artist)}"></label>
      <label>Album <input name="album" value="${escapeHtml(meta.album)}"></label>
      <label>Album artist <input name="albumArtist" value="${escapeHtml(
        meta.albumArtist || ""
      )}"></label>
      <label>Year <input name="year" value="${escapeHtml(meta.year || "")}"></label>
      <table>
        <thead>
          <tr><th>#</th><th>Title</th><th>Artist</th></tr>
        </thead>
        <tbody>
          ${meta.tracks
            .map(
              (track, index) => `
            <tr>
              <td>${index + 1}</td>
              <td><input name="title-${index}" value="${escapeHtml(track.title)}"></td>
              <td><input name="artist-${index}" value="${escapeHtml(
                track.artist || ""
              )}"></td>
            </tr>`
            )
            .join("")}
        </tbody>
      </table>
      <button type="submit">Save tags</button>
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
        <h3>Rip</h3>
        <div class="rip-meta"></div>
        <table>
          <thead>
            <tr><th>#</th><th>state</th><th>progress</th></tr>
          </thead>
          <tbody>
            ${rip.states
              .map(
                (trackState) => `
              <tr data-state-number="${trackState.number}">
                <td>${trackState.number}</td>
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
  for (const trackState of rip.states) {
    const row = slot.querySelector(`[data-state-number="${trackState.number}"]`);
    if (!row) continue;
    row.querySelector(".state").textContent = trackState.status;
    row.querySelector(".prog").textContent = progress(trackState);
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
  card.querySelector(".drive-status").textContent = [
    drive.status,
    drive.disc ? ` / ${drive.disc}` : "",
    drive.loadingToc ? " / reading TOC" : "",
  ]
    .filter(Boolean)
    .join("");
  card.querySelector(".drive-path").textContent = `${drive.path}${
    drive.demo ? " (demo)" : ""
  }`;
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
    const checkbox = event.target.closest('input[data-track]');
    if (!checkbox) return;
    const card = checkbox.closest(".drive");
    const record = cards.get(Number(card.dataset.index));
    if (!record || !record.checked) return;
    const number = Number(checkbox.dataset.track);
    if (checkbox.checked) record.checked.add(number);
    else record.checked.delete(number);
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

function connect() {
  const source = new EventSource("/api/events");
  source.onopen = () => {
    refresh().catch((error) => addLog(error.message));
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

const STATUS_LABELS = {
  planning: "规划中",
  in_progress: "进行中",
  blocked: "阻塞中",
  review: "待评审",
  paused: "已暂停",
  done: "已完成",
  cancelled: "已取消",
};

const CATEGORY_LABELS = {
  requirement: "需求开发",
  cr: "代码评审",
  oncall: "运维值班",
  analysis: "数据统计",
  unclassified: "待分类",
};

const state = {
  view: "overview",
  boardMode: "kanban",
  snapshot: null,
  csrfToken: "",
  filters: {
    status: "",
    category: "",
    priority: "",
    stale: false,
    blocked: false,
    active: false,
    q: "",
  },
  filteredTasks: [],
  selectedTaskIndex: -1,
};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

async function fetchJson(url, opts) {
  const resp = await fetch(url, opts);
  const data = await resp.json();
  if (!resp.ok) {
    throw new Error(data.error || `请求失败 (${resp.status})`);
  }
  return data;
}

function applyTaskFilters(tasks) {
  const f = state.filters;
  const activeSet = new Set(["planning", "in_progress", "blocked", "review", "paused"]);
  const q = f.q.trim().toLowerCase();
  return tasks.filter((t) => {
    if (f.status && t.status !== f.status) return false;
    if (f.category && t.category !== f.category) return false;
    if (f.priority && t.priority !== f.priority) return false;
    if (f.stale && !t.is_stale_7d) return false;
    if (f.blocked && !(t.status === "blocked" || (t.blockers && t.blockers.length > 0))) return false;
    if (f.active && !activeSet.has(t.status)) return false;
    if (q) {
      const hay = [t.id, t.title, t.owner, t.goal, ...(t.components || [])].join(" ").toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

function buildFilterQueryString() {
  const p = new URLSearchParams();
  const f = state.filters;
  if (f.status) p.set("status", f.status);
  if (f.category) p.set("category", f.category);
  if (f.priority) p.set("priority", f.priority);
  if (f.stale) p.set("stale", "1");
  if (f.blocked) p.set("blocked", "1");
  if (f.active) p.set("active", "1");
  if (f.q.trim()) p.set("q", f.q.trim());
  return p.toString();
}

function switchView(viewName, nextFilters) {
  state.view = viewName;
  if (nextFilters) {
    state.filters = {
      status: "",
      category: "",
      priority: "",
      stale: false,
      blocked: false,
      active: false,
      q: "",
      ...nextFilters,
    };
  }
  document.querySelectorAll("#nav-tabs button").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.view === viewName);
  });
  render();
}

function renderOverview(root, snap) {
  const ov = snap.overview || {};
  const cardsGrid = el("div", "grid-4");

  const metrics = [
    { label: "活跃任务", val: ov.active || 0, filter: { active: true } },
    { label: "阻塞中", val: ov.blocked || 0, filter: { blocked: true } },
    { label: "待评审", val: ov.review || 0, filter: { status: "review" } },
    { label: "满7天未更新待跟进", val: ov.stale_7d || 0, filter: { stale: true } },
  ];

  for (const m of metrics) {
    const card = el("div", "card metric-card");
    card.appendChild(el("div", "muted", m.label));
    card.appendChild(el("div", "metric-num", m.val));
    card.appendChild(el("div", "muted", "点击筛选任务列表 →"));
    card.addEventListener("click", () => switchView("board", m.filter));
    cardsGrid.appendChild(card);
  }
  root.appendChild(cardsGrid);

  const distGrid = el("div", "grid-2");

  const catCard = el("div", "card");
  catCard.appendChild(el("h3", "", "任务主类分布"));
  const catEntries = Object.entries(ov.by_category || {});
  for (const [k, v] of catEntries) {
    const row = el("div", "");
    const btn = el("button", "", `${CATEGORY_LABELS[k] || k} (${k}): ${v}`);
    btn.style.margin = "4px 6px 4px 0";
    btn.addEventListener("click", () => switchView("board", { category: k }));
    row.appendChild(btn);
    catCard.appendChild(row);
  }
  distGrid.appendChild(catCard);

  const stCard = el("div", "card");
  stCard.appendChild(el("h3", "", "任务状态分布"));
  for (const [k, v] of Object.entries(ov.by_status || {})) {
    const btn = el("button", "", `${STATUS_LABELS[k] || k} (${k}): ${v}`);
    btn.style.margin = "4px 6px 4px 0";
    btn.addEventListener("click", () => switchView("board", { status: k }));
    stCard.appendChild(btn);
  }
  distGrid.appendChild(stCard);
  root.appendChild(distGrid);

  const issues = snap.data_issues || [];
  const issueCard = el("div", "card");
  issueCard.appendChild(el("h3", "", `数据问题清单 (${issues.length})`));
  if (issues.length === 0) {
    issueCard.appendChild(el("div", "muted", "当前采集未发现损坏或超限文件。"));
  } else {
    for (const iss of issues) {
      const box = el("div", "issue-box");
      box.appendChild(el("strong", "", `[${iss.code}] ${iss.file || iss.task_id || ""}`));
      box.appendChild(el("div", "", iss.message));
      issueCard.appendChild(box);
    }
  }
  root.appendChild(issueCard);
}

function renderBoard(root, snap) {
  const tasks = snap.tasks || [];
  state.filteredTasks = applyTaskFilters(tasks);

  const bar = el("div", "toolbar");
  const modeBtn = el("button", "", state.boardMode === "kanban" ? "切换表格视图" : "切换看板视图");
  modeBtn.addEventListener("click", () => {
    state.boardMode = state.boardMode === "kanban" ? "table" : "kanban";
    render();
  });
  bar.appendChild(modeBtn);

  const stSelect = el("select");
  stSelect.appendChild(el("option", "", "全部状态")).value = "";
  for (const [k, label] of Object.entries(STATUS_LABELS)) {
    const opt = el("option", "", `${label} (${k})`);
    opt.value = k;
    if (state.filters.status === k) opt.selected = true;
    stSelect.appendChild(opt);
  }
  stSelect.addEventListener("change", () => {
    state.filters.status = stSelect.value;
    render();
  });
  bar.appendChild(stSelect);

  const catSelect = el("select");
  catSelect.appendChild(el("option", "", "全部主类")).value = "";
  for (const [k, label] of Object.entries(CATEGORY_LABELS)) {
    const opt = el("option", "", `${label} (${k})`);
    opt.value = k;
    if (state.filters.category === k) opt.selected = true;
    catSelect.appendChild(opt);
  }
  catSelect.addEventListener("change", () => {
    state.filters.category = catSelect.value;
    render();
  });
  bar.appendChild(catSelect);

  const prioSelect = el("select");
  prioSelect.appendChild(el("option", "", "全部优先级")).value = "";
  for (const p of ["P0", "P1", "P2", "P3"]) {
    const opt = el("option", "", p);
    opt.value = p;
    if (state.filters.priority === p) opt.selected = true;
    prioSelect.appendChild(opt);
  }
  prioSelect.addEventListener("change", () => {
    state.filters.priority = prioSelect.value;
    render();
  });
  bar.appendChild(prioSelect);

  const staleBtn = el("button", state.filters.stale ? "btn-primary" : "", "仅7天未更新");
  staleBtn.addEventListener("click", () => {
    state.filters.stale = !state.filters.stale;
    render();
  });
  bar.appendChild(staleBtn);

  const qInput = el("input");
  qInput.type = "search";
  qInput.placeholder = "搜索 ID / 标题 / 负责人 / 标签";
  qInput.value = state.filters.q;
  qInput.addEventListener("input", () => {
    state.filters.q = qInput.value;
    render();
  });
  bar.appendChild(qInput);

  const resetBtn = el("button", "", "重置筛选");
  resetBtn.addEventListener("click", () => switchView("board", {}));
  bar.appendChild(resetBtn);

  const csvBtn = el("button", "", "导出当前筛选 CSV");
  csvBtn.addEventListener("click", () => {
    const qs = buildFilterQueryString();
    window.location.href = `/api/export.csv${qs ? "?" + qs : ""}`;
  });
  bar.appendChild(csvBtn);

  root.appendChild(bar);

  if (state.boardMode === "kanban") {
    const kanban = el("div", "kanban");
    const cols = ["planning", "in_progress", "blocked", "review", "paused", "done"];
    for (const colStatus of cols) {
      const colTasks = state.filteredTasks.filter((t) => t.status === colStatus);
      const col = el("div", "kanban-col");
      const h = el("h3");
      h.appendChild(el("span", "", STATUS_LABELS[colStatus] || colStatus));
      h.appendChild(el("span", "muted", String(colTasks.length)));
      col.appendChild(h);

      for (const t of colTasks) {
        const card = el("div", "task-card");
        card.appendChild(el("strong", "", `${t.id} · ${t.title}`));
        const meta = el("div", "muted", `${t.priority} | ${CATEGORY_LABELS[t.category] || t.category} | ${t.owner || "-"}`);
        card.appendChild(meta);
        if (t.is_stale_7d) {
          card.appendChild(el("span", "pill pill-warn", `停滞 ${t.stale_days} 天`));
        }
        if (t.blockers && t.blockers.length > 0) {
          card.appendChild(el("span", "pill pill-danger", `阻塞 ${t.blockers.length}`));
        }
        card.addEventListener("click", () => openTaskDrawerById(t.id));
        col.appendChild(card);
      }
      kanban.appendChild(col);
    }
    root.appendChild(kanban);
  } else {
    const table = el("table");
    const thead = el("thead");
    const hr = el("tr");
    for (const colName of ["ID", "标题", "状态", "优先级", "主类", "负责人", "停滞天数", "更新时间"]) {
      hr.appendChild(el("th", "", colName));
    }
    thead.appendChild(hr);
    table.appendChild(thead);

    const tbody = el("tbody");
    for (const t of state.filteredTasks) {
      const tr = el("tr");
      tr.style.cursor = "pointer";
      tr.appendChild(el("td", "", t.id));
      tr.appendChild(el("td", "", t.title));
      tr.appendChild(el("td", "", STATUS_LABELS[t.status] || t.status));
      tr.appendChild(el("td", "", t.priority));
      tr.appendChild(el("td", "", CATEGORY_LABELS[t.category] || t.category));
      tr.appendChild(el("td", "", t.owner || "-"));
      tr.appendChild(el("td", "", String(t.stale_days || 0)));
      tr.appendChild(el("td", "", t.updated_at || "-"));
      tr.addEventListener("click", () => openTaskDrawerById(t.id));
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    root.appendChild(table);
  }
}

function renderSessions(root, snap) {
  const card = el("div", "card");
  const sessions = snap.sessions || [];
  card.appendChild(el("h3", "", `会话目录（按 session_id + host 去重，共 ${sessions.length} 条）`));

  const table = el("table");
  const thead = el("thead");
  const hr = el("tr");
  for (const h of ["会话标识", "主机", "分类", "标题", "关联任务", "操作"]) {
    hr.appendChild(el("th", "", h));
  }
  thead.appendChild(hr);
  table.appendChild(thead);

  const tbody = el("tbody");
  for (const s of sessions) {
    const tr = el("tr");
    tr.appendChild(el("td", "", s.session_id));
    tr.appendChild(el("td", "", s.host));
    tr.appendChild(el("td", "", `${CATEGORY_LABELS[s.category] || s.category} (${s.category})`));
    tr.appendChild(el("td", "", s.title));
    tr.appendChild(el("td", "", (s.task_ids || []).join(", ") || "-"));
    const tdAct = el("td");
    const copyBtn = el("button", "", "复制会话引用");
    copyBtn.addEventListener("click", async () => {
      const refText = `${s.session_id}@${s.host} ${s.title}`;
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(refText);
      }
      copyBtn.textContent = "已复制";
      setTimeout(() => {
        copyBtn.textContent = "复制会话引用";
      }, 1500);
    });
    tdAct.appendChild(copyBtn);
    tr.appendChild(tdAct);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  card.appendChild(table);
  root.appendChild(card);
}

function renderRepos(root, snap) {
  const card = el("div", "card");
  const repos = snap.repos || [];
  card.appendChild(el("h3", "", `仓库与评审汇总（共 ${repos.length} 项）`));

  const table = el("table");
  const thead = el("thead");
  const hr = el("tr");
  for (const h of ["任务", "交付项", "仓库 / Worktree", "声明分支", "实际分支", "未保存文件", "评审链接"]) {
    hr.appendChild(el("th", "", h));
  }
  thead.appendChild(hr);
  table.appendChild(thead);

  const tbody = el("tbody");
  for (const r of repos) {
    const tr = el("tr");
    tr.appendChild(el("td", "", `${r.task_id} (${r.task_status})`));
    tr.appendChild(el("td", "", r.item_id));
    tr.appendChild(el("td", "", r.worktree ? `${r.repo} (${r.worktree})` : r.repo));
    tr.appendChild(el("td", "", `${r.source_branch} -> ${r.target_branch}`));
    const actualTd = el("td");
    actualTd.textContent = r.actual_branch || "未同步";
    if (r.branch_mismatch) {
      actualTd.appendChild(el("span", "pill pill-danger", "分支不一致"));
    }
    tr.appendChild(actualTd);
    tr.appendChild(el("td", "", String(r.dirty_file_count || 0)));
    const linksTd = el("td");
    if (r.links && r.links.length > 0) {
      for (const link of r.links) {
        linksTd.appendChild(el("div", "", link));
      }
    } else {
      linksTd.textContent = "-";
    }
    tr.appendChild(linksTd);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  card.appendChild(table);
  root.appendChild(card);
}

function renderTimeline(root, snap) {
  const entries = snap.timeline || [];
  const grouped = new Map();
  for (const item of entries) {
    const key = item.date || "未知日期";
    if (!grouped.has(key)) grouped.set(key, []);
    grouped.get(key).push(item);
  }

  if (grouped.size === 0) {
    const empty = el("div", "card", "暂无跨任务进展动态。");
    root.appendChild(empty);
    return;
  }

  for (const [dateKey, list] of grouped.entries()) {
    const card = el("div", "card");
    card.style.marginBottom = "12px";
    card.appendChild(el("h3", "", `${dateKey}（${list.length} 条进展）`));
    for (const item of list) {
      const row = el("div", "");
      row.style.padding = "6px 0";
      row.style.borderBottom = "1px solid #eef2f8";
      row.appendChild(el("strong", "", `[${item.task_id}] ${item.task_title} `));
      row.appendChild(el("span", "muted", `${item.timestamp} [${item.status}]`));
      row.appendChild(el("div", "", item.summary));
      card.appendChild(row);
    }
    root.appendChild(card);
  }
}

function openTaskDrawerById(taskId) {
  const idx = state.filteredTasks.findIndex((t) => t.id === taskId);
  if (idx >= 0) {
    state.selectedTaskIndex = idx;
    renderDrawer(state.filteredTasks[idx]);
  }
}

function renderDrawer(task) {
  const backdrop = document.getElementById("drawer-backdrop");
  const titleEl = document.getElementById("drawer-title");
  const bodyEl = document.getElementById("drawer-body");
  titleEl.textContent = `${task.id} · ${task.title}`;
  clear(bodyEl);

  bodyEl.appendChild(el("div", "muted", `状态: ${task.status} | 优先级: ${task.priority} | 主类: ${task.category} | 负责人: ${task.owner || "-"}`));
  bodyEl.appendChild(el("div", "section-title", "目标"));
  bodyEl.appendChild(el("div", "", task.goal || "（未填写）"));

  bodyEl.appendChild(el("div", "section-title", "阻塞项"));
  if (task.blockers && task.blockers.length > 0) {
    for (const b of task.blockers) bodyEl.appendChild(el("div", "pill pill-danger", b));
  } else {
    bodyEl.appendChild(el("div", "muted", "无阻塞项"));
  }

  bodyEl.appendChild(el("div", "section-title", "下一步动作"));
  if (task.next_steps && task.next_steps.length > 0) {
    task.next_steps.forEach((s, i) => bodyEl.appendChild(el("div", "", `${i + 1}. ${s}`)));
  } else {
    bodyEl.appendChild(el("div", "muted", "暂无"));
  }

  bodyEl.appendChild(el("div", "section-title", "动作确认与证据状态"));
  if (task.actions_status && task.actions_status.length > 0) {
    for (const act of task.actions_status) {
      const box = el("div", "");
      box.style.marginBottom = "6px";
      const cls = act.valid ? "pill pill-ok" : "pill pill-warn";
      box.appendChild(el("span", cls, act.valid ? "VALID" : act.status.toUpperCase()));
      box.appendChild(el("span", "", `${act.id} (${act.kind}): ${act.reason}`));
      bodyEl.appendChild(box);
    }
  } else {
    bodyEl.appendChild(el("div", "muted", "无交付动作"));
  }

  bodyEl.appendChild(el("div", "section-title", "已登记 Markdown 文档（只读安全预览）"));
  const docs = [...(task.docs || []), ...(task.materials || [])].filter((p) => String(p).toLowerCase().endsWith(".md"));
  if (docs.length > 0) {
    for (const rel of docs) {
      const btn = el("button", "", `查看 ${rel}`);
      btn.style.margin = "4px 6px 4px 0";
      btn.addEventListener("click", () => openDocModal(task.id, rel));
      bodyEl.appendChild(btn);
    }
  } else {
    bodyEl.appendChild(el("div", "muted", "无已登记的 Markdown 文档"));
  }

  document.getElementById("btn-prev-task").disabled = state.selectedTaskIndex <= 0;
  document.getElementById("btn-next-task").disabled = state.selectedTaskIndex >= state.filteredTasks.length - 1;
  backdrop.classList.remove("hidden");
}

async function openDocModal(taskId, relPath) {
  const modalBackdrop = document.getElementById("doc-modal-backdrop");
  const titleEl = document.getElementById("doc-modal-title");
  const contentEl = document.getElementById("doc-modal-content");
  titleEl.textContent = `${relPath}（纯文本安全视图）`;
  contentEl.textContent = "加载中...";
  modalBackdrop.classList.remove("hidden");
  try {
    const qs = new URLSearchParams({ task_id: taskId, path: relPath });
    const data = await fetchJson(`/api/doc?${qs.toString()}`);
    contentEl.textContent = data.content || "";
  } catch (err) {
    contentEl.textContent = `加载失败: ${err.message}`;
  }
}

function render() {
  const root = document.getElementById("app-root");
  clear(root);
  const snap = state.snapshot;
  if (!snap) {
    root.appendChild(el("div", "card", "正在加载工作台数据快照..."));
    return;
  }

  document.getElementById("ws-name").textContent = snap.workspace?.name || "个人工作台";
  document.getElementById("ws-meta").textContent = `快照时间: ${snap.generated_at || "-"}`;

  if (state.view === "overview") renderOverview(root, snap);
  else if (state.view === "board") renderBoard(root, snap);
  else if (state.view === "sessions") renderSessions(root, snap);
  else if (state.view === "repos") renderRepos(root, snap);
  else if (state.view === "timeline") renderTimeline(root, snap);
}

async function bootstrapAndLoad() {
  try {
    const boot = await fetchJson("/api/bootstrap");
    state.csrfToken = boot.csrf_token || "";
    state.snapshot = await fetchJson("/api/snapshot");
    render();
  } catch (err) {
    const root = document.getElementById("app-root");
    clear(root);
    root.appendChild(el("div", "card", `加载看板失败: ${err.message}`));
  }
}

document.getElementById("nav-tabs").addEventListener("click", (ev) => {
  const btn = ev.target.closest("button[data-view]");
  if (btn) switchView(btn.dataset.view);
});

document.getElementById("btn-refresh").addEventListener("click", async () => {
  const statusEl = document.getElementById("refresh-status");
  statusEl.textContent = "刷新中...";
  try {
    const res = await fetchJson("/api/refresh", {
      method: "POST",
      headers: { "X-WB-CSRF-Token": state.csrfToken },
    });
    state.snapshot = await fetchJson("/api/snapshot");
    statusEl.textContent = res.updated ? "已更新" : `已保留旧快照: ${res.aborted_reason || ""}`;
    render();
  } catch (err) {
    statusEl.textContent = `刷新失败: ${err.message}`;
  }
});

document.getElementById("btn-close-drawer").addEventListener("click", () => {
  document.getElementById("drawer-backdrop").classList.add("hidden");
});

document.getElementById("btn-prev-task").addEventListener("click", () => {
  if (state.selectedTaskIndex > 0) {
    state.selectedTaskIndex -= 1;
    renderDrawer(state.filteredTasks[state.selectedTaskIndex]);
  }
});

document.getElementById("btn-next-task").addEventListener("click", () => {
  if (state.selectedTaskIndex < state.filteredTasks.length - 1) {
    state.selectedTaskIndex += 1;
    renderDrawer(state.filteredTasks[state.selectedTaskIndex]);
  }
});

document.getElementById("btn-close-doc").addEventListener("click", () => {
  document.getElementById("doc-modal-backdrop").classList.add("hidden");
});

bootstrapAndLoad();

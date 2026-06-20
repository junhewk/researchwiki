function selectedWorkspaceId() {
  return document.body?.dataset?.workspaceId || '1';
}

function confirmAction(event) {
  const message = event.currentTarget?.dataset?.confirm;
  if (message && !window.confirm(message)) {
    event.preventDefault();
  }
}

for (const form of document.querySelectorAll('form[data-confirm]')) {
  form.addEventListener('submit', confirmAction);
}

async function loadStatusBlocks() {
  for (const el of document.querySelectorAll('[data-status-url]')) {
    const url = el.dataset.statusUrl;
    if (!url) continue;
    try {
      const response = await fetch(url);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      el.textContent = JSON.stringify(data, null, 2);
    } catch (error) {
      el.textContent = error instanceof Error ? error.message : 'Status load failed';
    }
  }
}

if (document.querySelector('[data-status-url]')) {
  loadStatusBlocks();
  window.setInterval(loadStatusBlocks, 5000);
}

async function initGraph() {
  const canvas = document.getElementById('graph-canvas');
  if (!(canvas instanceof HTMLCanvasElement)) return;

  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  const limitInput = document.getElementById('graph-limit');
  const minDegreeInput = document.getElementById('graph-min-degree');
  const typeInput = document.getElementById('graph-types');
  const loadButton = document.getElementById('graph-load');
  const detail = document.getElementById('graph-detail');
  let nodes = [];
  let edges = [];
  let hovered = null;
  const maxLabels = 24;

  function fitCanvas() {
    const rect = canvas.getBoundingClientRect();
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(300, Math.floor(rect.width * ratio));
    canvas.height = Math.max(300, Math.floor(rect.height * ratio));
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  }

  function graphPosition(node, rect) {
    const scale = Math.min(rect.width, rect.height) * 0.44;
    const cx = rect.width / 2;
    const cy = rect.height / 2;
    const x = Number(node.properties?.x ?? 0);
    const y = Number(node.properties?.y ?? 0);
    return {
      x: cx + (Number.isFinite(x) ? x : 0) * scale,
      y: cy + (Number.isFinite(y) ? y : 0) * scale,
    };
  }

  function nodeRadius(node) {
    const radius = Number(node.properties?.radius);
    if (Number.isFinite(radius)) return Math.max(5, Math.min(18, radius));
    const degree = Number(node.properties?.degree || 0);
    return Math.max(5, Math.min(16, 5 + degree / 2));
  }

  function nodeColor(node) {
    const color = String(node.properties?.color || '');
    return /^#[0-9a-f]{6}$/i.test(color) ? color : '#64748b';
  }

  function shouldLabel(node) {
    const priority = Number(node.properties?.label_priority);
    return hovered?.id === node.id || (Number.isFinite(priority) && priority < maxLabels);
  }

  function edgeWidth(edge) {
    const weight = Number(edge.properties?.weight || 1);
    return Math.max(0.8, Math.min(2.4, Math.sqrt(Math.max(0.1, weight))));
  }

  function labelText(node) {
    return node.properties?.name || node.id;
  }

  function labelColor() {
    return getComputedStyle(document.documentElement).getPropertyValue('--text').trim() || '#17202a';
  }

  function draw() {
    const rect = canvas.getBoundingClientRect();
    fitCanvas();
    ctx.clearRect(0, 0, rect.width, rect.height);
    const positions = new Map(nodes.map((node) => [node.id, graphPosition(node, rect)]));

    ctx.strokeStyle = '#cbd5e1';
    for (const edge of edges) {
      const source = positions.get(edge.source);
      const target = positions.get(edge.target);
      if (!source || !target) continue;
      ctx.globalAlpha = hovered && edge.source !== hovered.id && edge.target !== hovered.id ? 0.12 : 0.45;
      ctx.lineWidth = edgeWidth(edge);
      ctx.beginPath();
      ctx.moveTo(source.x, source.y);
      ctx.lineTo(target.x, target.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;

    for (const node of nodes) {
      const pos = positions.get(node.id);
      if (!pos) continue;
      const r = nodeRadius(node);
      const isHover = hovered?.id === node.id;
      ctx.beginPath();
      ctx.arc(pos.x, pos.y, isHover ? r + 2 : r, 0, Math.PI * 2);
      ctx.fillStyle = nodeColor(node);
      ctx.fill();
      ctx.lineWidth = isHover ? 2 : 1;
      ctx.strokeStyle = isHover ? '#111827' : '#475569';
      ctx.stroke();
      if (shouldLabel(node)) {
        ctx.fillStyle = labelColor();
        ctx.font = '12px system-ui';
        ctx.fillText(labelText(node), pos.x + r + 6, pos.y - r);
      }
    }
  }

  function updateDetail(node) {
    if (!detail) return;
    if (!node) {
      detail.innerHTML = '<p class="muted">Hover or click a node to inspect it.</p>';
      return;
    }
    const type = node.properties?.entity_type || 'UNKNOWN';
    const description = node.properties?.description || '';
    const mentions = node.properties?.mention_count || 0;
    const wikiHref = `/wiki?workspace_id=${encodeURIComponent(selectedWorkspaceId())}&entity=${encodeURIComponent(node.id)}`;
    detail.innerHTML = `
      <h2>${escapeHtml(node.id)}</h2>
      <p><span class="pill">${escapeHtml(type)}</span></p>
      <p class="muted">${escapeHtml(description || 'No description')}</p>
      <p>${mentions} mentions</p>
      <p><a class="btn" href="${wikiHref}">Open wiki concept</a></p>
    `;
  }

  function nearestNode(event) {
    const rect = canvas.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const y = event.clientY - rect.top;
    let best = null;
    let bestDist = 999999;
    for (const node of nodes) {
      const pos = graphPosition(node, rect);
      const dx = pos.x - x;
      const dy = pos.y - y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < bestDist && dist < nodeRadius(node) + 8) {
        best = node;
        bestDist = dist;
      }
    }
    return best;
  }

  canvas.addEventListener('mousemove', (event) => {
    hovered = nearestNode(event);
    updateDetail(hovered);
    draw();
  });

  canvas.addEventListener('click', () => {
    if (hovered) {
      const href = `/wiki?workspace_id=${encodeURIComponent(selectedWorkspaceId())}&entity=${encodeURIComponent(hovered.id)}`;
      window.location.href = href;
    }
  });

  async function loadGraph() {
    if (loadButton) loadButton.textContent = 'Loading...';
    const params = new URLSearchParams();
    params.set('workspace_id', selectedWorkspaceId());
    params.set('limit', limitInput?.value || '200');
    params.set('min_degree', minDegreeInput?.value || '0');
    if (typeInput?.value) params.set('entity_types', typeInput.value);
    const response = await fetch(`/api/graph-data?${params.toString()}`);
    const data = await response.json();
    nodes = data.nodes || [];
    edges = data.edges || [];
    draw();
    if (loadButton) loadButton.textContent = 'Load graph';
  }

  loadButton?.addEventListener('click', loadGraph);
  window.addEventListener('resize', () => {
    draw();
  });
  updateDetail(null);
  loadGraph();
}

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

initGraph();

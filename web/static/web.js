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

  function fitCanvas() {
    const rect = canvas.getBoundingClientRect();
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(300, Math.floor(rect.width * ratio));
    canvas.height = Math.max(300, Math.floor(rect.height * ratio));
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  }

  function layout() {
    const rect = canvas.getBoundingClientRect();
    const cx = rect.width / 2;
    const cy = rect.height / 2;
    const radius = Math.max(80, Math.min(rect.width, rect.height) * 0.38);
    nodes = nodes.map((node, index) => {
      const angle = (Math.PI * 2 * index) / Math.max(nodes.length, 1);
      const degree = Number(node.properties?.degree || 0);
      const ring = radius * (0.55 + Math.min(degree, 20) / 50);
      return { ...node, x: cx + Math.cos(angle) * ring, y: cy + Math.sin(angle) * ring };
    });
  }

  function draw() {
    fitCanvas();
    const rect = canvas.getBoundingClientRect();
    ctx.clearRect(0, 0, rect.width, rect.height);
    const byId = new Map(nodes.map((n) => [n.id, n]));

    ctx.lineWidth = 1;
    ctx.strokeStyle = '#cbd5e1';
    for (const edge of edges) {
      const source = byId.get(edge.source);
      const target = byId.get(edge.target);
      if (!source || !target) continue;
      ctx.globalAlpha = hovered && edge.source !== hovered.id && edge.target !== hovered.id ? 0.12 : 0.45;
      ctx.beginPath();
      ctx.moveTo(source.x, source.y);
      ctx.lineTo(target.x, target.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;

    for (const node of nodes) {
      const degree = Number(node.properties?.degree || 0);
      const r = Math.max(5, Math.min(16, 5 + degree / 2));
      const isHover = hovered?.id === node.id;
      ctx.beginPath();
      ctx.arc(node.x, node.y, isHover ? r + 3 : r, 0, Math.PI * 2);
      ctx.fillStyle = isHover ? '#2563eb' : '#64748b';
      ctx.fill();
      if (isHover) {
        ctx.fillStyle = '#17202a';
        ctx.font = '12px system-ui';
        ctx.fillText(node.id, node.x + r + 6, node.y - r);
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
      const dx = node.x - x;
      const dy = node.y - y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < bestDist && dist < 24) {
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
    layout();
    draw();
    if (loadButton) loadButton.textContent = 'Load graph';
  }

  loadButton?.addEventListener('click', loadGraph);
  window.addEventListener('resize', () => {
    layout();
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

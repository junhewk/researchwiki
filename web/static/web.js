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
  const layoutInput = document.getElementById('graph-layout');
  const loadButton = document.getElementById('graph-load');
  const resetLayoutButton = document.getElementById('graph-reset-layout');
  const detail = document.getElementById('graph-detail');
  let nodes = [];
  let edges = [];
  let hovered = null;
  let selected = null;
  let drag = null;
  let detailRequestSeq = 0;
  const detailCache = new Map();
  const view = { zoom: 1, panX: 0, panY: 0 };
  const maxLabels = 24;
  const minZoom = 0.35;
  const maxZoom = 4;

  function finiteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function clamp(value, min, max) {
    return Math.min(max, Math.max(min, value));
  }

  function fitCanvas() {
    const rect = canvas.getBoundingClientRect();
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(300, Math.floor(rect.width * ratio));
    canvas.height = Math.max(300, Math.floor(rect.height * ratio));
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  }

  function baseScale(rect) {
    return Math.min(rect.width, rect.height) * 0.44;
  }

  function graphPosition(node, rect) {
    const scale = baseScale(rect) * view.zoom;
    const cx = rect.width / 2;
    const cy = rect.height / 2;
    return {
      x: cx + view.panX + finiteNumber(node.x) * scale,
      y: cy + view.panY + finiteNumber(node.y) * scale,
    };
  }

  function graphPointFromCanvas(point, rect = canvas.getBoundingClientRect()) {
    const scale = baseScale(rect) * view.zoom;
    return {
      x: (point.x - rect.width / 2 - view.panX) / scale,
      y: (point.y - rect.height / 2 - view.panY) / scale,
    };
  }

  function canvasPoint(event) {
    const rect = canvas.getBoundingClientRect();
    return {
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
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
    return selected?.id === node.id || hovered?.id === node.id || (Number.isFinite(priority) && priority < maxLabels);
  }

  function edgeWidth(edge) {
    const weight = Number(edge.properties?.weight || 1);
    return Math.max(0.8, Math.min(2.4, Math.sqrt(Math.max(0.1, weight))));
  }

  function labelText(node) {
    return node.properties?.name || node.id;
  }

  function wikiHref(entity) {
    return `/wiki?workspace_id=${encodeURIComponent(selectedWorkspaceId())}&entity=${encodeURIComponent(entity)}`;
  }

  function formatCount(value) {
    const count = Number(value || 0);
    return Number.isFinite(count) ? count.toLocaleString() : '0';
  }

  function nodeDegree(node) {
    return Number(node?.properties?.degree || 0);
  }

  function nodeMentions(node) {
    return Number(node?.properties?.mention_count || 0);
  }

  function nodeType(node) {
    return node?.properties?.entity_type || 'UNKNOWN';
  }

  function graphNodeById(entity) {
    return nodes.find((node) => node.id === entity || node.properties?.name === entity) || null;
  }

  function connectedNodeIds(node) {
    if (!node) return null;
    const ids = new Set([node.id]);
    for (const edge of edges) {
      if (edge.source === node.id) ids.add(edge.target);
      if (edge.target === node.id) ids.add(edge.source);
    }
    return ids;
  }

  function labelColor() {
    return getComputedStyle(document.documentElement).getPropertyValue('--text').trim() || '#17202a';
  }

  function draw() {
    const rect = canvas.getBoundingClientRect();
    fitCanvas();
    ctx.clearRect(0, 0, rect.width, rect.height);
    const positions = new Map(nodes.map((node) => [node.id, graphPosition(node, rect)]));
    const focus = selected || hovered;
    const connected = connectedNodeIds(focus);

    ctx.strokeStyle = '#cbd5e1';
    for (const edge of edges) {
      const source = positions.get(edge.source);
      const target = positions.get(edge.target);
      if (!source || !target) continue;
      const highlighted = focus && (edge.source === focus.id || edge.target === focus.id);
      ctx.globalAlpha = focus && !highlighted ? 0.12 : highlighted ? 0.72 : 0.45;
      ctx.lineWidth = highlighted ? edgeWidth(edge) + 0.8 : edgeWidth(edge);
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
      const isSelected = selected?.id === node.id;
      const dimmed = connected && !connected.has(node.id) && !isHover && !isSelected;
      ctx.globalAlpha = dimmed ? 0.28 : 1;
      ctx.beginPath();
      ctx.arc(pos.x, pos.y, isSelected ? r + 3 : isHover ? r + 2 : r, 0, Math.PI * 2);
      ctx.fillStyle = nodeColor(node);
      ctx.fill();
      ctx.lineWidth = isSelected ? 3 : isHover ? 2 : 1;
      ctx.strokeStyle = isSelected || isHover ? '#111827' : '#475569';
      ctx.stroke();
      if (shouldLabel(node)) {
        ctx.fillStyle = labelColor();
        ctx.font = '12px system-ui';
        ctx.fillText(labelText(node), pos.x + r + 6, pos.y - r);
      }
    }
    ctx.globalAlpha = 1;
  }

  function updateDetail(node, options = {}) {
    if (!detail) return;
    if (!node) {
      detail.innerHTML = '<p class="muted">Select a node.</p>';
      return;
    }
    const entity = options.entity;
    const error = options.error;
    const description = entity?.description || node.properties?.description || '';
    const aliases = Array.isArray(entity?.aliases) ? entity.aliases : [];
    const keyAspects = Array.isArray(entity?.synthesis_key_aspects) ? entity.synthesis_key_aspects.slice(0, 5) : [];
    const neighbors = Array.isArray(entity?.neighbors) ? entity.neighbors.slice(0, 8) : [];
    const summary = entity?.synthesis_summary || '';
    const type = entity?.entity_type || nodeType(node);
    const mentions = entity?.mention_count ?? nodeMentions(node);
    const degree = nodeDegree(node);
    const stale = entity?.synthesis_stale === true ? '<span class="pill">stale wiki</span>' : '';
    const aliasMarkup = aliases.length
      ? `<p class="muted">Aliases: ${aliases.map(escapeHtml).join(', ')}</p>`
      : '';
    const keyAspectMarkup = keyAspects.length
      ? `<h3>Key aspects</h3><ul class="graph-aspect-list">${keyAspects.map((aspect) => `<li>${escapeHtml(aspect)}</li>`).join('')}</ul>`
      : '';
    const neighborMarkup = neighbors.length
      ? `<h3>Relationships</h3><div class="graph-neighbor-list">${neighbors.map((neighbor) => {
          const inGraph = graphNodeById(neighbor.entity);
          const focusControl = inGraph
            ? `<button type="button" data-graph-focus="${escapeHtml(neighbor.entity)}">${escapeHtml(neighbor.entity)}</button>`
            : `<a href="${wikiHref(neighbor.entity)}">${escapeHtml(neighbor.entity)}</a>`;
          const evidence = neighbor.evidence_summary
            ? `<div class="graph-neighbor-meta">${escapeHtml(neighbor.evidence_summary)}</div>`
            : '';
          return `
            <div class="graph-neighbor-item">
              <div class="graph-neighbor-title">
                ${focusControl}
                <span class="pill">${escapeHtml(neighbor.entity_type || 'UNKNOWN')}</span>
              </div>
              <div class="graph-neighbor-meta">${escapeHtml(neighbor.relationship || 'related')} &middot; weight ${Number(neighbor.weight || 0).toFixed(1)}</div>
              ${evidence}
            </div>
          `;
        }).join('')}</div>`
      : '';
    const loading = options.loading ? '<p class="muted">Loading entity detail...</p>' : '';
    const errorMarkup = error ? `<p class="notice error">${escapeHtml(error)}</p>` : '';
    detail.innerHTML = `
      <h2>${escapeHtml(node.id)}</h2>
      <div class="graph-detail-actions">
        <span class="pill">${escapeHtml(type)}</span>
        ${stale}
      </div>
      <div class="graph-detail-metrics">
        <div class="graph-detail-metric"><span>Mentions</span><strong>${formatCount(mentions)}</strong></div>
        <div class="graph-detail-metric"><span>Degree</span><strong>${formatCount(degree)}</strong></div>
        <div class="graph-detail-metric"><span>Neighbors</span><strong>${formatCount(entity?.neighbors?.length || degree)}</strong></div>
      </div>
      <div class="graph-detail-actions">
        <a class="btn" href="${wikiHref(node.id)}">Open wiki concept</a>
        <button class="button" type="button" data-graph-center="${escapeHtml(node.id)}">Center node</button>
      </div>
      ${errorMarkup}
      ${loading}
      <p class="muted">${escapeHtml(description || 'No description')}</p>
      ${aliasMarkup}
      ${summary ? `<h3>Wiki summary</h3><p>${escapeHtml(summary)}</p>` : ''}
      ${keyAspectMarkup}
      ${neighborMarkup}
    `;
  }

  async function loadEntityDetail(node) {
    if (!node) return;
    const cached = detailCache.get(node.id);
    if (cached) {
      updateDetail(node, { entity: cached });
      return;
    }

    const seq = ++detailRequestSeq;
    updateDetail(node, { loading: true });
    try {
      const params = new URLSearchParams();
      params.set('workspace_id', selectedWorkspaceId());
      params.set('entity', node.id);
      const response = await fetch(`/api/entity?${params.toString()}`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const entity = await response.json();
      detailCache.set(node.id, entity);
      if (seq === detailRequestSeq && selected?.id === node.id) {
        updateDetail(node, { entity });
      }
    } catch (error) {
      if (seq === detailRequestSeq && selected?.id === node.id) {
        updateDetail(node, { error: error instanceof Error ? error.message : 'Entity load failed' });
      }
    }
  }

  function centerNode(node) {
    if (!node) return;
    const rect = canvas.getBoundingClientRect();
    const scale = baseScale(rect) * view.zoom;
    view.panX = -finiteNumber(node.x) * scale;
    view.panY = -finiteNumber(node.y) * scale;
    draw();
  }

  function selectNode(node, options = {}) {
    if (!node) return;
    selected = node;
    hovered = node;
    updateDetail(node, { entity: detailCache.get(node.id), loading: !detailCache.has(node.id) });
    if (options.center) centerNode(node);
    loadEntityDetail(node);
    draw();
  }

  function nearestNodeAt(point, rect = canvas.getBoundingClientRect()) {
    let best = null;
    let bestDist = 999999;
    for (const node of nodes) {
      const pos = graphPosition(node, rect);
      const dx = pos.x - point.x;
      const dy = pos.y - point.y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < bestDist && dist < nodeRadius(node) + 8) {
        best = node;
        bestDist = dist;
      }
    }
    return best;
  }

  function fitView() {
    const rect = canvas.getBoundingClientRect();
    if (!nodes.length || rect.width <= 0 || rect.height <= 0) {
      view.zoom = 1;
      view.panX = 0;
      view.panY = 0;
      return;
    }

    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    for (const node of nodes) {
      minX = Math.min(minX, finiteNumber(node.x));
      maxX = Math.max(maxX, finiteNumber(node.x));
      minY = Math.min(minY, finiteNumber(node.y));
      maxY = Math.max(maxY, finiteNumber(node.y));
    }

    const graphWidth = Math.max(0.1, maxX - minX);
    const graphHeight = Math.max(0.1, maxY - minY);
    const padding = 72;
    const scale = baseScale(rect);
    const fitZoom = Math.min(
      Math.max(0.1, rect.width - padding) / (graphWidth * scale),
      Math.max(0.1, rect.height - padding) / (graphHeight * scale),
    );

    view.zoom = clamp(fitZoom, minZoom, maxZoom);
    view.panX = -((minX + maxX) / 2) * scale * view.zoom;
    view.panY = -((minY + maxY) / 2) * scale * view.zoom;
  }

  function zoomAt(factor, point) {
    const rect = canvas.getBoundingClientRect();
    const oldZoom = view.zoom;
    const nextZoom = clamp(oldZoom * factor, minZoom, maxZoom);
    if (Math.abs(nextZoom - oldZoom) < 0.0001) return;

    const graphPoint = graphPointFromCanvas(point, rect);
    const scale = baseScale(rect);
    view.zoom = nextZoom;
    view.panX = point.x - rect.width / 2 - graphPoint.x * scale * view.zoom;
    view.panY = point.y - rect.height / 2 - graphPoint.y * scale * view.zoom;
    draw();
  }

  function setHoverFromEvent(event) {
    hovered = nearestNodeAt(canvasPoint(event));
    canvas.style.cursor = drag ? 'grabbing' : hovered ? 'grab' : 'move';
    if (!selected) {
      updateDetail(hovered);
    }
  }

  canvas.addEventListener('pointerdown', (event) => {
    if (event.button !== 0) return;
    const point = canvasPoint(event);
    const node = nearestNodeAt(point);
    canvas.setPointerCapture(event.pointerId);
    if (node) {
      const graphPoint = graphPointFromCanvas(point);
      drag = {
        type: 'node',
        pointerId: event.pointerId,
        node,
        startX: point.x,
        startY: point.y,
        offsetX: finiteNumber(node.x) - graphPoint.x,
        offsetY: finiteNumber(node.y) - graphPoint.y,
        moved: false,
      };
    } else {
      drag = {
        type: 'pan',
        pointerId: event.pointerId,
        startX: point.x,
        startY: point.y,
        originPanX: view.panX,
        originPanY: view.panY,
        moved: false,
      };
    }
    canvas.style.cursor = 'grabbing';
  });

  canvas.addEventListener('pointermove', (event) => {
    const point = canvasPoint(event);
    if (drag && drag.pointerId === event.pointerId) {
      const dx = point.x - drag.startX;
      const dy = point.y - drag.startY;
      if (Math.hypot(dx, dy) > 3) drag.moved = true;

      if (drag.type === 'node') {
        const graphPoint = graphPointFromCanvas(point);
        drag.node.x = graphPoint.x + drag.offsetX;
        drag.node.y = graphPoint.y + drag.offsetY;
        hovered = drag.node;
        if (!selected || selected.id === drag.node.id) updateDetail(drag.node);
      } else {
        view.panX = drag.originPanX + dx;
        view.panY = drag.originPanY + dy;
        hovered = nearestNodeAt(point);
      }
      draw();
      return;
    }

    setHoverFromEvent(event);
    draw();
  });

  canvas.addEventListener('pointerup', (event) => {
    if (!drag || drag.pointerId !== event.pointerId) return;
    const point = canvasPoint(event);
    if (canvas.hasPointerCapture(event.pointerId)) {
      canvas.releasePointerCapture(event.pointerId);
    }

    if (!drag.moved) {
      if (drag.type === 'node') {
        selectNode(drag.node);
      } else {
        selected = null;
        hovered = nearestNodeAt(point);
        updateDetail(hovered);
      }
    }
    drag = null;
    setHoverFromEvent(event);
    if (selected) updateDetail(selected);
    draw();
  });

  canvas.addEventListener('dblclick', (event) => {
    const node = nearestNodeAt(canvasPoint(event));
    if (node) {
      window.location.href = wikiHref(node.id);
    }
  });

  canvas.addEventListener('pointercancel', (event) => {
    if (drag?.pointerId === event.pointerId) {
      drag = null;
      canvas.style.cursor = hovered ? 'grab' : 'move';
    }
  });

  canvas.addEventListener('wheel', (event) => {
    event.preventDefault();
    const factor = event.deltaY < 0 ? 1.16 : 1 / 1.16;
    zoomAt(factor, canvasPoint(event));
  }, { passive: false });

  async function loadGraph() {
    if (loadButton) loadButton.textContent = 'Loading...';
    const params = new URLSearchParams();
    params.set('workspace_id', selectedWorkspaceId());
    params.set('limit', limitInput?.value || '200');
    params.set('min_degree', minDegreeInput?.value || '0');
    if (typeInput?.value) params.set('entity_types', typeInput.value);
    if (layoutInput?.value) params.set('layout', layoutInput.value);
    const response = await fetch(`/api/graph-data?${params.toString()}`);
    const data = await response.json();
    nodes = (data.nodes || []).map((node) => {
      const x = finiteNumber(node.properties?.x);
      const y = finiteNumber(node.properties?.y);
      return { ...node, x, y, homeX: x, homeY: y };
    });
    edges = data.edges || [];
    detailCache.clear();
    detailRequestSeq += 1;
    hovered = null;
    selected = null;
    drag = null;
    fitView();
    updateDetail(null);
    draw();
    if (loadButton) loadButton.textContent = 'Load graph';
  }

  loadButton?.addEventListener('click', loadGraph);
  layoutInput?.addEventListener('change', loadGraph);
  resetLayoutButton?.addEventListener('click', () => {
    for (const node of nodes) {
      node.x = node.homeX;
      node.y = node.homeY;
    }
    hovered = null;
    fitView();
    updateDetail(selected, { entity: selected ? detailCache.get(selected.id) : null });
    draw();
  });
  detail?.addEventListener('click', (event) => {
    const focus = event.target.closest('[data-graph-focus]');
    if (focus) {
      const node = graphNodeById(focus.getAttribute('data-graph-focus'));
      if (node) selectNode(node, { center: true });
      return;
    }
    const center = event.target.closest('[data-graph-center]');
    if (center) {
      const node = graphNodeById(center.getAttribute('data-graph-center'));
      if (node) centerNode(node);
    }
  });
  window.addEventListener('resize', () => {
    fitView();
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

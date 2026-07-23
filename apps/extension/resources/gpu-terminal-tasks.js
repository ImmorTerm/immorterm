/**
 * GPU Terminal — Tasks Panel.
 *
 * Self-contained, IDE-agnostic module: task rendering, lane management,
 * task modal (create/edit), hover actions, context menus, keyboard navigation,
 * drag with lane highlighting, and task board/archive modals.
 *
 * The host provides messages:
 *   - tasks-load       (host → webview, on init / change)
 *   - tasks-save       (webview → host, on any mutation)
 *   - drop-task-on-session  (webview → host, on DnD drop)
 *   - switch-to-task-session (webview → host, on paperclip click)
 *
 * Imported by gpu-terminal.html via dynamic import.
 */

// ── Constants ──────────────────────────────────────────────────

const TYPE_EMOJI = { bug: '\uD83D\uDC1B', feature: '\u2728', investigate: '\uD83D\uDD0D', other: '\uD83D\uDCCC' };
const TYPE_LABEL = { bug: 'Bug', feature: 'Feature', investigate: 'Investigate', other: 'Other' };
const LANES = ['now', 'next', 'later'];
const LANE_LABEL = { now: 'Now', next: 'Next', later: 'Later' };
const STATUS_ICON = { todo: '\u25CB', in_progress: '\u25D4', done: '\u2713' };
const STATUS_LABEL = { todo: 'Todo', in_progress: 'In Progress', done: 'Done' };

// ── Helpers ────────────────────────────────────────────────────

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function relativeTime(ts) {
  const diff = Date.now() - ts;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return mins + 'm ago';
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return hrs + 'h ago';
  const days = Math.floor(hrs / 24);
  if (days === 1) return 'yesterday';
  return days + 'd ago';
}

/**
 * Position a popover relative to an anchor, appended to document.body
 * to escape overflow clipping. Automatically flips below if near the top.
 * @param {HTMLElement} pop - popover element (not yet appended)
 * @param {HTMLElement | DOMRect | {left,top,right,bottom,width,height}} target
 *   - an element (uses its getBoundingClientRect) OR a rect-like object
 */
export function positionPopover(pop, target) {
  const rect = (target && typeof target.getBoundingClientRect === 'function')
    ? target.getBoundingClientRect()
    : target;
  pop.style.position = 'fixed';
  pop.style.left = (rect.left + rect.width / 2) + 'px';
  pop.style.zIndex = '10100';
  const spaceAbove = rect.top;
  if (spaceAbove < 40) {
    pop.style.top = (rect.bottom + 4) + 'px';
    pop.dataset.placement = 'below';
  } else {
    pop.style.top = (rect.top - 4) + 'px';
    pop.dataset.placement = 'above';
  }
  document.body.appendChild(pop);
  // Clamp within viewport + reveal (after layout so getBoundingClientRect is accurate)
  requestAnimationFrame(() => {
    const r = pop.getBoundingClientRect();
    const vw = window.innerWidth;
    if (r.right > vw - 8) pop.style.left = (parseFloat(pop.style.left) - (r.right - vw + 8)) + 'px';
    if (r.left < 8) pop.style.left = (parseFloat(pop.style.left) + (8 - r.left)) + 'px';
    pop.classList.add('visible');
  });
}

/**
 * Show a transient popover styled like the Tasks hover popover, auto-dismissed
 * after `duration` ms. Reuses `.task-hover-popover` CSS so appearance stays
 * consistent across the terminal UI.
 * @param {string} text - popover body text
 * @param {HTMLElement | DOMRect} target - anchor element or rect
 * @param {number} duration - visible duration in ms (default 3000)
 */
export function showTransientPopover(text, target, duration = 3000) {
  const pop = el('div', 'task-hover-popover');
  pop.appendChild(el('div', 'task-hover-popover-title', text));
  positionPopover(pop, target);
  setTimeout(() => {
    pop.classList.remove('visible');
    pop.classList.add('fading');
    setTimeout(() => pop.remove(), 400);
  }, duration);
  return pop;
}

// ── Public factory ─────────────────────────────────────────────

/**
 * Creates the tasks panel system. Returns { render, setTasks, dispose, startQuickAdd, openNewTaskModal }.
 */
export function createTasksPanel({ taskListEl, tasksHeaderEl, postMessage, onDragState, getActiveSessionName, getTasksMode, onAttachToTerminal }) {
  let _tasks = [];
  let _selectedTaskId = null;

  // ── Drag state ───────────────────────────────────────────────
  let _taskDragState = null;
  let _dragJustEnded = false;

  // ── Rendering ────────────────────────────────────────────────

  function render() {
    taskListEl.textContent = '';

    // Separate active tasks from archived (done) tasks
    const activeTasks = _tasks.filter(t => t.status !== 'done');
    const archivedTasks = _tasks.filter(t => t.status === 'done');

    // Show/hide archive button based on archived task count
    const archiveBtn = document.getElementById('archive-tasks-btn');
    if (archiveBtn) archiveBtn.style.display = archivedTasks.length > 0 ? '' : 'none';

    if (activeTasks.length === 0) {
      const empty = el('div', 'task-empty');
      const icon = el('span', 'task-empty-icon', '\uD83D\uDCCB');
      empty.appendChild(icon);
      const hint = el('span', null);
      hint.appendChild(document.createTextNode('No tasks yet. '));
      const kbd = el('kbd', null, 'Ctrl+Shift+T');
      hint.appendChild(kbd);
      hint.appendChild(document.createTextNode(' to create one.'));
      empty.appendChild(hint);
      taskListEl.appendChild(empty);
      return;
    }

    // Group active tasks by lane (done tasks excluded from sidebar)
    const byLane = { now: [], next: [], later: [] };
    for (const t of activeTasks) {
      byLane[t.lane].push(t);
    }
    for (const lane of LANES) {
      byLane[lane].sort((a, b) => b.updatedAt - a.updatedAt);
    }

    for (const lane of LANES) {
      const tasks = byLane[lane];
      // Lane divider (always show if there are any tasks)
      const divider = el('div', 'task-lane-divider');
      divider.dataset.lane = lane;
      const label = el('span', 'task-lane-label', LANE_LABEL[lane]);
      const count = el('span', 'task-lane-count', String(tasks.length));
      divider.appendChild(label);
      divider.appendChild(count);
      taskListEl.appendChild(divider);

      if (tasks.length === 0) continue;

      for (const task of tasks) {
        const item = buildTaskItem(task);
        item.dataset.lane = lane;
        taskListEl.appendChild(item);
      }
    }
  }

  function buildTaskItem(task) {
    const item = el('div', 'task-item' + (task.status === 'done' ? ' done' : ''));
    item.dataset.taskId = task.id;
    if (task.id === _selectedTaskId) item.classList.add('selected');

    // Type emoji
    const typeEl = el('span', 'task-type', TYPE_EMOJI[task.type] || TYPE_EMOJI.other);
    typeEl.title = TYPE_LABEL[task.type] || 'Other';
    item.appendChild(typeEl);

    // Title
    const title = el('span', 'task-title', task.title);
    item.appendChild(title);

    // Link indicator (paperclip)
    if (task.linkedSessions.length > 0) {
      const link = el('span', 'task-link');
      if (task.linkedSessions.length === 1) {
        link.textContent = '\uD83D\uDCCE'; // 📎
        link.title = 'Linked to: ' + task.linkedSessions[0].sessionName;
      } else {
        link.textContent = '\uD83D\uDCCE' + task.linkedSessions.length;
        link.title = 'Linked to ' + task.linkedSessions.length + ' sessions';
      }
      link.addEventListener('click', (e) => {
        e.stopPropagation();
        const current = task.linkedSessions[0];
        postMessage({ type: 'switch-to-task-session', immortermId: current.immortermId });
      });
      item.appendChild(link);
    }

    // Status indicator
    const status = el('span', 'task-status', STATUS_ICON[task.status]);
    status.title = task.status.replace('_', ' ');
    item.appendChild(status);

    // Hover action: delete only (edit via double-click)
    const deleteBtn = el('button', 'task-hover-btn task-hover-delete', '\u00d7');
    deleteBtn.title = 'Delete task';
    deleteBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      confirmDelete(item, task.id);
    });
    item.appendChild(deleteBtn);

    // Hover popover — full text after 500ms
    let _hoverTimer = null;
    item.addEventListener('mouseenter', () => {
      _hoverTimer = setTimeout(() => {
        // Only show if title is actually truncated
        const titleEl = item.querySelector('.task-title');
        if (!titleEl || titleEl.scrollWidth <= titleEl.clientWidth) return;
        dismissHoverPopover();
        const pop = el('div', 'task-hover-popover');
        pop.appendChild(el('div', 'task-hover-popover-title', task.title));
        positionPopover(pop, item);
        _activeHoverPopover = pop;
      }, 500);
    });
    item.addEventListener('mouseleave', () => {
      clearTimeout(_hoverTimer);
      dismissHoverPopover();
    });

    // Right-click context menu
    item.addEventListener('contextmenu', (e) => showTaskContextMenu(e, task));

    // Drag to session — mousedown initiates; click deferred until mouseup confirms no drag
    item.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      if (e.target.closest('.task-hover-actions')) return; // Don't drag from action buttons
      _taskDragState = {
        taskId: task.id,
        taskTitle: task.title,
        sourceLane: task.lane,
        startX: e.clientX,
        startY: e.clientY,
        itemEl: item,
        dragging: false,
      };
    });

    // Click to select
    item.addEventListener('click', () => {
      if (_dragJustEnded) return;
      _selectedTaskId = task.id;
      for (const si of taskListEl.querySelectorAll('.task-item.selected')) {
        si.classList.remove('selected');
      }
      item.classList.add('selected');
    });

    // Double-click to edit
    item.addEventListener('dblclick', (e) => {
      e.stopPropagation();
      openTaskModal(task);
    });

    return item;
  }

  // ── Task create/edit modal ──────────────────────────────────

  function openTaskModal(task) {
    const existing = document.querySelector('.task-modal-overlay');
    if (existing) existing.remove();

    const isEdit = !!task;
    const overlay = el('div', 'task-modal-overlay');
    const modal = el('div', 'task-modal');

    // Header
    const header = el('div', 'task-modal-header');
    header.appendChild(el('span', null, isEdit ? 'Edit Task' : 'New Task'));
    const closeBtn = el('button', 'task-board-close', '\u00d7');
    closeBtn.addEventListener('click', () => overlay.remove());
    header.appendChild(closeBtn);
    modal.appendChild(header);

    // Body
    const body = el('div', 'task-modal-body');

    // Title input
    const titleLabel = el('label', 'task-modal-label', 'Title');
    const titleInput = el('input', 'task-modal-input');
    titleInput.value = task ? task.title : '';
    titleInput.placeholder = 'What needs to be done?';
    body.appendChild(titleLabel);
    body.appendChild(titleInput);

    // Description textarea (supports markdown)
    const descLabel = el('label', 'task-modal-label', 'Description');
    const descInput = document.createElement('textarea');
    descInput.className = 'task-modal-input task-modal-textarea';
    descInput.value = task?.description || '';
    descInput.placeholder = 'Optional — supports Markdown. AI will auto-fill if left empty.';
    descInput.rows = 3;
    body.appendChild(descLabel);
    body.appendChild(descInput);

    // Type picker
    const typeLabel = el('label', 'task-modal-label', 'Type');
    const typePicker = el('div', 'task-modal-type-picker');
    let selectedType = task ? task.type : 'other';
    for (const [type, emoji] of Object.entries(TYPE_EMOJI)) {
      const btn = el('button', 'task-modal-type-btn' + (type === selectedType ? ' active' : ''));
      btn.appendChild(el('span', null, emoji));
      btn.appendChild(el('span', null, ' ' + TYPE_LABEL[type]));
      btn.addEventListener('click', () => {
        selectedType = type;
        for (const b of typePicker.querySelectorAll('.task-modal-type-btn')) b.classList.remove('active');
        btn.classList.add('active');
      });
      typePicker.appendChild(btn);
    }
    body.appendChild(typeLabel);
    body.appendChild(typePicker);

    // Lane picker
    const laneLabel = el('label', 'task-modal-label', 'Lane');
    const lanePicker = el('div', 'task-modal-lane-picker');
    let selectedLane = task ? task.lane : 'next';
    for (const lane of LANES) {
      const btn = el('button', 'task-modal-lane-btn' + (lane === selectedLane ? ' active' : ''), LANE_LABEL[lane]);
      btn.addEventListener('click', () => {
        selectedLane = lane;
        for (const b of lanePicker.querySelectorAll('.task-modal-lane-btn')) b.classList.remove('active');
        btn.classList.add('active');
      });
      lanePicker.appendChild(btn);
    }
    body.appendChild(laneLabel);
    body.appendChild(lanePicker);

    // Status picker (edit only)
    if (isEdit) {
      const statusLabel = el('label', 'task-modal-label', 'Status');
      const statusPicker = el('div', 'task-modal-lane-picker');
      let selectedStatus = task.status;
      for (const [status, label] of Object.entries(STATUS_LABEL)) {
        const btn = el('button', 'task-modal-lane-btn' + (status === selectedStatus ? ' active' : ''));
        btn.textContent = STATUS_ICON[status] + ' ' + label;
        btn.addEventListener('click', () => {
          selectedStatus = status;
          for (const b of statusPicker.querySelectorAll('.task-modal-lane-btn')) b.classList.remove('active');
          btn.classList.add('active');
        });
        statusPicker.appendChild(btn);
      }
      body.appendChild(statusLabel);
      body.appendChild(statusPicker);

      // Store getter for save
      body._getStatus = () => selectedStatus;

      // Metadata: created/updated timestamps
      const meta = el('div', 'task-modal-meta');
      meta.textContent = 'Created ' + relativeTime(task.createdAt);
      if (task.updatedAt !== task.createdAt) meta.textContent += ' · Updated ' + relativeTime(task.updatedAt);
      if (task.completedAt) meta.textContent += ' · Completed ' + relativeTime(task.completedAt);
      body.appendChild(meta);
    }

    modal.appendChild(body);

    // Footer with save/cancel
    const footer = el('div', 'task-modal-footer');
    const cancelBtn = el('button', 'task-modal-cancel', 'Cancel');
    cancelBtn.addEventListener('click', () => overlay.remove());
    const saveBtn = el('button', 'task-modal-save', isEdit ? 'Save' : 'Create');
    saveBtn.addEventListener('click', () => {
      const titleVal = titleInput.value.trim();
      if (!titleVal) { titleInput.focus(); return; }

      const descVal = descInput.value.trim();
      if (isEdit) {
        const fields = {};
        if (titleVal !== task.title) fields.title = titleVal;
        if (descVal !== (task.description || '')) fields.description = descVal;
        if (selectedType !== task.type) fields.taskType = selectedType;
        if (selectedLane !== task.lane) fields.lane = selectedLane;
        const newStatus = body._getStatus();
        if (newStatus !== task.status) fields.status = newStatus;
        if (Object.keys(fields).length > 0) {
          emitUpdate(task.id, fields);
        }
      } else {
        postMessage({ type: 'create-task', title: titleVal, description: descVal || undefined, taskType: selectedType, lane: selectedLane });
      }
      overlay.remove();
    });
    footer.appendChild(cancelBtn);
    footer.appendChild(saveBtn);
    modal.appendChild(footer);

    overlay.appendChild(modal);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) overlay.remove(); });
    document.body.appendChild(overlay);

    // Focus title + keyboard shortcuts
    requestAnimationFrame(() => titleInput.focus());
    function onKey(e) {
      if (e.key === 'Escape') { overlay.remove(); document.removeEventListener('keydown', onKey); }
      if (e.key === 'Enter' && e.metaKey) { saveBtn.click(); document.removeEventListener('keydown', onKey); }
    }
    document.addEventListener('keydown', onKey);
  }

  function openNewTaskModal() {
    openTaskModal(null);
  }

  // ── Context menu ─────────────────────────────────────────────

  function showTaskContextMenu(e, task) {
    e.preventDefault();
    e.stopPropagation();
    const old = document.querySelector('.sb-context-menu');
    if (old) old.remove();

    const menu = el('div', 'sb-context-menu');
    menu.style.left = e.clientX + 'px';
    menu.style.top = e.clientY + 'px';

    // Edit
    addMenuItem(menu, 'Edit task', () => openTaskModal(task));
    addSeparator(menu);

    // Move to lane
    for (const lane of LANES) {
      if (lane === task.lane) continue;
      addMenuItem(menu, 'Move to ' + LANE_LABEL[lane], () => {
        emitUpdate(task.id, { lane });
      });
    }
    addSeparator(menu);

    // Status
    const statuses = [['todo', 'Mark as Todo'], ['in_progress', 'Mark as In Progress'], ['done', 'Mark as Done']];
    for (const [status, label] of statuses) {
      if (status === task.status) continue;
      addMenuItem(menu, label, () => {
        emitUpdate(task.id, { status });
      });
    }

    // Unlink
    if (task.linkedSessions.length > 0) {
      addSeparator(menu);
      for (const ls of task.linkedSessions) {
        addMenuItem(menu, 'Unlink from ' + ls.sessionName, () => {
          postMessage({ type: 'unlink-task-session', taskId: task.id, immortermId: ls.immortermId });
        });
      }
    }

    addSeparator(menu);
    // Delete
    const deleteItem = addMenuItem(menu, 'Delete task', () => {
      const taskItem = taskListEl.querySelector(`.task-item[data-task-id="${task.id}"]`);
      if (taskItem) confirmDelete(taskItem, task.id);
    });
    deleteItem.style.color = '#f38ba8';

    document.body.appendChild(menu);

    // Flip upward if menu would overflow the viewport
    requestAnimationFrame(() => {
      const rect = menu.getBoundingClientRect();
      if (rect.bottom > window.innerHeight) {
        menu.style.top = Math.max(4, e.clientY - rect.height) + 'px';
      }
      if (rect.right > window.innerWidth) {
        menu.style.left = Math.max(4, e.clientX - rect.width) + 'px';
      }
    });

    // Dismiss on click outside
    function dismiss(ev) {
      if (!menu.contains(ev.target)) { menu.remove(); document.removeEventListener('mousedown', dismiss, true); }
    }
    setTimeout(() => document.addEventListener('mousedown', dismiss, true), 0);
  }

  function addMenuItem(menu, label, onClick) {
    const item = el('div', 'sb-context-menu-item', label);
    item.addEventListener('click', () => { menu.remove(); onClick(); });
    menu.appendChild(item);
    return item;
  }

  function addSeparator(menu) {
    const sep = el('div', 'task-ctx-separator');
    menu.appendChild(sep);
  }

  // ── Keyboard ─────────────────────────────────────────────────

  function handleKeydown(e) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const items = [...taskListEl.querySelectorAll('.task-item')];
      if (items.length === 0) return;
      const idx = items.findIndex(i => i.dataset.taskId === _selectedTaskId);
      let next;
      if (e.key === 'ArrowDown') {
        next = idx < items.length - 1 ? idx + 1 : 0;
      } else {
        next = idx > 0 ? idx - 1 : items.length - 1;
      }
      _selectedTaskId = items[next].dataset.taskId;
      render();
      items[next]?.scrollIntoView({ block: 'nearest' });
    } else if (e.key === 'Enter' && _selectedTaskId) {
      const task = _tasks.find(t => t.id === _selectedTaskId);
      if (task) openTaskModal(task);
    } else if ((e.key === 'Delete' || e.key === 'Backspace') && _selectedTaskId) {
      const selectedItem = taskListEl.querySelector(`.task-item[data-task-id="${_selectedTaskId}"]`);
      if (selectedItem) confirmDelete(selectedItem, _selectedTaskId);
    }
  }

  // ── Drag support (document-level listeners) ──────────────────

  let _dragListenersAttached = false;

  // Drop indicator for reorder within task list
  const taskDropIndicator = el('div', 'task-drop-indicator');
  taskDropIndicator.style.display = 'none';

  /** Find the drop position among items within a specific lane. */
  function calcDropPosition(clientY) {
    const items = [...taskListEl.querySelectorAll('.task-item')];
    for (let i = 0; i < items.length; i++) {
      const rect = items[i].getBoundingClientRect();
      if (clientY < rect.top + rect.height / 2) return { index: i, el: items[i] };
    }
    return { index: items.length, el: items.length > 0 ? items[items.length - 1] : null };
  }

  /** Determine which lane the cursor is over by finding the last lane divider above clientY. */
  function getLaneAtY(clientY) {
    const dividers = [...taskListEl.querySelectorAll('.task-lane-divider')];
    let lane = 'now'; // default to first lane
    for (const div of dividers) {
      const rect = div.getBoundingClientRect();
      if (clientY >= rect.top) lane = div.dataset.lane;
    }
    return lane;
  }

  /** Highlight the target lane divider during drag. */
  function highlightLane(lane) {
    for (const div of taskListEl.querySelectorAll('.task-lane-divider')) {
      if (div.dataset.lane === lane) {
        div.classList.add('drag-target');
      } else {
        div.classList.remove('drag-target');
      }
    }
  }

  function clearLaneHighlights() {
    for (const div of taskListEl.querySelectorAll('.task-lane-divider.drag-target')) {
      div.classList.remove('drag-target');
    }
  }

  function attachDragListeners() {
    if (_dragListenersAttached) return;
    _dragListenersAttached = true;

    // Ensure task list has relative positioning for drop indicator
    if (!taskListEl.style.position) taskListEl.style.position = 'relative';

    document.addEventListener('mousemove', (e) => {
      if (!_taskDragState) return;
      const dx = e.clientX - _taskDragState.startX;
      const dy = e.clientY - _taskDragState.startY;
      if (!_taskDragState.dragging && (Math.abs(dx) > 4 || Math.abs(dy) > 4)) {
        _taskDragState.dragging = true;
        _taskDragState.itemEl.classList.add('dragging');

        // Create floating ghost that follows cursor
        const ghost = el('div', 'task-drag-ghost');
        const emoji = _taskDragState.itemEl.querySelector('.task-type');
        ghost.textContent = (emoji ? emoji.textContent + ' ' : '') + _taskDragState.taskTitle;
        ghost.style.left = e.clientX + 'px';
        ghost.style.top = e.clientY + 'px';
        document.body.appendChild(ghost);
        _taskDragState.ghostEl = ghost;

        // Add drop indicator to task list
        taskListEl.appendChild(taskDropIndicator);
      }
      if (!_taskDragState.dragging) return;

      // Move ghost to follow cursor
      if (_taskDragState.ghostEl) {
        _taskDragState.ghostEl.style.left = e.clientX + 8 + 'px';
        _taskDragState.ghostEl.style.top = e.clientY + 8 + 'px';
      }

      // Check cursor location: outside sidebar, over sessions, or over task list
      const sidebar = taskListEl.closest('#sidebar');
      const sidebarRect = sidebar ? sidebar.getBoundingClientRect() : null;
      const outsideSidebar = sidebarRect && e.clientX < sidebarRect.left;
      const taskListRect = taskListEl.getBoundingClientRect();
      const overTaskList = e.clientY >= taskListRect.top && e.clientY <= taskListRect.bottom
        && e.clientX >= taskListRect.left && e.clientX <= taskListRect.right;

      // Clear session highlights
      for (const si of document.querySelectorAll('.session-item.task-drop-target')) {
        si.classList.remove('task-drop-target');
      }

      if (outsideSidebar) {
        // Outside sidebar → show terminal drop zone, hide task reorder indicator
        if (onDragState) onDragState(true, _taskDragState.taskTitle);
        taskDropIndicator.style.display = 'none';
        clearLaneHighlights();
      } else if (overTaskList) {
        // Over task list → show reorder indicator + highlight target lane
        if (onDragState) onDragState(false, null);
        taskDropIndicator.style.display = '';

        const targetLane = getLaneAtY(e.clientY);
        highlightLane(targetLane);

        const { index, el: targetEl } = calcDropPosition(e.clientY);
        const items = [...taskListEl.querySelectorAll('.task-item')];
        if (items.length > 0) {
          if (index < items.length) {
            const rect = items[index].getBoundingClientRect();
            taskDropIndicator.style.top = (rect.top - taskListRect.top - 1) + 'px';
          } else if (targetEl) {
            const lastRect = targetEl.getBoundingClientRect();
            taskDropIndicator.style.top = (lastRect.bottom - taskListRect.top - 1) + 'px';
          }
        }
      } else {
        // Over session list or other sidebar area → highlight session drop targets
        if (onDragState) onDragState(false, null);
        taskDropIndicator.style.display = 'none';
        clearLaneHighlights();
        if (_taskDragState.ghostEl) _taskDragState.ghostEl.style.pointerEvents = 'none';
        const hovered = document.elementFromPoint(e.clientX, e.clientY);
        const hoveredSession = hovered?.closest?.('.session-item');
        if (hoveredSession) hoveredSession.classList.add('task-drop-target');
      }
    });

    document.addEventListener('mouseup', (e) => {
      if (!_taskDragState) return;
      if (_taskDragState.dragging) {
        _taskDragState.itemEl.classList.remove('dragging');

        // Remove ghost
        if (_taskDragState.ghostEl) _taskDragState.ghostEl.remove();

        // Hide indicators
        if (onDragState) onDragState(false, null);
        taskDropIndicator.style.display = 'none';
        clearLaneHighlights();
        for (const si of document.querySelectorAll('.session-item.task-drop-target')) {
          si.classList.remove('task-drop-target');
        }

        // Suppress the click event that fires after mouseup
        _dragJustEnded = true;
        setTimeout(() => { _dragJustEnded = false; }, 50);

        // Determine drop zone
        const sidebar = taskListEl.closest('#sidebar');
        const sidebarRect = sidebar ? sidebar.getBoundingClientRect() : null;
        const outsideSidebar = sidebarRect && e.clientX < sidebarRect.left;
        const taskListRect = taskListEl.getBoundingClientRect();
        const overTaskList = e.clientY >= taskListRect.top && e.clientY <= taskListRect.bottom
          && e.clientX >= taskListRect.left && e.clientX <= taskListRect.right;

        if (outsideSidebar) {
          // Dropped on terminal → STAGE the task as a pill (the comments way),
          // serialized + pasted into the prompt on next Enter. Falls back to
          // the legacy queue message if the host didn't wire the callback.
          const activeName = getActiveSessionName ? getActiveSessionName() : null;
          if (activeName) {
            if (onAttachToTerminal) {
              onAttachToTerminal({
                taskId: _taskDragState.taskId,
                taskTitle: _taskDragState.taskTitle,
                targetName: activeName,
              });
            } else {
              postMessage({ type: 'drop-task-on-session', taskId: _taskDragState.taskId, targetName: activeName });
            }
          }
        } else if (overTaskList) {
          // Dropped within task list → reorder + possible lane change
          const targetLane = getLaneAtY(e.clientY);
          const laneChanged = _taskDragState.sourceLane !== targetLane;

          // If lane changed, update the lane
          if (laneChanged) {
            emitUpdate(_taskDragState.taskId, { lane: targetLane });
          }

          // Reorder: build new order from DOM positions
          const items = [...taskListEl.querySelectorAll('.task-item')];
          const taskIds = items.map(i => i.dataset.taskId);
          const { index: dropIndex } = calcDropPosition(e.clientY);
          const dragIndex = taskIds.indexOf(_taskDragState.taskId);
          if (dragIndex !== -1) {
            const newOrder = [...taskIds];
            newOrder.splice(dragIndex, 1);
            const insertAt = dropIndex > dragIndex ? dropIndex - 1 : dropIndex;
            newOrder.splice(Math.max(0, insertAt), 0, _taskDragState.taskId);
            // Always send reorder (even same-lane) if position changed
            if (newOrder.join(',') !== taskIds.join(',')) {
              postMessage({ type: 'reorder-tasks', taskIds: newOrder });
            }
          }
        } else {
          // Dropped on a specific session item in sidebar
          const target = document.elementFromPoint(e.clientX, e.clientY);
          const sessionItem = target?.closest?.('.session-item');
          if (sessionItem) {
            const targetName = sessionItem.dataset.name;
            if (targetName) {
              postMessage({
                type: 'drop-task-on-session',
                taskId: _taskDragState.taskId,
                targetName,
              });
            }
          }
        }
      }
      _taskDragState = null;
    });
  }

  // ── Mutations → host ─────────────────────────────────────────

  function emitUpdate(taskId, fields) {
    postMessage({ type: 'update-task', taskId, ...fields });
  }

  // ── Task board modal ─────────────────────────────────────

  function showTaskBoard(tab) {
    const existing = document.querySelector('.task-board-overlay');
    if (existing) existing.remove();

    const overlay = el('div', 'task-board-overlay');
    const modal = el('div', 'task-board-modal');

    // Header
    const header = el('div', 'task-board-header');
    header.appendChild(el('span', null, '\uD83D\uDCCB Task Board'));
    const closeBtn = el('button', 'task-board-close', '\u00d7');
    closeBtn.addEventListener('click', () => { overlay.remove(); _boardRenderFn = null; });
    header.appendChild(closeBtn);
    modal.appendChild(header);

    // Tabs
    const tabBar = el('div', 'task-board-tabs');
    const activeTab = el('button', 'task-board-tab' + (tab !== 'archive' ? ' active' : ''), 'Active');
    const archiveTab = el('button', 'task-board-tab' + (tab === 'archive' ? ' active' : ''), 'Archive');
    activeTab.addEventListener('click', () => { _currentBoardMode = 'active'; renderBoard('active'); setActiveTab(activeTab); });
    archiveTab.addEventListener('click', () => { _currentBoardMode = 'archive'; _activeLaneFilter = 'all'; _archiveTypeFilter = 'all'; resetLaneFilterUI(); resetArchiveFilterUI(); renderBoard('archive'); setActiveTab(archiveTab); });
    tabBar.appendChild(activeTab);
    tabBar.appendChild(archiveTab);
    modal.appendChild(tabBar);

    function setActiveTab(selected) {
      for (const t of tabBar.querySelectorAll('.task-board-tab')) t.classList.remove('active');
      selected.classList.add('active');
    }

    // Lane filter bar (visible only in active mode)
    const laneFilterBar = el('div', 'task-board-lane-filter');
    let _activeLaneFilter = 'all';

    const allFilterBtn = el('button', 'task-board-lane-pill active', 'All');
    allFilterBtn.dataset.lane = 'all';
    laneFilterBar.appendChild(allFilterBtn);
    for (const lane of LANES) {
      const pill = el('button', 'task-board-lane-pill', LANE_LABEL[lane]);
      pill.dataset.lane = lane;
      laneFilterBar.appendChild(pill);
    }
    laneFilterBar.addEventListener('click', (e) => {
      const pill = e.target.closest('.task-board-lane-pill');
      if (!pill) return;
      _activeLaneFilter = pill.dataset.lane;
      for (const p of laneFilterBar.querySelectorAll('.task-board-lane-pill')) p.classList.remove('active');
      pill.classList.add('active');
      renderBoard('active');
    });

    function resetLaneFilterUI() {
      for (const p of laneFilterBar.querySelectorAll('.task-board-lane-pill')) {
        p.classList.toggle('active', p.dataset.lane === 'all');
      }
    }
    function resetArchiveFilterUI() {
      for (const p of archiveFilterBar.querySelectorAll('.task-board-lane-pill:not(.task-board-sort-btn)')) {
        p.classList.toggle('active', p.dataset.type === 'all');
      }
    }

    modal.appendChild(laneFilterBar);

    // Archive filter bar (type filter + sort toggle, visible only in archive mode)
    const archiveFilterBar = el('div', 'task-board-lane-filter');
    archiveFilterBar.style.display = 'none';
    let _archiveTypeFilter = 'all';
    let _archiveSortNewest = true;

    const allTypeBtn = el('button', 'task-board-lane-pill active', 'All');
    allTypeBtn.dataset.type = 'all';
    archiveFilterBar.appendChild(allTypeBtn);
    for (const [type, emoji] of Object.entries(TYPE_EMOJI)) {
      const pill = el('button', 'task-board-lane-pill', emoji + ' ' + TYPE_LABEL[type]);
      pill.dataset.type = type;
      archiveFilterBar.appendChild(pill);
    }
    const sortBtn = el('button', 'task-board-lane-pill task-board-sort-btn', '\u2193 Newest');
    archiveFilterBar.appendChild(sortBtn);
    sortBtn.addEventListener('click', () => {
      _archiveSortNewest = !_archiveSortNewest;
      sortBtn.textContent = _archiveSortNewest ? '\u2193 Newest' : '\u2191 Oldest';
      renderBoard('archive');
    });
    archiveFilterBar.addEventListener('click', (e) => {
      const pill = e.target.closest('.task-board-lane-pill');
      if (!pill || pill === sortBtn) return;
      _archiveTypeFilter = pill.dataset.type;
      for (const p of archiveFilterBar.querySelectorAll('.task-board-lane-pill:not(.task-board-sort-btn)')) p.classList.remove('active');
      pill.classList.add('active');
      renderBoard('archive');
    });

    modal.appendChild(archiveFilterBar);

    // Body
    const body = el('div', 'task-board-body');
    modal.appendChild(body);

    function renderBoard(mode) {
      body.textContent = '';

      // Show/hide filter bars based on mode
      laneFilterBar.style.display = mode === 'archive' ? 'none' : '';
      archiveFilterBar.style.display = mode === 'archive' ? '' : 'none';

      let tasks;
      if (mode === 'archive') {
        tasks = _tasks.filter(t => t.status === 'done');
        if (_archiveTypeFilter !== 'all') tasks = tasks.filter(t => t.type === _archiveTypeFilter);
        tasks.sort((a, b) => {
          const ta = a.completedAt || a.updatedAt, tb = b.completedAt || b.updatedAt;
          return _archiveSortNewest ? tb - ta : ta - tb;
        });
        // Update type pill counts
        const allDone = _tasks.filter(t => t.status === 'done');
        for (const pill of archiveFilterBar.querySelectorAll('.task-board-lane-pill:not(.task-board-sort-btn)')) {
          const type = pill.dataset.type;
          const count = type === 'all' ? allDone.length : allDone.filter(t => t.type === type).length;
          const label = type === 'all' ? 'All' : TYPE_EMOJI[type] + ' ' + TYPE_LABEL[type];
          pill.textContent = label + (count > 0 ? ' (' + count + ')' : '');
        }
      } else {
        tasks = _tasks.filter(t => t.status !== 'done');
      }

      if (tasks.length === 0) {
        body.appendChild(el('div', 'task-board-empty', mode === 'archive' ? 'No completed tasks yet' : 'No active tasks'));
        return;
      }

      if (mode === 'archive') {
        const lane = el('div', 'task-board-lane');
        for (const task of tasks) lane.appendChild(buildBoardItem(task, true));
        body.appendChild(lane);
      } else {
        const lanesToShow = _activeLaneFilter === 'all' ? LANES : [_activeLaneFilter];
        const byLane = { now: [], next: [], later: [] };
        for (const t of tasks) byLane[t.lane].push(t);
        for (const l of LANES) byLane[l].sort((a, b) => b.updatedAt - a.updatedAt);

        for (const l of lanesToShow) {
          const section = el('div', 'task-board-lane');
          section.dataset.lane = l;
          // Always show lane title when viewing all lanes
          if (_activeLaneFilter === 'all') {
            section.appendChild(el('div', 'task-board-lane-title', LANE_LABEL[l] + ' (' + byLane[l].length + ')'));
          }
          if (byLane[l].length === 0) {
            const empty = el('div', 'task-board-lane-empty', 'No tasks');
            section.appendChild(empty);
          } else {
            for (const task of byLane[l]) section.appendChild(buildBoardItem(task));
          }
          body.appendChild(section);
        }

        // Update pill counts
        for (const pill of laneFilterBar.querySelectorAll('.task-board-lane-pill')) {
          const lane = pill.dataset.lane;
          const count = lane === 'all' ? tasks.length : byLane[lane].length;
          const label = lane === 'all' ? 'All' : LANE_LABEL[lane];
          pill.textContent = label + (count > 0 ? ' (' + count + ')' : '');
        }
      }
    }

    // ── Board DnD state ──────────────────────────────────────
    let _boardDrag = null;
    let _boardDragJustEnded = false;
    const boardDropIndicator = el('div', 'task-board-drop-indicator');
    boardDropIndicator.style.display = 'none';

    function calcBoardDropPosition(clientY, laneEl) {
      const items = [...laneEl.querySelectorAll('.task-board-item')];
      for (let i = 0; i < items.length; i++) {
        const rect = items[i].getBoundingClientRect();
        if (clientY < rect.top + rect.height / 2) return { index: i, el: items[i] };
      }
      return { index: items.length, el: items.length > 0 ? items[items.length - 1] : null };
    }

    function getBoardLaneAt(clientY) {
      const lanes = [...body.querySelectorAll('.task-board-lane')];
      for (let i = lanes.length - 1; i >= 0; i--) {
        const rect = lanes[i].getBoundingClientRect();
        if (clientY >= rect.top) return lanes[i];
      }
      return lanes[0] || null;
    }

    function boardDragCleanup() {
      if (_boardDrag?.ghostEl) _boardDrag.ghostEl.remove();
      if (_boardDrag?.itemEl) _boardDrag.itemEl.classList.remove('board-dragging');
      boardDropIndicator.style.display = 'none';
      boardDropIndicator.remove();
      for (const it of body.querySelectorAll('.task-board-item.board-drag-over')) {
        it.classList.remove('board-drag-over');
      }
      // Clear lane header highlights
      for (const lane of body.querySelectorAll('.task-board-lane')) {
        lane.classList.remove('board-lane-drop-target');
      }
      _boardDrag = null;
    }

    modal.addEventListener('mousemove', (e) => {
      if (!_boardDrag) return;
      const dx = e.clientX - _boardDrag.startX;
      const dy = e.clientY - _boardDrag.startY;

      if (!_boardDrag.dragging && (Math.abs(dx) > 4 || Math.abs(dy) > 4)) {
        _boardDrag.dragging = true;
        _boardDrag.itemEl.classList.add('board-dragging');

        const ghost = el('div', 'task-drag-ghost');
        ghost.textContent = _boardDrag.emoji + ' ' + _boardDrag.taskTitle;
        ghost.style.left = e.clientX + 'px';
        ghost.style.top = e.clientY + 'px';
        document.body.appendChild(ghost);
        _boardDrag.ghostEl = ghost;
      }
      if (!_boardDrag.dragging) return;

      _boardDrag.ghostEl.style.left = (e.clientX + 8) + 'px';
      _boardDrag.ghostEl.style.top = (e.clientY + 8) + 'px';

      // Find which lane section the cursor is over
      const targetLane = getBoardLaneAt(e.clientY);

      // Highlight the target lane (and clear others)
      for (const lane of body.querySelectorAll('.task-board-lane')) {
        const isTarget = lane === targetLane && targetLane.dataset.lane !== _boardDrag.sourceLane;
        lane.classList.toggle('board-lane-drop-target', isTarget);
      }

      if (targetLane) {
        if (!targetLane.contains(boardDropIndicator)) {
          targetLane.style.position = 'relative';
          targetLane.appendChild(boardDropIndicator);
        }
        boardDropIndicator.style.display = '';

        const { index, el: targetEl } = calcBoardDropPosition(e.clientY, targetLane);
        const items = [...targetLane.querySelectorAll('.task-board-item')];
        const laneRect = targetLane.getBoundingClientRect();
        if (items.length > 0) {
          if (index < items.length) {
            const rect = items[index].getBoundingClientRect();
            boardDropIndicator.style.top = (rect.top - laneRect.top - 1) + 'px';
          } else if (targetEl) {
            const rect = targetEl.getBoundingClientRect();
            boardDropIndicator.style.top = (rect.bottom - laneRect.top - 1) + 'px';
          }
        }
      }
    });

    modal.addEventListener('mouseup', (e) => {
      if (!_boardDrag || !_boardDrag.dragging) {
        _boardDrag = null;
        return;
      }

      // Suppress click that fires after mouseup
      _boardDragJustEnded = true;
      setTimeout(() => { _boardDragJustEnded = false; }, 50);

      // Build new full task order from all visible board items
      const allItems = [...body.querySelectorAll('.task-board-item')];
      const currentOrder = allItems.map(i => i.dataset.taskId);
      const targetLane = getBoardLaneAt(e.clientY);

      if (targetLane) {
        const { index: dropIndex } = calcBoardDropPosition(e.clientY, targetLane);
        const laneItems = [...targetLane.querySelectorAll('.task-board-item')];
        const laneIds = laneItems.map(i => i.dataset.taskId);
        const dragIdx = laneIds.indexOf(_boardDrag.taskId);

        // Cross-lane drop: use dataset.lane or fall back to filter
        const targetLaneName = targetLane.dataset.lane
          || (_activeLaneFilter !== 'all' ? _activeLaneFilter : _boardDrag.sourceLane);

        if (targetLaneName && targetLaneName !== _boardDrag.sourceLane) {
          emitUpdate(_boardDrag.taskId, { lane: targetLaneName });
        }

        // Build reorder from full visible order
        const fullIds = [...currentOrder];
        const fromIdx = fullIds.indexOf(_boardDrag.taskId);
        if (fromIdx !== -1) {
          fullIds.splice(fromIdx, 1);

          // Calculate absolute insert position
          const laneStartIdx = currentOrder.indexOf(laneIds[0]);
          const absInsert = laneStartIdx >= 0
            ? laneStartIdx + (dropIndex > dragIdx && dragIdx >= 0 ? dropIndex - 1 : dropIndex)
            : fullIds.length;
          // Adjust for removal shift
          const adjustedInsert = fromIdx < absInsert ? absInsert : absInsert;
          fullIds.splice(Math.max(0, Math.min(adjustedInsert, fullIds.length)), 0, _boardDrag.taskId);

          if (fullIds.join(',') !== currentOrder.join(',')) {
            postMessage({ type: 'reorder-tasks', taskIds: fullIds });
          }
        }
      }

      boardDragCleanup();
    });

    function buildBoardItem(task, showCompleted) {
      const item = el('div', 'task-board-item' + (task.status === 'done' ? ' done' : ''));
      item.dataset.taskId = task.id;

      // Drag handle
      const grip = el('span', 'task-board-grip', '\u2847'); // ⠇ braille dots
      grip.title = 'Drag to reorder';
      item.appendChild(grip);

      item.appendChild(el('span', 'task-type', TYPE_EMOJI[task.type] || TYPE_EMOJI.other));
      item.appendChild(el('span', 'task-title', task.title));
      if (showCompleted && (task.completedAt || task.updatedAt)) {
        item.appendChild(el('span', 'task-board-time', relativeTime(task.completedAt || task.updatedAt)));
      }
      if (task.linkedSessions.length > 0) {
        const linkEl = el('span', 'task-link', '\uD83D\uDCCE' + (task.linkedSessions.length > 1 ? task.linkedSessions.length : ''));
        linkEl.title = task.linkedSessions.map(ls => ls.sessionName).join(', ');
        item.appendChild(linkEl);
      }
      item.appendChild(el('span', 'task-status', STATUS_ICON[task.status]));

      // Drag initiation on mousedown
      item.addEventListener('mousedown', (e) => {
        if (e.button !== 0) return;
        const emoji = TYPE_EMOJI[task.type] || TYPE_EMOJI.other;
        _boardDrag = {
          taskId: task.id,
          taskTitle: task.title,
          sourceLane: task.lane,
          emoji,
          startX: e.clientX,
          startY: e.clientY,
          itemEl: item,
          dragging: false,
          ghostEl: null,
        };
      });

      // Right-click for context menu
      item.addEventListener('contextmenu', (e) => showTaskContextMenu(e, task));
      // Click to edit (suppressed after drag)
      item.addEventListener('click', () => {
        if (_boardDragJustEnded) return;
        closeOverlay();
        openTaskModal(task);
      });
      return item;
    }

    // Clean up if mouse leaves the modal during drag
    modal.addEventListener('mouseleave', () => {
      if (_boardDrag?.dragging) boardDragCleanup();
    });

    // Track current board mode for re-render bridge
    let _currentBoardMode = tab || 'active';
    renderBoard(_currentBoardMode);

    // Wire up board re-render bridge so setTasks can refresh the modal
    const origRenderBoard = renderBoard;
    _boardRenderFn = () => {
      if (document.body.contains(overlay)) origRenderBoard(_currentBoardMode);
    };
    // Patch renderBoard to track mode changes
    const _origActiveClick = activeTab.onclick;
    const _origArchiveClick = archiveTab.onclick;

    overlay.appendChild(modal);
    function closeOverlay() {
      overlay.remove();
      _boardRenderFn = null;
    }
    overlay.addEventListener('click', (e) => { if (e.target === overlay) closeOverlay(); });
    document.body.appendChild(overlay);

    // Escape to close
    function onKey(e) { if (e.key === 'Escape') { closeOverlay(); document.removeEventListener('keydown', onKey); } }
    document.addEventListener('keydown', onKey);
  }

  // ── Board re-render bridge ───────────────────────────────────
  // When the board modal is open, setTasks needs to re-render it.
  let _boardRenderFn = null;

  // ── Public API ───────────────────────────────────────────────

  let _animatingTaskIds = new Set();
  let _pendingTasks = null;

  function setTasks(tasks) {
    _tasks = tasks || [];
    // If any tasks are currently animating, defer re-render
    if (_animatingTaskIds.size > 0) {
      _pendingTasks = _tasks;
      return;
    }
    render();
    // Re-render board modal if open
    if (_boardRenderFn) _boardRenderFn();
    // Section visibility is owned by the S5a accordion (applySectionLayout
    // in gpu-terminal.html) — this module never touches style.display.
  }

  function _finishAnimation(taskId) {
    _animatingTaskIds.delete(taskId);
    if (_animatingTaskIds.size === 0 && _pendingTasks) {
      _tasks = _pendingTasks;
      _pendingTasks = null;
      render();
      if (_boardRenderFn) _boardRenderFn();
    }
  }

  function dispose() {
    // No persistent listeners to clean up beyond document-level ones
  }

  // Wire up new task button → opens modal instead of inline quick-add
  const newTaskBtn = tasksHeaderEl?.querySelector('#new-task-btn');
  if (newTaskBtn) {
    newTaskBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      openNewTaskModal();
    });
  }

  // Wire up archive button
  const archiveBtn = tasksHeaderEl?.querySelector('#archive-tasks-btn');
  if (archiveBtn) {
    archiveBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      showTaskBoard('archive');
    });
  }

  // Board opens from the header's hover action icon — the header itself
  // toggles the accordion section (S5a B2).
  const boardBtn = tasksHeaderEl?.querySelector('#task-board-btn');
  if (boardBtn) {
    boardBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      showTaskBoard('active');
    });
  }

  // Suppress VS Code default context menu on task list
  taskListEl?.addEventListener('contextmenu', (e) => { e.preventDefault(); });

  // Keyboard navigation when task list is focused
  taskListEl?.addEventListener('keydown', handleKeydown);
  taskListEl?.setAttribute('tabindex', '0');

  // Attach drag listeners
  attachDragListeners();

  // ── Sparkle animation system ─────────────────────────────────

  const SPARKLE_COLORS_GREEN = ['#a6e3a1', '#4ade80', '#86efac', '#bbf7d0'];

  /** Build accent-derived sparkle palette from current theme CSS variables. */
  function getThemeSparkleColors() {
    const style = getComputedStyle(document.documentElement);
    const accent = style.getPropertyValue('--sidebar-accent').trim() || '#b482ff';
    // Parse accent to get base RGB
    const temp = document.createElement('div');
    temp.style.color = accent;
    document.body.appendChild(temp);
    const rgb = getComputedStyle(temp).color;
    temp.remove();
    const m = rgb.match(/(\d+)/g);
    if (!m) return [accent, accent, accent];
    const [r, g, b] = m.map(Number);
    // Generate palette: base, lighter, more saturated, shifted hue
    return [
      `rgb(${r}, ${g}, ${b})`,
      `rgb(${Math.min(255, r + 40)}, ${Math.min(255, g + 40)}, ${Math.min(255, b + 40)})`,
      `rgb(${Math.min(255, r + 20)}, ${Math.min(255, g + 60)}, ${Math.min(255, b + 30)})`,
      `rgb(${Math.max(0, r - 20)}, ${Math.min(255, g + 30)}, ${Math.min(255, b + 50)})`,
      `rgb(${Math.min(255, r + 60)}, ${Math.min(255, g + 20)}, ${Math.max(0, b - 20)})`,
    ];
  }

  /**
   * Spawn sparkle particles around a DOM element's bounding rect.
   * @param {HTMLElement} anchor - element to sparkle around
   * @param {string[]} colors - palette for particles
   * @param {number} count - number of particles (default 14)
   */
  function spawnSparkles(anchor, colors, count = 14) {
    const rect = anchor.getBoundingClientRect();
    // Position relative to taskListEl's scroll container
    const containerRect = taskListEl.getBoundingClientRect();
    const cx = rect.left + rect.width / 2 - containerRect.left;
    const cy = rect.top + rect.height / 2 - containerRect.top + taskListEl.scrollTop;

    const container = el('div', 'sparkle-container');
    container.style.position = 'absolute';
    container.style.left = '0';
    container.style.top = '0';
    container.style.width = '100%';
    container.style.height = '100%';
    container.style.pointerEvents = 'none';
    container.style.overflow = 'visible';
    container.style.zIndex = '100';
    taskListEl.style.position = 'relative';
    taskListEl.appendChild(container);

    for (let i = 0; i < count; i++) {
      const spark = document.createElement('div');
      spark.className = 'sparkle-particle';
      const color = colors[i % colors.length];
      const size = 3 + Math.random() * 4;
      // Random angle from center, biased toward edges
      const angle = (Math.PI * 2 * i) / count + (Math.random() - 0.5) * 0.5;
      const startDist = Math.max(rect.width, rect.height) / 2 * 0.3;
      const endDist = startDist + 20 + Math.random() * 30;
      const startX = cx + Math.cos(angle) * startDist;
      const startY = cy + Math.sin(angle) * startDist;
      const endX = cx + Math.cos(angle) * endDist;
      const endY = cy + Math.sin(angle) * endDist;
      const delay = Math.random() * 200;
      const duration = 500 + Math.random() * 400;

      spark.style.cssText = `
        position: absolute;
        left: ${startX}px; top: ${startY}px;
        width: ${size}px; height: ${size}px;
        background: ${color};
        border-radius: 50%;
        box-shadow: 0 0 ${size + 2}px ${color};
        opacity: 0;
        animation: sparkle-fly ${duration}ms ${delay}ms cubic-bezier(0.25, 0.46, 0.45, 0.94) forwards;
        --sparkle-tx: ${endX - startX}px;
        --sparkle-ty: ${endY - startY}px;
      `;
      container.appendChild(spark);
    }

    // Cleanup after all animations complete
    setTimeout(() => container.remove(), 1200);
  }

  // ── Hover popover state ──────────────────────────────────
  let _activeHoverPopover = null;
  function dismissHoverPopover() {
    if (_activeHoverPopover) {
      _activeHoverPopover.remove();
      _activeHoverPopover = null;
    }
  }

  /**
   * Show a floating popover badge next to a task item.
   * @param {HTMLElement} anchor - task item element
   * @param {string} text - badge text
   * @param {string} cls - CSS modifier class (e.g. 'enhanced' or 'completed')
   * @param {number} duration - how long to show (ms)
   */
  function showPopover(anchor, text, cls, duration = 2000) {
    const pop = el('div', 'task-popover ' + cls);
    pop.textContent = text;
    positionPopover(pop, anchor);
    setTimeout(() => {
      pop.classList.remove('visible');
      pop.classList.add('fading');
      setTimeout(() => pop.remove(), 400);
    }, duration);
  }

  /**
   * Show a confirmation popover anchored near an element.
   * @param {HTMLElement} anchor - element to anchor near
   * @param {string} message - confirmation text
   * @param {Function} onConfirm - called if user confirms
   */
  function confirmDelete(anchor, taskId) {
    // Remove any existing confirm popover
    document.querySelectorAll('.task-confirm-popover').forEach(p => p.remove());

    const pop = el('div', 'task-confirm-popover');
    positionPopover(pop, anchor);

    const label = el('span', 'task-confirm-label', 'Delete?');
    pop.appendChild(label);

    const yesBtn = el('button', 'task-confirm-btn task-confirm-yes', 'Yes');
    yesBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      pop.classList.add('fading');
      setTimeout(() => pop.remove(), 200);
      postMessage({ type: 'delete-task', taskId });
    });
    const noBtn = el('button', 'task-confirm-btn task-confirm-no', 'No');
    noBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      pop.classList.add('fading');
      setTimeout(() => pop.remove(), 200);
    });
    pop.appendChild(yesBtn);
    pop.appendChild(noBtn);

    // Auto-dismiss after 5s
    const timer = setTimeout(() => {
      if (pop.parentNode) {
        pop.classList.add('fading');
        setTimeout(() => pop.remove(), 200);
      }
    }, 5000);
    // Clear timer if manually dismissed
    pop.addEventListener('click', () => clearTimeout(timer), { once: true });
  }

  /**
   * Play AI Enhanced animation on a task: sparkles + popover.
   * @param {string} taskId
   */
  function animateEnhanced(taskId) {
    const item = taskListEl.querySelector(`.task-item[data-task-id="${taskId}"]`);
    if (!item) return;
    item.classList.add('task-enhanced-glow');
    spawnSparkles(item, getThemeSparkleColors(), 16);
    showPopover(item, '\u2728 AI Enhanced', 'enhanced', 2500);
    setTimeout(() => item.classList.remove('task-enhanced-glow'), 2000);
  }

  /**
   * Play Done animation: green sparkles + checkmark popover, then archive.
   * @param {string} taskId
   */
  function animateDone(taskId) {
    const item = taskListEl.querySelector(`.task-item[data-task-id="${taskId}"]`);
    if (!item) return;
    _animatingTaskIds.add(taskId);
    item.classList.add('task-done-glow');
    spawnSparkles(item, SPARKLE_COLORS_GREEN, 18);
    showPopover(item, '\u2705 Done!', 'completed', 2500);
    // After 2.5s, shrink + fade out then flush pending re-render
    setTimeout(() => {
      item.classList.add('task-archive-out');
      const finish = () => _finishAnimation(taskId);
      item.addEventListener('transitionend', finish, { once: true });
      // Fallback if transitionend doesn't fire
      setTimeout(finish, 600);
    }, 2500);
  }

  return { render, setTasks, startQuickAdd: openNewTaskModal, openNewTaskModal, animateEnhanced, animateDone, dispose };
}

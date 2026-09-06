<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  darkTheme,
  NAlert,
  NButton,
  NCheckbox,
  NCheckboxGroup,
  NCollapse,
  NCollapseItem,
  NConfigProvider,
  NDropdown,
  NEmpty,
  NForm,
  NFormItem,
  NInput,
  NLayout,
  NLayoutContent,
  NLayoutSider,
  NList,
  NListItem,
  NModal,
  NPopconfirm,
  NScrollbar,
  NSelect,
  NSpace,
  NSpin,
  NTabPane,
  NTabs,
  NTag,
  NText,
  NTree,
} from "naive-ui";
import type { DropdownOption, TreeOption } from "naive-ui";
import {
  addAclEntry,
  canLoadTree,
  cancelSearchIndex,
  closeTab,
  createChildNode,
  deleteTreeNode,
  deleteSavedConnection,
  disconnectConnection,
  formatOperationError,
  formatJsonDraft,
  handleNodeEvent,
  handleSearchIndexState,
  handleSessionState,
  loadSavedConnections,
  loadTreeNode,
  listNodeChildren,
  openConnection,
  removeAclEntry,
  revealPath,
  runShellCommand,
  saveConnection,
  saveNodeAcl,
  saveNodeData,
  searchInTab,
  selectNode,
  setPasswordRequester,
  shellHistoryNavigate,
  startSearchIndex,
  store,
  tabById,
  updateExpandedKeys,
  type AuthType,
  type ConnectionTab,
  type KeyringStatus,
  type NodeEvent,
  type SavedConnection,
  type SessionStateEvent,
} from "./store";
import type { SearchIndexStateEvent } from "./zk-search";

const showNewConnection = ref(false);
const editingConnectionId = ref("");
const modalError = ref("");
const modalInfo = ref("");
const testingConnection = ref(false);
const keyringAvailable = ref(true);
const appError = ref("");
// ── 搜索 Spotlight 与树滚动 ─────────────────────────────
// Template ref callback 会在渲染期写入 registry；这些实例不参与视图状态，
// 必须保持非响应式，避免“写 ref → 重渲染 → 再写 ref”的递归更新。
const treeInstRefs: Record<string, any> = {};
const scrollbarRefs: Record<string, any> = {};
const searchInputRefs: Record<string, any> = {};
const shellScrollRefs: Record<string, HTMLElement> = {};
const shellInputRefs: Record<string, HTMLElement> = {};
const searchScopeCollapsed = reactive<Record<string, boolean>>({});
const searchDebounceTimers = new Map<string, number>();

// ── 可调整面板 ─────────────────────────────────────────────
const navWidth = ref(Number(localStorage.getItem('zoopeek:navWidth') || 260));
const detailWidth = ref(Number(localStorage.getItem('zoopeek:detailWidth') || 440));
const eventHeight = ref(Number(localStorage.getItem('zoopeek:eventHeight') || 180));
type PaneName = 'nav' | 'detail' | 'event';
let draggingPane: PaneName | null = null;
let dragStartX = 0;
let dragStartY = 0;
let dragStartW = 0;
function persistPaneWidths(): void {
  localStorage.setItem('zoopeek:navWidth', String(navWidth.value));
  localStorage.setItem('zoopeek:detailWidth', String(detailWidth.value));
  localStorage.setItem('zoopeek:eventHeight', String(eventHeight.value));
}
function startDrag(e: MouseEvent, which: PaneName): void {
  draggingPane = which;
  dragStartX = e.clientX;
  dragStartY = e.clientY;
  dragStartW =
    which === 'nav' ? navWidth.value : which === 'detail' ? detailWidth.value : eventHeight.value;
  const onMove = (ev: MouseEvent) => {
    const dx = ev.clientX - dragStartX;
    const dy = ev.clientY - dragStartY;
    if (draggingPane === 'nav') {
      navWidth.value = Math.round(Math.min(400, Math.max(200, dragStartW + dx)));
    } else if (draggingPane === 'detail') {
      // detail 在右侧，拖动左移应增大（dx 负值增大），所以反向
      detailWidth.value = Math.round(Math.min(620, Math.max(360, dragStartW - dx)));
    } else if (draggingPane === 'event') {
      // 事件流在底部，拖动上移应增大（dy 负值增大），所以反向
      eventHeight.value = Math.round(Math.min(480, Math.max(96, dragStartW - dy)));
    }
    bumpLayout();
  };
  const onUp = () => {
    draggingPane = null;
    window.removeEventListener('mousemove', onMove);
    window.removeEventListener('mouseup', onUp);
    persistPaneWidths();
  };
  window.addEventListener('mousemove', onMove);
  window.addEventListener('mouseup', onUp);
  e.preventDefault();
}

function setTreeRef(tabId: string, el: any): void {
  if (el) treeInstRefs[tabId] = el;
  else delete treeInstRefs[tabId];
}
function setScrollbarRef(tabId: string, el: any): void {
  if (el) scrollbarRefs[tabId] = el;
  else delete scrollbarRefs[tabId];
}
function setSearchInputRef(tabId: string, el: any): void {
  if (el) searchInputRefs[tabId] = el;
  else delete searchInputRefs[tabId];
}

// ── Shell 面板 ─────────────────────────────────────────────
function setShellScrollRef(tabId: string, el: any): void {
  if (el) shellScrollRefs[tabId] = el as HTMLElement;
  else delete shellScrollRefs[tabId];
}
function setShellInputRef(tabId: string, el: any): void {
  if (el) shellInputRefs[tabId] = el as HTMLElement;
  else delete shellInputRefs[tabId];
}
function scrollShellToBottom(tabId: string): void {
  const el = shellScrollRefs[tabId];
  if (el) el.scrollTop = el.scrollHeight;
}
async function runShell(tab: ConnectionTab): Promise<void> {
  await runShellCommand(tab);
  await nextTick();
  scrollShellToBottom(tab.id);
}
async function switchBottomPanel(tab: ConnectionTab, panel: "events" | "shell"): Promise<void> {
  tab.bottomPanel = panel;
  await nextTick();
  if (panel === "shell") {
    scrollShellToBottom(tab.id);
    shellInputRefs[tab.id]?.focus();
  }
}

// ── 搜索结果浮层高度：随树面板实际空间自适应 ─────────────────
// 浮层绝对定位覆盖在树上，必须给节点树保留最小可见空间，
// 窗口变矮时浮层跟着变矮，而不是完全盖住节点树。
const TREE_MIN_VISIBLE = 160;
const PANEL_MIN_HEIGHT = 96;
const PANEL_FALLBACK_HEIGHT = 320;
const treePaneRefs: Record<string, HTMLElement> = {};
const spotlightRefs: Record<string, HTMLElement> = {};
const layoutTick = ref(0);
let layoutRaf = 0;
const layoutObserver = new ResizeObserver(() => {
  // 合并同一帧内的多次通知，避免高频重渲染
  if (layoutRaf) return;
  layoutRaf = requestAnimationFrame(() => {
    layoutRaf = 0;
    layoutTick.value += 1;
  });
});
function bumpLayout(): void {
  layoutTick.value += 1;
}
// Vue 每次重渲染都会以 null/元素重新调用 inline ref；observer 注册保持幂等。
const observedEls = new WeakSet<Element>();
function setTreePaneRef(tabId: string, el: any): void {
  if (el) {
    treePaneRefs[tabId] = el as HTMLElement;
    if (!observedEls.has(el as Element)) {
      observedEls.add(el as Element);
      layoutObserver.observe(el as Element);
    }
  } else {
    delete treePaneRefs[tabId];
  }
}
function setSpotlightRef(tabId: string, el: any): void {
  if (el) {
    spotlightRefs[tabId] = el as HTMLElement;
    if (!observedEls.has(el as Element)) {
      observedEls.add(el as Element);
      layoutObserver.observe(el as Element);
    }
  } else {
    delete spotlightRefs[tabId];
  }
}
function searchPanelMaxHeight(tabId: string): number {
  void layoutTick.value; // 依赖跟踪：面板尺寸变化时重新计算
  const pane = treePaneRefs[tabId];
  const spotlight = spotlightRefs[tabId];
  if (!pane || !spotlight || pane.clientHeight === 0) return PANEL_FALLBACK_HEIGHT;
  const available = pane.clientHeight - spotlight.offsetHeight - TREE_MIN_VISIBLE;
  return Math.max(
    PANEL_MIN_HEIGHT,
    Math.min(available, Math.round(window.innerHeight * 0.52))
  );
}
function dismissSearch(tab: ConnectionTab): void {
  tab.searchQuery = "";
  tab.searchResults = [];
  tab.searchTotalMatches = 0;
  tab.searchLoading = false;
  tab.searchError = "";
  const timer = searchDebounceTimers.get(tab.id);
  if (timer) {
    window.clearTimeout(timer);
    searchDebounceTimers.delete(tab.id);
  }
}

function searchStatusLabel(tab: ConnectionTab): string {
  const s = tab.searchIndexStatus;
  if (!s) return "未构建";
  const map: Record<string, string> = {
    empty: "未构建",
    building: "构建中…",
    ready: "就绪",
    incomplete: "部分就绪",
    truncated: "已截断",
    failed: "失败",
    cancelled: "已取消",
  };
  return map[s.state] ?? s.state;
}
function searchStatusType(tab: ConnectionTab): "default" | "info" | "success" | "warning" | "error" {
  const s = tab.searchIndexStatus?.state;
  if (s === "ready") return "success";
  if (s === "building") return "warning";
  if (s === "incomplete" || s === "truncated") return "warning";
  if (s === "failed") return "error";
  return "default";
}
function scheduleSearch(tab: ConnectionTab): void {
  const existing = searchDebounceTimers.get(tab.id);
  if (existing) window.clearTimeout(existing);
  const timer = window.setTimeout(() => {
    searchDebounceTimers.delete(tab.id);
    void searchInTab(tab, tab.searchQuery);
  }, 180);
  searchDebounceTimers.set(tab.id, timer as unknown as number);
}
function onSearchQueryUpdate(tab: ConnectionTab, value: string): void {
  tab.searchQuery = value;
  tab.searchError = "";
  if (value.trim().length === 0) {
    dismissSearch(tab);
    return;
  }
  scheduleSearch(tab);
}
async function onScopePathUpdate(tab: ConnectionTab, value: string): Promise<void> {
  tab.searchScopePath = value;
  if (tab.searchQuery.trim().length > 0) {
    await searchInTab(tab, tab.searchQuery);
  }
}
function toggleScopeCollapsed(tabId: string): void {
  searchScopeCollapsed[tabId] = !searchScopeCollapsed[tabId];
}
async function handleBuildIndex(tab: ConnectionTab, force = false): Promise<void> {
  await startSearchIndex(tab, force);
}
async function handleCancelBuild(tab: ConnectionTab): Promise<void> {
  await cancelSearchIndex(tab);
}
async function handleSearchResultClick(tab: ConnectionTab, path: string): Promise<void> {
  const ok = await revealPath(tab, path);
  if (!ok) return;
  // 选中结果后收起浮层，把空间还给节点树（Spotlight 行为）
  dismissSearch(tab);
  await nextTick();
  // 优先尝试 TreeInst.scrollTo（virtualScroll 模式），失败则回退到 DOM 滚动
  const inst = treeInstRefs[tab.id] as any;
  if (inst?.scrollTo) {
    try {
      inst.scrollTo({ key: path });
      return;
    } catch {}
  }
  // 回退：查询 data-key 并滚动（适配外层 NScrollbar）
  try {
    const keyAttr = CSS.escape(path);
    const el = document.querySelector(`[data-key="${keyAttr}"]`) as HTMLElement | null;
    if (el) {
      el.scrollIntoView({ block: "center", behavior: "smooth" });
      return;
    }
    const fallback = document.querySelector(`.tree-scroll[data-tab="${tab.id}"] [data-key]`) as HTMLElement | null;
    // 若仍未找到，尝试按文本匹配
    if (fallback) fallback.scrollIntoView({ block: "center" });
  } catch {}
}
function focusSearchForActiveTab(): void {
  const id = store.activeTabId;
  const el = searchInputRefs[id] as any;
  if (el?.focus) el.focus();
  else {
    const input = document.querySelector(`.search-input[data-tab="${id}"] input`) as HTMLElement | null;
    input?.focus();
  }
}
function isMac(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}

const newConnection = reactive({
  name: "",
  servers: "127.0.0.1:2181",
  authType: "none" as AuthType,
  username: "",
  password: "",
  savePassword: true,
});
const unlisteners: UnlistenFn[] = [];

const authTypeOptions = [
  { label: "无", value: "none" },
  { label: "digest", value: "digest" },
  { label: "SASL DIGEST-MD5", value: "sasl_digest_md5" },
];

const showPasswordPrompt = ref(false);
const passwordPrompt = reactive({
  connectionName: "",
  password: "",
});
interface PasswordPromptRequest {
  connection: SavedConnection;
  resolve: (password: string | null) => void;
}
const passwordPromptQueue: PasswordPromptRequest[] = [];
let activePasswordPrompt: PasswordPromptRequest | null = null;

function showNextPasswordPrompt(): void {
  activePasswordPrompt = passwordPromptQueue.shift() ?? null;
  if (!activePasswordPrompt) {
    showPasswordPrompt.value = false;
    return;
  }
  passwordPrompt.connectionName = activePasswordPrompt.connection.name;
  passwordPrompt.password = "";
  showPasswordPrompt.value = true;
}

function requestConnectionPassword(
  connection: SavedConnection
): Promise<string | null> {
  return new Promise((resolve) => {
    passwordPromptQueue.push({ connection, resolve });
    if (!activePasswordPrompt) showNextPasswordPrompt();
  });
}

function finishPasswordPrompt(password: string | null): void {
  const request = activePasswordPrompt;
  if (!request) return;
  activePasswordPrompt = null;
  showPasswordPrompt.value = false;
  passwordPrompt.password = "";
  request.resolve(password);
  showNextPasswordPrompt();
}

function submitPasswordPrompt(): void {
  if (!passwordPrompt.password) return;
  finishPasswordPrompt(passwordPrompt.password);
}

function resetConnectionForm(): void {
  editingConnectionId.value = "";
  newConnection.name = "";
  newConnection.servers = "127.0.0.1:2181";
  newConnection.authType = "none";
  newConnection.username = "";
  newConnection.password = "";
  newConnection.savePassword = true;
  modalError.value = "";
  modalInfo.value = "";
}

async function openNewConnectionModal(): Promise<void> {
  resetConnectionForm();
  showNewConnection.value = true;
  try {
    const status = await invoke<KeyringStatus>("keyring_status");
    keyringAvailable.value = status.available;
  } catch {
    keyringAvailable.value = false;
  }
}

async function openEditConnectionModal(connection: SavedConnection): Promise<void> {
  resetConnectionForm();
  editingConnectionId.value = connection.id;
  newConnection.name = connection.name;
  newConnection.servers = connection.servers;
  newConnection.authType = connection.auth_type;
  newConnection.username = connection.username;
  newConnection.savePassword = connection.save_password;
  showNewConnection.value = true;
  try {
    const status = await invoke<KeyringStatus>("keyring_status");
    keyringAvailable.value = status.available;
  } catch {
    keyringAvailable.value = false;
  }
}

const treeMenu = reactive({
  show: false,
  x: 0,
  y: 0,
  tabId: "",
  path: "",
});
const treeMenuOptions = ref<DropdownOption[]>([]);
const showNewNode = ref(false);
const creatingNode = ref(false);
const nodeModalError = ref("");
const newNode = reactive({ tabId: "", parentPath: "", name: "", data: "" });
const showDeleteNode = ref(false);
const deletingNode = ref(false);
const deleteTarget = reactive({ tabId: "", path: "", recursive: false });

const aclSchemeOptions = ["world", "auth", "digest", "ip"].map((value) => ({
  label: value,
  value,
}));
const aclPermissionOptions = [
  { label: "R", value: 1 },
  { label: "W", value: 2 },
  { label: "C", value: 4 },
  { label: "D", value: 8 },
  { label: "A", value: 16 },
];

const statusLabels: Record<string, string> = {
  Connecting: "连接中",
  SyncConnected: "已连接",
  ConnectedReadOnly: "只读连接",
  Disconnected: "已断开",
  Expired: "会话过期",
  AuthFailed: "认证失败",
  Closed: "已关闭",
  SaslAuthenticated: "SASL 已认证",
  WaitingForManualReconnect: "等待手动重连",
  ReconnectFailed: "重连失败",
};

function statusLabel(status: string): string {
  return statusLabels[status] ?? status;
}

function statusType(
  status: string
): "default" | "error" | "info" | "success" | "warning" {
  if (status === "SyncConnected") return "success";
  if (status === "Connecting") return "warning";
  if (status === "ConnectedReadOnly" || status === "SaslAuthenticated") {
    return "info";
  }
  if (
    status === "Expired" ||
    status === "AuthFailed" ||
    status === "ReconnectFailed"
  ) {
    return "error";
  }
  if (status === "WaitingForManualReconnect") return "warning";
  return "default";
}

async function createConnection(): Promise<void> {
  modalError.value = "";
  modalInfo.value = "";
  const name = newConnection.name.trim();
  const servers = newConnection.servers.trim();
  if (!name || !servers) {
    modalError.value = "名称和地址不能为空";
    return;
  }
  const authenticated = newConnection.authType !== "none";
  const username = newConnection.username.trim();
  const existing = editingConnectionId.value
    ? store.savedConnections.find((item) => item.id === editingConnectionId.value)
    : undefined;
  const canReuseSavedPassword = Boolean(
    existing?.save_password &&
    existing.auth_type === newConnection.authType &&
    existing.username === username
  );
  if (
    authenticated &&
    (!username || (!newConnection.password && !canReuseSavedPassword))
  ) {
    modalError.value = canReuseSavedPassword
      ? "用户名不能为空"
      : "用户名和密码不能为空";
    return;
  }
  const connection: SavedConnection = {
    id:
      editingConnectionId.value ||
      (globalThis.crypto?.randomUUID?.() ??
        `connection-${Date.now().toString(36)}`),
    name,
    servers,
    auth_type: newConnection.authType,
    username: authenticated ? username : "",
    save_password: authenticated && newConnection.savePassword,
  };
  const password = authenticated && newConnection.password
    ? newConnection.password
    : undefined;
  try {
    const result = await saveConnection(connection, password);
    if (connection.save_password && !result.password_saved) {
      keyringAvailable.value = false;
    }
    showNewConnection.value = false;
    newConnection.password = "";
    const openTab = tabById(connection.id);
    if (openTab) await disconnectConnection(openTab);
    await openConnection(connection, password);
    resetConnectionForm();
  } catch (error) {
    modalError.value = String(error);
  }
}

async function testNewConnection(): Promise<void> {
  modalError.value = "";
  modalInfo.value = "";
  const servers = newConnection.servers.trim();
  const authenticated = newConnection.authType !== "none";
  const username = newConnection.username.trim();
  if (!servers) {
    modalError.value = "地址不能为空";
    return;
  }
  if (authenticated && (!username || !newConnection.password)) {
    modalError.value = "用户名和密码不能为空";
    return;
  }
  testingConnection.value = true;
  try {
    await invoke("test_connection", {
      servers,
      authType: newConnection.authType,
      username: authenticated ? username : "",
      password: authenticated ? newConnection.password : undefined,
    });
    modalInfo.value = authenticated
      ? "连接已建立；凭证将在访问受限节点时继续验证"
      : "连接测试成功";
  } catch (error) {
    const friendly = formatOperationError(error);
    modalError.value = friendly === String(error)
      ? "无法连接，请检查地址和认证方式"
      : friendly;
  } finally {
    testingConnection.value = false;
  }
}

async function removeSavedConnection(id: string): Promise<void> {
  try {
    await deleteSavedConnection(id);
  } catch (error) {
    appError.value = String(error);
  }
}

function treeNodeProps(
  tab: ConnectionTab,
  { option }: { option: TreeOption }
): Record<string, unknown> {
  return {
    onContextmenu: (event: MouseEvent) => {
      event.preventDefault();
      treeMenu.show = false;
      treeMenu.tabId = tab.id;
      treeMenu.path = String(option.key);
      treeMenu.x = event.clientX;
      treeMenu.y = event.clientY;
      treeMenuOptions.value = [
        { label: "新建子节点", key: "create" },
        { label: "删除节点", key: "delete", disabled: option.key === "/" },
      ];
      void nextTick(() => {
        treeMenu.show = true;
      });
    },
  };
}

async function handleTreeMenuSelect(key: string | number): Promise<void> {
  treeMenu.show = false;
  const tab = tabById(treeMenu.tabId);
  if (!tab) return;

  if (key === "create") {
    newNode.tabId = tab.id;
    newNode.parentPath = treeMenu.path;
    newNode.name = "";
    newNode.data = "";
    nodeModalError.value = "";
    showNewNode.value = true;
    return;
  }

  if (key === "delete" && treeMenu.path !== "/") {
    tab.error = "";
    try {
      const children = await listNodeChildren(tab, treeMenu.path);
      deleteTarget.tabId = tab.id;
      deleteTarget.path = treeMenu.path;
      deleteTarget.recursive = children.length > 0;
      showDeleteNode.value = true;
    } catch (error) {
      tab.error = formatOperationError(error);
    }
  }
}

async function submitNewNode(): Promise<void> {
  const tab = tabById(newNode.tabId);
  if (!tab) return;
  const name = newNode.name.trim();
  if (!name || name.includes("/")) {
    nodeModalError.value = "节点名称不能为空且不能包含 /";
    return;
  }
  creatingNode.value = true;
  nodeModalError.value = "";
  try {
    await createChildNode(tab, newNode.parentPath, name, newNode.data);
    showNewNode.value = false;
  } catch (error) {
    nodeModalError.value = formatOperationError(error);
  } finally {
    creatingNode.value = false;
  }
}

async function confirmDeleteNode(): Promise<void> {
  const tab = tabById(deleteTarget.tabId);
  if (!tab) return;
  deletingNode.value = true;
  tab.error = "";
  try {
    await deleteTreeNode(
      tab,
      deleteTarget.path,
      deleteTarget.recursive
    );
    showDeleteNode.value = false;
  } catch (error) {
    tab.error = formatOperationError(error);
    showDeleteNode.value = false;
  } finally {
    deletingNode.value = false;
  }
}

function changeAclScheme(tab: ConnectionTab, scheme: string): void {
  tab.newAcl.scheme = scheme;
  tab.newAcl.id = scheme === "world" ? "anyone" : "";
}

function aclPermissionLabel(permission: number): string {
  return (
    aclPermissionOptions.find((option) => option.value === permission)?.label ??
    "?"
  );
}

onMounted(async () => {
  setPasswordRequester(requestConnectionPassword);
  try {
    unlisteners.push(
      await listen<SessionStateEvent>("zk-session-state", (event) => {
        void handleSessionState(event.payload);
      }),
      await listen<NodeEvent>("zk-node-event", (event) => {
        void handleNodeEvent(event.payload);
      }),
      await listen<SearchIndexStateEvent>("zk-search-index-state", (event) => {
        handleSearchIndexState(event.payload);
      })
    );
    await loadSavedConnections();
  } catch (error) {
    appError.value = String(error);
  }
  const onKeydown = (e: KeyboardEvent): void => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      focusSearchForActiveTab();
    }
    if (e.key === "Escape") {
      const tab = tabById(store.activeTabId);
      if (tab && tab.searchQuery) {
        dismissSearch(tab);
      }
    }
  };
  window.addEventListener("keydown", onKeydown);
  unlisteners.push(() => window.removeEventListener("keydown", onKeydown));
});

onUnmounted(() => {
  setPasswordRequester(null);
  activePasswordPrompt?.resolve(null);
  activePasswordPrompt = null;
  for (const request of passwordPromptQueue.splice(0)) request.resolve(null);
  layoutObserver.disconnect();
  unlisteners.forEach((unlisten) => unlisten());
});
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <n-layout has-sider class="app-shell">
      <n-layout-sider bordered :width="navWidth" content-style="padding: 16px;">
        <div class="brand-row">
          <div>
            <div class="brand">ZooPeek</div>
            <n-text depth="3">ZooKeeper 客户端</n-text>
          </div>
          <n-button type="primary" size="small" @click="openNewConnectionModal">
            新建
          </n-button>
        </div>

        <n-alert v-if="appError" type="error" closable @close="appError = ''">
          {{ appError }}
        </n-alert>

        <div class="section-title">已保存连接</div>
        <n-scrollbar class="connection-list-scroll">
          <n-list v-if="store.savedConnections.length" hoverable clickable>
            <n-list-item
              v-for="connection in store.savedConnections"
              :key="connection.id"
              @click="openConnection(connection)"
            >
              <div class="connection-item">
                <strong>{{ connection.name }}</strong>
                <n-text depth="3">{{ connection.servers }}</n-text>
              </div>
              <template #suffix>
                <n-space size="small">
                  <n-button text @click.stop="openEditConnectionModal(connection)">
                    编辑
                  </n-button>
                  <n-popconfirm
                    positive-text="删除"
                    negative-text="取消"
                    @positive-click="removeSavedConnection(connection.id)"
                  >
                    <template #trigger>
                      <n-button text type="error" @click.stop>删除</n-button>
                    </template>
                    删除这个连接配置？已打开的连接不会关闭。
                  </n-popconfirm>
                </n-space>
              </template>
            </n-list-item>
          </n-list>
          <n-empty v-else description="还没有保存的连接" />
        </n-scrollbar>
      </n-layout-sider>
      <div class="pane-splitter nav-splitter" @mousedown="(e: MouseEvent) => startDrag(e, 'nav')" title="拖动调整导航宽度"></div>

      <n-layout-content class="main-content">
        <n-tabs
          v-if="store.tabs.length"
          v-model:value="store.activeTabId"
          type="card"
          closable
          class="connection-tabs"
          @close="(id) => closeTab(String(id))"
        >
          <n-tab-pane
            v-for="tab in store.tabs"
            :key="tab.id"
            :name="tab.id"
            :tab="tab.name"
            display-directive="show"
          >
            <div class="tab-body">
              <div class="status-bar">
                <n-space align="center">
                  <n-tag :type="statusType(tab.status)" round>
                    {{ statusLabel(tab.status) }}
                  </n-tag>
                  <n-text depth="3">{{ tab.servers }}</n-text>
                  <n-text v-if="tab.sessionId" depth="3">
                    Session ID：{{ tab.sessionId }}
                  </n-text>
                </n-space>
                <n-button
                  size="small"
                  secondary
                  type="warning"
                  :disabled="tab.status === 'Disconnected'"
                  @click="disconnectConnection(tab)"
                >
                  断开
                </n-button>
              </div>

              <n-alert
                v-if="tab.error"
                type="error"
                closable
                @close="tab.error = ''"
              >
                {{ tab.error }}
              </n-alert>

              <div class="workspace">
                <section class="tree-pane panel" :ref="(el: any) => setTreePaneRef(tab.id, el)">
                  <div class="panel-title tree-title">
                    <span>节点树</span>
                    <n-tag
                      v-if="tab.searchIndexStatus"
                      :type="searchStatusType(tab)"
                      size="small"
                      round
                    >
                      {{ searchStatusLabel(tab) }}{{ tab.searchIndexStatus.dirty ? " · 已过期" : "" }}
                    </n-tag>
                  </div>
                  <!-- Spotlight 搜索：单输入框 + 结果浮层，作用域折叠 -->
                  <div class="search-spotlight" :ref="(el: any) => setSpotlightRef(tab.id, el)">
                    <n-input
                      :ref="(el: any) => setSearchInputRef(tab.id, el)"
                      :value="tab.searchQuery"
                      class="search-input"
                      :data-tab="tab.id"
                      clearable
                      :placeholder="isMac() ? '搜索节点… (⌘K)' : '搜索节点… (Ctrl+K)'"
                      @update:value="(v: string) => onSearchQueryUpdate(tab, v)"
                      @clear="() => { tab.searchResults = []; tab.searchTotalMatches = 0; }"
                    >
                      <template #prefix>
                        <span class="search-icon">⌕</span>
                      </template>
                    </n-input>
                    <div class="search-scope-row">
                      <n-text depth="3" class="scope-label">
                        范围：{{ tab.searchScopePath }}
                      </n-text>
                      <n-button text size="tiny" @click="toggleScopeCollapsed(tab.id)">
                        {{ searchScopeCollapsed[tab.id] ? "收起" : "展开" }}
                      </n-button>
                      <n-button
                        v-if="!tab.searchIndexStatus || tab.searchIndexStatus.state === 'empty' || tab.searchIndexStatus.state === 'failed' || tab.searchIndexStatus.state === 'cancelled'"
                        size="tiny"
                        type="primary"
                        secondary
                        @click="handleBuildIndex(tab, false)"
                      >
                        构建索引
                      </n-button>
                      <template v-else-if="tab.searchIndexStatus.state === 'building'">
                        <n-spin :size="12" />
                        <n-text depth="3" style="font-size: 12px">
                          {{ tab.searchIndexStatus.stats.visited }} 节点…
                        </n-text>
                        <n-button size="tiny" @click="handleCancelBuild(tab)">取消</n-button>
                      </template>
                      <template v-else>
                        <n-button size="tiny" secondary @click="handleBuildIndex(tab, true)">重建</n-button>
                        <n-button
                          v-if="tab.searchIndexStatus.dirty"
                          size="tiny"
                          type="warning"
                          secondary
                          @click="handleBuildIndex(tab, true)"
                        >
                          刷新
                        </n-button>
                      </template>
                    </div>
                    <n-collapse v-if="searchScopeCollapsed[tab.id]" class="scope-collapse">
                      <n-collapse-item title="高级范围" name="scope">
                        <div class="scope-edit">
                          <n-input
                            :value="tab.searchScopePath"
                            placeholder="/"
                            @update:value="(v: string) => onScopePathUpdate(tab, v)"
                          />
                          <n-text depth="3" style="font-size: 11px">仅显示该路径及子路径的匹配</n-text>
                        </div>
                      </n-collapse-item>
                    </n-collapse>
                    <div v-if="tab.searchError" class="search-error">
                      <n-text type="error" style="font-size: 12px">{{ tab.searchError }}</n-text>
                    </div>
                    <div v-if="tab.searchIndexStatus" class="search-stats">
                      <n-text depth="3" style="font-size: 11px">
                        <template v-if="tab.searchIndexStatus.state === 'ready' || tab.searchIndexStatus.state === 'incomplete' || tab.searchIndexStatus.state === 'truncated'">
                          已索引 {{ tab.searchIndexStatus.stats.visited }} 节点
                          <template v-if="tab.searchIndexStatus.stats.inaccessible_subtrees > 0"> · {{ tab.searchIndexStatus.stats.inaccessible_subtrees }} 不可达</template>
                          <template v-if="tab.searchIndexStatus.stats.skipped_nodes > 0"> · {{ tab.searchIndexStatus.stats.skipped_nodes }} 跳过</template>
                          <template v-if="tab.searchIndexStatus.state === 'truncated'"> · {{ tab.searchIndexStatus.stats.termination_reason }}</template>
                        </template>
                        <template v-else-if="tab.searchIndexStatus.state === 'building'">
                          正在遍历… {{ tab.searchIndexStatus.stats.visited }}
                        </template>
                      </n-text>
                    </div>
                    <!-- 结果浮层：仅当有查询且有结果/提示时展示；高度随树面板空间自适应 -->
                    <div
                      v-if="tab.searchQuery.trim().length > 0"
                      class="search-results-panel"
                      :style="{ maxHeight: searchPanelMaxHeight(tab.id) + 'px' }"
                    >
                      <div v-if="tab.searchLoading" class="search-loading">
                        <n-spin :size="14" /> <n-text depth="3" style="font-size: 12px">搜索中…</n-text>
                      </div>
                      <template v-else-if="tab.searchResults.length > 0">
                        <div class="search-result-meta">
                          <n-text depth="3" style="font-size: 12px">
                            匹配 {{ tab.searchTotalMatches }} 项，显示 {{ tab.searchResults.length }} 项
                            <template v-if="tab.searchIndexStatus?.dirty"> · 索引已过期</template>
                          </n-text>
                        </div>
                        <n-scrollbar
                          class="search-results-scroll"
                          :style="{ maxHeight: searchPanelMaxHeight(tab.id) - 48 + 'px' }"
                        >
                          <div
                            v-for="item in tab.searchResults"
                            :key="item.path"
                            class="search-result-item"
                            @click="handleSearchResultClick(tab, item.path)"
                          >
                            <div class="result-path">{{ item.path }}</div>
                            <div class="result-name">
                              {{ item.name }}
                              <n-tag size="tiny" :type="item.match_target === 'path' ? 'info' : 'default'" style="margin-left: 6px">
                                {{ item.match_target === 'path' ? '路径' : '名称' }}
                              </n-tag>
                            </div>
                          </div>
                        </n-scrollbar>
                      </template>
                      <n-empty
                        v-else-if="!tab.searchLoading && !tab.searchError"
                        size="small"
                        :description="tab.searchIndexStatus ? '无匹配' : '索引未构建'"
                      />
                    </div>
                  </div>
                  <n-scrollbar
                    :ref="(el: any) => setScrollbarRef(tab.id, el)"
                    class="tree-scroll"
                    :data-tab="tab.id"
                  >
                    <n-tree
                      :ref="(el: any) => setTreeRef(tab.id, el)"
                      block-line
                      show-line
                      :data="tab.tree"
                      :expanded-keys="tab.expandedKeys"
                      :selected-keys="tab.selectedPath ? [tab.selectedPath] : []"
                      :node-props="(info) => treeNodeProps(tab, info)"
                      :on-load="canLoadTree(tab.status) ? (option) => loadTreeNode(tab, option) : undefined"
                      @update:expanded-keys="
                        (keys) => updateExpandedKeys(tab, keys)
                      "
                      @update:selected-keys="(keys) => selectNode(tab, keys)"
                    />
                  </n-scrollbar>
                </section>
                <div class="pane-splitter detail-splitter" @mousedown="(e: MouseEvent) => startDrag(e, 'detail')" title="拖动调整详情宽度"></div>

                <section class="detail-pane panel" :style="{ width: detailWidth + 'px' }">
                  <div class="panel-title detail-title">
                    <span>节点详情</span>
                    <n-text v-if="tab.selectedNode" code class="detail-path">
                      {{ tab.selectedPath }}
                    </n-text>
                  </div>
                  <n-scrollbar class="detail-scroll">
                    <template v-if="tab.selectedNode">
                      <!-- 元数据压缩为一行小字，主体留给数据编辑器 -->
                      <n-text depth="3" class="meta-line">
                        长度 {{ tab.selectedNode.data_length }} · 版本
                        {{ tab.selectedNode.version }} · 子版本
                        {{ tab.selectedNode.cversion }} · 子节点
                        {{ tab.selectedNode.num_children }} ·
                        {{ tab.selectedNode.is_ephemeral ? "临时节点" : "持久节点" }}
                      </n-text>
                      <n-alert
                        v-if="tab.selectedNode.is_binary"
                        type="warning"
                        class="binary-alert"
                      >
                        该节点为二进制数据，当前为 UTF-8 有损展示，已禁止编辑以防保存损坏原始字节
                      </n-alert>
                      <div class="editor-actions">
                        <n-button
                          size="small"
                          secondary
                          :disabled="tab.selectedNode.is_binary"
                          @click="formatJsonDraft(tab)"
                        >
                          格式化 JSON
                        </n-button>
                        <n-button
                          size="small"
                          type="primary"
                          :loading="tab.saving"
                          :disabled="tab.selectedNode.is_binary"
                          @click="saveNodeData(tab)"
                        >
                          保存数据
                        </n-button>
                      </div>
                      <n-input
                        v-model:value="tab.dataDraft"
                        type="textarea"
                        class="data-editor"
                        :readonly="tab.selectedNode.is_binary"
                        :autosize="{ minRows: 14, maxRows: 26 }"
                        placeholder="节点无数据"
                      />

                      <n-collapse class="acl-collapse">
                        <n-collapse-item title="ACL 权限" name="acl">
                          <n-text v-if="tab.aclLoading" depth="3">
                            正在加载 ACL…
                          </n-text>
                          <template v-else>
                            <div v-if="tab.aclDraft.length" class="acl-list">
                              <div
                                v-for="(entry, index) in tab.aclDraft"
                                :key="`${entry.scheme}:${entry.id}:${index}`"
                                class="acl-row"
                              >
                                <n-text code>{{ entry.scheme }}:{{ entry.id }}</n-text>
                                <n-space size="small" class="acl-tags">
                                  <n-tag
                                    v-for="permission in entry.permissions"
                                    :key="permission"
                                    size="small"
                                    type="info"
                                  >
                                    {{ aclPermissionLabel(permission) }}
                                  </n-tag>
                                </n-space>
                                <n-button
                                  text
                                  type="error"
                                  @click="removeAclEntry(tab, index)"
                                >
                                  删除
                                </n-button>
                              </div>
                            </div>
                            <n-empty
                              v-else
                              size="small"
                              description="ACL 列表为空"
                            />

                            <div class="acl-add-form">
                              <n-select
                                :value="tab.newAcl.scheme"
                                :options="aclSchemeOptions"
                                class="acl-scheme"
                                @update:value="(value) => changeAclScheme(tab, value)"
                              />
                              <n-input
                                v-model:value="tab.newAcl.id"
                                placeholder="id"
                              />
                              <n-checkbox-group
                                v-model:value="tab.newAcl.permissions"
                                class="acl-checkboxes"
                              >
                                <n-checkbox
                                  v-for="permission in aclPermissionOptions"
                                  :key="permission.value"
                                  :value="permission.value"
                                  :label="permission.label"
                                />
                              </n-checkbox-group>
                              <n-button @click="addAclEntry(tab)">新增条目</n-button>
                            </div>
                            <div class="acl-save-row">
                              <n-text depth="3">
                                保存会整体替换该节点当前 ACL
                              </n-text>
                              <n-button
                                type="primary"
                                size="small"
                                :loading="tab.aclSaving"
                                @click="saveNodeAcl(tab)"
                              >
                                保存 ACL
                              </n-button>
                            </div>
                          </template>
                        </n-collapse-item>
                      </n-collapse>
                    </template>
                    <n-empty v-else description="请选择一个节点查看详情" />
                  </n-scrollbar>
                </section>
              </div>

              <div class="pane-splitter-row" @mousedown="(e: MouseEvent) => startDrag(e, 'event')" title="拖动调整事件流高度"></div>
              <section class="event-pane panel" :style="{ height: eventHeight + 'px' }">
                <div class="panel-title bottom-panel-title">
                  <div class="bottom-tabs">
                    <button
                      class="bottom-tab"
                      :class="{ active: tab.bottomPanel === 'events' }"
                      @click="switchBottomPanel(tab, 'events')"
                    >
                      事件流
                    </button>
                    <button
                      class="bottom-tab"
                      :class="{ active: tab.bottomPanel === 'shell' }"
                      @click="switchBottomPanel(tab, 'shell')"
                    >
                      Shell
                    </button>
                  </div>
                  <n-text
                    v-if="tab.bottomPanel === 'shell'"
                    depth="3"
                    class="shell-hint"
                  >
                    zkCli 命令，help 查看支持列表
                  </n-text>
                </div>
                <n-scrollbar v-if="tab.bottomPanel === 'events'" class="event-scroll">
                  <div v-if="tab.events.length" class="event-list">
                    <div v-for="event in tab.events" :key="event.id" class="event-row">
                      <span class="event-time">{{ event.time }}</span>
                      <span>{{ event.text }}</span>
                    </div>
                  </div>
                  <n-empty v-else size="small" description="暂无事件" />
                </n-scrollbar>
                <template v-else>
                  <div
                    :ref="(el: any) => setShellScrollRef(tab.id, el)"
                    class="shell-scroll"
                  >
                    <div
                      v-for="entry in tab.shellEntries"
                      :key="entry.id"
                      class="shell-line"
                      :class="`shell-${entry.kind}`"
                    >{{ entry.text || " " }}</div>
                    <n-empty
                      v-if="!tab.shellEntries.length"
                      size="small"
                      description="输入 zkCli 命令开始操作，help 查看支持列表"
                    />
                  </div>
                  <div class="shell-input-row">
                    <span class="shell-prompt">zk&gt;</span>
                    <input
                      :ref="(el: any) => setShellInputRef(tab.id, el)"
                      v-model="tab.shellDraft"
                      class="shell-input"
                      :disabled="!canLoadTree(tab.status) || tab.shellRunning"
                      :placeholder="canLoadTree(tab.status) ? 'ls /' : '连接未就绪'"
                      spellcheck="false"
                      autocomplete="off"
                      @keydown.enter="runShell(tab)"
                      @keydown.up.prevent="shellHistoryNavigate(tab, -1)"
                      @keydown.down.prevent="shellHistoryNavigate(tab, 1)"
                    />
                  </div>
                </template>
              </section>
            </div>
          </n-tab-pane>
        </n-tabs>

        <div v-else class="welcome">
          <n-empty description="从左侧选择或新建一个 ZooKeeper 连接" />
        </div>
      </n-layout-content>
    </n-layout>

    <n-modal
      v-model:show="showNewConnection"
      preset="card"
      :title="editingConnectionId ? '编辑连接' : '新建连接'"
      :style="{ width: '480px' }"
      @after-leave="newConnection.password = ''"
    >
      <n-form label-placement="top">
        <n-form-item label="连接名称" required>
          <n-input v-model:value="newConnection.name" placeholder="例如：本地开发环境" />
        </n-form-item>
        <n-form-item label="ZooKeeper 地址" required>
          <n-input
            v-model:value="newConnection.servers"
            placeholder="127.0.0.1:2181"
          />
        </n-form-item>
        <n-form-item label="认证类型">
          <n-select
            v-model:value="newConnection.authType"
            :options="authTypeOptions"
          />
        </n-form-item>
        <template v-if="newConnection.authType !== 'none'">
          <n-form-item label="用户名" required>
            <n-input
              v-model:value="newConnection.username"
              autocomplete="username"
              placeholder="用户名"
            />
          </n-form-item>
          <n-form-item label="密码" required>
            <n-input
              v-model:value="newConnection.password"
              type="password"
              show-password-on="click"
              autocomplete="new-password"
              :placeholder="editingConnectionId ? '留空则沿用已保存密码' : '密码'"
            />
          </n-form-item>
          <n-form-item :show-feedback="false">
            <n-checkbox v-model:checked="newConnection.savePassword">
              保存到系统钥匙串
            </n-checkbox>
          </n-form-item>
          <div
            v-if="newConnection.savePassword && !keyringAvailable"
            class="auth-hint"
          >
            当前系统不支持安全存储，密码将不会被保存
          </div>
          <div class="auth-hint">
            {{
              newConnection.authType === "digest"
                ? "digest 认证不加密传输，建议仅在可信网络使用"
                : "该方式已过时，仅在服务端要求时使用"
            }}
          </div>
        </template>
        <n-alert v-if="modalError" type="error">{{ modalError }}</n-alert>
        <n-alert v-if="modalInfo" type="info">{{ modalInfo }}</n-alert>
      </n-form>
      <template #footer>
        <n-space justify="space-between">
          <n-button :loading="testingConnection" @click="testNewConnection">
            测试连接
          </n-button>
          <n-space>
            <n-button @click="showNewConnection = false">取消</n-button>
            <n-button type="primary" @click="createConnection">
              {{ editingConnectionId ? "保存并重连" : "保存并连接" }}
            </n-button>
          </n-space>
        </n-space>
      </template>
    </n-modal>

    <n-modal
      v-model:show="showPasswordPrompt"
      preset="card"
      title="输入连接密码"
      :style="{ width: '420px' }"
      :mask-closable="false"
      :close-on-esc="false"
      :closable="false"
    >
      <n-form label-placement="top" @submit.prevent="submitPasswordPrompt">
        <n-form-item :label="passwordPrompt.connectionName" required>
          <n-input
            v-model:value="passwordPrompt.password"
            type="password"
            show-password-on="click"
            autocomplete="current-password"
            autofocus
            placeholder="密码"
            @keyup.enter="submitPasswordPrompt"
          />
        </n-form-item>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="finishPasswordPrompt(null)">取消</n-button>
          <n-button
            type="primary"
            :disabled="!passwordPrompt.password"
            @click="submitPasswordPrompt"
          >
            连接
          </n-button>
        </n-space>
      </template>
    </n-modal>

    <n-dropdown
      trigger="manual"
      placement="bottom-start"
      :show="treeMenu.show"
      :x="treeMenu.x"
      :y="treeMenu.y"
      :options="treeMenuOptions"
      :on-clickoutside="() => (treeMenu.show = false)"
      @select="handleTreeMenuSelect"
    />

    <n-modal
      v-model:show="showNewNode"
      preset="card"
      title="新建子节点"
      :style="{ width: '520px' }"
    >
      <n-form label-placement="top">
        <n-form-item label="父节点">
          <n-text code>{{ newNode.parentPath }}</n-text>
        </n-form-item>
        <n-form-item label="节点名称" required>
          <n-input v-model:value="newNode.name" placeholder="child" />
        </n-form-item>
        <n-form-item label="初始数据">
          <n-input
            v-model:value="newNode.data"
            type="textarea"
            :autosize="{ minRows: 5, maxRows: 12 }"
            placeholder="可留空"
          />
        </n-form-item>
        <n-alert v-if="nodeModalError" type="error">
          {{ nodeModalError }}
        </n-alert>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showNewNode = false">取消</n-button>
          <n-button type="primary" :loading="creatingNode" @click="submitNewNode">
            创建
          </n-button>
        </n-space>
      </template>
    </n-modal>

    <n-modal
      v-model:show="showDeleteNode"
      preset="card"
      title="删除节点"
      :style="{ width: '480px' }"
    >
      <n-alert :type="deleteTarget.recursive ? 'warning' : 'info'">
        <template v-if="deleteTarget.recursive">
          将递归删除该节点及全部子节点：{{ deleteTarget.path }}
        </template>
        <template v-else>确认删除节点：{{ deleteTarget.path }}？</template>
      </n-alert>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showDeleteNode = false">取消</n-button>
          <n-button type="error" :loading="deletingNode" @click="confirmDeleteNode">
            删除
          </n-button>
        </n-space>
      </template>
    </n-modal>
  </n-config-provider>
</template>

<style>
:root {
  color-scheme: dark;
  font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei",
    sans-serif;
}

html,
body,
#app {
  width: 100%;
  height: 100%;
  margin: 0;
}

body {
  overflow: hidden;
}

.app-shell {
  height: 100vh;
}

.brand-row,
.status-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.brand {
  font-size: 21px;
  font-weight: 700;
  letter-spacing: 0.3px;
}

.section-title,
.panel-title,
.editor-title {
  font-weight: 600;
  color: rgba(255, 255, 255, 0.82);
}

.section-title {
  margin: 24px 0 10px;
  font-size: 13px;
}

.connection-list-scroll {
  max-height: calc(100vh - 135px);
}

.connection-item {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 3px;
}

.connection-item .n-text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.main-content,
.connection-tabs {
  height: 100vh;
}

.connection-tabs > .n-tabs-pane-wrapper {
  height: calc(100vh - 46px);
}

.tab-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: calc(100vh - 70px);
  padding: 0 16px 16px;
  box-sizing: border-box;
}

.status-bar {
  flex: none;
  min-height: 34px;
}

.tab-body > .n-alert {
  flex: none;
}

.workspace {
  display: flex;
  flex: 1;
  min-height: 0;
}

.panel {
  min-height: 0;
  padding: 14px;
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.025);
  box-sizing: border-box;
}

.tree-pane,
.detail-pane,
.event-pane {
  display: flex;
  flex-direction: column;
}

.panel-title {
  margin-bottom: 12px;
  font-size: 14px;
}

.tree-scroll {
  flex: 1;
  min-height: 0;
}

.detail-scroll {
  flex: 1;
  min-height: 0;
}

/* 让树随最宽节点撑开：横向滚动时高亮块覆盖完整宽度 */
.tree-scroll .n-tree {
  width: max-content;
  min-width: 100%;
}

.detail-title {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.detail-path {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.meta-line {
  display: block;
  margin-bottom: 10px;
  font-size: 12px;
}

.data-editor {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.binary-alert {
  margin-bottom: 10px;
}

.editor-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-bottom: 10px;
}

.acl-collapse {
  margin-top: 18px;
}

.acl-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.acl-row {
  display: grid;
  grid-template-columns: minmax(140px, auto) 1fr auto;
  align-items: center;
  gap: 10px;
}

.acl-tags {
  min-width: 0;
}

.acl-add-form {
  display: grid;
  grid-template-columns: 110px minmax(140px, 1fr) auto auto;
  align-items: center;
  gap: 10px;
  margin-top: 14px;
}

.acl-checkboxes {
  display: flex;
  gap: 8px;
  white-space: nowrap;
}

.acl-save-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-top: 14px;
}

.event-pane {
  flex: none;
}

.event-scroll {
  flex: 1;
  min-height: 0;
}

.event-list {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
}

.event-row {
  display: flex;
  gap: 12px;
  padding: 4px 2px;
  line-height: 1.5;
}

.event-time {
  flex: 0 0 auto;
  color: #7e8a9a;
}

/* ── 底部面板切换（事件流 / Shell） ─────────────────────── */
.bottom-panel-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.bottom-tabs {
  display: flex;
  gap: 4px;
}
.bottom-tab {
  padding: 2px 10px;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: rgba(255, 255, 255, 0.55);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
}
.bottom-tab:hover {
  color: rgba(255, 255, 255, 0.82);
}
.bottom-tab.active {
  background: rgba(36, 200, 219, 0.16);
  color: #24c8db;
}
.shell-hint {
  font-size: 11px;
}

/* ── Shell 终端 ─────────────────────────────────────────── */
.shell-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
}
.shell-line {
  padding: 1px 2px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
}
.shell-cmd {
  color: #24c8db;
  font-weight: 600;
}
.shell-err {
  color: #e88080;
}
.shell-info {
  color: #7e8a9a;
}
.shell-input-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: none;
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
}
.shell-prompt {
  color: #24c8db;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  font-weight: 600;
}
.shell-input {
  flex: 1;
  min-width: 0;
  border: none;
  outline: none;
  background: transparent;
  color: rgba(255, 255, 255, 0.88);
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
}
.shell-input:disabled {
  color: rgba(255, 255, 255, 0.35);
  cursor: not-allowed;
}

.welcome {
  display: grid;
  height: 100%;
  place-items: center;
}

.auth-hint {
  margin: 8px 0;
  color: rgba(255, 255, 255, 0.58);
  font-size: 12px;
  line-height: 1.5;
}

.tree-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.search-spotlight {
  position: relative;
  margin-bottom: 8px;
  z-index: 10;
}
.search-icon {
  opacity: 0.6;
  font-size: 14px;
}
.search-scope-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
  flex-wrap: wrap;
}
.scope-label {
  font-size: 12px;
}
.scope-collapse {
  margin-top: 6px;
}
.scope-edit {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.search-error {
  margin-top: 6px;
}
.search-stats {
  margin-top: 4px;
}
.search-results-panel {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  right: 0;
  z-index: 30;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 8px;
  background: #1e2227;
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.45);
  padding: 8px;
  overflow: hidden;
}
.search-loading {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 0;
}
.search-result-meta {
  margin-bottom: 6px;
}
.search-result-item {
  padding: 6px 8px;
  border-radius: 4px;
  cursor: pointer;
}
.search-result-item:hover {
  background: rgba(255, 255, 255, 0.06);
}
.result-path {
  font-size: 12px;
  color: rgba(255, 255, 255, 0.88);
  word-break: break-all;
}
.result-name {
  font-size: 11px;
  color: rgba(255, 255, 255, 0.55);
  margin-top: 2px;
}
.pane-splitter {
  flex: 0 0 12px;
  width: 12px;
  margin: 0;
  cursor: col-resize;
  background: transparent;
  position: relative;
  z-index: 5;
}
.pane-splitter::after {
  content: "";
  position: absolute;
  left: 5px;
  right: 5px;
  top: 0;
  bottom: 0;
  border-radius: 2px;
  background: transparent;
  transition: background 0.15s;
}
.pane-splitter-row {
  flex: 0 0 10px;
  height: 10px;
  margin: -10px 0;
  cursor: row-resize;
  background: transparent;
  position: relative;
  z-index: 5;
}
.pane-splitter-row::after {
  content: "";
  position: absolute;
  top: 5px;
  bottom: 5px;
  left: 0;
  right: 0;
  border-radius: 2px;
  background: transparent;
  transition: background 0.15s;
}
.pane-splitter:hover::after,
.pane-splitter:active::after,
.pane-splitter-row:hover::after,
.pane-splitter-row:active::after {
  background: rgba(36, 200, 219, 0.22);
}
.tree-pane {
  min-width: 320px;
  min-height: 280px;
  flex: 1 1 auto;
  position: relative;
  overflow: visible;
}
.detail-pane {
  min-width: 360px;
  flex: 0 0 auto;
}
.main-content {
  min-width: 0;
}
@media (max-width: 920px) {
  .workspace { flex-direction: column; gap: 12px; }
  .pane-splitter, .pane-splitter-row { display: none; }
  .tree-pane, .detail-pane { width: 100% !important; min-width: 0; }
}
</style>

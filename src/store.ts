import { invoke } from "@tauri-apps/api/core";
import { nextTick, reactive } from "vue";
import type { TreeOption } from "naive-ui";
import { ancestorPaths, pathChain, validateZkPath } from "./zk-path.ts";
import type {
  SearchIndexStateEvent,
  SearchIndexStatus,
  SearchResult,
  SearchResponse,
} from "./zk-search.ts";
import {
  cancelSearchIndex as cancelSearchIndexApi,
  getSearchIndexStatus as getSearchIndexStatusApi,
  searchNodes as searchNodesApi,
  startSearchIndex as startSearchIndexApi,
} from "./zk-search.ts";

export type AuthType = "none" | "digest" | "sasl_digest_md5";

export interface SavedConnection {
  id: string;
  name: string;
  servers: string;
  auth_type: AuthType;
  username: string;
  save_password: boolean;
}

export interface SaveConnectionResult {
  password_saved: boolean;
  keyring_available: boolean;
}

export interface KeyringStatus {
  available: boolean;
}

export interface NodeDetail {
  data: string;
  data_length: number;
  version: number;
  cversion: number;
  num_children: number;
  is_ephemeral: boolean;
  is_binary: boolean;
}

export interface AclEntry {
  scheme: string;
  id: string;
  perms: number;
}

export interface AclDraftEntry {
  scheme: string;
  id: string;
  permissions: number[];
}

export interface ZkTreeOption extends TreeOption {
  key: string;
  label: string;
  path: string;
  children?: ZkTreeOption[];
}

export interface EventItem {
  id: number;
  time: string;
  text: string;
}

export type ShellEntryKind = "cmd" | "out" | "err" | "info";

export interface ShellEntry {
  id: number;
  kind: ShellEntryKind;
  text: string;
}

export type BottomPanel = "events" | "shell";

export interface ConnectionTab {
  id: string;
  name: string;
  servers: string;
  authType: AuthType;
  username: string;
  savePassword: boolean;
  status: string;
  sessionId: string;
  error: string;
  tree: ZkTreeOption[];
  expandedKeys: string[];
  selectedPath: string;
  selectedNode: NodeDetail | null;
  dataDraft: string;
  saving: boolean;
  aclDraft: AclDraftEntry[];
  newAcl: AclDraftEntry;
  aclLoading: boolean;
  aclSaving: boolean;
  events: EventItem[];
  // --- 搜索状态（Rust 侧的 search directory 镜像，Vue 中仅保存轻量快照） ---
  searchIndexStatus: SearchIndexStatus | null;
  searchQuery: string;
  searchScopePath: string;
  searchResults: SearchResult[];
  searchTotalMatches: number;
  searchQuerySeq: number;
  searchLoading: boolean;
  searchError: string;
  revealSeq: number;
  // --- Shell（zkCli 命令行）：历史与输出均保存在前端，后端 shell_exec 无状态 ---
  bottomPanel: BottomPanel;
  shellEntries: ShellEntry[];
  shellHistory: string[];
  shellHistoryIndex: number;
  shellDraft: string;
  shellRunning: boolean;
}

export interface NodeEvent {
  conn_id: string;
  event_type: string;
  path: string;
  zxid: number;
}

export interface SessionStateEvent {
  conn_id: string;
  state: string;
}

export const store = reactive({
  savedConnections: [] as SavedConnection[],
  tabs: [] as ConnectionTab[],
  activeTabId: "",
});

let eventSequence = 0;
type PasswordRequester = (connection: SavedConnection) => Promise<string | null>;
let passwordRequester: PasswordRequester | null = null;
const passwordFlights = new Map<string, Promise<string | null>>();

const CONNECTION_ERROR = "无法连接，请检查地址和认证方式";
const NO_AUTH_ERROR = "权限不足：密码可能不正确，或该账号无权访问此节点";

export function setPasswordRequester(requester: PasswordRequester | null): void {
  passwordRequester = requester;
}

async function requestPassword(connection: SavedConnection): Promise<string | null> {
  const existing = passwordFlights.get(connection.id);
  if (existing) return existing;
  if (!passwordRequester) return null;
  const flight = passwordRequester(connection).finally(() => {
    passwordFlights.delete(connection.id);
  });
  passwordFlights.set(connection.id, flight);
  return flight;
}

export function formatOperationError(error: unknown): string {
  const message = String(error);
  return message.includes("NO_AUTH") ? NO_AUTH_ERROR : message;
}

function isPasswordRequired(error: unknown): boolean {
  return String(error).includes("PASSWORD_REQUIRED");
}

export function canLoadTree(status: string): boolean {
  return status === "SyncConnected" || status === "ConnectedReadOnly";
}

// 连接刚建立/重建的同步窗口内，后端句柄可能尚未就绪，这类错误会随重试自愈，不应当作失败展示
function isTransientConnectionError(error: unknown): boolean {
  const message = String(error);
  return message.includes("不存在或已断开") || message.includes("ConnectionLoss");
}

export function tabById(connId: string): ConnectionTab | undefined {
  return store.tabs.find((tab) => tab.id === connId);
}

function appendEvent(tab: ConnectionTab, text: string): void {
  tab.events.unshift({
    id: ++eventSequence,
    time: new Date().toLocaleTimeString("zh-CN", { hour12: false }),
    text,
  });
  if (tab.events.length > 200) tab.events.length = 200;
}

function childPath(parentPath: string, name: string): string {
  return parentPath === "/" ? `/${name}` : `${parentPath}/${name}`;
}

function newAclEntry(): AclDraftEntry {
  return {
    scheme: "world",
    id: "anyone",
    permissions: [1, 2, 4, 8, 16],
  };
}

function permissionValues(perms: number): number[] {
  return [1, 2, 4, 8, 16].filter((permission) =>
    Boolean(perms & permission)
  );
}

function findTreeNode(
  nodes: ZkTreeOption[],
  path: string
): ZkTreeOption | undefined {
  for (const node of nodes) {
    if (node.path === path) return node;
    if (node.children) {
      const found = findTreeNode(node.children, path);
      if (found) return found;
    }
  }
  return undefined;
}

function mergeChildren(
  node: ZkTreeOption,
  names: string[]
): ZkTreeOption[] {
  const oldChildren = new Map(
    (node.children ?? []).map((child) => [child.path, child])
  );
  return names.map((name) => {
    const path = childPath(node.path, name);
    return (
      oldChildren.get(path) ?? {
        key: path,
        label: name,
        path,
        isLeaf: false,
      }
    );
  });
}

export async function loadSavedConnections(): Promise<void> {
  store.savedConnections = await invoke<SavedConnection[]>(
    "list_saved_connections"
  );
}

export async function saveConnection(
  connection: SavedConnection,
  password?: string
): Promise<SaveConnectionResult> {
  const result = await invoke<SaveConnectionResult>("save_connection", {
    connection,
    password,
  });
  await loadSavedConnections();
  return result;
}

export async function deleteSavedConnection(id: string): Promise<void> {
  await invoke("delete_saved_connection", { id });
  await loadSavedConnections();
}

export async function refreshChildren(
  tab: ConnectionTab,
  path: string
): Promise<void> {
  const names = await invoke<string[]>("watch_children", {
    connId: tab.id,
    path,
  });
  const node = findTreeNode(tab.tree, path);
  if (node) node.children = mergeChildren(node, names);
}

export async function loadTreeNode(
  tab: ConnectionTab,
  option: TreeOption
): Promise<void> {
  // 连接未就绪时（如 connect 进行中 n-tree 自动触发 on-load）不打扰后端，
  // openConnection 会在连接成功后显式刷新根节点
  if (tab.status !== "SyncConnected" && tab.status !== "ConnectedReadOnly") {
    return;
  }
  try {
    await refreshChildren(tab, String(option.key));
  } catch (error) {
    if (isTransientConnectionError(error)) {
      appendEvent(tab, `加载子节点失败（连接同步中，已忽略）：${String(error)}`);
      return;
    }
    tab.error = formatOperationError(error);
    throw error;
  }
}

export function updateExpandedKeys(
  tab: ConnectionTab,
  keys: Array<string | number>
): void {
  tab.expandedKeys = keys.map(String);
}

export async function selectNode(
  tab: ConnectionTab,
  keys: Array<string | number>
): Promise<void> {
  const path = keys.length > 0 ? String(keys[0]) : "";
  tab.selectedPath = path;
  tab.selectedNode = null;
  tab.dataDraft = "";
  tab.aclDraft = [];
  tab.newAcl = newAclEntry();
  tab.aclLoading = Boolean(path);
  tab.error = "";
  if (!path) return;

  try {
    const detail = await invoke<NodeDetail>("watch_data", {
      connId: tab.id,
      path,
    });
    // 快速切换节点时，不让较早的请求覆盖当前详情。
    if (tab.selectedPath === path) {
      tab.selectedNode = detail;
      tab.dataDraft = detail.data;
    }
    if (tab.selectedPath !== path) return;

    const acl = await invoke<AclEntry[]>("get_acl", {
      connId: tab.id,
      path,
    });
    if (tab.selectedPath === path) {
      tab.aclDraft = acl.map((entry) => ({
        scheme: entry.scheme,
        id: entry.id,
        permissions: permissionValues(entry.perms),
      }));
    }
  } catch (error) {
    if (tab.selectedPath !== path) return;
    if (isNoNodeError(error)) {
      // 节点刚被删除（本端或外部），静默取消选中而不是报错
      tab.selectedPath = "";
      tab.selectedNode = null;
      tab.dataDraft = "";
      tab.aclDraft = [];
      tab.newAcl = newAclEntry();
      tab.aclLoading = false;
    } else if (isTransientConnectionError(error)) {
      // 重连窗口内的瞬态失败，仅记录事件，等待会话恢复后用户重新选择
      appendEvent(tab, `读取节点详情失败（连接同步中，已忽略）：${String(error)}`);
    } else {
      tab.error = formatOperationError(error);
    }
  } finally {
    if (tab.selectedPath === path) tab.aclLoading = false;
  }
}

export function addAclEntry(tab: ConnectionTab): void {
  const entry = tab.newAcl;
  if (!entry.scheme || (entry.scheme !== "auth" && !entry.id.trim())) {
    tab.error = "ACL 的 scheme 和 id 不能为空";
    return;
  }
  if (entry.permissions.length === 0) {
    tab.error = "请至少选择一项 ACL 权限";
    return;
  }
  tab.aclDraft.push({
    scheme: entry.scheme,
    id: entry.id.trim(),
    permissions: [...entry.permissions],
  });
  tab.newAcl = newAclEntry();
  tab.error = "";
}

export function removeAclEntry(tab: ConnectionTab, index: number): void {
  tab.aclDraft.splice(index, 1);
}

export async function saveNodeAcl(tab: ConnectionTab): Promise<void> {
  if (!tab.selectedPath) return;
  if (tab.aclDraft.length === 0) {
    tab.error = "ACL 列表不能为空";
    return;
  }
  const path = tab.selectedPath;
  tab.aclSaving = true;
  tab.error = "";
  try {
    await invoke("set_acl", {
      connId: tab.id,
      path,
      // set_acl 是整体替换，始终提交界面中的完整列表。
      acl: tab.aclDraft.map((entry) => ({
        scheme: entry.scheme,
        id: entry.id,
        perms: entry.permissions.reduce(
          (sum, permission) => sum | permission,
          0
        ),
      })),
    });
    appendEvent(tab, `已保存 ACL：${path}`);
  } catch (error) {
    tab.error = formatOperationError(error);
  } finally {
    tab.aclSaving = false;
  }
}

export async function createChildNode(
  tab: ConnectionTab,
  parentPath: string,
  name: string,
  data: string
): Promise<void> {
  await invoke("create_node", {
    connId: tab.id,
    parentPath,
    name,
    data,
  });
  appendEvent(tab, `已新建节点：${childPath(parentPath, name)}`);
  // 本端创建会使搜索快照变脏（后端已 mark_dirty，未建索引时为 no-op）
  if (tab.searchIndexStatus) {
    tab.searchIndexStatus.dirty = true;
  }
}

export async function listNodeChildren(
  tab: ConnectionTab,
  path: string
): Promise<string[]> {
  return invoke<string[]>("list_children", { connId: tab.id, path });
}

function isNoNodeError(error: unknown): boolean {
  return String(error).includes("node not exists");
}

/** 节点删除后选择最近仍存在的祖先，兼容外部递归删除的事件延迟。 */
async function selectAncestorAfterDelete(
  tab: ConnectionTab,
  deletedPath: string
): Promise<void> {
  for (const candidate of ancestorPaths(deletedPath)) {
    await selectNode(tab, [candidate]);
    if (tab.selectedNode) return;
    // NoNode 会清空 selectedPath；其他错误保留路径和错误信息，不应继续回溯。
    if (tab.selectedPath) return;
  }
}

export async function deleteTreeNode(
  tab: ConnectionTab,
  path: string,
  recursive: boolean
): Promise<number> {
  // 删除期间 watcher 可能改变 selectedPath，必须在请求前快照回选意图。
  const shouldSelectParent =
    tab.selectedPath === path || tab.selectedPath.startsWith(`${path}/`);
  const deleted = recursive
    ? await invoke<number>("delete_node_recursive", { connId: tab.id, path })
    : await invoke("delete_node", { connId: tab.id, path }).then(() => 1);
  appendEvent(tab, `已删除节点：${path}（共 ${deleted} 个）`);
  if (tab.searchIndexStatus) {
    tab.searchIndexStatus.dirty = true;
  }
  if (shouldSelectParent) {
    await selectAncestorAfterDelete(tab, path);
  }
  return deleted;
}

export async function saveNodeData(tab: ConnectionTab): Promise<void> {
  if (!tab.selectedPath) return;
  // 二进制数据禁止保存：展示层是 UTF-8 有损转换，保存会损坏原始字节。
  if (tab.selectedNode?.is_binary) {
    tab.error = "二进制数据不支持编辑保存";
    return;
  }
  tab.saving = true;
  tab.error = "";
  try {
    await invoke("set_data", {
      connId: tab.id,
      path: tab.selectedPath,
      data: tab.dataDraft,
    });
    appendEvent(tab, `已保存节点数据：${tab.selectedPath}`);
    await selectNode(tab, [tab.selectedPath]);
  } catch (error) {
    tab.error = formatOperationError(error);
  } finally {
    tab.saving = false;
  }
}

export function formatJsonDraft(tab: ConnectionTab): void {
  try {
    tab.dataDraft = JSON.stringify(JSON.parse(tab.dataDraft), null, 2);
    tab.error = "";
  } catch {
    tab.error = "当前内容不是合法 JSON，无法格式化";
  }
}

export async function openConnection(
  connection: SavedConnection,
  suppliedPassword?: string
): Promise<void> {
  let tab = tabById(connection.id);
  if (!tab) {
    tab = reactive<ConnectionTab>({
      id: connection.id,
      name: connection.name,
      servers: connection.servers,
      authType: connection.auth_type,
      username: connection.username,
      savePassword: connection.save_password,
      status: "Disconnected",
      sessionId: "",
      error: "",
      tree: [
        {
          key: "/",
          label: "/",
          path: "/",
          isLeaf: false,
        },
      ],
      expandedKeys: ["/"],
      selectedPath: "",
      selectedNode: null,
      dataDraft: "",
      saving: false,
      aclDraft: [],
      newAcl: newAclEntry(),
      aclLoading: false,
      aclSaving: false,
      events: [],
      searchIndexStatus: null,
      searchQuery: "",
      searchScopePath: "/",
      searchResults: [],
      searchTotalMatches: 0,
      searchQuerySeq: 0,
      searchLoading: false,
      searchError: "",
      revealSeq: 0,
      bottomPanel: "events",
      shellEntries: [],
      shellHistory: [],
      shellHistoryIndex: -1,
      shellDraft: "",
      shellRunning: false,
    });
    store.tabs.push(tab);
  }
  store.activeTabId = tab.id;
  if (
    tab.status === "SyncConnected" ||
    tab.status === "ConnectedReadOnly" ||
    tab.status === "Connecting"
  ) {
    return;
  }

  tab.status = "Connecting";
  tab.error = "";
  tab.name = connection.name;
  tab.servers = connection.servers;
  tab.authType = connection.auth_type;
  tab.username = connection.username;
  tab.savePassword = connection.save_password;
  tab.tree = [
    {
      key: "/",
      label: "/",
      path: "/",
      isLeaf: false,
    },
  ];
  tab.expandedKeys = ["/"];
  tab.selectedPath = "";
  tab.selectedNode = null;
  tab.dataDraft = "";
  tab.aclDraft = [];
  tab.newAcl = newAclEntry();
  tab.aclLoading = false;
  // 连接重置时同步重置搜索状态（epoch 已变化，旧快照失效）
  tab.searchIndexStatus = null;
  tab.searchQuery = "";
  tab.searchResults = [];
  tab.searchTotalMatches = 0;
  tab.searchQuerySeq = 0;
  tab.searchLoading = false;
  tab.searchError = "";
  tab.revealSeq += 1;
  appendEvent(tab, `正在连接 ${connection.servers}`);
  const connectWithPassword = (password?: string) =>
    invoke<{ session_id: string }>("connect", {
      request: {
        connId: tab.id,
        servers: tab.servers,
        authType: tab.authType,
        username: tab.username,
        savePassword: tab.savePassword,
        password,
      },
    });

  let result: { session_id: string };
  try {
    try {
      result = await connectWithPassword(suppliedPassword);
    } catch (error) {
      if (!isPasswordRequired(error)) throw error;
      const password = await requestPassword(connection);
      if (password === null) {
        tab.status = "WaitingForManualReconnect";
        appendEvent(tab, "已取消密码输入，等待手动重连");
        return;
      }
      result = await connectWithPassword(password);
    }
  } catch (error) {
    tab.status = "Disconnected";
    tab.error = CONNECTION_ERROR;
    appendEvent(tab, `连接失败：${CONNECTION_ERROR}`);
    return;
  }

  tab.sessionId = result.session_id;
  appendEvent(tab, `连接成功，session id = ${result.session_id}`);
  // 刚连上时后端句柄可能有同步延迟，瞬态失败做有限重试，不以红条误导用户
  for (let attempt = 1; ; attempt++) {
    try {
      await refreshChildren(tab, "/");
      break;
    } catch (error) {
      if (isTransientConnectionError(error)) {
        if (attempt < 3 && tab.status !== "Disconnected") {
          await new Promise((resolve) => setTimeout(resolve, 300));
          continue;
        }
        appendEvent(
          tab,
          `读取根节点失败（连接同步中，已重试 ${attempt} 次）：${String(error)}`
        );
      } else {
        tab.error = formatOperationError(error);
        appendEvent(tab, `读取根节点失败：${tab.error}`);
      }
      break;
    }
  }
}

export async function disconnectConnection(tab: ConnectionTab): Promise<void> {
  tab.error = "";
  try {
    await invoke("disconnect", { connId: tab.id });
    appendEvent(tab, "已主动断开连接");
  } catch (error) {
    tab.error = formatOperationError(error);
  } finally {
    tab.status = "Disconnected";
    tab.sessionId = "";
    tab.searchIndexStatus = null;
    tab.searchResults = [];
    tab.searchTotalMatches = 0;
    tab.searchError = "";
    tab.revealSeq += 1;
  }
}

export async function closeTab(connId: string): Promise<void> {
  const tab = tabById(connId);
  if (tab) await disconnectConnection(tab);
  const index = store.tabs.findIndex((item) => item.id === connId);
  if (index >= 0) store.tabs.splice(index, 1);
  if (store.activeTabId === connId) {
    store.activeTabId = store.tabs[Math.max(0, index - 1)]?.id ?? "";
  }
}

export async function handleSessionState(
  event: SessionStateEvent
): Promise<void> {
  const tab = tabById(event.conn_id);
  if (!tab) return;
  tab.status = event.state;
  appendEvent(tab, `会话状态：${event.state}`);
  if (event.state === "Expired") {
    tab.sessionId = "";
    tab.searchIndexStatus = null;
    tab.revealSeq += 1;
  }
  if (event.state === "ReconnectFailed") {
    tab.error = CONNECTION_ERROR;
  }
  if (event.state === "Closed" || event.state === "AuthFailed") {
    tab.searchIndexStatus = null;
    tab.revealSeq += 1;
  }
}

export async function handleNodeEvent(event: NodeEvent): Promise<void> {
  const tab = tabById(event.conn_id);
  if (!tab) return;
  appendEvent(
    tab,
    `节点事件：${event.event_type} path=${event.path} zxid=${event.zxid}`
  );

  try {
    if (event.event_type === "NodeChildrenChanged") {
      // ZooKeeper watcher 是一次性的；折叠期间也要刷新并重新注册，避免再次展开时数据陈旧。
      await refreshChildren(tab, event.path);
    }
    if (
      event.event_type === "NodeDataChanged" &&
      tab.selectedPath === event.path
    ) {
      await selectNode(tab, [event.path]);
    }
    if (
      event.event_type === "NodeDeleted" &&
      tab.selectedPath === event.path
    ) {
      // 外部递归删除可能已移除多级父节点，回溯到最近仍存在的祖先。
      await selectAncestorAfterDelete(tab, event.path);
    }
  } catch (error) {
    if (isTransientConnectionError(error)) {
      appendEvent(tab, `节点事件刷新失败（已忽略）：${String(error)}`);
    } else {
      tab.error = formatOperationError(error);
    }
  }
}

// ── 搜索：索引生命周期与本地查询 ───────────────────────────────

function isNoAuthError(error: unknown): boolean {
  return String(error).includes("NO_AUTH");
}

export function handleSearchIndexState(event: SearchIndexStateEvent): void {
  const tab = tabById(event.conn_id);
  if (!tab) return;
  const prev = tab.searchIndexStatus;
  // 后端事件不含 built_at_ms/refreshing，基于 prev 做乐观更新，最终以 refreshSearchStatus 校准
  const isBuilding = event.state === "building";
  const hadSnapshot = prev !== null && prev.state !== "empty";
  tab.searchIndexStatus = {
    connection_epoch: event.connection_epoch,
    generation: event.generation,
    state: event.state,
    dirty: event.dirty,
    refreshing: isBuilding && hadSnapshot,
    built_at_ms: prev?.built_at_ms ?? null,
    stats: event.stats,
  } as SearchIndexStatus;
  if (event.state !== "building") {
    // 完成/失败态，刷新一次权威状态以拿到 built_at_ms 与 refreshing
    void refreshSearchStatus(tab).catch(() => {});
  }
  const stateLabel: Record<string, string> = {
    building: "构建中",
    ready: "就绪",
    incomplete: "部分就绪",
    truncated: "已截断",
    failed: "失败",
    cancelled: "已取消",
    empty: "未构建",
  };
  appendEvent(
    tab,
    `搜索索引：${stateLabel[event.state] ?? event.state}${event.dirty ? "（已过期）" : ""}`
  );
}

export async function refreshSearchStatus(tab: ConnectionTab): Promise<void> {
  try {
    const status = await getSearchIndexStatusApi(tab.id);
    tab.searchIndexStatus = status;
  } catch (error) {
    // 连接不存在时后端返回错误，保留 null
    if (String(error).includes("不存在或已断开")) {
      tab.searchIndexStatus = null;
    }
  }
}

export async function startSearchIndex(
  tab: ConnectionTab,
  force = false
): Promise<void> {
  tab.searchError = "";
  try {
    const ticket = await startSearchIndexApi(tab.id, force);
    // 乐观更新为 building，等待事件与 status 轮询校准
    const prev = tab.searchIndexStatus;
    tab.searchIndexStatus = {
      connection_epoch: ticket.connection_epoch,
      generation: ticket.generation,
      state: ticket.state,
      dirty: prev?.dirty ?? false,
      refreshing: ticket.state === "building" && prev !== null && prev.state !== "empty",
      built_at_ms: prev?.built_at_ms ?? null,
      stats: prev?.stats ?? {
        visited: 0,
        inaccessible_subtrees: 0,
        skipped_nodes: 0,
        elapsed_ms: 0,
        termination_reason: null,
      },
    };
    appendEvent(
      tab,
      ticket.state === "building" ? "搜索索引：开始构建" : "搜索索引：无需重建"
    );
    // 拉取权威状态
    await refreshSearchStatus(tab);
  } catch (error) {
    tab.searchError = formatOperationError(error);
    appendEvent(tab, `搜索索引启动失败：${tab.searchError}`);
    throw error;
  }
}

export async function cancelSearchIndex(tab: ConnectionTab): Promise<void> {
  const status = tab.searchIndexStatus;
  if (!status) return;
  try {
    await cancelSearchIndexApi(tab.id, status.connection_epoch, status.generation);
    appendEvent(tab, "搜索索引：已取消构建");
    await refreshSearchStatus(tab);
  } catch (error) {
    tab.searchError = formatOperationError(error);
  }
}

export async function searchInTab(
  tab: ConnectionTab,
  query: string,
  limit = 50
): Promise<void> {
  const trimmed = query.trim();
  tab.searchQuery = query;
  if (trimmed.length === 0) {
    tab.searchResults = [];
    tab.searchTotalMatches = 0;
    tab.searchError = "";
    tab.searchLoading = false;
    return;
  }
  // 作用域校验
  if (!validateZkPath(tab.searchScopePath)) {
    tab.searchError = "搜索范围不是合法的 ZooKeeper 路径";
    return;
  }
  if (trimmed.includes("/") && !validateZkPath(trimmed)) {
    tab.searchError = "路径查询不是合法的 ZooKeeper 路径";
    return;
  }
  const status = tab.searchIndexStatus;
  if (!status) {
    tab.searchError = "索引未就绪，请先构建";
    return;
  }
  const querySeq = ++tab.searchQuerySeq;
  tab.searchLoading = true;
  tab.searchError = "";
  try {
    const response: SearchResponse = await searchNodesApi({
      connId: tab.id,
      connectionEpoch: status.connection_epoch,
      querySeq,
      query: trimmed,
      scopePath: tab.searchScopePath,
      limit,
    });
    // 拒绝晚到的旧响应
    if (querySeq !== tab.searchQuerySeq) return;
    // 拒绝 epoch 已变化的响应
    if (response.connection_epoch !== tab.searchIndexStatus?.connection_epoch) return;
    tab.searchResults = response.results;
    tab.searchTotalMatches = response.total_matches;
    // 同步 dirty 与 snapshot_status
    if (tab.searchIndexStatus) {
      tab.searchIndexStatus.dirty = response.dirty;
    }
  } catch (error) {
    if (querySeq !== tab.searchQuerySeq) return;
    const msg = String(error);
    if (msg.includes("INDEX_NOT_READY")) {
      tab.searchError = "索引未就绪，请先构建";
    } else if (msg.includes("SEARCH_CONNECTION_CHANGED")) {
      tab.searchError = "连接已变化，索引已失效，请重建";
      await refreshSearchStatus(tab);
    } else {
      tab.searchError = formatOperationError(error);
    }
    tab.searchResults = [];
    tab.searchTotalMatches = 0;
  } finally {
    if (querySeq === tab.searchQuerySeq) {
      tab.searchLoading = false;
    }
  }
}

// ── revealPath：按祖先逐层加载并滚动 ─────────────────────────────

function isNoNodeLike(error: unknown): boolean {
  return String(error).includes("node not exists") || String(error).includes("NoNode");
}

export async function revealPath(
  tab: ConnectionTab,
  targetPath: string
): Promise<boolean> {
  if (!validateZkPath(targetPath)) {
    tab.error = "目标路径不是合法的 ZooKeeper 路径";
    return false;
  }
  const revealSeq = (tab.revealSeq ?? 0) + 1;
  tab.revealSeq = revealSeq;
  // 捕获 epoch
  let epoch = tab.searchIndexStatus?.connection_epoch ?? 0;
  if (epoch === 0) {
    try {
      const s = await getSearchIndexStatusApi(tab.id);
      tab.searchIndexStatus = s;
      epoch = s.connection_epoch;
    } catch {
      epoch = 0;
    }
  }
  const chain = pathChain(targetPath);
  // 逐层确保祖先存在
  for (let i = 1; i < chain.length; i++) {
    if (tab.revealSeq !== revealSeq) return false;
    if (epoch !== 0) {
      try {
        const cur = await getSearchIndexStatusApi(tab.id);
        if (cur.connection_epoch !== epoch) return false;
      } catch {
        // 状态获取失败不阻断，仅在 epoch 明确变化时阻断
      }
    }
    const desired = chain[i];
    const parent = chain[i - 1];
    const existing = findTreeNode(tab.tree, desired);
    if (existing) {
      if (!tab.expandedKeys.includes(parent)) {
        tab.expandedKeys = [...tab.expandedKeys, parent];
      }
      continue;
    }
    // 父节点未展开或子节点未加载，刷新父节点
    try {
      await refreshChildren(tab, parent);
    } catch (error) {
      if (isNoNodeLike(error) || isNoAuthError(error)) {
        tab.error = formatOperationError(error);
        return false;
      }
      tab.error = formatOperationError(error);
      return false;
    }
    // 刷新后再次确认
    if (!findTreeNode(tab.tree, desired)) {
      tab.error = `路径不存在：${desired}`;
      return false;
    }
    if (!tab.expandedKeys.includes(parent)) {
      tab.expandedKeys = [...tab.expandedKeys, parent];
    }
    if (tab.revealSeq !== revealSeq) return false;
  }
  // 确保所有祖先已展开
  const ancestors = chain.slice(0, -1);
  const expandedSet = new Set(tab.expandedKeys);
  for (const p of ancestors) expandedSet.add(p);
  tab.expandedKeys = Array.from(expandedSet);
  // 选中目标
  await selectNode(tab, [targetPath]);
  await nextTick();
  return true;
}

// ── Shell（zkCli 命令行） ─────────────────────────────────────

let shellEntrySeq = 0;

function appendShellEntry(
  tab: ConnectionTab,
  kind: ShellEntryKind,
  text: string
): void {
  tab.shellEntries.push({ id: ++shellEntrySeq, kind, text });
  if (tab.shellEntries.length > 500) {
    tab.shellEntries.splice(0, tab.shellEntries.length - 500);
  }
}

/** 会改变树结构或数据的命令：执行成功后同步搜索脏标记并记入事件流。 */
const SHELL_MUTATING = /^(create|delete|deleteall|rmr|set|setacl|addauth)\b/i;
const SHELL_TREE_MUTATING = /^(create|delete|deleteall|rmr)\b/i;

export async function runShellCommand(tab: ConnectionTab): Promise<void> {
  const line = tab.shellDraft.trim();
  if (!line || tab.shellRunning) return;
  appendShellEntry(tab, "cmd", `[zk: ${tab.servers}] ${line}`);
  if (tab.shellHistory[tab.shellHistory.length - 1] !== line) {
    tab.shellHistory.push(line);
    if (tab.shellHistory.length > 200) tab.shellHistory.shift();
  }
  tab.shellHistoryIndex = -1;
  tab.shellDraft = "";

  const command = line.split(/\s+/)[0].toLowerCase();
  // 本地命令：不依赖连接
  if (command === "clear") {
    tab.shellEntries = [];
    return;
  }
  if (command === "history") {
    tab.shellHistory.forEach((item, index) =>
      appendShellEntry(tab, "out", `${index} - ${item}`)
    );
    return;
  }
  if (command === "quit" || command === "close") {
    appendShellEntry(tab, "info", "正在断开连接…");
    await disconnectConnection(tab);
    return;
  }
  if (!canLoadTree(tab.status)) {
    appendShellEntry(tab, "err", "连接未就绪，无法执行命令");
    return;
  }

  tab.shellRunning = true;
  try {
    const output = await invoke<string>("shell_exec", { connId: tab.id, line });
    if (output) {
      for (const outputLine of output.split("\n")) {
        appendShellEntry(tab, "out", outputLine);
      }
    }
    if (SHELL_MUTATING.test(line)) {
      appendEvent(tab, `Shell 执行：${line}`);
      // 树结构变化使搜索快照过期（后端已 mark_dirty，未建索引时为 no-op）
      if (SHELL_TREE_MUTATING.test(line) && tab.searchIndexStatus) {
        tab.searchIndexStatus.dirty = true;
      }
    }
  } catch (error) {
    appendShellEntry(tab, "err", formatOperationError(error));
  } finally {
    tab.shellRunning = false;
  }
}

/** ↑/↓ 翻阅命令历史：向下越过最后一条时恢复空输入。 */
export function shellHistoryNavigate(
  tab: ConnectionTab,
  direction: 1 | -1
): void {
  const length = tab.shellHistory.length;
  if (length === 0) return;
  if (tab.shellHistoryIndex === -1) {
    if (direction === 1) return;
    tab.shellHistoryIndex = length - 1;
  } else {
    tab.shellHistoryIndex += direction;
    if (tab.shellHistoryIndex >= length) {
      tab.shellHistoryIndex = -1;
      tab.shellDraft = "";
      return;
    }
    if (tab.shellHistoryIndex < 0) tab.shellHistoryIndex = 0;
  }
  tab.shellDraft = tab.shellHistory[tab.shellHistoryIndex];
}

